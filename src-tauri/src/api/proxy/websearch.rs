//! WebSearch 劫持：把 server-side web_search 改写为自定义 tool，
//! 在代理内本地跑 tool-calling loop（Bing 搜索），累积内容转 SSE 返回。
//!
//! IR 版本：所有格式操作通过 IR 中间表示，支持三种客户端格式 × 三种上游格式。

use std::sync::Arc;
use std::time::Duration;

use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use bytes::Bytes;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::sync::oneshot;

use crate::gateway::server::AppState;

use super::auth::ServiceKeyInfo;
use super::ir;
use super::ir::types::*;
use super::key_rotation::{pick_key_for, update_key_health};
use super::route::ResolvedRoute;
use super::stream::{send_error_event, ClientFormat};

/// IR 请求的 tools 里是否含 server-side web_search 工具。
pub(super) fn has_websearch_tool_ir(req: &IrRequest) -> bool {
    req.tools
        .iter()
        .any(|t| t.name.starts_with("web_search"))
}

/// 把 Bing 结果格式化成喂给 LLM 的 tool_result 文本。
fn format_search_text(results: &[crate::search::SearchResult]) -> String {
    results
        .iter()
        .enumerate()
        .map(|(i, r)| format!("[{}] {}\n{}\n{}", i + 1, r.title, r.url, r.snippet))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// WebSearch 劫持 loop 入口：操作 IR，支持三种客户端格式。
///
/// 与主代理路径一致：立即返回 Response（含 `:keepalive` 首字节），hijack loop
/// 在后台 spawn 完成。上游错误通过 SSE error event 传达。
pub(super) async fn run_websearch_loop(
    state: Arc<AppState>,
    ir_request: IrRequest,
    resolved: ResolvedRoute,
    client_format: ClientFormat,
    trace_id: String,
    service_key: ServiceKeyInfo,
) -> Result<Response, (StatusCode, HeaderMap, Json<Value>)> {
    use std::convert::Infallible;

    let (tx, rx) = mpsc::channel::<Result<Bytes, Infallible>>(100);

    // 初始 keepalive
    let _ = tx.send(Ok(Bytes::from(":keepalive\n\n"))).await;

    tokio::spawn(async move {
        let trace_id = &trace_id;

        // ── keepalive 心跳 + 取消信号 ─────────────────────────────
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
        let keepalive_tx = tx.clone();
        let keepalive_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(super::stream::SSE_KEEPALIVE_SECS));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if keepalive_tx.send(Ok(Bytes::from(":keepalive\n\n"))).await.is_err() {
                            break;
                        }
                    }
                    _ = &mut cancel_rx => break,
                }
            }
        });
        struct CancelOnDrop(Option<oneshot::Sender<()>>);
        impl Drop for CancelOnDrop {
            fn drop(&mut self) {
                if let Some(tx) = self.0.take() {
                    let _ = tx.send(());
                }
            }
        }
        let _cancel_guard = CancelOnDrop(Some(cancel_tx));
        let _keepalive_handle = keepalive_handle;

        let picked = match pick_key_for(&state, &resolved.provider_id) {
            Some(p) => p,
            None => {
                send_error_event(&tx, client_format, "api_error", "No available upstream keys");
                return;
            }
        };

        let client = crate::http::build_http_client()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap();

        let upstream_url = resolved.upstream_url.clone();
        let model = resolved.real_model_id.clone();
        let max_tokens = ir_request.max_tokens.unwrap_or(4096);
        let provider_kind = resolved.provider_kind.clone();
        let model_display_name = ir_request.model.clone();

        // 构建 web_search 工具定义（IR 格式）
        let web_search_tool = IrTool {
            name: "web_search".to_string(),
            description: Some("Search the web (Bing) for up-to-date information.".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {"query": {"type": "string", "description": "The search query"}},
                "required": ["query"]
            }),
        };

        // 构建劫持用的 IR 请求（替换 tools 为 web_search）
        let mut hijack_ir = ir_request.clone();
        hijack_ir.tools = vec![web_search_tool];
        hijack_ir.tool_choice = Some(IrToolChoice::Auto);
        hijack_ir.stream = false;

        let mut accumulated: Vec<IrContentBlock> = Vec::new();
        let mut final_stop = IrStopReason::EndTurn;
        let mut accum_usage = IrUsage::default();

        let outcome: Result<(), (String, String)> = hijack_upstream(
            &client,
            &upstream_url,
            &model,
            &picked.key_hash,
            &provider_kind,
            &hijack_ir,
            max_tokens,
            &state.keys,
            &resolved.provider_id,
            &mut accumulated,
            &mut final_stop,
            &mut accum_usage,
        )
        .await;

        if let Err((err_type, msg)) = outcome {
            send_error_event(&tx, client_format, &err_type, &msg);
            return;
        }

        let _ = state.database.insert_usage_log(
            chrono::Utc::now().timestamp(),
            &resolved.provider_id,
            resolved.provider_name.as_str(),
            &resolved.model_row_id,
            &model_display_name,
            Some(&picked.id),
            picked.name.as_str(),
            picked.key_masked.as_str(),
            Some(service_key.id.as_str()),
            service_key.name.as_str(),
            service_key.key_masked.as_str(),
            match client_format {
                ClientFormat::Messages => "/v1/messages",
                ClientFormat::ChatCompletions => "/v1/chat/completions",
                ClientFormat::Responses => "/v1/responses",
            },
            accum_usage.input_tokens as i64,
            accum_usage.output_tokens as i64,
            0,
            true,
            None,
            accum_usage.cache_read_input_tokens as i64,
        );

        // 渲染累积内容为客户端格式 SSE
        let segments = render_accumulated_ir(
            client_format,
            &accumulated,
            final_stop,
            &accum_usage,
        );
        for seg in segments {
            if tx.send(Ok(seg)).await.is_err() {
                break;
            }
        }
    });

    Ok(super::stream::sse_response(rx))
}

/// 统一上游劫持 loop：根据 provider_kind 从 IR 生成上游请求，
/// 解析上游非流式响应为 IR content blocks，累积到 accumulated。
async fn hijack_upstream(
    client: &reqwest::Client,
    upstream_url: &str,
    model: &str,
    api_key: &str,
    provider_kind: &str,
    ir_request: &IrRequest,
    max_tokens: u64,
    pool: &crate::keys::KeyPool,
    provider_id: &str,
    accumulated: &mut Vec<IrContentBlock>,
    final_stop: &mut IrStopReason,
    accum_usage: &mut IrUsage,
) -> Result<(), (String, String)> {
    // 构建对话消息（可变，每轮追加 tool_result）
    let mut messages = ir_request.messages.clone();

    for _ in 0..5 {
        // 从 IR 生成上游请求体
        let mut req_body = match provider_kind {
            "messages" => ir::to_messages::ir_req_to_messages(ir_request),
            "responses" => ir::to_responses::ir_req_to_responses(ir_request),
            _ => ir::to_chat_completions::ir_req_to_chat_completions(ir_request),
        };

        // 替换 messages 为当前对话状态
        match provider_kind {
            "messages" => {
                let msgs: Vec<Value> = messages
                    .iter()
                    .map(|m| {
                        let role = match m.role {
                            IrRole::User => "user",
                            IrRole::Assistant => "assistant",
                        };
                        let content = ir_content_to_messages_value(&m.content);
                        json!({"role": role, "content": content})
                    })
                    .collect();
                req_body["messages"] = json!(msgs);
                req_body["stream"] = json!(false);
                req_body["max_tokens"] = json!(max_tokens);
            }
            "responses" => {
                // Responses 格式较复杂，简化处理
                req_body["stream"] = json!(false);
                if let Some(mt) = req_body.get_mut("max_output_tokens") {
                    *mt = json!(max_tokens);
                }
            }
            _ => {
                let msgs: Vec<Value> = ir_messages_to_chat_completions_value(&messages);
                req_body["messages"] = json!(msgs);
                req_body["stream"] = json!(false);
                req_body["max_tokens"] = json!(max_tokens);
            }
        }

        // 发送请求
        let mut req_builder = client.post(upstream_url);
        if provider_kind == "messages" {
            req_builder = req_builder
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01");
        } else {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", api_key));
        }

        let resp = req_builder
            .header("Content-Type", "application/json")
            .json(&req_body)
            .send()
            .await
            .map_err(|e| ("api_error".to_string(), e.to_string()))?;

        let status = resp.status().as_u16();
        let msg_val: Value = resp
            .json()
            .await
            .map_err(|e| ("api_error".to_string(), e.to_string()))?;

        if status >= 400 {
            update_key_health(pool, provider_id, api_key, status);
            let msg = msg_val["error"]["message"]
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("upstream error {}", status));
            return Err(("api_error".to_string(), msg));
        }
        update_key_health(pool, provider_id, api_key, status);

        // 解析上游响应为 IR content blocks
        let (content_blocks, stop_reason, usage) = match provider_kind {
            "messages" => parse_anthropic_response(&msg_val),
            "responses" => parse_responses_response(&msg_val),
            _ => parse_chat_response(&msg_val),
        };

        // 累积 usage
        accum_usage.input_tokens += usage.input_tokens;
        accum_usage.output_tokens += usage.output_tokens;
        accum_usage.cache_read_input_tokens += usage.cache_read_input_tokens;

        // 累积 content blocks
        accumulated.extend(content_blocks.clone());

        // 检查是否需要继续 tool-calling loop
        if stop_reason != IrStopReason::ToolUse {
            *final_stop = stop_reason;
            break;
        }

        // 提取 web_search tool_use 的 (id, query) — 在 move content_blocks 之前
        let ws_calls: Vec<(String, String)> = content_blocks
            .iter()
            .filter_map(|b| {
                if let IrContentBlock::ToolUse { id, name, input } = b {
                    if name == "web_search" {
                        let query = input["query"].as_str().unwrap_or("").to_string();
                        return Some((id.clone(), query));
                    }
                }
                None
            })
            .collect();

        if ws_calls.is_empty() {
            *final_stop = stop_reason;
            break;
        }

        // 追加 assistant 消息（含 tool_use）
        messages.push(IrMessage {
            role: IrRole::Assistant,
            content: content_blocks,
        });

        // 执行 Bing 搜索并构建 tool_result
        let mut tool_results: Vec<IrContentBlock> = Vec::new();
        for (id, query) in &ws_calls {
            let bing = crate::search::bing::search(query).await.unwrap_or_default();
            let search_text = format_search_text(&bing);

            // 累积 web_search_tool_result（给客户端看的）
            accumulated.push(IrContentBlock::ToolResult {
                tool_use_id: id.clone(),
                content: IrToolResultContent::Text(search_text.clone()),
                is_error: false,
            });

            // tool_result 消息（给上游下一轮用的）
            tool_results.push(IrContentBlock::ToolResult {
                tool_use_id: id.clone(),
                content: IrToolResultContent::Text(search_text),
                is_error: false,
            });
        }

        messages.push(IrMessage {
            role: IrRole::User,
            content: tool_results,
        });
    }

    Ok(())
}

/// 解析 Anthropic 非流式响应为 IR content blocks。
fn parse_anthropic_response(msg: &Value) -> (Vec<IrContentBlock>, IrStopReason, IrUsage) {
    let stop = msg["stop_reason"]
        .as_str()
        .map(IrStopReason::from_messages)
        .unwrap_or(IrStopReason::EndTurn);

    let content = msg["content"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|b| {
                    match b["type"].as_str()? {
                        "text" => Some(IrContentBlock::Text {
                            text: b["text"].as_str().unwrap_or("").to_string(),
                            cache_control: None,
                        }),
                        "tool_use" => Some(IrContentBlock::ToolUse {
                            id: b["id"].as_str().unwrap_or("").to_string(),
                            name: b["name"].as_str().unwrap_or("").to_string(),
                            input: b.get("input").cloned().unwrap_or(Value::Null),
                        }),
                        "thinking" => Some(IrContentBlock::Thinking {
                            thinking: b["thinking"].as_str().unwrap_or("").to_string(),
                            signature: None,
                        }),
                        _ => None,
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let usage = IrUsage {
        input_tokens: msg["usage"]["input_tokens"].as_u64().unwrap_or(0)
            + msg["usage"]["cache_creation_input_tokens"]
                .as_u64()
                .unwrap_or(0),
        output_tokens: msg["usage"]["output_tokens"].as_u64().unwrap_or(0),
        cache_read_input_tokens: msg["usage"]["cache_read_input_tokens"]
            .as_u64()
            .unwrap_or(0),
        ..Default::default()
    };

    (content, stop, usage)
}

/// 解析 Chat Completions 非流式响应为 IR content blocks。
fn parse_chat_response(msg: &Value) -> (Vec<IrContentBlock>, IrStopReason, IrUsage) {
    let choice = &msg["choices"][0];
    let finish = choice["finish_reason"]
        .as_str()
        .map(IrStopReason::from_chat_completions)
        .unwrap_or(IrStopReason::EndTurn);

    let mut content: Vec<IrContentBlock> = Vec::new();

    // reasoning_content
    if let Some(rc) = choice["message"]["reasoning_content"].as_str() {
        if !rc.is_empty() {
            content.push(IrContentBlock::Thinking {
                thinking: rc.to_string(),
                signature: None,
            });
        }
    }

    // text content
    if let Some(text) = choice["message"]["content"].as_str() {
        if !text.is_empty() {
            content.push(IrContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            });
        }
    }

    // tool_calls
    if let Some(tool_calls) = choice["message"]["tool_calls"].as_array() {
        for tc in tool_calls {
            let id = tc["id"].as_str().unwrap_or("").to_string();
            let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
            let arguments = tc["function"]["arguments"].as_str().unwrap_or("{}");
            let input: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
            content.push(IrContentBlock::ToolUse { id, name, input });
        }
    }

    let usage = IrUsage {
        input_tokens: msg["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
        output_tokens: msg["usage"]["completion_tokens"].as_u64().unwrap_or(0),
        cache_read_input_tokens: msg["usage"]
            .get("prompt_cache_hit_tokens")
            .or_else(|| {
                msg["usage"]
                    .get("prompt_tokens_details")
                    .and_then(|d| d.get("cached_tokens"))
            })
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        ..Default::default()
    };

    (content, finish, usage)
}

/// 解析 Responses API 非流式响应为 IR content blocks。
fn parse_responses_response(msg: &Value) -> (Vec<IrContentBlock>, IrStopReason, IrUsage) {
    let mut content: Vec<IrContentBlock> = Vec::new();

    if let Some(output) = msg.get("output").and_then(|v| v.as_array()) {
        for item in output {
            match item["type"].as_str().unwrap_or("") {
                "message" => {
                    if let Some(parts) = item.get("content").and_then(|v| v.as_array()) {
                        for part in parts {
                            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                content.push(IrContentBlock::Text {
                                    text: text.to_string(),
                                    cache_control: None,
                                });
                            }
                        }
                    }
                }
                "function_call" => {
                    let id = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = item["name"].as_str().unwrap_or("").to_string();
                    let arguments = item["arguments"].as_str().unwrap_or("{}");
                    let input: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
                    content.push(IrContentBlock::ToolUse { id, name, input });
                }
                "reasoning" => {
                    if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                        content.push(IrContentBlock::Thinking {
                            thinking: text.to_string(),
                            signature: None,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    let status = msg
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("completed");
    let stop = match status {
        "completed" => IrStopReason::EndTurn,
        "incomplete" => IrStopReason::MaxTokens,
        _ => IrStopReason::EndTurn,
    };

    let usage = ir::usage::extract_responses_usage(msg);

    (content, stop, usage)
}

/// 将 IR content blocks 渲染为 Anthropic content Value。
fn ir_content_to_messages_value(blocks: &[IrContentBlock]) -> Value {
    let arr: Vec<Value> = blocks
        .iter()
        .filter_map(|block| match block {
            IrContentBlock::Text { text, cache_control } => {
                let mut obj = json!({"type": "text", "text": text});
                if let Some(cc) = cache_control {
                    obj["cache_control"] = cc.clone();
                }
                Some(obj)
            }
            IrContentBlock::Thinking { thinking, .. } => {
                Some(json!({"type": "thinking", "thinking": thinking}))
            }
            IrContentBlock::ToolUse { id, name, input } => {
                Some(json!({"type": "tool_use", "id": id, "name": name, "input": input}))
            }
            IrContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let c = match content {
                    IrToolResultContent::Text(t) => json!(t),
                    IrToolResultContent::Blocks(blocks) => ir_content_to_messages_value(blocks),
                };
                let mut obj = json!({"type": "tool_result", "tool_use_id": tool_use_id, "content": c});
                if *is_error {
                    obj["is_error"] = json!(true);
                }
                Some(obj)
            }
            IrContentBlock::Image { source } => {
                let src = match source {
                    IrImageSource::Base64 { media_type, data } => {
                        json!({"type": "base64", "media_type": media_type, "data": data})
                    }
                    IrImageSource::Url { url } => json!({"type": "url", "url": url}),
                };
                Some(json!({"type": "image", "source": src}))
            }
        })
        .collect();
    json!(arr)
}

/// 将 IR messages 渲染为 Chat Completions messages Value。
fn ir_messages_to_chat_completions_value(messages: &[IrMessage]) -> Vec<Value> {
    let mut result = Vec::new();

    for msg in messages {
        let role = match msg.role {
            IrRole::User => "user",
            IrRole::Assistant => "assistant",
        };

        let mut text_parts: Vec<String> = vec![];
        let mut tool_calls: Vec<Value> = vec![];
        let mut reasoning_content: Option<String> = None;
        let mut tool_results: Vec<(String, String)> = vec![];

        for block in &msg.content {
            match block {
                IrContentBlock::Text { text, .. } => text_parts.push(text.clone()),
                IrContentBlock::Thinking { thinking, .. } => {
                    reasoning_content = Some(thinking.clone())
                }
                IrContentBlock::ToolUse { id, name, input } => {
                    tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": serde_json::to_string(input).unwrap_or_else(|_| "{}".to_string())
                        }
                    }));
                }
                IrContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => {
                    let text = match content {
                        IrToolResultContent::Text(t) => t.clone(),
                        IrToolResultContent::Blocks(blocks) => blocks
                            .iter()
                            .filter_map(|b| {
                                if let IrContentBlock::Text { text, .. } = b {
                                    Some(text.clone())
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    };
                    tool_results.push((tool_use_id.clone(), text));
                }
                _ => {}
            }
        }

        // Tool results 作为独立的 tool role 消息
        for (tool_call_id, text) in &tool_results {
            result.push(json!({
                "role": "tool",
                "tool_call_id": tool_call_id,
                "content": text
            }));
        }

        // 普通消息
        if !text_parts.is_empty() || !tool_calls.is_empty() || reasoning_content.is_some() {
            let mut msg_obj = json!({"role": role});
            if !text_parts.is_empty() {
                msg_obj["content"] = json!(text_parts.join("\n"));
            } else if tool_calls.is_empty() {
                msg_obj["content"] = json!("");
            }
            if let Some(rc) = reasoning_content {
                msg_obj["reasoning_content"] = json!(rc);
            }
            if !tool_calls.is_empty() {
                msg_obj["tool_calls"] = json!(tool_calls);
                if msg_obj.get("content").is_none() {
                    msg_obj["content"] = Value::Null;
                }
            }
            result.push(msg_obj);
        }
    }

    result
}

/// 将累积的 IR content blocks 渲染为客户端格式 SSE 字节序列。
fn render_accumulated_ir(
    client_format: ClientFormat,
    content: &[IrContentBlock],
    stop_reason: IrStopReason,
    usage: &IrUsage,
) -> Vec<Bytes> {
    let msg_id = format!("msg_{}", uuid::Uuid::new_v4().simple());

    match client_format {
        ClientFormat::Messages => {
            render_anthropic_sse(&msg_id, content, stop_reason, usage)
        }
        ClientFormat::ChatCompletions => {
            render_chat_sse(&msg_id, content, stop_reason, usage)
        }
        ClientFormat::Responses => {
            render_responses_sse(&msg_id, content, stop_reason, usage)
        }
    }
}

/// 渲染 Anthropic SSE 字节序列。
fn render_anthropic_sse(
    msg_id: &str,
    content: &[IrContentBlock],
    stop_reason: IrStopReason,
    usage: &IrUsage,
) -> Vec<Bytes> {
    let mk = |event_type: &str, payload: Value| -> Bytes {
        let data = serde_json::to_string(&payload).unwrap_or_default();
        Bytes::from(format!("event: {}\ndata: {}\n\n", event_type, data))
    };

    let mut out = vec![mk(
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": msg_id, "type": "message", "role": "assistant",
                "model": "", "content": [], "stop_reason": null,
                "stop_sequence": null,
                "usage": {"input_tokens": 0, "output_tokens": 0}
            }
        }),
    )];

    for (i, block) in content.iter().enumerate() {
        match block {
            IrContentBlock::Text { text, .. } => {
                out.push(mk("content_block_start", json!({"type": "content_block_start", "index": i, "content_block": {"type": "text", "text": ""}})));
                out.push(mk("content_block_delta", json!({"type": "content_block_delta", "index": i, "delta": {"type": "text_delta", "text": text}})));
                out.push(mk("content_block_stop", json!({"type": "content_block_stop", "index": i})));
            }
            IrContentBlock::ToolUse { id, name, input } => {
                out.push(mk("content_block_start", json!({"type": "content_block_start", "index": i, "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}})));
                let input_json = serde_json::to_string(input).unwrap_or_default();
                out.push(mk("content_block_delta", json!({"type": "content_block_delta", "index": i, "delta": {"type": "input_json_delta", "partial_json": input_json}})));
                out.push(mk("content_block_stop", json!({"type": "content_block_stop", "index": i})));
            }
            IrContentBlock::ToolResult { tool_use_id, content: tc, .. } => {
                // web_search_tool_result 在 Anthropic 格式中作为特殊块
                let result_blocks = match tc {
                    IrToolResultContent::Text(t) => json!([{"type": "text", "text": t}]),
                    IrToolResultContent::Blocks(blocks) => ir_content_to_messages_value(blocks),
                };
                out.push(mk("content_block_start", json!({"type": "content_block_start", "index": i, "content_block": {"type": "web_search_tool_result", "tool_use_id": tool_use_id, "content": result_blocks}})));
                out.push(mk("content_block_stop", json!({"type": "content_block_stop", "index": i})));
            }
            IrContentBlock::Thinking { thinking, .. } => {
                out.push(mk("content_block_start", json!({"type": "content_block_start", "index": i, "content_block": {"type": "thinking", "thinking": ""}})));
                out.push(mk("content_block_delta", json!({"type": "content_block_delta", "index": i, "delta": {"type": "thinking_delta", "thinking": thinking}})));
                out.push(mk("content_block_stop", json!({"type": "content_block_stop", "index": i})));
            }
            _ => {}
        }
    }

    out.push(mk(
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason.as_anthropic_str(), "stop_sequence": null},
            "usage": {"output_tokens": usage.output_tokens, "cache_read_input_tokens": usage.cache_read_input_tokens}
        }),
    ));
    out.push(mk("message_stop", json!({"type": "message_stop"})));
    out
}

/// 渲染 Chat Completions SSE 字节序列。
fn render_chat_sse(
    msg_id: &str,
    content: &[IrContentBlock],
    stop_reason: IrStopReason,
    usage: &IrUsage,
) -> Vec<Bytes> {
    let mk = |payload: Value| -> Bytes {
        let data = serde_json::to_string(&payload).unwrap_or_default();
        Bytes::from(format!("data: {}\n\n", data))
    };

    let created = chrono::Utc::now().timestamp();
    let mut out = vec![];

    // 第一个 chunk: role
    out.push(mk(json!({
        "id": msg_id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": "",
        "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": null}]
    })));

    // 文本内容
    for block in content {
        match block {
            IrContentBlock::Text { text, .. } => {
                out.push(mk(json!({
                    "id": msg_id,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": "",
                    "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": null}]
                })));
            }
            IrContentBlock::Thinking { thinking, .. } => {
                out.push(mk(json!({
                    "id": msg_id,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": "",
                    "choices": [{"index": 0, "delta": {"reasoning_content": thinking}, "finish_reason": null}]
                })));
            }
            IrContentBlock::ToolUse { id, name, input } => {
                let args = serde_json::to_string(input).unwrap_or_else(|_| "{}".to_string());
                out.push(mk(json!({
                    "id": msg_id,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": "",
                    "choices": [{"index": 0, "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": id,
                            "type": "function",
                            "function": {"name": name, "arguments": args}
                        }]
                    }, "finish_reason": null}]
                })));
            }
            _ => {}
        }
    }

    // finish chunk with usage
    let output_tokens = if usage.output_tokens > 0 {
        usage.output_tokens
    } else {
        usage.output_chars / 4
    };
    out.push(mk(json!({
        "id": msg_id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": "",
        "choices": [{"index": 0, "delta": {}, "finish_reason": stop_reason.as_chat_finish_reason()}],
        "usage": {
            "prompt_tokens": usage.input_tokens + usage.cache_read_input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": usage.input_tokens + usage.cache_read_input_tokens + output_tokens,
            "prompt_tokens_details": {"cached_tokens": usage.cache_read_input_tokens}
        }
    })));
    out.push(Bytes::from("data: [DONE]\n\n"));
    out
}

/// 渲染 Responses SSE 字节序列。
fn render_responses_sse(
    msg_id: &str,
    content: &[IrContentBlock],
    stop_reason: IrStopReason,
    usage: &IrUsage,
) -> Vec<Bytes> {
    let mk = |event_type: &str, payload: Value| -> Bytes {
        let data = serde_json::to_string(&payload).unwrap_or_default();
        Bytes::from(format!("event: {}\ndata: {}\n\n", event_type, data))
    };

    let mut out = vec![];

    // response.created
    out.push(mk("response.created", json!({
        "type": "response.created",
        "response": {"id": msg_id, "model": "", "status": "in_progress", "output": []}
    })));

    // 内容
    for (i, block) in content.iter().enumerate() {
        match block {
            IrContentBlock::Text { text, .. } => {
                out.push(mk("response.output_item.added", json!({
                    "type": "response.output_item.added",
                    "output_index": i,
                    "item": {"type": "message", "role": "assistant", "content": []}
                })));
                out.push(mk("response.output_text.delta", json!({
                    "type": "response.output_text.delta",
                    "output_index": i,
                    "delta": text
                })));
                out.push(mk("response.output_item.done", json!({
                    "type": "response.output_item.done",
                    "output_index": i,
                    "item": {}
                })));
            }
            IrContentBlock::ToolUse { id, name, input } => {
                let args = serde_json::to_string(input).unwrap_or_else(|_| "{}".to_string());
                out.push(mk("response.output_item.added", json!({
                    "type": "response.output_item.added",
                    "output_index": i,
                    "item": {"type": "function_call", "call_id": id, "name": name, "arguments": args}
                })));
                out.push(mk("response.output_item.done", json!({
                    "type": "response.output_item.done",
                    "output_index": i,
                    "item": {}
                })));
            }
            _ => {}
        }
    }

    // response.completed
    let output_tokens = if usage.output_tokens > 0 {
        usage.output_tokens
    } else {
        usage.output_chars / 4
    };
    out.push(mk("response.completed", json!({
        "type": "response.completed",
        "response": {
            "id": msg_id,
            "model": "",
            "status": "completed",
            "output": [],
            "usage": {
                "input_tokens": usage.input_tokens + usage.cache_read_input_tokens,
                "output_tokens": output_tokens,
                "input_tokens_details": {"cached_tokens": usage.cache_read_input_tokens}
            }
        }
    })));
    out
}
