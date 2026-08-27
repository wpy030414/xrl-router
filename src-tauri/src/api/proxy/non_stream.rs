//! 非流式代理：客户端 `stream: false` 时的完整 JSON 响应路径。
//!
//! 上游仍走流式（复用 IR 解析 / 密钥轮换 / failover 基础设施），代理收集
//! 全部 IR 事件后组装成完整的非流式 JSON 返回。与 SSE 路径的关键差异：
//!
//! - 错误用真实 HTTP 状态码 + JSON 错误体表达。非流式下没有 SSE 通道，
//!   客户端能正确识别并处理错误——不会再出现「non-streaming request
//!   answered with a stream」这类协议错配。
//! - 无需后台 spawn / keepalive：整个上游调用在请求处理内 await 完成，
//!   客户端等待期间由自身的 HTTP 超时兜底。
//! - 请求级错误（400）与流内密钥级错误（401/402/403/429）的轮换语义
//!   与流式路径保持一致。

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::Response;
use futures::stream::StreamExt;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::gateway::server::AppState;

use super::forward::{extract_non_sse_error, extract_stream_error};
use super::ir;
use super::ir::types::{IrContentBlockStart, IrContentDelta, IrStopReason, IrStreamEvent, IrUsage};
use super::key_rotation::{pick_key_for, update_key_health};
use super::route::ResolvedRoute;
use super::stream::{
    extract_error_message, is_key_quota_error, resolve_candidates, ClientFormat, ErrorTuple,
    StreamContext,
};

/// 原始响应体捕获上限（与 forward.rs 一致）：用于「200 + 非 SSE 纯 JSON
/// 错误体」检测，超限放弃。
const RAW_CAPTURE_LIMIT: usize = 64 * 1024;

/// 非流式代理主入口：路由解析 → 双循环（provider × key）→ 收集上游流 →
/// 组装完整 JSON 响应。
pub(super) async fn proxy_non_stream(
    state: Arc<AppState>,
    ctx: StreamContext,
) -> Result<Response, ErrorTuple> {
    let trace_id = ctx.trace_id;
    let model_name = ctx.model_name;
    let start_time = ctx.start_time;
    let header_timeout_secs = ctx.header_timeout_secs;
    let client_format = ctx.client_format;
    let est_input = ctx.est_input;
    let endpoint = ctx.endpoint;
    let service_key = ctx.service_key;
    let ir_request = ctx.ir_request;

    info!(trace_id = %trace_id, model = %model_name, "Non-stream proxy request");

    // ── 1. 路由解析（与流式路径共享同一逻辑） ────────────────────────
    let (candidates, failover, is_combo_req) =
        resolve_candidates(&state, &model_name, &trace_id).await?;

    // 上下文超限预警（软警告，与流式路径一致，不阻断）
    let max_input = candidates.iter().map(|c| c.context_window).max().unwrap_or(0);
    if max_input > 0 && est_input as usize > max_input {
        warn!(
            trace_id = %trace_id,
            est_input,
            max_input,
            model = %model_name,
            "Estimated input tokens exceed context window (soft warning, forwarding to upstream)"
        );
    }

    let client = state.http_client.clone();

    // ── 2. 双循环：外层 provider 候选，内层 key 轮换 ──────────────────
    let mut last_candidate: ResolvedRoute = candidates[0].clone();
    let mut last_key_id: Option<String> = None;
    let mut last_key_name: Option<String> = None;
    let mut last_key_masked: Option<String> = None;
    // 最后一次失败的状态码与消息（耗尽所有候选后用于错误响应）
    let mut last_error: Option<(u16, String)> = None;

    'provider: for (ci, cand) in candidates.iter().enumerate() {
        if failover && super::failover::is_provider_cooling(&state, &cand.provider_id) {
            info!(trace_id = %trace_id, provider = %cand.provider_id, "provider cooling, skipping");
            continue;
        }
        last_candidate = cand.clone();

        let max_attempts = state
            .keys
            .get_stats(&cand.provider_id)
            .map(|s| s.total as u32)
            .unwrap_or(1);
        let mut attempts: u32 = 0;
        loop {
            attempts += 1;
            if attempts > max_attempts {
                if failover && ci + 1 < candidates.len() {
                    super::failover::mark_provider_failed(&state, &cand.provider_id);
                    continue 'provider;
                }
                break;
            }
            let picked = match pick_key_for(&state, &cand.provider_id) {
                Some(p) => p,
                None => {
                    if failover && ci + 1 < candidates.len() {
                        super::failover::mark_provider_failed(&state, &cand.provider_id);
                        continue 'provider;
                    }
                    break;
                }
            };
            last_key_id = Some(picked.id.clone());
            last_key_name = Some(picked.name.clone());
            last_key_masked = Some(picked.key_masked.clone());

            // 构造上游请求体（与流式路径一致：始终 stream=true）
            let mut attempt_body = match cand.provider_kind.as_str() {
                "messages" => ir::to_messages::ir_req_to_messages(&ir_request),
                "responses" => ir::to_responses::ir_req_to_responses(&ir_request),
                _ => ir::to_chat_completions::ir_req_to_chat_completions(&ir_request),
            };
            if let Some(obj) = attempt_body.as_object_mut() {
                obj.insert("model".to_string(), json!(cand.real_model_id));
                obj.insert("stream".to_string(), json!(true));
                // Chat Completions 需要 stream_options 才能拿到 usage
                if cand.provider_kind != "messages" && cand.provider_kind != "responses" {
                    obj.insert(
                        "stream_options".to_string(),
                        json!({"include_usage": true}),
                    );
                }
            }

            let mut req_builder = client.post(&cand.upstream_url);
            if cand.provider_kind == "messages" {
                req_builder = req_builder
                    .header("x-api-key", &picked.key_hash)
                    .header("anthropic-version", "2023-06-01");
            } else {
                req_builder = req_builder.header("Authorization", format!("Bearer {}", picked.key_hash));
            }

            info!(
                trace_id = %trace_id,
                upstream_url = %cand.upstream_url,
                "Calling upstream API (non-stream client, streaming upstream)"
            );

            let resp = match tokio::time::timeout(
                Duration::from_secs(header_timeout_secs),
                req_builder
                    .header("Content-Type", "application/json")
                    .json(&attempt_body)
                    .send(),
            )
            .await
            {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    warn!(trace_id = %trace_id, error = %e, "Upstream call failed");
                    if failover && ci + 1 < candidates.len() {
                        super::failover::mark_provider_failed(&state, &cand.provider_id);
                        last_error = Some((502, e.to_string()));
                        continue 'provider;
                    }
                    last_error = Some((502, e.to_string()));
                    break 'provider;
                }
                Err(_) => {
                    let msg = format!(
                        "upstream timed out after {}s waiting for response headers",
                        header_timeout_secs
                    );
                    warn!(trace_id = %trace_id, key_id = %picked.id, "{}", msg);
                    if failover && ci + 1 < candidates.len() {
                        super::failover::mark_provider_failed(&state, &cand.provider_id);
                        last_error = Some((504, msg));
                        continue 'provider;
                    }
                    last_error = Some((504, msg));
                    break 'provider;
                }
            };

            let status = resp.status().as_u16();

            // 400 + 配额耗尽：视为密钥级错误，轮换到下一把密钥重试。
            if status == 400 {
                let body_str = resp.text().await.unwrap_or_default();
                if is_key_quota_error(&body_str) {
                    warn!(
                        trace_id = %trace_id,
                        status,
                        key_id = %picked.id,
                        upstream_body = %body_str,
                        "upstream 400 with quota error, rotating key"
                    );
                    update_key_health(&state.keys, &cand.provider_id, &picked.key_hash, 402);
                    last_error = Some((402, extract_error_message(&body_str)));
                    continue;
                }
                // 普通 400：请求级错误，直接透传状态码与错误体。
                update_key_health(&state.keys, &cand.provider_id, &picked.key_hash, status);
                warn!(trace_id = %trace_id, status, upstream_body = %body_str, "upstream 400");
                let msg = extract_error_message(&body_str);
                let _ = state.database.insert_usage_log(
                    chrono::Utc::now().timestamp(),
                    &cand.provider_id,
                    cand.provider_name.as_str(),
                    &cand.model_row_id,
                    model_name.as_str(),
                    Some(&picked.id),
                    picked.name.as_str(),
                    picked.key_masked.as_str(),
                    Some(service_key.id.as_str()),
                    service_key.name.as_str(),
                    service_key.key_masked.as_str(),
                    endpoint,
                    0,
                    0,
                    start_time.elapsed().as_millis() as i64,
                    false,
                    Some(&format!(
                        "upstream status {}: {}",
                        status,
                        body_str.chars().take(200).collect::<String>()
                    )),
                    0,
                );
                return Ok(error_response(
                    client_format,
                    400,
                    "invalid_request_error",
                    &msg,
                ));
            }

            update_key_health(&state.keys, &cand.provider_id, &picked.key_hash, status);

            if matches!(status, 401 | 402 | 403 | 429) {
                warn!(trace_id = %trace_id, status, key_id = %picked.id, "upstream rejected key, rotating");
                let body_str = resp.text().await.unwrap_or_default();
                last_error = Some((status, extract_error_message(&body_str)));
                continue;
            }
            if matches!(status, 500..=599) {
                let body_str = resp.text().await.unwrap_or_default();
                warn!(trace_id = %trace_id, status, key_id = %picked.id, "upstream 5xx");
                last_error = Some((status, extract_error_message(&body_str)));
                if failover && ci + 1 < candidates.len() {
                    super::failover::mark_provider_failed(&state, &cand.provider_id);
                    continue 'provider;
                }
                break 'provider;
            }

            // 2xx：收集整个流并组装非流式响应。
            super::failover::mark_provider_ok(&state, &cand.provider_id);
            info!(
                trace_id = %trace_id,
                status = status,
                duration_ms = start_time.elapsed().as_millis(),
                "Upstream response received (non-stream client, collecting)"
            );

            match collect_upstream_stream(resp, &cand.provider_kind, est_input).await {
                Ok((acc, usage)) => {
                    let body = match client_format {
                        ClientFormat::Messages => {
                            build_messages_response(&acc, &usage, &model_name)
                        }
                        ClientFormat::ChatCompletions => {
                            build_chat_completions_response(&acc, &usage, &model_name)
                        }
                        ClientFormat::Responses => {
                            build_responses_response(&acc, &usage, &model_name)
                        }
                    };
                    // usage 日志（与 forward_stream_ir 成功路径一致）
                    let output_tokens = if usage.output_tokens > 0 {
                        usage.output_tokens as i64
                    } else {
                        (usage.output_chars / 4) as i64
                    };
                    let _ = state.database.insert_usage_log(
                        chrono::Utc::now().timestamp(),
                        &cand.provider_id,
                        cand.provider_name.as_str(),
                        &cand.model_row_id,
                        model_name.as_str(),
                        Some(&picked.id),
                        picked.name.as_str(),
                        picked.key_masked.as_str(),
                        Some(service_key.id.as_str()),
                        service_key.name.as_str(),
                        service_key.key_masked.as_str(),
                        endpoint,
                        usage.input_tokens as i64,
                        output_tokens,
                        start_time.elapsed().as_millis() as i64,
                        true,
                        None,
                        usage.cache_read_input_tokens as i64,
                    );
                    return Ok(json_response(body));
                }
                Err(err) => {
                    if err.retryable {
                        // 密钥级错误（401/402/403/429）或瞬态空流（503）：
                        // 换密钥重试。非流式下尚未向客户端发送任何字节，
                        // 重试天然安全（流式路径则需要 sent_any 判断）。
                        warn!(
                            trace_id = %trace_id,
                            status = err.status,
                            key_id = %picked.id,
                            upstream_message = %err.message,
                            "upstream stream error (non-stream client), rotating key"
                        );
                        update_key_health(
                            &state.keys,
                            &cand.provider_id,
                            &picked.key_hash,
                            err.status,
                        );
                        last_error = Some((err.status, err.message));
                        continue;
                    }
                    // 非密钥级流内错误：透传给客户端。
                    warn!(
                        trace_id = %trace_id,
                        status = err.status,
                        upstream_message = %err.message,
                        "upstream stream error (non-stream client), forwarding error"
                    );
                    let _ = state.database.insert_usage_log(
                        chrono::Utc::now().timestamp(),
                        &cand.provider_id,
                        cand.provider_name.as_str(),
                        &cand.model_row_id,
                        model_name.as_str(),
                        Some(&picked.id),
                        picked.name.as_str(),
                        picked.key_masked.as_str(),
                        Some(service_key.id.as_str()),
                        service_key.name.as_str(),
                        service_key.key_masked.as_str(),
                        endpoint,
                        0,
                        0,
                        start_time.elapsed().as_millis() as i64,
                        false,
                        Some(&format!(
                            "upstream stream error {}: {}",
                            err.status, err.message
                        )),
                        0,
                    );
                    return Ok(error_response(
                        client_format,
                        err.status,
                        error_type_for_status(err.status),
                        &err.message,
                    ));
                }
            }
        }
    }

    // ── 3. 全部候选耗尽：用最后一次失败信息构造错误响应 ──────────────
    let _ = is_combo_req; // 语义与流式路径对齐：普通 400 已在上面提前透传
    let (status, msg) =
        last_error.unwrap_or((503, "No available upstream keys".to_string()));
    let duration_ms = start_time.elapsed().as_millis() as i64;
    warn!(
        trace_id = %trace_id,
        status = status,
        duration_ms,
        "All upstream candidates exhausted (non-stream client)"
    );
    let _ = state.database.insert_usage_log(
        chrono::Utc::now().timestamp(),
        &last_candidate.provider_id,
        last_candidate.provider_name.as_str(),
        &last_candidate.model_row_id,
        model_name.as_str(),
        last_key_id.as_deref(),
        last_key_name.as_deref().unwrap_or(""),
        last_key_masked.as_deref().unwrap_or(""),
        Some(service_key.id.as_str()),
        service_key.name.as_str(),
        service_key.key_masked.as_str(),
        endpoint,
        0,
        0,
        duration_ms,
        false,
        Some(&msg),
        0,
    );
    Ok(error_response(
        client_format,
        status,
        error_type_for_status(status),
        &msg,
    ))
}

// ═══════════════════════════════════════════════════════════════════
// 上游流收集
// ═══════════════════════════════════════════════════════════════════

/// 流收集错误。
struct CollectError {
    status: u16,
    message: String,
    /// 密钥级错误（401/402/403/429）或瞬态空流（503）→ 可换密钥重试。
    retryable: bool,
}

/// 累积中的内容块（IR block index → 位置一一对应）。
#[derive(Debug, Clone, PartialEq)]
enum AccBlock {
    Text(String),
    Thinking {
        thinking: String,
        signature: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        input_json: String,
    },
}

/// IR 事件累积器：把流式事件流还原成完整的响应内容。
#[derive(Debug, Default)]
struct IrAccumulator {
    msg_id: String,
    model: String,
    /// 按 IR block index 顺序排列的完整内容块。
    blocks: Vec<AccBlock>,
    stop_reason: Option<IrStopReason>,
}

impl IrAccumulator {
    fn process_event(&mut self, ev: &IrStreamEvent) {
        match ev {
            IrStreamEvent::MessageStart { id, model, .. } => {
                self.msg_id = id.clone();
                self.model = model.clone();
            }
            IrStreamEvent::ContentBlockStart { index, block } => {
                while self.blocks.len() <= *index {
                    self.blocks.push(AccBlock::Text(String::new()));
                }
                self.blocks[*index] = match block {
                    IrContentBlockStart::Text => AccBlock::Text(String::new()),
                    IrContentBlockStart::Thinking { signature } => AccBlock::Thinking {
                        thinking: String::new(),
                        signature: signature.clone(),
                    },
                    IrContentBlockStart::ToolUse { id, name } => AccBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input_json: String::new(),
                    },
                };
            }
            IrStreamEvent::ContentBlockDelta { index, delta } => {
                if let Some(b) = self.blocks.get_mut(*index) {
                    match (b, delta) {
                        (AccBlock::Text(s), IrContentDelta::TextDelta(t)) => s.push_str(t),
                        (AccBlock::Thinking { thinking, .. }, IrContentDelta::ThinkingDelta(t)) => {
                            thinking.push_str(t)
                        }
                        (AccBlock::ToolUse { input_json, .. }, IrContentDelta::InputJsonDelta(t)) => {
                            input_json.push_str(t)
                        }
                        _ => {}
                    }
                }
            }
            IrStreamEvent::MessageDelta { stop_reason, .. } => {
                if let Some(sr) = stop_reason {
                    self.stop_reason = Some(*sr);
                }
            }
            _ => {}
        }
    }
}

/// 读取上游 SSE 流 → IR 事件 → 累积成完整内容 + usage。
///
/// 错误语义与 `forward_stream_ir` 对齐：
/// - 流内错误事件：密钥级（401/402/403/429）→ retryable；其余 → 透传。
///   与流式路径的差异：非流式下无论错误出现在流的哪个位置都尚未向客户端
///   发送字节，所以密钥级错误一律可重试（流式路径要求 `!sent_any`）。
/// - 空流（0 SSE 帧）：视为瞬态错误 → retryable（503）。
async fn collect_upstream_stream(
    response: reqwest::Response,
    provider_kind: &str,
    est_input: u64,
) -> Result<(IrAccumulator, IrUsage), CollectError> {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut chunk_count = 0u64;

    // 原始响应体捕获：用于「200 + 非 SSE 纯 JSON 错误体」检测。
    let mut raw_capture: Option<String> = Some(String::new());

    // 上游解析器状态（按 provider_kind 选择）
    let mut anthropic_parse = ir::from_messages::MessagesParseState::new();
    let mut chat_parse = ir::from_chat_completions::ChatCompletionsParseState::new();
    let mut responses_parse = ir::from_responses::ResponsesParseState::new();
    // 预填充估算的 input tokens（与 forward.rs 一致）
    anthropic_parse.usage.input_tokens = est_input;
    chat_parse.usage.input_tokens = est_input;
    responses_parse.usage.input_tokens = est_input;

    let mut acc = IrAccumulator::default();

    'outer: loop {
        let chunk = match tokio::time::timeout(
            Duration::from_secs(super::UPSTREAM_CHUNK_TIMEOUT_SECS),
            stream.next(),
        )
        .await
        {
            Ok(Some(Ok(c))) => c,
            Ok(Some(Err(e))) => {
                warn!("upstream stream error during non-stream collection: {}", e);
                break;
            }
            Ok(None) => break,
            Err(_) => {
                warn!(
                    "upstream stream silent for {}s during non-stream collection",
                    super::UPSTREAM_CHUNK_TIMEOUT_SECS
                );
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

            // 提取 data: 行（与 forward.rs 相同的提取逻辑）
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
                chunk_count += 1;

                // 流内错误事件检测
                if let Some((status, msg)) = extract_stream_error(&chunk_json, provider_kind) {
                    let retryable = matches!(status, 401 | 402 | 403 | 429);
                    return Err(CollectError { status, message: msg, retryable });
                }

                let ir_events = match provider_kind {
                    "messages" => {
                        ir::from_messages::messages_chunk_to_ir(&chunk_json, &mut anthropic_parse)
                    }
                    "responses" => {
                        ir::from_responses::responses_chunk_to_ir(&chunk_json, &mut responses_parse)
                    }
                    _ => ir::from_chat_completions::chat_completions_chunk_to_ir(
                        &chunk_json,
                        &mut chat_parse,
                    ),
                };
                for ev in &ir_events {
                    acc.process_event(ev);
                }
            }
        }
    }

    // 流结束且从未出现任何 SSE 帧 → 检查原始体是否为非 SSE 纯 JSON 错误体。
    if chunk_count == 0 {
        if let Some(ref body) = raw_capture {
            if let Some((status, msg)) = extract_non_sse_error(body, provider_kind) {
                let retryable = matches!(status, 401 | 402 | 403 | 429);
                return Err(CollectError { status, message: msg, retryable });
            }
        }
        // 兜底：HTTP 200 + 完全空的流 → 视为上游瞬态错误，可重试。
        return Err(CollectError {
            status: 503,
            message: format!(
                "upstream returned empty stream (0 SSE events, body {}B)",
                raw_capture.as_ref().map(|b| b.len()).unwrap_or(0)
            ),
            retryable: true,
        });
    }

    let usage = match provider_kind {
        "messages" => anthropic_parse.usage.clone(),
        "responses" => responses_parse.usage.clone(),
        _ => chat_parse.usage.clone(),
    };

    Ok((acc, usage))
}

// ═══════════════════════════════════════════════════════════════════
// 非流式响应组装
// ═══════════════════════════════════════════════════════════════════

/// 空文本占位：msg_id 缺失时生成（上游异常流的兜底）。
fn or_msg_id(id: &str) -> String {
    if id.is_empty() {
        format!("msg_{}", uuid::Uuid::new_v4().simple())
    } else {
        id.to_string()
    }
}

/// 组装 Anthropic Messages 非流式响应。
fn build_messages_response(acc: &IrAccumulator, usage: &IrUsage, model_alias: &str) -> Value {
    let content: Vec<Value> = acc
        .blocks
        .iter()
        .filter_map(|b| match b {
            AccBlock::Text(text) if !text.is_empty() => {
                Some(json!({"type": "text", "text": text}))
            }
            AccBlock::Thinking { thinking, signature } if !thinking.is_empty() => {
                let mut obj = json!({"type": "thinking", "thinking": thinking});
                if let Some(sig) = signature {
                    obj["signature"] = json!(sig);
                }
                Some(obj)
            }
            AccBlock::ToolUse { id, name, input_json } => {
                let input: Value = if input_json.is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(input_json).unwrap_or_else(|_| json!({}))
                };
                Some(json!({"type": "tool_use", "id": id, "name": name, "input": input}))
            }
            _ => None,
        })
        .collect();

    let stop_reason = acc
        .stop_reason
        .map(|sr| sr.as_anthropic_str())
        .unwrap_or("end_turn");
    let output_tokens = if usage.output_tokens > 0 {
        usage.output_tokens
    } else {
        usage.output_chars / 4
    };

    json!({
        "id": or_msg_id(&acc.msg_id),
        "type": "message",
        "role": "assistant",
        "model": if acc.model.is_empty() { model_alias } else { &acc.model },
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": Value::Null,
        "usage": {
            // 客户端口径：input_tokens 含 cache_creation（与 message_start 渲染一致）
            "input_tokens": usage.input_tokens + usage.cache_creation_input_tokens,
            "output_tokens": output_tokens,
            "cache_creation_input_tokens": usage.cache_creation_input_tokens,
            "cache_read_input_tokens": usage.cache_read_input_tokens,
        }
    })
}

/// 组装 OpenAI Chat Completions 非流式响应。
fn build_chat_completions_response(
    acc: &IrAccumulator,
    usage: &IrUsage,
    model_alias: &str,
) -> Value {
    let mut content_text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();

    for b in &acc.blocks {
        match b {
            AccBlock::Text(t) => content_text.push_str(t),
            AccBlock::Thinking { thinking, .. } => reasoning.push_str(thinking),
            AccBlock::ToolUse { id, name, input_json } => {
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": if input_json.is_empty() {
                            "{}".to_string()
                        } else {
                            input_json.clone()
                        }
                    }
                }));
            }
        }
    }

    let finish_reason = acc
        .stop_reason
        .map(|sr| sr.as_chat_finish_reason())
        .unwrap_or("stop");
    let output_tokens = if usage.output_tokens > 0 {
        usage.output_tokens
    } else {
        usage.output_chars / 4
    };

    let mut message = json!({
        "role": "assistant",
        // OpenAI 规范：纯 tool_calls 响应 content 为 null；纯文本为空时为 ""
        "content": if content_text.is_empty() && !tool_calls.is_empty() {
            Value::Null
        } else {
            json!(content_text)
        },
    });
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(reasoning);
    }
    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(tool_calls);
    }

    let id = if acc.msg_id.is_empty() {
        format!("chatcmpl-{}", uuid::Uuid::new_v4().simple())
    } else {
        acc.msg_id.clone()
    };

    json!({
        "id": id,
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": if acc.model.is_empty() { model_alias } else { &acc.model },
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
            "logprobs": Value::Null,
        }],
        "usage": {
            "prompt_tokens": usage.input_tokens + usage.cache_read_input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": usage.input_tokens + usage.cache_read_input_tokens + output_tokens,
            "prompt_tokens_details": {
                "cached_tokens": usage.cache_read_input_tokens
            }
        }
    })
}

/// 组装 OpenAI Responses 非流式响应。
fn build_responses_response(acc: &IrAccumulator, usage: &IrUsage, model_alias: &str) -> Value {
    let mut output: Vec<Value> = Vec::new();

    for b in &acc.blocks {
        match b {
            AccBlock::Text(t) if !t.is_empty() => {
                output.push(json!({
                    "type": "message",
                    "status": "completed",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": t, "annotations": []}]
                }));
            }
            AccBlock::Thinking { thinking, .. } if !thinking.is_empty() => {
                output.push(json!({
                    "type": "reasoning",
                    "status": "completed",
                    "content": [{"type": "reasoning_text", "text": thinking, "summary": []}],
                    "summary": []
                }));
            }
            AccBlock::ToolUse { id, name, input_json } => {
                output.push(json!({
                    "type": "function_call",
                    "status": "completed",
                    "call_id": id,
                    "name": name,
                    "arguments": if input_json.is_empty() {
                        "{}".to_string()
                    } else {
                        input_json.clone()
                    }
                }));
            }
            _ => {}
        }
    }

    // 截断（max_tokens）时按 Responses 规范标 incomplete
    let truncated = acc.stop_reason == Some(IrStopReason::MaxTokens);
    let status = if truncated { "incomplete" } else { "completed" };
    let output_tokens = if usage.output_tokens > 0 {
        usage.output_tokens
    } else {
        usage.output_chars / 4
    };

    let id = if acc.msg_id.is_empty() {
        format!("resp_{}", uuid::Uuid::new_v4().simple())
    } else {
        acc.msg_id.clone()
    };

    let mut resp = json!({
        "id": id,
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
        "model": if acc.model.is_empty() { model_alias } else { &acc.model },
        "status": status,
        "output": output,
        "usage": {
            "input_tokens": usage.input_tokens + usage.cache_read_input_tokens,
            "output_tokens": output_tokens,
            "total_tokens": usage.input_tokens + usage.cache_read_input_tokens + output_tokens,
            "input_tokens_details": {
                "cached_tokens": usage.cache_read_input_tokens
            }
        }
    });
    if truncated {
        resp["incomplete_details"] = json!({"reason": "max_output_tokens"});
    }
    resp
}

// ═══════════════════════════════════════════════════════════════════
// HTTP 响应构造
// ═══════════════════════════════════════════════════════════════════

/// 成功响应：200 + application/json。
fn json_response(body: Value) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap()
}

/// 错误响应：真实 HTTP 状态码 + 按客户端格式的 JSON 错误体。
fn error_response(
    client_format: ClientFormat,
    status: u16,
    error_type: &str,
    message: &str,
) -> Response {
    let body = match client_format {
        ClientFormat::Messages => json!({
            "type": "error",
            "error": {"type": error_type, "message": message}
        }),
        ClientFormat::ChatCompletions => json!({
            "error": {"message": message, "type": error_type, "code": null}
        }),
        ClientFormat::Responses => json!({
            "error": {"code": error_type, "message": message}
        }),
    };
    Response::builder()
        .status(
            StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
        )
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap()
}

/// 状态码 → Anthropic 错误类型字符串（错误体里的 type 字段）。
fn error_type_for_status(status: u16) -> &'static str {
    match status {
        400 => "invalid_request_error",
        401 | 403 => "authentication_error",
        404 => "not_found_error",
        408 | 504 => "timeout_error",
        413 => "request_too_large",
        429 => "rate_limit_error",
        500..=599 => "api_error",
        _ => "api_error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_events() -> Vec<IrStreamEvent> {
        vec![
            IrStreamEvent::MessageStart {
                id: "msg_1".to_string(),
                model: "claude-x".to_string(),
                usage: Some(IrUsage::default()),
            },
            IrStreamEvent::ContentBlockStart {
                index: 0,
                block: IrContentBlockStart::Text,
            },
            IrStreamEvent::ContentBlockDelta {
                index: 0,
                delta: IrContentDelta::TextDelta("Hello".to_string()),
            },
            IrStreamEvent::ContentBlockDelta {
                index: 0,
                delta: IrContentDelta::TextDelta(" world".to_string()),
            },
            IrStreamEvent::ContentBlockStop { index: 0 },
            IrStreamEvent::MessageDelta {
                stop_reason: Some(IrStopReason::EndTurn),
                usage: None,
            },
            IrStreamEvent::MessageStop,
        ]
    }

    fn tool_events() -> Vec<IrStreamEvent> {
        vec![
            IrStreamEvent::MessageStart {
                id: "msg_2".to_string(),
                model: "claude-x".to_string(),
                usage: Some(IrUsage::default()),
            },
            IrStreamEvent::ContentBlockStart {
                index: 0,
                block: IrContentBlockStart::Thinking {
                    signature: Some("sig_1".to_string()),
                },
            },
            IrStreamEvent::ContentBlockDelta {
                index: 0,
                delta: IrContentDelta::ThinkingDelta("planning".to_string()),
            },
            IrStreamEvent::ContentBlockStop { index: 0 },
            IrStreamEvent::ContentBlockStart {
                index: 1,
                block: IrContentBlockStart::ToolUse {
                    id: "toolu_1".to_string(),
                    name: "get_weather".to_string(),
                },
            },
            IrStreamEvent::ContentBlockDelta {
                index: 1,
                delta: IrContentDelta::InputJsonDelta("{\"city\":".to_string()),
            },
            IrStreamEvent::ContentBlockDelta {
                index: 1,
                delta: IrContentDelta::InputJsonDelta("\"Tokyo\"}".to_string()),
            },
            IrStreamEvent::ContentBlockStop { index: 1 },
            IrStreamEvent::MessageDelta {
                stop_reason: Some(IrStopReason::ToolUse),
                usage: None,
            },
            IrStreamEvent::MessageStop,
        ]
    }

    fn accumulate(events: &[IrStreamEvent]) -> IrAccumulator {
        let mut acc = IrAccumulator::default();
        for ev in events {
            acc.process_event(ev);
        }
        acc
    }

    #[test]
    fn test_accumulator_text() {
        let acc = accumulate(&text_events());
        assert_eq!(acc.msg_id, "msg_1");
        assert_eq!(acc.model, "claude-x");
        assert_eq!(acc.stop_reason, Some(IrStopReason::EndTurn));
        assert_eq!(acc.blocks.len(), 1);
        assert_eq!(acc.blocks[0], AccBlock::Text("Hello world".to_string()));
    }

    #[test]
    fn test_accumulator_thinking_and_tool() {
        let acc = accumulate(&tool_events());
        assert_eq!(acc.blocks.len(), 2);
        assert_eq!(
            acc.blocks[0],
            AccBlock::Thinking {
                thinking: "planning".to_string(),
                signature: Some("sig_1".to_string()),
            }
        );
        assert_eq!(
            acc.blocks[1],
            AccBlock::ToolUse {
                id: "toolu_1".to_string(),
                name: "get_weather".to_string(),
                input_json: "{\"city\":\"Tokyo\"}".to_string(),
            }
        );
        assert_eq!(acc.stop_reason, Some(IrStopReason::ToolUse));
    }

    #[test]
    fn test_build_messages_response_text() {
        let acc = accumulate(&text_events());
        let usage = IrUsage {
            input_tokens: 100,
            output_tokens: 20,
            cache_read_input_tokens: 50,
            ..Default::default()
        };
        let v = build_messages_response(&acc, &usage, "alias");
        assert_eq!(v["id"], "msg_1");
        assert_eq!(v["type"], "message");
        assert_eq!(v["role"], "assistant");
        assert_eq!(v["model"], "claude-x");
        assert_eq!(v["content"][0]["type"], "text");
        assert_eq!(v["content"][0]["text"], "Hello world");
        assert_eq!(v["stop_reason"], "end_turn");
        assert_eq!(v["usage"]["input_tokens"], 100);
        assert_eq!(v["usage"]["output_tokens"], 20);
        assert_eq!(v["usage"]["cache_read_input_tokens"], 50);
    }

    #[test]
    fn test_build_messages_response_tool_use() {
        let acc = accumulate(&tool_events());
        let usage = IrUsage::default();
        let v = build_messages_response(&acc, &usage, "alias");
        // thinking 块保留 + tool_use input 解析为对象
        assert_eq!(v["content"][0]["type"], "thinking");
        assert_eq!(v["content"][0]["signature"], "sig_1");
        assert_eq!(v["content"][1]["type"], "tool_use");
        assert_eq!(v["content"][1]["input"]["city"], "Tokyo");
        assert_eq!(v["stop_reason"], "tool_use");
    }

    #[test]
    fn test_build_chat_response_tool_only_content_null() {
        let acc = accumulate(&tool_events());
        let usage = IrUsage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        };
        let v = build_chat_completions_response(&acc, &usage, "alias");
        assert_eq!(v["object"], "chat.completion");
        assert_eq!(v["choices"][0]["message"]["content"], Value::Null);
        assert_eq!(v["choices"][0]["message"]["tool_calls"][0]["function"]["name"], "get_weather");
        assert_eq!(
            v["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]
                .as_str()
                .unwrap(),
            "{\"city\":\"Tokyo\"}"
        );
        assert_eq!(v["choices"][0]["finish_reason"], "tool_calls");
        // thinking → reasoning_content
        assert_eq!(v["choices"][0]["message"]["reasoning_content"], "planning");
        assert_eq!(v["usage"]["prompt_tokens"], 10);
    }

    #[test]
    fn test_build_chat_response_text() {
        let acc = accumulate(&text_events());
        let usage = IrUsage::default();
        let v = build_chat_completions_response(&acc, &usage, "alias");
        assert_eq!(v["choices"][0]["message"]["content"], "Hello world");
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
        // 无 thinking → 无 reasoning_content
        assert!(v["choices"][0]["message"].get("reasoning_content").is_none());
    }

    #[test]
    fn test_build_responses_response() {
        let acc = accumulate(&tool_events());
        let usage = IrUsage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        };
        let v = build_responses_response(&acc, &usage, "alias");
        assert_eq!(v["object"], "response");
        assert_eq!(v["status"], "completed");
        assert_eq!(v["output"][0]["type"], "reasoning");
        assert_eq!(v["output"][1]["type"], "function_call");
        assert_eq!(v["output"][1]["call_id"], "toolu_1");
        assert_eq!(v["output"][1]["arguments"], "{\"city\":\"Tokyo\"}");
    }

    #[test]
    fn test_build_responses_response_truncated() {
        let mut events = text_events();
        // 把 stop_reason 换成 max_tokens
        if let Some(IrStreamEvent::MessageDelta { stop_reason, .. }) = events.iter_mut().rev().nth(1)
        {
            *stop_reason = Some(IrStopReason::MaxTokens);
        }
        let acc = accumulate(&events);
        let usage = IrUsage::default();
        let v = build_responses_response(&acc, &usage, "alias");
        assert_eq!(v["status"], "incomplete");
        assert_eq!(v["incomplete_details"]["reason"], "max_output_tokens");
    }

    #[test]
    fn test_empty_text_block_filtered_in_messages() {
        let events = vec![IrStreamEvent::MessageStart {
            id: "msg_e".to_string(),
            model: "m".to_string(),
            usage: None,
        }];
        let acc = accumulate(&events);
        let v = build_messages_response(&acc, &IrUsage::default(), "alias");
        assert_eq!(v["content"].as_array().unwrap().len(), 0);
        assert_eq!(v["model"], "m");
    }

    #[test]
    fn test_fallback_model_and_id() {
        // 上游异常流（无 MessageStart）：id/model 用别名兜底
        let acc = IrAccumulator::default();
        let v = build_messages_response(&acc, &IrUsage::default(), "my-alias");
        assert!(v["id"].as_str().unwrap().starts_with("msg_"));
        assert_eq!(v["model"], "my-alias");
    }

    #[test]
    fn test_error_type_for_status() {
        assert_eq!(error_type_for_status(400), "invalid_request_error");
        assert_eq!(error_type_for_status(401), "authentication_error");
        assert_eq!(error_type_for_status(429), "rate_limit_error");
        assert_eq!(error_type_for_status(500), "api_error");
        assert_eq!(error_type_for_status(504), "timeout_error");
    }
}
