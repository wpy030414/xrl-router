//! WebSearch 劫持：把 server-side web_search 改写为自定义 tool，
//! 在代理内本地跑 tool-calling loop（Bing 搜索），累积内容转 SSE 返回。
//! 支持 Anthropic 与 OpenAI 兼容两种上游格式。

use std::sync::Arc;
use std::time::Duration;

use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use bytes::Bytes;
use futures::stream::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::sync::oneshot;

use crate::gateway::server::AppState;

use super::auth::ServiceKeyInfo;
use super::key_rotation::{pick_key_for, update_key_health};
use super::route::ResolvedRoute;
use super::stream::{send_error_event, ClientFormat};
use super::translate;

/// 请求 body 的 tools 里是否含 server-side web_search 工具。
pub(super) fn has_websearch_tool(body: &Value) -> bool {
    body.get("tools")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter().any(|t| {
                t.get("type")
                    .and_then(|v| v.as_str())
                    .map(|s| s.starts_with("web_search"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
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

/// 把 Bing 结果转成 Anthropic web_search_tool_result 的 content 数组。
fn search_result_blocks(results: &[crate::search::SearchResult]) -> Vec<Value> {
    results
        .iter()
        .map(|r| json!({"title": r.title, "url": r.url, "encrypted_content": ""}))
        .collect()
}

/// 把最终累积的 content blocks + stop_reason + usage 渲染成 Anthropic SSE 字节段序列。
fn build_sse_bytes(msg_id: &str, model: &str, content: &[Value], stop_reason: &str, usage: &Value) -> Vec<Bytes> {
    let mk = |event_type: &str, payload: Value| -> Bytes {
        let data = serde_json::to_string(&payload).unwrap_or_default();
        Bytes::from(format!("event: {}\ndata: {}\n\n", event_type, data))
    };
    let mut out = vec![mk(
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": msg_id, "type": "message", "role": "assistant", "model": model,
                "content": [], "stop_reason": null, "stop_sequence": null,
                "usage": {"input_tokens": 0, "output_tokens": 0}
            }
        }),
    )];

    for (i, block) in content.iter().enumerate() {
        let bt = block["type"].as_str().unwrap_or("text");
        let start_block = match bt {
            "text" => json!({"type": "text", "text": ""}),
            "tool_use" => json!({"type": "tool_use", "id": block["id"], "name": block["name"], "input": {}}),
            _ => block.clone(),
        };
        out.push(mk("content_block_start", json!({"type": "content_block_start", "index": i, "content_block": start_block})));
        match bt {
            "text" => {
                let text = block["text"].as_str().unwrap_or("");
                out.push(mk("content_block_delta", json!({"type": "content_block_delta", "index": i, "delta": {"type": "text_delta", "text": text}})));
            }
            "tool_use" => {
                let input_json = serde_json::to_string(&block["input"]).unwrap_or_default();
                out.push(mk("content_block_delta", json!({"type": "content_block_delta", "index": i, "delta": {"type": "input_json_delta", "partial_json": input_json}})));
            }
            _ => {} // web_search_tool_result: 无 delta
        }
        out.push(mk("content_block_stop", json!({"type": "content_block_stop", "index": i})));
    }

    out.push(mk(
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason, "stop_sequence": null},
            "usage": {"output_tokens": usage["output_tokens"]}
        }),
    ));
    out.push(mk("message_stop", json!({"type": "message_stop"})));
    out
}

/// WebSearch 劫持 loop 入口：根据上游类型走 Anthropic 或 OpenAI 格式，
/// 在代理内跑 tool-calling loop（本地 Bing 搜索），累积内容转 SSE 返回客户端。
///
/// 与主代理路径一致：立即返回 Response（含 `:keepalive` 首字节），hijack loop
/// 在后台 spawn 完成，避免多轮 tool-calling 期间客户端因长时间无字节而超时。
/// 上游错误通过 SSE error event 传达（不再返回 HTTP 4xx/5xx）。
pub(super) async fn run_websearch_loop(
    state: Arc<AppState>,
    body: Value,
    resolved: ResolvedRoute,
    provider_is_anthropic: bool,
    trace_id: String,
    service_key: ServiceKeyInfo,
) -> Result<Response, (StatusCode, HeaderMap, Json<Value>)> {
    use std::convert::Infallible;

    let client_format = ClientFormat::Anthropic; // websearch 仅经 /v1/messages 入口
    let (tx, rx) = mpsc::channel::<Result<Bytes, Infallible>>(100);

    // 初始 keepalive：客户端立即知道连接存活
    let _ = tx.send(Ok(Bytes::from(":keepalive\n\n"))).await;

    tokio::spawn(async move {
        let trace_id = &trace_id;

        // ── keepalive 心跳 + 取消信号（与 stream.rs 主路径同构）────────
        // 主任务任何路径结束 → Drop guard 触发 cancel → keepalive 退出 →
        // channel 关闭 → 流干净收尾。tx 唯一属于主任务，keepalive 只持取消信号。
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
        let max_tokens = body["max_tokens"].as_u64().unwrap_or(4096);
        let msg_id = format!("msg_{}", uuid::Uuid::new_v4().simple());

        let mut accumulated: Vec<Value> = Vec::new();
        let mut final_stop = "end_turn".to_string();
        let mut accum_input: i64 = 0;
        let mut accum_output: i64 = 0;
        let mut accum_cache_read: i64 = 0;

        let model_display_name = body["model"].as_str().unwrap_or("").to_string();
        let outcome: Result<(), (String, String)> = if provider_is_anthropic {
            hijack_anthropic(
                &client, &upstream_url, &model, &picked.key_hash, &body, max_tokens,
                &state.keys, &resolved.provider_id,
                &mut accumulated, &mut final_stop,
                &mut accum_input, &mut accum_output,
                &mut accum_cache_read,
            )
            .await
        } else {
            hijack_openai(
                &client, &upstream_url, &model, &picked.key_hash, &body, max_tokens,
                &state.keys, &resolved.provider_id,
                &mut accumulated, &mut final_stop,
                &mut accum_input, &mut accum_output,
            )
            .await
        };

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
            "/v1/messages",
            accum_input,
            accum_output,
            0,
            true,
            None,
            accum_cache_read,
        );

        let final_usage = json!({
            "input_tokens": accum_input,
            "output_tokens": accum_output,
            "cache_read_input_tokens": accum_cache_read,
        });
        let segments = build_sse_bytes(&msg_id, &model, &accumulated, &final_stop, &final_usage);
        for seg in segments {
            if tx.send(Ok(seg)).await.is_err() {
                break;
            }
        }
        // tx drop → channel 关闭 → rx 流收尾
    });

    Ok(super::stream::sse_response(rx))
}

/// Anthropic 上游的劫持 loop。
async fn hijack_anthropic(
    client: &reqwest::Client,
    upstream_url: &str,
    model: &str,
    api_key: &str,
    body: &Value,
    max_tokens: u64,
    pool: &crate::keys::KeyPool,
    provider_id: &str,
    accumulated: &mut Vec<Value>,
    final_stop: &mut String,
    accum_input: &mut i64,
    accum_output: &mut i64,
    accum_cache_read: &mut i64,
) -> Result<(), (String, String)> {
    let custom_tool = json!({
        "name": "web_search",
        "description": "Search the web (Bing) for up-to-date information.",
        "input_schema": {
            "type": "object",
            "properties": {"query": {"type": "string", "description": "The search query"}},
            "required": ["query"]
        }
    });
    let req_system = body.get("system").cloned();
    let mut messages = body["messages"].as_array().cloned().unwrap_or_default();

    for _ in 0..5 {
        let mut req = serde_json::Map::new();
        req.insert("model".into(), json!(model));
        req.insert("messages".into(), json!(messages.clone()));
        req.insert("tools".into(), json!([custom_tool.clone()]));
        req.insert("tool_choice".into(), json!({"type": "auto"}));
        req.insert("stream".into(), json!(false));
        req.insert("max_tokens".into(), json!(max_tokens));
        if let Some(s) = &req_system {
            req.insert("system".into(), s.clone());
        }

        let resp = client
            .post(upstream_url)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&Value::Object(req))
            .send()
            .await
            .map_err(|e| ("api_error".to_string(), e.to_string()))?;
        let status = resp.status().as_u16();
        let msg_val: Value = resp.json().await
            .map_err(|e| ("api_error".to_string(), e.to_string()))?;
        if status >= 400 {
            update_key_health(pool, provider_id, api_key, status);
            let msg = msg_val["error"]["message"].as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("upstream error {}", status));
            return Err(("api_error".to_string(), msg));
        }
        update_key_health(pool, provider_id, api_key, status);

        let stop = msg_val["stop_reason"].as_str().unwrap_or("end_turn").to_string();
        let content = msg_val["content"].as_array().cloned().unwrap_or_default();
        // Accumulate usage across all tool-calling rounds (not overwrite).
        let usage = &msg_val["usage"];
        // input 含写缓存（cache_creation 只是首次处理输入，并入输入）
        *accum_input += usage["input_tokens"].as_i64().unwrap_or(0)
            + usage["cache_creation_input_tokens"].as_i64().unwrap_or(0);
        *accum_output += usage["output_tokens"].as_i64().unwrap_or(0);
        *accum_cache_read += usage["cache_read_input_tokens"].as_i64().unwrap_or(0);
        accumulated.extend(content.clone());

        if stop != "tool_use" {
            *final_stop = stop;
            break;
        }

        let tool_uses: Vec<Value> = content
            .iter()
            .filter(|b| b["type"] == "tool_use" && b["name"] == "web_search")
            .cloned()
            .collect();
        messages.push(json!({"role": "assistant", "content": content}));

        let mut results: Vec<Value> = Vec::new();
        for tu in &tool_uses {
            let query = tu["input"]["query"].as_str().unwrap_or("");
            let bing = crate::search::bing::search(query).await.unwrap_or_default();
            accumulated.push(json!({"type": "web_search_tool_result", "tool_use_id": tu["id"], "content": search_result_blocks(&bing)}));
            results.push(json!({"type": "tool_result", "tool_use_id": tu["id"], "content": format_search_text(&bing)}));
        }
        messages.push(json!({"role": "user", "content": results}));
    }
    Ok(())
}

/// OpenAI 兼容上游（如 qwen / 钉钉 DEAP）的劫持 loop。
async fn hijack_openai(
    client: &reqwest::Client,
    upstream_url: &str,
    model: &str,
    api_key: &str,
    body: &Value,
    max_tokens: u64,
    pool: &crate::keys::KeyPool,
    provider_id: &str,
    accumulated: &mut Vec<Value>,
    final_stop: &mut String,
    accum_input: &mut i64,
    accum_output: &mut i64,
) -> Result<(), (String, String)> {
    let custom_fn = json!({
        "type": "function",
        "function": {
            "name": "web_search",
            "description": "Search the web (Bing) for up-to-date information.",
            "parameters": {"type": "object", "properties": {"query": {"type": "string", "description": "The search query"}}, "required": ["query"]}
        }
    });
    // 翻译客户端 Anthropic 请求为 OpenAI 格式（messages + system）
    let init = translate::anthropic_req_to_openai(body);
    let mut messages = init["messages"].as_array().cloned().unwrap_or_default();

    for _ in 0..5 {
        let req = json!({
            "model": model,
            "messages": messages,
            "tools": [custom_fn.clone()],
            "tool_choice": "auto",
            "stream": false,
            "max_tokens": max_tokens,
        });
        let resp = client
            .post(upstream_url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&req)
            .send()
            .await
            .map_err(|e| ("api_error".to_string(), e.to_string()))?;
        let status = resp.status().as_u16();
        let msg_val: Value = resp.json().await
            .map_err(|e| ("api_error".to_string(), e.to_string()))?;
        if status >= 400 {
            update_key_health(pool, provider_id, api_key, status);
            let msg = msg_val["error"]["message"].as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("upstream error {}", status));
            return Err(("api_error".to_string(), msg));
        }
        update_key_health(pool, provider_id, api_key, status);

        let choice = &msg_val["choices"][0];
        let finish = choice["finish_reason"].as_str().unwrap_or("stop");
        let content_text = choice["message"]["content"].as_str().unwrap_or("");
        let tool_calls = choice["message"]["tool_calls"].as_array().cloned().unwrap_or_default();
        // Accumulate usage across all tool-calling rounds (not overwrite).
        *accum_input += msg_val["usage"]["prompt_tokens"].as_i64().unwrap_or(0);
        *accum_output += msg_val["usage"]["completion_tokens"].as_i64().unwrap_or(0);

        if !content_text.is_empty() {
            accumulated.push(json!({"type": "text", "text": content_text}));
        }

        if finish != "tool_calls" || tool_calls.is_empty() {
            *final_stop = match finish { "length" => "max_tokens", _ => "end_turn" }.to_string();
            break;
        }

        // 追加 assistant（OpenAI 格式，含 tool_calls）
        messages.push(json!({"role": "assistant", "content": content_text, "tool_calls": tool_calls.clone()}));

        // 并行搜索所有 web_search tool_call（一轮多个时省时间）
        let ws_calls: Vec<Value> = tool_calls
            .iter()
            .filter(|tc| tc["function"]["name"].as_str() == Some("web_search"))
            .cloned()
            .collect();
        let ws_results = futures::future::join_all(ws_calls.iter().map(|tc| async move {
            let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
            let input: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
            let query = input["query"].as_str().unwrap_or("").to_string();
            let bing = crate::search::bing::search(&query).await.unwrap_or_default();
            (tc.clone(), input, bing)
        }))
        .await;
        for (tc, input, bing) in ws_results {
            accumulated.push(json!({"type": "tool_use", "id": tc["id"], "name": "web_search", "input": input}));
            accumulated.push(json!({"type": "web_search_tool_result", "tool_use_id": tc["id"], "content": search_result_blocks(&bing)}));
            messages.push(json!({"role": "tool", "tool_call_id": tc["id"], "content": format_search_text(&bing)}));
        }
    }
    Ok(())
}
