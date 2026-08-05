//! 流式转发分支：在 spawn 内调用，通过 tx 发送字节给客户端。
//!
//! 三种模式：
//! - `forward_stream_passthrough`: 同格式直通（SniffStream 透传 + 嗅探 usage）
//! - `forward_stream_openai_to_anthropic`: OpenAI 上游 → Anthropic 客户端翻译
//! - `forward_stream_anthropic_to_openai`: Anthropic 上游 → OpenAI 客户端翻译

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
use super::route::ResolvedRoute;
use super::{sniff, translate, UPSTREAM_CHUNK_TIMEOUT_SECS};

/// Passthrough（同格式直通）：SniffStream 透传上游字节 + 嗅探 usage。
pub(super) async fn forward_stream_passthrough(
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
) {
    let provider_kind = &resolved.provider_kind;
    let mut sniff = sniff::SniffStream::new(response.bytes_stream(), provider_kind);

    loop {
        let item = match tokio::time::timeout(
            Duration::from_secs(UPSTREAM_CHUNK_TIMEOUT_SECS),
            sniff.next(),
        )
        .await
        {
            Ok(Some(Ok(bytes))) => bytes,
            Ok(Some(Err(e))) => {
                warn!(trace_id = %trace_id, error = %e, "upstream stream error during passthrough");
                break;
            }
            Ok(None) => break,
            Err(_) => {
                warn!(trace_id = %trace_id, "upstream stream silent for {}s, closing", UPSTREAM_CHUNK_TIMEOUT_SECS);
                break;
            }
        };
        if tx.send(Ok(item)).await.is_err() {
            break;
        }
    }

    // 记录 usage
    let usage = sniff.into_usage();
    let output_tokens = if usage.output_tokens > 0 {
        usage.output_tokens as i64
    } else {
        (usage.output_chars / 4) as i64
    };
    let input_t = usage.input_tokens as i64;
    let cr = usage.cache_read_input_tokens as i64;
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

/// Translation: OpenAI 上游 → Anthropic 客户端。
pub(super) async fn forward_stream_openai_to_anthropic(
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
    est_input: u64,
) {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut chunk_count = 0u64;
    let mut stream_state = translate::StreamState::new();
    stream_state.input_tokens = est_input;
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
                warn!(trace_id = %trace_id, error = %e, "upstream stream error during O→A translation");
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
            let event = buffer[..newline_pos].to_string();
            buffer = buffer[newline_pos + 2..].to_string();

            if let Some(data) = event.strip_prefix("data: ") {
                if data == "[DONE]" {
                    saw_done = true;
                    break 'outer;
                }

                if let Ok(chunk_json) = serde_json::from_str::<Value>(data) {
                    let events: Vec<Value> = translate::translate_openai_chunk_to_anthropic(
                        &chunk_json,
                        &model_name.to_string(),
                        &mut stream_state,
                    );
                    for ev in events {
                        if ev != Value::Null {
                            chunk_count += 1;
                            let event_type = ev["type"].as_str().unwrap_or("message");
                            let json_str = serde_json::to_string(&ev).unwrap();
                            let sse_bytes = Bytes::from(format!("event: {}\ndata: {}\n\n", event_type, json_str));
                            let _ = tx.send(Ok(sse_bytes)).await;
                        }
                    }
                }
            }
        }
    }

    // 发送 finalize 事件（关闭 open blocks + message_delta + message_stop）
    for ev in translate::finalize_openai_to_anthropic(&mut stream_state) {
        let event_type = ev["type"].as_str().unwrap_or("message");
        let json_str = serde_json::to_string(&ev).unwrap();
        let sse_bytes = Bytes::from(format!("event: {}\ndata: {}\n\n", event_type, json_str));
        let _ = tx.send(Ok(sse_bytes)).await;
    }

    info!(
        trace_id = %trace_id,
        total_chunks = chunk_count,
        done = saw_done,
        "Stream ended (O→A)"
    );

    // 记录 usage
    let output_tokens = if stream_state.output_tokens > 0 {
        stream_state.output_tokens as i64
    } else {
        (stream_state.output_chars / 4) as i64
    };
    let input_t = stream_state.input_tokens as i64;
    let cr = stream_state.cache_read_input_tokens as i64;
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

/// Translation: Anthropic 上游 → OpenAI 客户端。
pub(super) async fn forward_stream_anthropic_to_openai(
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
) {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut chunk_count = 0u64;
    let mut accum_input: u64 = 0;
    let mut accum_output: u64 = 0;
    let mut accum_cache_read: u64 = 0;
    let mut accum_chars: u64 = 0;
    let mut oa_state = translate::OaStreamState::new();

    loop {
        let chunk = match tokio::time::timeout(
            Duration::from_secs(UPSTREAM_CHUNK_TIMEOUT_SECS),
            stream.next(),
        )
        .await
        {
            Ok(Some(Ok(c))) => c,
            Ok(Some(Err(e))) => {
                warn!(trace_id = %trace_id, error = %e, "upstream stream error during A→O translation");
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
            let event = buffer[..newline_pos].to_string();
            buffer = buffer[newline_pos + 2..].to_string();

            if let Some(data) = event.strip_prefix("data: ") {
                if data == "[DONE]" {
                    info!(trace_id = %trace_id, total_chunks = chunk_count, "Stream completed (A→O)");
                    let _ = tx.send(Ok(Bytes::from("data: [DONE]\n\n"))).await;
                    record_usage(
                        &state.database, provider_id, resolved, model_name,
                        last_key_id, last_key_name, last_key_masked,
                        service_key, endpoint, start_time,
                        accum_input, accum_output, accum_chars, accum_cache_read,
                    );
                    return;
                }

                if let Ok(chunk_json) = serde_json::from_str::<Value>(data) {
                    let (it, ot, cr, ch) = translate::extract_anthropic_usage(&chunk_json);
                    accum_input = accum_input.max(it);
                    if ot > 0 {
                        accum_output = ot;
                    }
                    accum_cache_read = accum_cache_read.max(cr);
                    accum_chars += ch;

                    let translated = translate::translate_anthropic_chunk_to_openai(&chunk_json, &mut oa_state);
                    if translated != Value::Null {
                        chunk_count += 1;
                        let json_str = serde_json::to_string(&translated).unwrap();
                        let sse_bytes = Bytes::from(format!("data: {}\n\n", json_str));
                        let _ = tx.send(Ok(sse_bytes)).await;
                    }
                }
            }
        }
    }

    // Stream ended without [DONE]
    info!(trace_id = %trace_id, total_chunks = chunk_count, "Stream ended (A→O, no [DONE])");
    let _ = tx.send(Ok(Bytes::from("data: [DONE]\n\n"))).await;

    record_usage(
        &state.database, provider_id, resolved, model_name,
        last_key_id, last_key_name, last_key_masked,
        service_key, endpoint, start_time,
        accum_input, accum_output, accum_chars, accum_cache_read,
    );
}

/// 记录 usage_log（A→O 翻译分支的 [DONE] 和 stream-end 两条路径共用）。
fn record_usage(
    db: &crate::db::Database,
    provider_id: &str,
    resolved: &ResolvedRoute,
    model_name: &str,
    last_key_id: &Option<String>,
    last_key_name: &Option<String>,
    last_key_masked: &Option<String>,
    service_key: &ServiceKeyInfo,
    endpoint: &'static str,
    start_time: Instant,
    accum_input: u64,
    accum_output: u64,
    accum_chars: u64,
    accum_cache_read: u64,
) {
    let output_tokens = if accum_output > 0 {
        accum_output as i64
    } else {
        (accum_chars / 4) as i64
    };
    let cr = accum_cache_read as i64;
    let _ = db.insert_usage_log(
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
        accum_input as i64,
        output_tokens,
        start_time.elapsed().as_millis() as i64,
        true,
        None,
        cr,
    );
}
