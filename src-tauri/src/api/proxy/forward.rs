//! 流式转发：统一 IR 路径。
//!
//! 单一函数 `forward_stream_ir` 处理所有格式组合：
//! 从上游字节流解析 SSE → 按 provider_kind 转为 IR 事件 →
//! 按 client_format 渲染为客户端 SSE 字节 → 同步累积 IrUsage。

use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::stream::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::gateway::server::AppState;

use super::auth::ServiceKeyInfo;
use super::ir;
use super::ir::types::IrUsage;
use super::route::ResolvedRoute;
use super::stream::{send_error_event, ClientFormat};
use super::UPSTREAM_CHUNK_TIMEOUT_SECS;

/// 流式转发结果。
///
/// 上游可能以 HTTP 200 + SSE error event 表达密钥级错误(欠费/限流/认证),
/// 客户端无法区分——Claude Code 表现为 "empty or malformed response (HTTP 200)"。
/// 转发函数负责检测这类错误,回传给双循环决定是否换密钥重试。
#[derive(Debug)]
pub(super) enum ForwardOutcome {
    /// 正常完成(完整流已转发给客户端)。
    Completed,
    /// 上游 200 + 流内密钥级错误事件,尚未向客户端发送任何内容。
    /// status 为推断的 HTTP 语义(401/402/403/429),用于密钥健康更新。
    UpstreamKeyError { status: u16, message: String },
    /// 上游错误已透传(发送了 SSE error event);或流内错误非密钥级
    /// (换密钥无意义),已透传。
    ErrorDelivered,
}

/// 统一 IR 转发：上游字节 → IR 事件 → 客户端 SSE 字节。
///
/// 替代旧的三路分支（passthrough / O→A / A→O），
/// 所有格式组合都走同一条路径。
pub(super) async fn forward_stream_ir(
    response: reqwest::Response,
    tx: &mpsc::Sender<Result<Bytes, Infallible>>,
    state: &Arc<AppState>,
    trace_id: &str,
    start_time: Instant,
    provider_id: &str,
    resolved: &ResolvedRoute,
    model_name: &str,
    service_key: &ServiceKeyInfo,
    last_key_id: &Option<String>,
    last_key_name: &Option<String>,
    last_key_masked: &Option<String>,
    endpoint: &'static str,
    provider_kind: &str,
    client_format: ClientFormat,
    est_input: u64,
) -> ForwardOutcome {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut chunk_count = 0u64;

    // 是否已向客户端发送过渲染字节(:keepalive 由 proxy_stream 发送,不计入)。
    // 流内错误事件出现时,若已发过内容则无法换密钥重试,只能透传。
    let mut sent_any = false;
    // 原始响应体捕获:用于「200 + 非 SSE 纯 JSON 错误体」检测(无 \n\n 帧)。
    // 上限 64KB,超限放弃;出现任何 SSE 帧即放弃(帧循环会检测错误事件)。
    let mut raw_capture: Option<String> = Some(String::new());
    const RAW_CAPTURE_LIMIT: usize = 64 * 1024;

    // 上游解析器状态（按 provider_kind 选择）
    let mut anthropic_parse = ir::from_messages::MessagesParseState::new();
    let mut chat_parse = ir::from_chat_completions::ChatCompletionsParseState::new();
    let mut responses_parse = ir::from_responses::ResponsesParseState::new();

    // 客户端渲染器状态（按 client_format 选择）
    let mut anthropic_render = ir::to_messages::MessagesRenderState::new();
    let mut chat_render = ir::to_chat_completions::ChatCompletionsRenderState::new();
    let mut responses_render = ir::to_responses::ResponsesRenderState::new();

    // 预填充估算的 input tokens（供 message_start 占位）
    anthropic_parse.usage.input_tokens = est_input;
    chat_parse.usage.input_tokens = est_input;
    responses_parse.usage.input_tokens = est_input;

    let mut saw_done = false;

    'outer: loop {
        let chunk = match tokio::time::timeout(
            Duration::from_secs(UPSTREAM_CHUNK_TIMEOUT_SECS),
            stream.next(),
        )
        .await
        {
            Ok(Some(Ok(c))) => c,
            Ok(Some(Err(e))) => {
                warn!(trace_id = %trace_id, error = %e, "upstream stream error during IR forwarding");
                break;
            }
            Ok(None) => break,
            Err(_) => {
                warn!(trace_id = %trace_id, "upstream stream silent for {}s, closing", UPSTREAM_CHUNK_TIMEOUT_SECS);
                break;
            }
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        if let Some(cap) = raw_capture.as_mut() {
            cap.push_str(&String::from_utf8_lossy(&chunk));
            if cap.len() > RAW_CAPTURE_LIMIT {
                raw_capture = None;
            }
        }

        while let Some(newline_pos) = buffer.find("\n\n") {
            // 出现任何 SSE 帧 → 非 SSE 检测放弃
            raw_capture = None;
            let frame = buffer[..newline_pos].to_string();
            buffer = buffer[newline_pos + 2..].to_string();

            // 跳过 keepalive 注释
            if frame.starts_with(':') {
                continue;
            }

            // 提取 data: 行
            let data = if let Some(d) = frame.strip_prefix("data: ") {
                d
            } else {
                // 可能有 event: 行 + data: 行
                let mut found_data = None;
                for line in frame.lines() {
                    if let Some(d) = line.strip_prefix("data: ") {
                        found_data = Some(d);
                    }
                }
                match found_data {
                    Some(d) => d,
                    None => continue,
                }
            };

            if data == "[DONE]" {
                saw_done = true;
                break 'outer;
            }

            if let Ok(chunk_json) = serde_json::from_str::<Value>(data) {
                chunk_count += 1;

                // 流内错误事件检测(必须在 IR 解析之前:Chat 错误 chunk 带
                // id/model 时解析器会先发出 MessageStart,导致误判为已发内容)。
                // 密钥级错误(401/402/403/429)且未发任何内容 → 可换密钥重试;
                // 其余情况(非密钥级或已发内容)→ 透传 SSE error event。
                if let Some((status, msg)) = extract_stream_error(&chunk_json, provider_kind) {
                    if matches!(status, 401 | 402 | 403 | 429) && !sent_any {
                        warn!(trace_id = %trace_id, status, upstream_error = %msg, "upstream 200 with SSE error event (key-level)");
                        return ForwardOutcome::UpstreamKeyError { status, message: msg };
                    }
                    warn!(trace_id = %trace_id, status, upstream_error = %msg, "upstream 200 with SSE error event, forwarding");
                    send_error_event(tx, client_format, "api_error", &msg);
                    return ForwardOutcome::ErrorDelivered;
                }

                // 1. 解析上游 chunk → IR 事件
                let ir_events = match provider_kind {
                    "messages" => {
                        ir::from_messages::messages_chunk_to_ir(&chunk_json, &mut anthropic_parse)
                    }
                    "responses" => {
                        ir::from_responses::responses_chunk_to_ir(&chunk_json, &mut responses_parse)
                    }
                    _ => {
                        // ChatCompletions 兼容的所有上游格式统一按 Chat Completions 解析
                        ir::from_chat_completions::chat_completions_chunk_to_ir(&chunk_json, &mut chat_parse)
                    }
                };

                // 2. 渲染 IR 事件 → 客户端 SSE 字节
                for ev in &ir_events {
                    let bytes = match client_format {
                        ClientFormat::Messages => anthropic_render.render_event(ev),
                        ClientFormat::ChatCompletions => chat_render.render_event(ev),
                        ClientFormat::Responses => responses_render.render_event(ev),
                    };
                    if let Some(b) = bytes {
                        if tx.send(Ok(b)).await.is_err() {
                            return ForwardOutcome::Completed; // 客户端断开
                        }
                        sent_any = true;
                    }
                }
            }
        }
    }

    // 流结束且从未出现任何 SSE 帧(chunk_count == 0)→ 检查原始体是否为
    // 非 SSE 纯 JSON 错误体(如 {"error":{...}} 无 \n\n)。内容审核空流
    // ({"choices":[{...finish_reason":"content_filter"}]})无 error 键,不触发。
    if chunk_count == 0 {
        if let Some(body) = raw_capture {
            if let Some((status, msg)) = extract_non_sse_error(&body, provider_kind) {
                if matches!(status, 401 | 402 | 403 | 429) && !sent_any {
                    warn!(trace_id = %trace_id, status, upstream_error = %msg, "upstream 200 non-SSE error body (key-level)");
                    return ForwardOutcome::UpstreamKeyError { status, message: msg };
                }
                warn!(trace_id = %trace_id, status, upstream_error = %msg, "upstream 200 non-SSE error body, forwarding");
                send_error_event(tx, client_format, "api_error", &msg);
                return ForwardOutcome::ErrorDelivered;
            }
        }
    }

    // 3. 渲染收尾事件
    let final_usage = match provider_kind {
        "messages" => anthropic_parse.usage.clone(),
        "responses" => responses_parse.usage.clone(),
        _ => chat_parse.usage.clone(),
    };

    let finalize_bytes = match client_format {
        ClientFormat::Messages => anthropic_render.finalize(&final_usage),
        ClientFormat::ChatCompletions => chat_render.finalize(&final_usage),
        ClientFormat::Responses => responses_render.finalize(&final_usage),
    };
    for b in finalize_bytes {
        if tx.send(Ok(b)).await.is_err() {
            break;
        }
    }

    info!(
        trace_id = %trace_id,
        total_chunks = chunk_count,
        done = saw_done,
        provider_kind = provider_kind,
        "Stream ended (IR)"
    );

    // 4. 记录 usage
    let output_tokens = if final_usage.output_tokens > 0 {
        final_usage.output_tokens as i64
    } else {
        (final_usage.output_chars / 4) as i64
    };
    let input_t = final_usage.input_tokens as i64;
    let cr = final_usage.cache_read_input_tokens as i64;
    let _ = state.database.insert_usage_log(
        chrono::Utc::now().timestamp(),
        provider_id,
        resolved.provider_name.as_str(),
        &resolved.model_row_id,
        model_name,
        last_key_id.as_deref(),
        last_key_name.as_deref().unwrap_or(""),
        last_key_masked.as_deref().unwrap_or(""),
        Some(service_key.id.as_str()),
        service_key.name.as_str(),
        service_key.key_masked.as_str(),
        endpoint,
        input_t,
        output_tokens,
        start_time.elapsed().as_millis() as i64,
        true,
        None,
        cr,
    );

    ForwardOutcome::Completed
}

/// 缓冲转发：读取完整上游响应，累积 IR 事件 + 渲染字节，但不发送到 tx。
///
/// 返回 `(ir_events, final_usage, rendered_bytes, stream_error)`：
/// - `ir_events`：完整的 IR 事件序列，用于 `accumulate_ir_events` 重建消息
/// - `final_usage`：最终 token 用量
/// - `rendered_bytes`：渲染好的客户端 SSE 字节（含 finalize 事件）
/// - `stream_error`：流内错误事件（HTTP 200 + SSE error / 非 SSE JSON 错误体），
///   命中时前三个值为空（调用方应据此决定换密钥重试或透传）
///
/// 用于两步 websearch 劫持：缓冲第一次上游调用的完整响应，
/// 检查是否有 web_search tool_use，决定是回放缓冲还是执行搜索后二次调用。
pub(super) async fn forward_stream_ir_to_buffer(
    response: reqwest::Response,
    trace_id: &str,
    provider_kind: &str,
    client_format: ClientFormat,
    est_input: u64,
) -> (Vec<ir::types::IrStreamEvent>, IrUsage, Vec<Bytes>, Option<(u16, String)>) {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();

    // 上游解析器状态
    let mut anthropic_parse = ir::from_messages::MessagesParseState::new();
    let mut chat_parse = ir::from_chat_completions::ChatCompletionsParseState::new();
    let mut responses_parse = ir::from_responses::ResponsesParseState::new();

    // 客户端渲染器状态
    let mut anthropic_render = ir::to_messages::MessagesRenderState::new();
    let mut chat_render = ir::to_chat_completions::ChatCompletionsRenderState::new();
    let mut responses_render = ir::to_responses::ResponsesRenderState::new();

    // 预填充估算的 input tokens
    anthropic_parse.usage.input_tokens = est_input;
    chat_parse.usage.input_tokens = est_input;
    responses_parse.usage.input_tokens = est_input;

    let mut all_ir_events: Vec<ir::types::IrStreamEvent> = Vec::new();
    let mut rendered_bytes: Vec<Bytes> = Vec::new();
    let mut had_error: Option<(u16, String)> = None;

    'outer: loop {
        let chunk = match tokio::time::timeout(
            Duration::from_secs(UPSTREAM_CHUNK_TIMEOUT_SECS),
            stream.next(),
        )
        .await
        {
            Ok(Some(Ok(c))) => c,
            Ok(Some(Err(e))) => {
                warn!(trace_id = %trace_id, error = %e, "upstream stream error during buffer");
                break;
            }
            Ok(None) => break,
            Err(_) => {
                warn!(trace_id = %trace_id, "upstream stream silent during buffer, closing");
                break;
            }
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(newline_pos) = buffer.find("\n\n") {
            let frame = buffer[..newline_pos].to_string();
            buffer = buffer[newline_pos + 2..].to_string();

            if frame.starts_with(':') {
                continue;
            }

            let data = if let Some(d) = frame.strip_prefix("data: ") {
                d
            } else {
                let mut found_data = None;
                for line in frame.lines() {
                    if let Some(d) = line.strip_prefix("data: ") {
                        found_data = Some(d);
                    }
                }
                match found_data {
                    Some(d) => d,
                    None => continue,
                }
            };

            if data == "[DONE]" {
                break 'outer;
            }

            if let Ok(chunk_json) = serde_json::from_str::<Value>(data) {
                // 流内错误事件检测(必须在 IR 解析之前,同 forward_stream_ir)
                if let Some(err) = extract_stream_error(&chunk_json, provider_kind) {
                    warn!(trace_id = %trace_id, status = err.0, upstream_error = %err.1, "upstream 200 with SSE error event (buffered)");
                    had_error = Some(err);
                    break 'outer;
                }

                let ir_events = match provider_kind {
                    "messages" => {
                        ir::from_messages::messages_chunk_to_ir(&chunk_json, &mut anthropic_parse)
                    }
                    "responses" => {
                        ir::from_responses::responses_chunk_to_ir(&chunk_json, &mut responses_parse)
                    }
                    _ => {
                        ir::from_chat_completions::chat_completions_chunk_to_ir(&chunk_json, &mut chat_parse)
                    }
                };

                // 累积 IR 事件
                all_ir_events.extend(ir_events.iter().cloned());

                // 渲染字节（但不发送）
                for ev in &ir_events {
                    let bytes = match client_format {
                        ClientFormat::Messages => anthropic_render.render_event(ev),
                        ClientFormat::ChatCompletions => chat_render.render_event(ev),
                        ClientFormat::Responses => responses_render.render_event(ev),
                    };
                    if let Some(b) = bytes {
                        rendered_bytes.push(b);
                    }
                }
            }
        }
    }

    // 流内错误：丢弃已累积内容,直接返回错误(调用方决定重试或透传)
    if let Some(err) = had_error {
        return (vec![], IrUsage::default(), vec![], Some(err));
    }

    // 收尾事件
    let final_usage = match provider_kind {
        "messages" => anthropic_parse.usage.clone(),
        "responses" => responses_parse.usage.clone(),
        _ => chat_parse.usage.clone(),
    };

    let finalize_bytes = match client_format {
        ClientFormat::Messages => anthropic_render.finalize(&final_usage),
        ClientFormat::ChatCompletions => chat_render.finalize(&final_usage),
        ClientFormat::Responses => responses_render.finalize(&final_usage),
    };
    for b in finalize_bytes {
        rendered_bytes.push(b);
    }

    info!(
        trace_id = %trace_id,
        ir_events_count = all_ir_events.len(),
        rendered_bytes_count = rendered_bytes.len(),
        provider_kind = provider_kind,
        "Buffered stream complete"
    );

    (all_ir_events, final_usage, rendered_bytes, None)
}

// ═══════════════════════════════════════════════════════════════════
// 流内错误检测
// ═══════════════════════════════════════════════════════════════════

/// 根据上游错误 type/message 推断密钥健康状态。
///
/// 与 HTTP 级密钥轮换语义对齐:401/403 = 密钥无效(红),402/429 = 欠费/限流(黄)。
/// 关键词不命中返回 400(非密钥级错误,换密钥无意义)。
fn classify_error(err_type: &str, message: &str) -> u16 {
    let haystack = format!("{} {}", err_type, message).to_lowercase();

    // 429: 限流
    for kw in ["rate_limit", "ratelimit", "too_many", "limit_reached", "rate limit"] {
        if haystack.contains(kw) {
            return 429;
        }
    }
    // 402: 欠费/配额耗尽
    for kw in [
        "quota", "insufficient", "billing", "balance", "payment", "credit", "free limit",
    ] {
        if haystack.contains(kw) {
            return 402;
        }
    }
    // 401: 认证失败
    for kw in ["authentication", "invalid_api_key", "unauthorized", "api_key", "apikey"] {
        if haystack.contains(kw) {
            return 401;
        }
    }
    // 403: 权限不足
    for kw in ["permission", "forbidden", "access denied"] {
        if haystack.contains(kw) {
            return 403;
        }
    }
    400
}

/// 检测 SSE chunk 是否为上游错误事件(HTTP 200 但流内 error)。
///
/// 返回 (推断状态码, 错误消息)。正常 chunk 返回 None。
/// 各 provider 格式:
/// - messages:  `{"type":"error","error":{"type":"insufficient_quota","message":...}}`
/// - responses: `{"type":"response.failed","response":{"error":{...}}}` 或 `{"type":"error",...}`
/// - chat:      `{"error":{"code":...,"message":...}}`
fn extract_stream_error(chunk: &Value, provider_kind: &str) -> Option<(u16, String)> {
    let (err_type, message) = match provider_kind {
        "messages" => {
            if chunk["type"].as_str() != Some("error") {
                return None;
            }
            let err = &chunk["error"];
            if !err.is_object() {
                return None;
            }
            (
                err["type"].as_str().unwrap_or("").to_string(),
                err["message"].as_str().unwrap_or("").to_string(),
            )
        }
        "responses" => {
            let ty = chunk["type"].as_str().unwrap_or("");
            let err = if ty == "response.failed" {
                &chunk["response"]["error"]
            } else if ty == "error" {
                &chunk["error"]
            } else {
                return None;
            };
            if !err.is_object() {
                return None;
            }
            (
                err["type"]
                    .as_str()
                    .or_else(|| err["code"].as_str())
                    .unwrap_or("")
                    .to_string(),
                err["message"].as_str().unwrap_or("").to_string(),
            )
        }
        _ => {
            let err = &chunk["error"];
            if !err.is_object() {
                return None;
            }
            (
                err["type"]
                    .as_str()
                    .or_else(|| err["code"].as_str())
                    .unwrap_or("")
                    .to_string(),
                err["message"].as_str().unwrap_or("").to_string(),
            )
        }
    };
    let message = if message.is_empty() {
        err_type.clone()
    } else {
        message
    };
    Some((classify_error(&err_type, &message), message))
}

/// 检测 HTTP 200 非 SSE 响应体(无 `\n\n` 帧)是否为上游错误 JSON。
///
/// 仅当响应体是完整 JSON 且含顶层 error 对象时视为错误——内容审核空流
/// ({"choices":[{...finish_reason":"content_filter"}]})没有 error 键,不触发;
/// 空响应体 / HTML / 其他 JSON 均不触发。
fn extract_non_sse_error(body: &str, provider_kind: &str) -> Option<(u16, String)> {
    let v = serde_json::from_str::<Value>(body).ok()?;
    extract_stream_error(&v, provider_kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── classify_error ──

    #[test]
    fn test_classify_rate_limit() {
        assert_eq!(classify_error("rate_limit_exceeded", ""), 429);
        assert_eq!(classify_error("", "Too many requests, please retry later"), 429);
        assert_eq!(classify_error("requests_ratelimited", ""), 429);
        assert_eq!(classify_error("", "you have reached the limit of requests"), 429);
    }

    #[test]
    fn test_classify_quota_insufficient() {
        assert_eq!(classify_error("insufficient_quota", ""), 402);
        assert_eq!(classify_error("", "You exceeded your current quota"), 402);
        assert_eq!(classify_error("billing_error", ""), 402);
        assert_eq!(classify_error("", "No balance"), 402);
        assert_eq!(classify_error("", "Insufficient credit"), 402);
        assert_eq!(classify_error("", "Payment required"), 402);
        assert_eq!(classify_error("", "free limit reached"), 402);
    }

    #[test]
    fn test_classify_auth() {
        assert_eq!(classify_error("authentication_error", ""), 401);
        assert_eq!(classify_error("invalid_api_key", ""), 401);
        assert_eq!(classify_error("", "Unauthorized"), 401);
        assert_eq!(classify_error("", "Invalid API key provided"), 401);
        assert_eq!(classify_error("apikey_invalid", ""), 401);
    }

    #[test]
    fn test_classify_permission() {
        assert_eq!(classify_error("permission_error", ""), 403);
        assert_eq!(classify_error("", "forbidden"), 403);
        assert_eq!(classify_error("", "access denied"), 403);
    }

    #[test]
    fn test_classify_non_key_errors() {
        assert_eq!(classify_error("overloaded_error", ""), 400);
        assert_eq!(classify_error("", "model does not support this parameter"), 400);
        assert_eq!(classify_error("", ""), 400);
    }

    #[test]
    fn test_classify_case_insensitive() {
        assert_eq!(classify_error("QUOTA_EXCEEDED", "INSUFFICIENT QUOTA"), 402);
        assert_eq!(classify_error("RateLimitError", ""), 429);
    }

    // ── extract_stream_error ──

    #[test]
    fn test_extract_messages_error() {
        let chunk = json!({"type": "error", "error": {"type": "insufficient_quota", "message": "Your credit balance is too low"}});
        let (status, msg) = extract_stream_error(&chunk, "messages").unwrap();
        assert_eq!(status, 402);
        assert_eq!(msg, "Your credit balance is too low");
    }

    #[test]
    fn test_extract_messages_normal_not_error() {
        let chunk = json!({"type": "message_start", "message": {"id": "msg_1"}});
        assert!(extract_stream_error(&chunk, "messages").is_none());
    }

    #[test]
    fn test_extract_chat_error() {
        let chunk = json!({"error": {"code": "invalid_api_key", "message": "Incorrect API key provided"}});
        let (status, msg) = extract_stream_error(&chunk, "chat").unwrap();
        assert_eq!(status, 401);
        assert_eq!(msg, "Incorrect API key provided");
    }

    #[test]
    fn test_extract_chat_quota_message() {
        let chunk = json!({"error": {"message": "Insufficient quota"}});
        let (status, msg) = extract_stream_error(&chunk, "chat").unwrap();
        assert_eq!(status, 402);
        assert_eq!(msg, "Insufficient quota");
    }

    #[test]
    fn test_extract_chat_normal_not_error() {
        let chunk = json!({"id": "chatcmpl-1", "model": "gpt-4o", "choices": [{"delta": {"content": "hi"}}]});
        assert!(extract_stream_error(&chunk, "chat").is_none());
    }

    #[test]
    fn test_extract_responses_failed() {
        let chunk = json!({"type": "response.failed", "response": {"status": "failed", "error": {"code": "insufficient_quota", "message": "Quota exceeded"}}});
        let (status, msg) = extract_stream_error(&chunk, "responses").unwrap();
        assert_eq!(status, 402);
        assert_eq!(msg, "Quota exceeded");
    }

    #[test]
    fn test_extract_responses_error_event() {
        let chunk = json!({"type": "error", "error": {"code": "rate_limit_exceeded", "message": "Too many requests"}});
        let (status, msg) = extract_stream_error(&chunk, "responses").unwrap();
        assert_eq!(status, 429);
        assert_eq!(msg, "Too many requests");
    }

    #[test]
    fn test_extract_responses_normal_not_error() {
        let chunk = json!({"type": "response.created", "response": {"id": "resp_1"}});
        assert!(extract_stream_error(&chunk, "responses").is_none());
    }

    #[test]
    fn test_extract_error_empty_message_falls_back_to_type() {
        let chunk = json!({"type": "error", "error": {"type": "permission_error"}});
        let (status, msg) = extract_stream_error(&chunk, "messages").unwrap();
        assert_eq!(status, 403);
        assert_eq!(msg, "permission_error");
    }

    // ── extract_non_sse_error ──

    #[test]
    fn test_non_sse_json_error_body() {
        let body = r#"{"error": {"code": "insufficient_quota", "message": "Insufficient quota"}}"#;
        let (status, msg) = extract_non_sse_error(body, "chat").unwrap();
        assert_eq!(status, 402);
        assert_eq!(msg, "Insufficient quota");
    }

    #[test]
    fn test_non_sse_content_filter_not_error() {
        // 内容审核空流:无 error 键 → 不触发,避免误伤
        let body = r#"{"id":"x","choices":[{"index":0,"delta":{},"finish_reason":"content_filter"}]}"#;
        assert!(extract_non_sse_error(body, "chat").is_none());
    }

    #[test]
    fn test_non_sse_non_json_not_error() {
        assert!(extract_non_sse_error("<html>502 Bad Gateway</html>", "chat").is_none());
        assert!(extract_non_sse_error("", "chat").is_none());
        assert!(extract_non_sse_error("not json at all", "chat").is_none());
    }

    #[test]
    fn test_non_sse_no_error_key_not_error() {
        let body = r#"{"message":"some other error"}"#;
        assert!(extract_non_sse_error(body, "chat").is_none());
    }
}
