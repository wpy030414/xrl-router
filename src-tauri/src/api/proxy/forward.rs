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
use super::stream::ClientFormat;
use super::UPSTREAM_CHUNK_TIMEOUT_SECS;

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
) {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut chunk_count = 0u64;

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

        while let Some(newline_pos) = buffer.find("\n\n") {
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

                // 1. 解析上游 chunk → IR 事件
                let ir_events = match provider_kind {
                    "messages" => {
                        ir::from_messages::messages_chunk_to_ir(&chunk_json, &mut anthropic_parse)
                    }
                    "responses" => {
                        ir::from_responses::responses_chunk_to_ir(&chunk_json, &mut responses_parse)
                    }
                    _ => {
                        // "chat_completions" / "deap" / "custom" 都当 Chat Completions 处理
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
                            return;
                        }
                    }
                }
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
}
