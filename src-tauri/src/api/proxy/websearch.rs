//! WebSearch 工具调用循环：模型自主决定是否搜索，代理执行搜索并回传结果。
//!
//! 当 websearch 功能开启时，代理层确保 `web_search` 工具定义存在于 IR 请求中
//! （若客户端未提供则主动注入）。上游模型通过标准 tool-calling 协议自主决定
//! 何时调用 `web_search`，代理在本地执行 Bing 搜索并将结果作为 `tool_result`
//! 回传给模型，直到模型不再调用搜索工具并给出最终回答。
//!
//! 轮数上限为安全网（10 轮），正常情况模型自主决断；另有「无进展检测」——
//! 连续 2 轮查询词相似时提前收尾（模型反复搜同一关键词通常意味着搜索结果
//! 无新信息）。收尾时清理工具痕迹，把搜索结果合并为文本指令强制无搜索回答。
//!
//! 所有中间搜索轮次的响应均被缓冲，仅最终回答流式转发给客户端。

use serde_json::{json, Value};
use tracing::{info, warn};

use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::mpsc;

use super::ir::types::*;
use super::ir::to_messages::MessagesRenderState;
use super::route::ResolvedRoute;
use super::forward::ForwardOutcome;
use super::stream::{send_error_event, ClientFormat};
use crate::gateway::server::AppState;

/// 工具调用循环最大轮数（防止无限循环的安全网，正常情况模型自主决断）。
const MAX_TOOL_ROUNDS: usize = 10;

/// 无进展检测：连续多少轮查询词重复视为「无进展」，提前收尾。
///
/// 模型对争议性问题可能反复用几乎相同的查询词搜索（Bing 持续返回降级/矛盾
/// 信息时尤其常见）。重复查询通常意味着搜索结果没有新信息，继续搜只会白耗
/// 时间，此时应结束循环并基于已有结果回答。
const NO_PROGRESS_ROUNDS: usize = 2;

/// 无进展判定：查询词相似度阈值（编辑距离 / 较长串长度）。
/// 低于阈值视为「重复查询」。
const QUERY_SIMILARITY_THRESHOLD: f64 = 0.6;

/// 归一化查询词：小写 + 去空白，供相似度比较。
fn normalize_query(q: &str) -> String {
    q.to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// 计算两个查询词的相似度（0.0~1.0，1.0 为完全相同）。
/// 用 Levenshtein 编辑距离的归一化版本：1 - dist / max_len。
/// 注意：长度用 `chars().count()`（字符数），不能用 `len()`（字节数）——
/// 中文每字 3 字节，用字节长度会高估 max_len、低估相似度。
fn query_similarity(a: &str, b: &str) -> f64 {
    let a = normalize_query(a);
    let b = normalize_query(b);
    if a == b {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let max_len = a.chars().count().max(b.chars().count()) as f64;
    if max_len == 0.0 {
        return 0.0;
    }
    // Levenshtein 距离（两行 DP，按字符迭代）
    let b_chars: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b_chars.iter().enumerate() {
            let cost = if ca == *cb { 0 } else { 1 };
            let min = (prev[j + 1] + 1) // 删除
                .min(cur[j] + 1) // 插入
                .min(prev[j] + cost); // 替换
            cur.push(min);
        }
        prev = cur;
    }
    let dist = prev[b_chars.len()] as f64;
    1.0 - dist / max_len
}

/// 判断工具名是否属于搜索类（代理应劫持/替换的范畴）。
///
/// 覆盖三种来源：代理注入的 `web_search`、Anthropic 服务端内置的 `web_search_*`、
/// Claude Code 客户端的 `WebSearch`。
fn is_search_tool_name(name: &str) -> bool {
    name.starts_with("web_search") || name.eq_ignore_ascii_case("WebSearch")
}

/// IR 请求的 tools 里是否含搜索类工具。
#[cfg(test)]
pub(super) fn has_websearch_tool_ir(req: &IrRequest) -> bool {
    req.tools.iter().any(|t| is_search_tool_name(&t.name))
}

/// 确保 IR 请求中包含 web_search 工具定义（替换模式）。
///
/// 移除客户端自带的所有搜索类工具（如 Claude Code 的 `WebSearch`、Anthropic 内置的
/// `web_search_20250305` 等），替换为代理自己的 `web_search` 工具定义。
/// 这样模型只能看到代理控制的搜索工具，确保 tool-calling loop 能拦截并走本地 Bing 搜索。
pub(super) fn ensure_websearch_tool(ir_request: &mut IrRequest) {
    let before = ir_request.tools.len();
    ir_request.tools.retain(|t| {
        let dominated = is_search_tool_name(&t.name);
        if dominated {
            info!(tool = %t.name, "websearch: removing client search tool");
        }
        !dominated
    });
    let removed = before - ir_request.tools.len();

    // 若客户端 tool_choice 强制指定了被移除的搜索工具，改写为代理注入的 web_search，
    // 避免上游因引用不存在的工具而报错
    if let Some(IrToolChoice::Tool { name }) = &ir_request.tool_choice {
        if is_search_tool_name(name) {
            info!(from = %name, "websearch: rewriting tool_choice target");
            ir_request.tool_choice = Some(IrToolChoice::Tool {
                name: "web_search".to_string(),
            });
        }
    }

    ir_request.tools.push(IrTool {
        name: "web_search".to_string(),
        description: Some(
            "Search the web for current information. \
             Use this when the user's question requires up-to-date facts, \
             recent events, or information not in your training data."
                .to_string(),
        ),
        input_schema: json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                }
            },
            "required": ["query"]
        }),
    });
    info!(removed, "websearch: replaced search tools with proxy web_search");
}

/// 从 tool_use 的 input 中提取搜索关键词（LLM 已填好的精准搜索词）。
fn extract_query_from_tool_input(input: &Value) -> Option<String> {
    input
        .get("query")
        .or_else(|| input.get("q"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.trim().to_string())
}

/// 向客户端发送 SSE 进度注释（`: 文本`）。
///
/// SSE 注释行是协议标准的「可显示可忽略」消息——Claude Code 的 debug/verbose
/// 模式能看到搜索进度（「模型正在调用 Router 的 web_search 工具」），正常模式
/// 静默不打扰。发送失败（客户端断开）直接忽略。
fn send_progress_comment(tx: &mpsc::Sender<Result<Bytes, Infallible>>, msg: &str) {
    // 转义换行，避免注释内容破坏 SSE 帧结构
    let escaped = msg.replace('\n', " ").replace('\r', " ");
    let _ = tx.try_send(Ok(Bytes::from(format!(": {}\n\n", escaped))));
}

/// WebSearch 工具调用循环主入口。
///
/// 替代旧的 `execute_websearch_hijack()`。模型通过标准 tool-calling 自主决定是否搜索、
/// 搜索什么、搜索几次。代理在本地执行 Bing 搜索并将结果回传，直到模型不再调用
/// 搜索工具或达到轮数上限。所有中间轮次缓冲，仅最终回答流式给客户端。
pub(super) async fn execute_websearch_tool_loop(
    state: &Arc<AppState>,
    mut ir_request: IrRequest,
    tx: &mpsc::Sender<Result<Bytes, Infallible>>,
    resolved: &ResolvedRoute,
    picked: &super::route::PickedKey,
    client_format: ClientFormat,
    client: &reqwest::Client,
    trace_id: &str,
    start_time: Instant,
    est_input: u64,
    // 统计记录所需参数（与 forward_stream_ir 对齐）
    model_name: &str,
    service_key: &super::auth::ServiceKeyInfo,
    endpoint: &'static str,
) -> ForwardOutcome {
    let mut final_rendered_bytes: Vec<Bytes> = Vec::new();
    // 无进展检测：记录每轮查询词，检测连续重复（模型反复搜同一关键词）。
    let mut query_history: Vec<String> = Vec::new();
    // 收集所有搜索轮次（工具调用 id + 查询词 + 结构化结果），
    // 最终对 Messages 客户端以 server-side tool 格式渲染给 Claude Code。
    let mut search_rounds: Vec<SearchRound> = Vec::new();
    // 最终回答元信息（Messages 合成流用）：msg_id / model / usage / 文本。
    let mut final_meta: Option<(String, String, IrUsage, String)> = None;
    // 累积所有轮次的 usage（最终写一条统计记录，与 forward_stream_ir 对齐）。
    let mut total_usage = IrUsage::default();
    // 品牌消息是否已提前发送（仅 Messages 客户端）
    let mut brand_sent = false;

    for round in 0..MAX_TOOL_ROUNDS {
        info!(round, "websearch: tool loop round start");

        // 1. 发起上游请求（缓冲完整响应）
        let resp = match send_upstream_request(client, resolved, picked, &ir_request, trace_id, round).await {
            Ok(r) => r,
            Err(err_msg) => {
                send_error_event(tx, client_format, "api_error", &err_msg);
                return ForwardOutcome::ErrorDelivered;
            }
        };

        // 2. 缓冲完整响应
        let (ir_events, usage, rendered_bytes, stream_err) = super::forward::forward_stream_ir_to_buffer(
            resp,
            trace_id,
            &resolved.provider_kind,
            client_format,
            est_input,
        )
        .await;

        // 2.1 流内错误（HTTP 200 + SSE error event / 非 SSE JSON 错误体）。
        // round 0 时尚未向客户端发送任何内容（brand/progress 在第 5.5 步，
        // 位于缓冲之后）：密钥级错误 → 返回给双循环换密钥重试；
        // 非密钥级 → 透传。后续轮次已发过 brand/进度 → 只能透传。
        if let Some((status, msg)) = stream_err {
            if round == 0 && !brand_sent && matches!(status, 401 | 402 | 403 | 429) {
                return ForwardOutcome::UpstreamKeyError { status, message: msg };
            }
            warn!(trace_id, round, status, upstream_error = %msg, "websearch: upstream streamed error, forwarding");
            send_error_event(tx, client_format, "api_error", &msg);
            return ForwardOutcome::ErrorDelivered;
        }

        // 累积本轮 usage（最终写一条汇总记录）
        total_usage.input_tokens += usage.input_tokens;
        total_usage.output_tokens += usage.output_tokens;
        total_usage.cache_read_input_tokens += usage.cache_read_input_tokens;
        total_usage.cache_creation_input_tokens += usage.cache_creation_input_tokens;
        total_usage.output_chars += usage.output_chars;

        // 3. 重建完整 assistant 消息
        let (assistant_msg, stop_reason, msg_id, model) = accumulate_ir_events(&ir_events);

        // 4. 检测 web_search tool_use
        let websearch_calls = extract_websearch_tool_calls(&assistant_msg);

        info!(
            round,
            stop_reason = ?stop_reason,
            websearch_count = websearch_calls.len(),
            "websearch: buffered response analyzed"
        );

        if websearch_calls.is_empty() {
            // 模型没有调用搜索 → 最终回答
            info!(round, "websearch: no web_search tool_use, final answer");
            final_rendered_bytes = rendered_bytes;
            // 记录最终元信息（Messages 合成流用：server_tool_use 块插到文本前）
            final_meta = Some((
                msg_id,
                model,
                usage,
                extract_final_text(&assistant_msg),
            ));
            break;
        }

        // 5. 无进展检测：本轮查询词与上轮几乎相同 → 提前收尾
        let round_queries: Vec<String> = websearch_calls
            .iter()
            .filter_map(|(_, input)| extract_query_from_tool_input(input))
            .collect();
        let is_repetitive = query_history
            .iter()
            .rev()
            .take(NO_PROGRESS_ROUNDS)
            .any(|prev| {
                round_queries.iter().any(|q| {
                    query_similarity(prev, q) >= QUERY_SIMILARITY_THRESHOLD
                })
            });
        if is_repetitive {
            info!(
                round,
                queries = ?round_queries,
                "websearch: repetitive queries detected, stopping tool loop"
            );
            // 用本轮缓冲内容作为最终回答（含 tool_use，由第 7 步清理后强制收尾）
            break;
        }
        query_history.extend(round_queries.clone());

        // 5.5 向客户端发送搜索进度
        //
        // - SSE 注释（`: 文本\n\n`）：所有客户端格式，verbose 模式可见。
        // - Messages 客户端：立刻合成一条完整的 brand 消息（🌐 WebSearch Powered by XRL Router | 关键词: xxx），
        //   让主人在模型决定搜索的瞬间就看到通知，不用等到最终回答才开始渲染。
        //   这条消息独立于后续的最终回答流，index 从 0 开始、以 message_stop 结束。
        for q in &round_queries {
            send_progress_comment(
                tx,
                &format!("websearch: 模型调用 web_search 工具，正在搜索 \"{}\"", q),
            );
        }
        if client_format == ClientFormat::Messages && !brand_sent {
            emit_preliminary_brand_message(tx, &msg_id, &model, &round_queries).await;
            brand_sent = true;
        }

        // 6. 模型调用了搜索 → 执行并回传结果
        ir_request.messages.push(assistant_msg);

        for (tool_use_id, input) in &websearch_calls {
            let (result_text, results) = execute_websearch_tool(state, input).await;
            // 记录本轮搜索（最终以 server_tool_use / web_search_tool_result 渲染给 Claude Code）
            if let Some(query) = extract_query_from_tool_input(input) {
                search_rounds.push(SearchRound {
                    tool_use_id: tool_use_id.clone(),
                    query,
                    results,
                });
            }
            ir_request.messages.push(IrMessage {
                role: IrRole::User,
                content: vec![IrContentBlock::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    content: IrToolResultContent::Text(result_text),
                    is_error: false,
                }],
            });
        }

        // 7. 设置 tool_choice = Auto，让模型自己决定下一步
        ir_request.tool_choice = Some(IrToolChoice::Auto);

        info!(
            round,
            messages_count = ir_request.messages.len(),
            "websearch: tool results appended, continuing to next round"
        );
    }

    // 7. 如果循环耗尽（模型一直在搜索），最后一轮的缓冲就是最终回答
    if final_rendered_bytes.is_empty() {
        info!(rounds = MAX_TOOL_ROUNDS, "websearch: loop exhausted, using last round as final");
        send_progress_comment(tx, "websearch: 搜索完成，正在基于搜索结果生成回答…");

        // 收集所有搜索轮次的结果文本，清理工具痕迹
        let mut search_summary: Vec<String> = Vec::new();
        ir_request.messages.retain(|m| {
            let has_tool_use = m
                .content
                .iter()
                .any(|b| matches!(b, IrContentBlock::ToolUse { .. }));
            let has_tool_result = m
                .content
                .iter()
                .any(|b| matches!(b, IrContentBlock::ToolResult { .. }));

            if has_tool_result {
                for b in &m.content {
                    if let IrContentBlock::ToolResult { content, .. } = b {
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
                        if !text.is_empty() {
                            search_summary.push(text);
                        }
                    }
                }
            }

            // 移除含 tool_use / tool_result 的消息——否则 Chat Completions 序列化
            // 会从历史 tool_calls 补回 web_search 工具定义，上游仍能继续调用，
            // 导致最终轮依然返回 tool_use 而非文本回答（loop 耗尽死循环）
            !has_tool_use && !has_tool_result
        });

        // 移除 web_search 工具，强制模型不再搜索
        ir_request.tools.retain(|t| !t.name.starts_with("web_search"));
        ir_request.tool_choice = Some(IrToolChoice::None);

        // 把搜索结果作为文本指令追加（模型基于它回答，不再需要工具）
        if !search_summary.is_empty() {
            ir_request.messages.push(IrMessage {
                role: IrRole::User,
                content: vec![IrContentBlock::Text {
                    text: format!(
                        "以下是网络搜索获得的相关信息，请基于这些信息回答用户的问题，不要再调用任何工具：\n\n{}",
                        search_summary.join("\n\n")
                    ),
                    cache_control: None,
                }],
            });
        }

        // 追加一轮无工具调用
        match send_upstream_request(client, resolved, picked, &ir_request, trace_id, MAX_TOOL_ROUNDS).await {
            Ok(resp) => {
                let (ir_events, usage, rendered_bytes, stream_err) = super::forward::forward_stream_ir_to_buffer(
                    resp,
                    trace_id,
                    &resolved.provider_kind,
                    client_format,
                    est_input,
                )
                .await;
                if let Some((status, msg)) = stream_err {
                    // 强制轮之前已发送过 brand/进度 → 只能透传
                    warn!(trace_id, round = MAX_TOOL_ROUNDS, status, upstream_error = %msg, "websearch: forced final round streamed error, forwarding");
                    send_error_event(tx, client_format, "api_error", &msg);
                    return ForwardOutcome::ErrorDelivered;
                }
                // 累积最终轮 usage
                total_usage.input_tokens += usage.input_tokens;
                total_usage.output_tokens += usage.output_tokens;
                total_usage.cache_read_input_tokens += usage.cache_read_input_tokens;
                total_usage.cache_creation_input_tokens += usage.cache_creation_input_tokens;
                total_usage.output_chars += usage.output_chars;

                final_rendered_bytes = rendered_bytes;
                // 记录最终元信息（Messages 合成流用）
                let (assistant_msg, _, msg_id, model) = accumulate_ir_events(&ir_events);
                final_meta = Some((
                    msg_id,
                    model,
                    usage,
                    extract_final_text(&assistant_msg),
                ));
            }
            Err(err_msg) => {
                warn!("websearch: forced final round failed: {}", err_msg);
                send_error_event(tx, client_format, "api_error", &err_msg);
                return ForwardOutcome::ErrorDelivered;
            }
        }
    }

    // 8. 记录 usage（汇总所有轮次的 token 消耗，写一条统计记录）
    let output_tokens = if total_usage.output_tokens > 0 {
        total_usage.output_tokens as i64
    } else {
        (total_usage.output_chars / 4) as i64
    };
    let _ = state.database.insert_usage_log(
        chrono::Utc::now().timestamp(),
        &resolved.provider_id,
        resolved.provider_name.as_str(),
        &resolved.model_row_id,
        model_name,
        Some(&picked.id),
        picked.name.as_str(),
        picked.key_masked.as_str(),
        Some(service_key.id.as_str()),
        service_key.name.as_str(),
        service_key.key_masked.as_str(),
        endpoint,
        total_usage.input_tokens as i64,
        output_tokens,
        start_time.elapsed().as_millis() as i64,
        true,
        None,
        total_usage.cache_read_input_tokens as i64,
    );

    // 9. 将最终回答流式发给客户端
    //
    // Messages 客户端（Claude Code）且发生过搜索 → 合成 server-side 流：
    // server_tool_use + web_search_tool_result 块插到最终文本前，Claude Code
    // 显示「搜索中 + 搜索结果」卡片。其他格式（Chat/Responses）无 server-tool
    // 概念，保持现状直接发送缓冲的完整流。
    //
    // 品牌消息已在 step 5.5 提前发送（brand_sent=true），此处跳过品牌块。
    if client_format == ClientFormat::Messages {
        if let Some((msg_id, model, usage, final_text)) = final_meta.as_ref() {
            if !search_rounds.is_empty() && !final_text.is_empty() {
                info!(
                    search_rounds = search_rounds.len(),
                    "websearch: rendering server-side tool stream for Messages client"
                );
                render_websearch_messages_final(
                    tx,
                    msg_id,
                    model,
                    usage,
                    &search_rounds,
                    final_text,
                    brand_sent,
                )
                .await;
                return ForwardOutcome::Completed;
            }
        }
    }

    // 兜底：直接发送缓冲的最终回答流
    info!("websearch: streaming final answer to client");
    for b in final_rendered_bytes {
        if tx.send(Ok(b)).await.is_err() {
            return ForwardOutcome::Completed; // 客户端断开
        }
    }

    ForwardOutcome::Completed
}

// ── 辅助函数 ──────────────────────────────────────────────────────────────

/// 序列化 IR 请求并发起上游 HTTP 请求，返回响应。
async fn send_upstream_request(
    client: &reqwest::Client,
    resolved: &ResolvedRoute,
    picked: &super::route::PickedKey,
    ir_request: &IrRequest,
    trace_id: &str,
    round: usize,
) -> Result<reqwest::Response, String> {
    // 序列化 IR → 上游格式
    let mut body = match resolved.provider_kind.as_str() {
        "messages" => super::ir::to_messages::ir_req_to_messages(ir_request),
        "responses" => super::ir::to_responses::ir_req_to_responses(ir_request),
        _ => super::ir::to_chat_completions::ir_req_to_chat_completions(ir_request),
    };
    if let Some(obj) = body.as_object_mut() {
        obj.insert("model".to_string(), json!(resolved.real_model_id));
        obj.insert("stream".to_string(), json!(true));
        if resolved.provider_kind != "messages" && resolved.provider_kind != "responses" {
            obj.insert(
                "stream_options".to_string(),
                json!({"include_usage": true}),
            );
        }
    }

    // 构建请求
    let mut req_builder = client.post(&resolved.upstream_url);
    if resolved.provider_kind == "messages" {
        req_builder = req_builder
            .header("x-api-key", &picked.key_hash)
            .header("anthropic-version", "2023-06-01");
    } else {
        req_builder = req_builder.header("Authorization", format!("Bearer {}", picked.key_hash));
    }

    let resp = tokio::time::timeout(
        Duration::from_secs(60),
        req_builder
            .header("Content-Type", "application/json")
            .json(&body)
            .send(),
    )
    .await
    .map_err(|_| {
        warn!(trace_id, round, "websearch: upstream request timed out");
        "Upstream request timed out".to_string()
    })?
    .map_err(|e| {
        warn!(trace_id, round, error = %e, "websearch: upstream request failed");
        format!("Upstream request failed: {}", e)
    })?;

    if !resp.status().is_success() {
        let status_code = resp.status().as_u16();
        let body_str = resp
            .text()
            .await
            .unwrap_or_else(|_| "unknown error".to_string());
        warn!(trace_id, round, status = status_code, "websearch: upstream returned error");
        return Err(body_str);
    }

    Ok(resp)
}

/// 从 assistant 消息中提取 web_search tool_use 调用（id + input）。
fn extract_websearch_tool_calls(msg: &IrMessage) -> Vec<(String, Value)> {
    msg.content
        .iter()
        .filter_map(|b| match b {
            IrContentBlock::ToolUse { id, name, input } if name.starts_with("web_search") => {
                Some((id.clone(), input.clone()))
            }
            _ => None,
        })
        .collect()
}

/// 单轮搜索记录：用于最终以 Anthropic server-side tool 格式渲染给客户端。
struct SearchRound {
    tool_use_id: String,
    query: String,
    results: Vec<crate::search::SearchResult>,
}

/// 从 assistant 消息中提取最终回答文本（拼接所有 text 块）。
///
/// 用于 Messages 客户端合成 server-side web_search 流：最终文本块单独渲染。
fn extract_final_text(msg: &IrMessage) -> String {
    msg.content
        .iter()
        .filter_map(|b| match b {
            IrContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// 合成 Messages 客户端的完整 web_search 响应流（Anthropic server-side tool 格式）。
///
/// 结构：
/// ```text
/// message_start
///   text（品牌通知，所有关键词合并在最前面）  ← 先出来，让主人立刻知道在搜
///   ─── 短暂延迟（200ms）───                     ← 让客户端有时间先渲染品牌行
///   server_tool_use（每轮搜索）      ← Claude Code 显示「正在调用 WebSearch」卡片
///   web_search_tool_result（每轮搜索结果）  ← Claude Code 显示搜索结果
///   text（最终回答）                 ← 搜索完毕后再显示答案
/// message_delta + message_stop
/// ```
///
/// 品牌通知与搜索卡片/结果分开：先把所有「🌐 WebSearch by XRL Router | 关键词: xxx」
/// 作为一个 text 块全部输出（让客户端先看到），短暂延迟后再输出工具调用与结果，最后是最终回答。
/// 延迟保证客户端有时间分两次渲染，而非把所有事件合并为一个瞬间出现的块。
async fn render_websearch_messages_final(
    tx: &mpsc::Sender<Result<Bytes, Infallible>>,
    msg_id: &str,
    model: &str,
    usage: &IrUsage,
    search_rounds: &[SearchRound],
    final_text: &str,
    brand_sent: bool,
) {
    use super::ir::to_messages::MessagesRenderState;

    let mut state = MessagesRenderState::new();

    /// 发送单帧，客户端断开则提前返回。
    async fn send_byte(
        tx: &mpsc::Sender<Result<Bytes, Infallible>>,
        b: Option<Bytes>,
    ) -> bool {
        if let Some(b) = b {
            if tx.send(Ok(b)).await.is_err() {
                return false;
            }
        }
        true
    }

    // 1. message_start（复用状态机，保证 id/model/usage 格式与正常流一致）
    if !send_byte(
        tx,
        state.render_event(&IrStreamEvent::MessageStart {
            id: msg_id.to_string(),
            model: model.to_string(),
            usage: Some(usage.clone()),
        }),
    )
    .await
    {
        return;
    }

    let mut index = 0usize;

    // 2. 品牌通知块：先把所有关键词合并在一个 text 块里一次性输出，
    //    让客户端第一时间看到「正在搜索」提示，再去看搜索结果与回答。
    //    若品牌消息已在 step 5.5 提前发送（brand_sent=true），此处跳过。
    if !search_rounds.is_empty() && !brand_sent {
        let brand_lines: Vec<String> = search_rounds
            .iter()
            .map(|r| format!("🌐 WebSearch Powered by XRL Router | 关键词: {}", r.query))
            .collect();
        let brand_text = brand_lines.join("\n");
        if !send_byte(
            tx,
            state.render_event(&IrStreamEvent::ContentBlockStart {
                index,
                block: IrContentBlockStart::Text,
            }),
        )
        .await
        {
            return;
        }
        if !send_byte(
            tx,
            state.render_event(&IrStreamEvent::ContentBlockDelta {
                index,
                delta: IrContentDelta::TextDelta(brand_text),
            }),
        )
        .await
        {
            return;
        }
        if !send_byte(
            tx,
            state.render_event(&IrStreamEvent::ContentBlockStop { index }),
        )
        .await
        {
            return;
        }
        index += 1;

        // 短暂延迟，让客户端有时间先渲染品牌行，再渲染后续搜索卡片与回答。
        // 没有这个延迟，所有 SSE 事件在同一个瞬间到达，客户端会合并渲染看不出先后。
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    // 3. 每轮搜索：server_tool_use + web_search_tool_result（index 连续递增）
    for round in search_rounds {
        if tx.send(Ok(state.render_server_tool_use(index, &round.tool_use_id, &round.query))).await.is_err() {
            return;
        }
        index += 1;
        if tx.send(Ok(state.render_web_search_tool_result(index, &round.tool_use_id, &round.results))).await.is_err() {
            return;
        }
        index += 1;
    }

    // 4. 最终回答文本块（搜索完成后再显示答案）
    let _ = send_byte(
        tx,
        state.render_event(&IrStreamEvent::ContentBlockStart {
            index,
            block: IrContentBlockStart::Text,
        }),
    )
    .await;
    let _ = send_byte(
        tx,
        state.render_event(&IrStreamEvent::ContentBlockDelta {
            index,
            delta: IrContentDelta::TextDelta(final_text.to_string()),
        }),
    )
    .await;
    let _ = send_byte(
        tx,
        state.render_event(&IrStreamEvent::ContentBlockStop { index }),
    )
    .await;

    // 5. 收尾（message_delta + message_stop）
    for b in state.finalize(usage) {
        let _ = tx.send(Ok(b)).await;
    }
}

/// 执行 Bing 搜索：从 tool_use input 提取 query → 隐藏 WebView 搜索。
///
/// 返回 `(格式化文本, 结构化结果)`——文本喂给模型（tool_result），
/// 结构化结果用于最终以 `web_search_tool_result` 块渲染给 Claude Code。
async fn execute_websearch_tool(
    state: &AppState,
    input: &Value,
) -> (String, Vec<crate::search::SearchResult>) {
    let query = match extract_query_from_tool_input(input) {
        Some(q) => q,
        None => return ("Error: no search query provided in tool input".to_string(), vec![]),
    };

    info!(query = %query, "websearch: executing search");

    match crate::search::bing::search(&state.search_http, &query).await {
        Ok(results) if results.is_empty() => {
            warn!(query = %query, "websearch: Bing returned 0 results");
            (
                format!("No web search results found for: {}", query),
                results,
            )
        }
        Ok(results) => {
            info!(query = %query, results_count = results.len(), "websearch: Bing search succeeded");
            (format_search_text(&results), results)
        }
        Err(e) => {
            warn!(query = %query, error = %e, "websearch: Bing search failed");
            (
                format!(
                    "Web search unavailable: {}. Do NOT make up information.",
                    e
                ),
                vec![],
            )
        }
    }
}

/// 把 Bing 结果格式化成喂给 LLM 的文本。
fn format_search_text(results: &[crate::search::SearchResult]) -> String {
    results
        .iter()
        .enumerate()
        .map(|(i, r)| format!("[{}] {}\n{}\n摘要: {}", i + 1, r.title, r.url, r.snippet))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 提前发送品牌消息（仅 Messages 客户端）。
///
/// 当模型首次决定调用 web_search 工具时，立刻合成一条完整的 Messages 流发送给客户端：
/// - message_start（含 id/model/usage 占位）
/// - content_block_start (text)
/// - content_block_delta (品牌文本：🌐 WebSearch Powered by XRL Router | 关键词: xxx)
/// - content_block_stop
/// - message_delta + message_stop
///
/// 这条消息独立于后续的最终回答流，让主人在搜索开始的瞬间就看到通知。
async fn emit_preliminary_brand_message(
    tx: &mpsc::Sender<Result<Bytes, Infallible>>,
    msg_id: &str,
    model: &str,
    queries: &[String],
) {
    let mut state = MessagesRenderState::new();

    // message_start（usage 用占位值，后续最终回答会携带真实 usage）
    if let Some(b) = state.render_event(&IrStreamEvent::MessageStart {
        id: msg_id.to_string(),
        model: model.to_string(),
        usage: Some(IrUsage::default()),
    }) {
        if tx.send(Ok(b)).await.is_err() {
            return;
        }
    }

    // 品牌文本块
    let brand_lines: Vec<String> = queries
        .iter()
        .map(|q| format!("🌐 WebSearch by XRL Router | Keywords: {}", q))
        .collect();
    let brand_text = brand_lines.join("\n");

    if let Some(b) = state.render_event(&IrStreamEvent::ContentBlockStart {
        index: 0,
        block: IrContentBlockStart::Text,
    }) {
        if tx.send(Ok(b)).await.is_err() {
            return;
        }
    }
    if let Some(b) = state.render_event(&IrStreamEvent::ContentBlockDelta {
        index: 0,
        delta: IrContentDelta::TextDelta(brand_text),
    }) {
        if tx.send(Ok(b)).await.is_err() {
            return;
        }
    }
    if let Some(b) = state.render_event(&IrStreamEvent::ContentBlockStop { index: 0 }) {
        if tx.send(Ok(b)).await.is_err() {
            return;
        }
    }

    // message_delta + message_stop（usage 占位）
    for b in state.finalize(&IrUsage::default()) {
        if tx.send(Ok(b)).await.is_err() {
            return;
        }
    }

    info!("websearch: preliminary brand message sent");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_has_websearch_tool_ir() {
        let req = IrRequest {
            model: "test".to_string(),
            system: None,
            messages: vec![],
            tools: vec![IrTool {
                name: "web_search".to_string(),
                description: None,
                input_schema: json!({}),
            }],
            tool_choice: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            thinking: None,
            stream: false,
        };
        assert!(has_websearch_tool_ir(&req));

        // Claude Code 的 PascalCase 客户端工具也应识别
        let req_cc = IrRequest {
            model: "test".to_string(),
            system: None,
            messages: vec![],
            tools: vec![IrTool {
                name: "WebSearch".to_string(),
                description: None,
                input_schema: json!({}),
            }],
            tool_choice: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            thinking: None,
            stream: false,
        };
        assert!(has_websearch_tool_ir(&req_cc));

        let req_no_tools = IrRequest {
            model: "test".to_string(),
            system: None,
            messages: vec![],
            tools: vec![],
            tool_choice: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            thinking: None,
            stream: false,
        };
        assert!(!has_websearch_tool_ir(&req_no_tools));
    }

    #[test]
    fn test_ensure_websearch_tool_injects_when_missing() {
        let mut req = IrRequest {
            model: "test".to_string(),
            system: None,
            messages: vec![],
            tools: vec![IrTool {
                name: "other_tool".to_string(),
                description: None,
                input_schema: json!({}),
            }],
            tool_choice: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            thinking: None,
            stream: false,
        };
        assert!(!has_websearch_tool_ir(&req));
        ensure_websearch_tool(&mut req);
        assert!(has_websearch_tool_ir(&req));
        assert_eq!(req.tools.len(), 2);
        assert_eq!(req.tools[1].name, "web_search");
    }

    #[test]
    fn test_ensure_websearch_tool_replaces_client_tools() {
        let mut req = IrRequest {
            model: "test".to_string(),
            system: None,
            messages: vec![],
            tools: vec![
                IrTool {
                    name: "WebSearch".to_string(), // Claude Code 客户端工具
                    description: None,
                    input_schema: json!({}),
                },
                IrTool {
                    name: "web_search_20250305".to_string(), // Anthropic 服务端内置
                    description: None,
                    input_schema: json!({}),
                },
                IrTool {
                    name: "Read".to_string(), // 普通工具不受影响
                    description: None,
                    input_schema: json!({}),
                },
            ],
            tool_choice: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            thinking: None,
            stream: false,
        };
        ensure_websearch_tool(&mut req);
        // 搜索类工具全部被替换为唯一的 web_search
        let names: Vec<&str> = req.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["Read", "web_search"]);
        // 注入的工具带 query 参数 schema
        let injected = &req.tools[1];
        assert!(injected.input_schema["required"] == json!(["query"]));
    }

    #[test]
    fn test_ensure_websearch_tool_rewrites_tool_choice() {
        let mut req = IrRequest {
            model: "test".to_string(),
            system: None,
            messages: vec![],
            tools: vec![IrTool {
                name: "WebSearch".to_string(),
                description: None,
                input_schema: json!({}),
            }],
            tool_choice: Some(IrToolChoice::Tool {
                name: "WebSearch".to_string(),
            }),
            max_tokens: None,
            temperature: None,
            top_p: None,
            thinking: None,
            stream: false,
        };
        ensure_websearch_tool(&mut req);
        assert!(matches!(
            req.tool_choice,
            Some(IrToolChoice::Tool { ref name }) if name == "web_search"
        ));
        // 非搜索工具的目标不应被改写
        let mut req_other = IrRequest {
            model: "test".to_string(),
            system: None,
            messages: vec![],
            tools: vec![IrTool {
                name: "Bash".to_string(),
                description: None,
                input_schema: json!({}),
            }],
            tool_choice: Some(IrToolChoice::Tool {
                name: "Bash".to_string(),
            }),
            max_tokens: None,
            temperature: None,
            top_p: None,
            thinking: None,
            stream: false,
        };
        ensure_websearch_tool(&mut req_other);
        assert!(matches!(
            req_other.tool_choice,
            Some(IrToolChoice::Tool { ref name }) if name == "Bash"
        ));
    }

    #[test]
    fn test_ensure_websearch_tool_keeps_other_tools() {
        let mut req = IrRequest {
            model: "test".to_string(),
            system: None,
            messages: vec![],
            tools: vec![IrTool {
                name: "Bash".to_string(),
                description: None,
                input_schema: json!({}),
            }],
            tool_choice: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            thinking: None,
            stream: false,
        };
        let tools_count = req.tools.len();
        ensure_websearch_tool(&mut req);
        assert_eq!(req.tools.len(), tools_count + 1);
        assert_eq!(req.tools[0].name, "Bash");
        assert_eq!(req.tools[1].name, "web_search");
    }

    #[test]
    fn test_extract_query_from_tool_input() {
        let input = json!({"query": "今天北京天气"});
        assert_eq!(
            extract_query_from_tool_input(&input),
            Some("今天北京天气".to_string())
        );

        let input_q = json!({"q": "rust programming"});
        assert_eq!(
            extract_query_from_tool_input(&input_q),
            Some("rust programming".to_string())
        );

        let input_empty = json!({"query": ""});
        assert_eq!(extract_query_from_tool_input(&input_empty), None);

        let input_none = json!({});
        assert_eq!(extract_query_from_tool_input(&input_none), None);
    }

    #[test]
    fn test_extract_websearch_tool_calls() {
        let msg = IrMessage {
            role: IrRole::Assistant,
            content: vec![
                IrContentBlock::Text {
                    text: "Let me search for that.".to_string(),
                    cache_control: None,
                },
                IrContentBlock::ToolUse {
                    id: "tool_1".to_string(),
                    name: "web_search".to_string(),
                    input: json!({"query": "test query"}),
                },
            ],
        };
        let calls = extract_websearch_tool_calls(&msg);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "tool_1");

        // 非 web_search 工具不应被提取
        let msg_other = IrMessage {
            role: IrRole::Assistant,
            content: vec![IrContentBlock::ToolUse {
                id: "tool_2".to_string(),
                name: "file_read".to_string(),
                input: json!({"path": "/tmp/test"}),
            }],
        };
        let calls = extract_websearch_tool_calls(&msg_other);
        assert!(calls.is_empty());
    }

    #[test]
    fn test_format_search_text() {
        let results = vec![crate::search::SearchResult {
            title: "Test".to_string(),
            url: "https://example.com".to_string(),
            snippet: "A snippet".to_string(),
        }];
        let text = format_search_text(&results);
        assert!(text.contains("[1] Test"));
        assert!(text.contains("https://example.com"));
        assert!(text.contains("A snippet"));
        // 页面内容增强已移除
        assert!(!text.contains("页面内容"));
    }
}

#[cfg(test)]
mod exhausted_loop_tests {
    use super::*;
    use serde_json::json;

    /// 模拟循环耗尽后：消息历史含多轮 tool_use/tool_result，tools 已移除。
    /// 验证清理逻辑：tool_use/tool_result 消息被移除，搜索结果合并为文本指令。
    #[test]
    fn test_exhausted_loop_cleans_tool_history() {
        let mut req = IrRequest {
            model: "qwen3.7-max".to_string(),
            system: None,
            messages: vec![
                // 原始用户问题
                IrMessage {
                    role: IrRole::User,
                    content: vec![IrContentBlock::Text {
                        text: "张雪峰是否去世了？".to_string(),
                        cache_control: None,
                    }],
                },
                // round 0: assistant tool_use
                IrMessage {
                    role: IrRole::Assistant,
                    content: vec![IrContentBlock::ToolUse {
                        id: "call_1".to_string(),
                        name: "web_search".to_string(),
                        input: json!({"query": "张雪峰 去世"}),
                    }],
                },
                // round 0: user tool_result
                IrMessage {
                    role: IrRole::User,
                    content: vec![IrContentBlock::ToolResult {
                        tool_use_id: "call_1".to_string(),
                        content: IrToolResultContent::Text("[1] 张雪峰 2026-03-25 去世".to_string()),
                        is_error: false,
                    }],
                },
                // round 1: assistant tool_use
                IrMessage {
                    role: IrRole::Assistant,
                    content: vec![IrContentBlock::ToolUse {
                        id: "call_2".to_string(),
                        name: "web_search".to_string(),
                        input: json!({"query": "张雪峰 辟谣"}),
                    }],
                },
                // round 1: user tool_result
                IrMessage {
                    role: IrRole::User,
                    content: vec![IrContentBlock::ToolResult {
                        tool_use_id: "call_2".to_string(),
                        content: IrToolResultContent::Text("[1] 官方讣告已发布".to_string()),
                        is_error: false,
                    }],
                },
            ],
            tools: vec![IrTool {
                name: "web_search".to_string(),
                description: None,
                input_schema: json!({}),
            }],
            tool_choice: Some(IrToolChoice::Auto),
            max_tokens: None,
            temperature: None,
            top_p: None,
            thinking: None,
            stream: true,
        };

        // 模拟循环耗尽后的清理逻辑
        let mut search_summary: Vec<String> = Vec::new();
        req.messages.retain(|m| {
            let has_tool_use = m.content.iter().any(|b| matches!(b, IrContentBlock::ToolUse { .. }));
            let has_tool_result = m.content.iter().any(|b| matches!(b, IrContentBlock::ToolResult { .. }));
            if has_tool_result {
                for b in &m.content {
                    if let IrContentBlock::ToolResult { content, .. } = b {
                        let text = match content {
                            IrToolResultContent::Text(t) => t.clone(),
                            IrToolResultContent::Blocks(blocks) => blocks.iter().filter_map(|b| {
                                if let IrContentBlock::Text { text, .. } = b { Some(text.clone()) } else { None }
                            }).collect::<Vec<_>>().join("\n"),
                        };
                        if !text.is_empty() { search_summary.push(text); }
                    }
                }
            }
            !has_tool_use && !has_tool_result
        });
        req.tools.retain(|t| !t.name.starts_with("web_search"));
        req.tool_choice = Some(IrToolChoice::None);
        if !search_summary.is_empty() {
            req.messages.push(IrMessage {
                role: IrRole::User,
                content: vec![IrContentBlock::Text {
                    text: format!("以下是网络搜索获得的相关信息，请基于这些信息回答用户的问题，不要再调用任何工具：\n\n{}", search_summary.join("\n\n")),
                    cache_control: None,
                }],
            });
        }

        // 断言：只有原始问题 + 搜索结果指令，无工具痕迹
        assert_eq!(req.messages.len(), 2, "应只剩原始问题 + 搜索结果指令: {:?}", req.messages);
        assert!(!req.messages[1].content.iter().any(|b| matches!(b, IrContentBlock::ToolUse { .. })),
            "搜索结果指令不应含 tool_use");
        assert!(req.messages[1].content.iter().any(|b| matches!(b, IrContentBlock::Text { text, .. } if text.contains("张雪峰 2026-03-25 去世"))),
            "搜索结果应合并进文本指令");
        assert!(req.messages[1].content.iter().any(|b| matches!(b, IrContentBlock::Text { text, .. } if text.contains("官方讣告已发布"))),
            "第二轮搜索结果也应合并");
        assert!(req.tools.is_empty(), "web_search 工具应被移除");
        assert!(matches!(req.tool_choice, Some(IrToolChoice::None)), "tool_choice 应为 None");
    }

    /// 验证清理后的请求序列化为 Chat Completions 时不再补回 web_search 工具。
    #[test]
    fn test_exhausted_loop_chat_completions_no_tool_readd() {
        let req = IrRequest {
            model: "qwen3.7-max".to_string(),
            system: None,
            messages: vec![
                IrMessage {
                    role: IrRole::User,
                    content: vec![IrContentBlock::Text {
                        text: "问题".to_string(),
                        cache_control: None,
                    }],
                },
                IrMessage {
                    role: IrRole::User,
                    content: vec![IrContentBlock::Text {
                        text: "以下是网络搜索获得的相关信息，请基于这些信息回答用户的问题，不要再调用任何工具：\n\n[1] 结果".to_string(),
                        cache_control: None,
                    }],
                },
            ],
            tools: vec![],
            tool_choice: Some(IrToolChoice::None),
            max_tokens: None,
            temperature: None,
            top_p: None,
            thinking: None,
            stream: true,
        };
        let v = super::super::ir::to_chat_completions::ir_req_to_chat_completions(&req);
        // tools 应为空（无历史 tool_calls 可补）
        assert!(v.get("tools").is_none() || v["tools"].as_array().unwrap().is_empty(),
            "清理后不应补回工具: {:?}", v.get("tools"));
        assert_eq!(v["tool_choice"], "none");
        assert_eq!(v["messages"].as_array().unwrap().len(), 2);
    }
}

#[cfg(test)]
mod no_progress_tests {
    use super::*;

    #[test]
    fn test_query_similarity_exact_match() {
        assert_eq!(query_similarity("张雪峰 去世", "张雪峰 去世"), 1.0);
    }

    #[test]
    fn test_query_similarity_close_match() {
        // 只差一个字符 → 高相似度
        let s = query_similarity("张雪峰 去世 2026", "张雪峰 去世 2025");
        assert!(s >= 0.8, "近义查询应判定为重复: {}", s);
    }

    #[test]
    fn test_query_similarity_different() {
        // 完全不同 → 低相似度
        let s = query_similarity("今天北京天气", "Apple stock price");
        assert!(s < 0.3, "不相关查询不应判定为重复: {}", s);
    }

    #[test]
    fn test_query_similarity_ignores_case_whitespace() {
        let s = query_similarity("Zhang Xuefeng died", "zhang xuefeng  died");
        assert_eq!(s, 1.0, "大小写/空白差异应视为相同: {}", s);
    }

    #[test]
    fn test_query_similarity_empty() {
        assert_eq!(query_similarity("", "anything"), 0.0);
        assert_eq!(query_similarity("", ""), 1.0);
    }

    #[test]
    fn test_query_similarity_partial_overlap() {
        // 共享「张雪峰」但其余不同 → 中等相似度
        let s = query_similarity("张雪峰 去世 辟谣", "张雪峰 高考 志愿");
        assert!(s > 0.3 && s < 0.8, "部分重叠应为中等相似度: {}", s);
    }

    #[test]
    fn test_no_progress_detection_same_query() {
        // 模拟：第 0 轮查「张雪峰 去世」，第 1 轮又查几乎相同的
        let history = vec!["张雪峰 去世 辟谣".to_string()];
        let round_queries = vec!["张雪峰 去世 辟谣".to_string()];
        let repetitive = history.iter().any(|prev| {
            round_queries.iter().any(|q| query_similarity(prev, q) >= QUERY_SIMILARITY_THRESHOLD)
        });
        assert!(repetitive, "相同查询应触发无进展检测");
    }

    #[test]
    fn test_no_progress_detection_different_query() {
        // 第 0 轮查「张雪峰 去世」，第 1 轮查完全不同的 → 不触发
        let history = vec!["张雪峰 去世 辟谣".to_string()];
        let round_queries = vec!["今天北京天气".to_string()];
        let repetitive = history.iter().any(|prev| {
            round_queries.iter().any(|q| query_similarity(prev, q) >= QUERY_SIMILARITY_THRESHOLD)
        });
        assert!(!repetitive, "不同查询不应触发无进展检测");
    }

    #[test]
    fn test_no_progress_detection_similar_but_progress() {
        // 查询变了但有进展（从「去世」问到「辟谣」再问到「最新消息」）
        let history = vec!["张雪峰 去世 2026".to_string(), "张雪峰 去世 辟谣".to_string()];
        let round_queries = vec!["\"张雪峰\" 最新消息".to_string()];
        let repetitive = history.iter().rev().take(2).any(|prev| {
            round_queries.iter().any(|q| query_similarity(prev, q) >= QUERY_SIMILARITY_THRESHOLD)
        });
        assert!(!repetitive, "有明显进展的新查询不应触发");
    }
}

#[cfg(test)]
mod progress_comment_tests {
    use super::*;

    /// SSE 注释应为 `: 文本\n\n`，且换行被转义避免破坏帧结构。
    #[test]
    fn test_send_progress_comment_format() {
        let (tx, mut rx) = mpsc::channel::<Result<Bytes, Infallible>>(10);
        send_progress_comment(&tx, "正在搜索 \"张雪峰 2026\"");
        let bytes = rx.try_recv().unwrap().unwrap();
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.starts_with(": "), "SSE 注释应以冒号开头: {:?}", s);
        assert!(s.ends_with("\n\n"), "SSE 注释应以空行结尾: {:?}", s);
        assert!(s.contains("正在搜索"), "注释应包含内容");
    }

    /// 换行内容应被转义为空格，避免破坏 SSE 帧。
    #[test]
    fn test_send_progress_comment_escapes_newline() {
        let (tx, mut rx) = mpsc::channel::<Result<Bytes, Infallible>>(10);
        send_progress_comment(&tx, "line1\nline2\rline3");
        let bytes = rx.try_recv().unwrap().unwrap();
        let s = String::from_utf8_lossy(&bytes);
        // 帧末尾的 \n\n 是 SSE 分隔符，内容中间不应有换行
        let body = s.trim_end_matches('\n');
        assert!(!body.contains('\n'), "内容中间换行应被转义: {:?}", s);
        assert!(!body.contains('\r'), "回车应被转义: {:?}", s);
        assert!(body.contains("line1 line2 line3"), "换行应替换为空格: {:?}", s);
    }

    /// 客户端断开（channel 关闭）时静默失败，不 panic。
    #[test]
    fn test_send_progress_comment_silent_on_disconnect() {
        let (tx, _rx) = mpsc::channel::<Result<Bytes, Infallible>>(10);
        drop(_rx); // 客户端断开
        send_progress_comment(&tx, "should not panic");
    }
}

#[cfg(test)]
mod server_tool_render_tests {
    use super::*;
    use crate::api::proxy::ir::types::IrUsage;

    /// 合成 Messages 流应包含：message_start → 品牌通知 → server_tool_use → web_search_tool_result
    /// → text(最终回答) → message_delta → message_stop，且块 index 连续递增。
    #[tokio::test]
    async fn test_render_websearch_messages_final_full_stream() {
        let rounds = vec![
            SearchRound {
                tool_use_id: "toolu_1".to_string(),
                query: "张雪峰 高考志愿".to_string(),
                results: vec![crate::search::SearchResult {
                    title: "张雪峰报志愿逻辑".into(),
                    url: "https://zhuanlan.zhihu.com/p/1".into(),
                    snippet: "核心观点".into(),
                }],
            },
            SearchRound {
                tool_use_id: "toolu_2".to_string(),
                query: "2026 高考 指南".to_string(),
                results: vec![],
            },
        ];
        let usage = IrUsage {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        };

        let (tx, mut rx) = mpsc::channel::<Result<Bytes, Infallible>>(100);
        render_websearch_messages_final(
            &tx,
            "msg_123",
            "qwen3.7-max",
            &usage,
            &rounds,
            "根据搜索结果，张雪峰的高考志愿方法如下…",
            false,
        )
        .await;
        drop(tx);

        let mut bytes = Vec::new();
        while let Some(Ok(b)) = rx.recv().await {
            bytes.push(b);
        }
        let s = bytes.iter().map(|b| String::from_utf8_lossy(b).to_string()).collect::<String>();

        // 关键事件齐全
        assert!(s.contains("event: message_start"));
        assert!(s.contains("\"id\":\"msg_123\""));
        assert!(s.contains("event: content_block_start"));
        assert!(s.contains("\"type\":\"server_tool_use\""), "应有 server_tool_use 块");
        assert!(s.contains("\"name\":\"web_search\""));
        assert!(s.contains("\"type\":\"web_search_tool_result\""), "应有 web_search_tool_result 块");
        assert!(s.contains("\"title\":\"张雪峰报志愿逻辑\""));
        assert!(s.contains("event: message_delta"));
        assert!(s.contains("event: message_stop"));
        // 最终回答文本在 text_delta 中
        assert!(s.contains("根据搜索结果"));
        // 两轮搜索 → 两个 server_tool_use + 两个 tool_result，块 index 0..3
        assert_eq!(s.matches("\"type\":\"server_tool_use\"").count(), 2);
        assert_eq!(s.matches("\"type\":\"web_search_tool_result\"").count(), 2);
        // index 连续：0,1,2,3 各出现
        for i in 0..4 {
            assert!(s.contains(&format!("\"index\":{}", i)), "index {} 应存在", i);
        }
        // usage 渲染
        assert!(s.contains("\"input_tokens\":100"));
    }

    /// 合成流以 message_start 开头、以 message_stop 结尾，顺序合法。
    #[tokio::test]
    async fn test_render_websearch_messages_final_sequence() {
        let rounds = vec![SearchRound {
            tool_use_id: "toolu_1".to_string(),
            query: "测试".to_string(),
            results: vec![],
        }];
        let usage = IrUsage::default();

        let (tx, mut rx) = mpsc::channel::<Result<Bytes, Infallible>>(100);
        render_websearch_messages_final(
            &tx, "msg_1", "m", &usage, &rounds, "回答", false,
        )
        .await;
        drop(tx);

        let mut bytes = Vec::new();
        while let Some(Ok(b)) = rx.recv().await {
            bytes.push(b);
        }
        let s = bytes.iter().map(|b| String::from_utf8_lossy(b).to_string()).collect::<String>();

        let start_pos = s.find("event: message_start").expect("应以 message_start 开头");
        let tool_pos = s.find("server_tool_use").expect("应含 server_tool_use");
        let result_pos = s.find("web_search_tool_result").expect("应含 web_search_tool_result");
        let text_pos = s.rfind("event: content_block_start").expect("应有最终文本块 start");
        let end_pos = s.rfind("event: message_stop").expect("应以 message_stop 结尾");

        assert!(start_pos < tool_pos, "message_start 应在 server_tool_use 前");
        assert!(tool_pos < result_pos, "server_tool_use 应在 tool_result 前");
        assert!(result_pos < text_pos, "tool_result 应在最终文本前");
        assert!(text_pos < end_pos, "文本应在 message_stop 前");
    }

    /// 无搜索轮次时合成流仅含 message_start + text + 收尾（仍合法）。
    #[tokio::test]
    async fn test_render_websearch_messages_final_no_rounds() {
        let usage = IrUsage::default();
        let (tx, mut rx) = mpsc::channel::<Result<Bytes, Infallible>>(100);
        render_websearch_messages_final(&tx, "msg_2", "m", &usage, &[], "直接回答", false).await;
        drop(tx);

        let mut bytes = Vec::new();
        while let Some(Ok(b)) = rx.recv().await {
            bytes.push(b);
        }
        let s = bytes.iter().map(|b| String::from_utf8_lossy(b).to_string()).collect::<String>();
        assert!(s.contains("event: message_start"));
        assert!(s.contains("直接回答"));
        assert!(s.contains("event: message_stop"));
        assert!(!s.contains("server_tool_use"), "无搜索轮次不应有 server_tool_use");
    }

    /// brand_sent=true 时应跳过品牌块，直接渲染 server_tool_use 与最终文本。
    #[tokio::test]
    async fn test_render_websearch_messages_final_brand_already_sent() {
        let rounds = vec![SearchRound {
            tool_use_id: "toolu_1".to_string(),
            query: "蔡徐坤".to_string(),
            results: vec![],
        }];
        let usage = IrUsage::default();
        let (tx, mut rx) = mpsc::channel::<Result<Bytes, Infallible>>(100);
        render_websearch_messages_final(&tx, "msg_3", "m", &usage, &rounds, "最终回答", true).await;
        drop(tx);

        let mut bytes = Vec::new();
        while let Some(Ok(b)) = rx.recv().await {
            bytes.push(b);
        }
        let s = bytes.iter().map(|b| String::from_utf8_lossy(b).to_string()).collect::<String>();

        // 品牌块不应出现（已在 step 5.5 提前发送）
        assert!(!s.contains("WebSearch Powered by XRL Router"), "品牌块不应重复渲染");
        // 但 server_tool_use 与最终文本仍应正常渲染
        assert!(s.contains("server_tool_use"), "应有 server_tool_use");
        assert!(s.contains("最终回答"), "应有最终回答");
        assert!(s.contains("event: message_stop"));
    }
}

/// 提前发送品牌消息的测试
#[cfg(test)]
mod preliminary_brand_tests {
    use super::*;

    /// emit_preliminary_brand_message 应发送完整的 message 流（start → text → delta → stop）
    #[tokio::test]
    async fn test_emit_preliminary_brand_message_full_stream() {
        let (tx, mut rx) = mpsc::channel::<Result<Bytes, Infallible>>(100);
        emit_preliminary_brand_message(&tx, "msg_brand", "gpt-4o", &["蔡徐坤 2026 近况".to_string()]).await;
        drop(tx);

        let mut bytes = Vec::new();
        while let Some(Ok(b)) = rx.recv().await {
            bytes.push(b);
        }
        let s = bytes.iter().map(|b| String::from_utf8_lossy(b).to_string()).collect::<String>();

        assert!(s.contains("event: message_start"), "应有 message_start");
        assert!(s.contains("\"id\":\"msg_brand\""), "msg_id 应正确");
        assert!(s.contains("event: content_block_start"), "应有 content_block_start");
        assert!(s.contains("\"type\":\"text\""), "应为 text 块");
        assert!(s.contains("event: content_block_delta"), "应有 content_block_delta");
        assert!(s.contains("WebSearch Powered by XRL Router"), "应含品牌文本");
        assert!(s.contains("蔡徐坤 2026 近况"), "应含关键词");
        assert!(s.contains("event: content_block_stop"), "应有 content_block_stop");
        assert!(s.contains("event: message_delta"), "应有 message_delta");
        assert!(s.contains("event: message_stop"), "应有 message_stop");
    }

    /// 多个查询词应合并为多行品牌文本
    #[tokio::test]
    async fn test_emit_preliminary_brand_message_multiple_queries() {
        let (tx, mut rx) = mpsc::channel::<Result<Bytes, Infallible>>(100);
        emit_preliminary_brand_message(
            &tx,
            "msg_multi",
            "claude-3",
            &["查询1".to_string(), "查询2".to_string(), "查询3".to_string()],
        )
        .await;
        drop(tx);

        let mut bytes = Vec::new();
        while let Some(Ok(b)) = rx.recv().await {
            bytes.push(b);
        }
        let s = bytes.iter().map(|b| String::from_utf8_lossy(b).to_string()).collect::<String>();

        // 三个关键词都应出现
        assert!(s.contains("查询1"), "应含查询1");
        assert!(s.contains("查询2"), "应含查询2");
        assert!(s.contains("查询3"), "应含查询3");
        // 每行都应有品牌前缀（通过换行分隔）
        let brand_count = s.matches("WebSearch Powered by XRL Router").count();
        assert_eq!(brand_count, 3, "应有 3 行品牌文本");
    }

    /// 客户端断开时应静默失败，不 panic
    #[tokio::test]
    async fn test_emit_preliminary_brand_message_silent_on_disconnect() {
        let (tx, _rx) = mpsc::channel::<Result<Bytes, Infallible>>(10);
        drop(_rx); // 客户端断开
        emit_preliminary_brand_message(&tx, "msg_x", "m", &["test".to_string()]).await;
        // 应静默返回，不 panic
    }
}
