//! 流式代理引擎：上游连接 + 双循环重试 + 4 种流式分支。
//!
//! `proxy_stream()` 是两个 handler（Anthropic / OpenAI）共享的核心逻辑。
//! handler 负责认证 + 请求体准备，本模块负责路由解析、上游连接、密钥轮换、
//! 故障转移、流式转发。
//!
//! SSE 修复：
//! - Passthrough 分支：立即返回 Response + `:keepalive` 初始字节 + 后台心跳
//! - Translation 分支：初始 keepalive 事件 + axum KeepAlive
//! - 全部 passthrough 分支补全 Cache-Control / Connection / X-Accel-Buffering 头

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::stream::StreamExt;
use serde_json::{json, Value};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info, warn};

use crate::gateway::server::AppState;

use super::auth::ServiceKeyInfo;
use super::key_rotation::{pick_key_for, update_key_health};
use super::route::{resolve_route, ResolvedRoute};
use super::upstream::forward_upstream_error;
use super::websearch::{has_websearch_tool, run_websearch_loop};
use super::{sniff, translate, UPSTREAM_CHUNK_TIMEOUT_SECS, UPSTREAM_HEADER_TIMEOUT_SECS};

/// HTTP error tuple returned by proxy handlers.
pub type ErrorTuple = (StatusCode, HeaderMap, Json<Value>);

/// 客户端期望的响应格式。
pub enum ClientFormat {
    Anthropic,
    Openai,
}

/// 已认证的流式请求上下文（handler 构建后传入 `proxy_stream`）。
pub struct StreamContext {
    pub(super) trace_id: String,
    pub(super) start_time: Instant,
    pub(super) service_key: ServiceKeyInfo,
    pub(super) model_name: String,
    pub(super) endpoint: &'static str,
    /// Anthropic 格式请求体（stream=true 已设置）。
    pub(super) body_anthropic: Value,
    /// OpenAI 格式请求体（stream=true + stream_options 已设置）。
    pub(super) body_openai: Value,
    pub(super) client_format: ClientFormat,
    /// 输入 token 估算（translation 路径 message_start 占位用）。
    pub(super) est_input: u64,
}

/// provider 级失败（5xx/网络错误/响应头超时）：failover 切换后若无任何
/// provider 成功，按它转发/报错。
enum ProviderFailure {
    Network {
        cand: ResolvedRoute,
        key_id: String,
        key_name: String,
        key_masked: String,
        msg: String,
    },
    HeaderTimeout {
        cand: ResolvedRoute,
        key_id: String,
        key_name: String,
        key_masked: String,
        secs: u64,
    },
    Upstream5xx {
        cand: ResolvedRoute,
        key_id: String,
        key_name: String,
        key_masked: String,
        status: u16,
    },
}

/// SSE keepalive 心跳间隔（秒）。上游等待期间（~8s）保持连接存活，
/// 防止客户端因无数据传输而超时断开。
const SSE_KEEPALIVE_SECS: u64 = 15;

/// 流式代理引擎：路由解析 → 上游连接（双循环）→ 流式转发。
///
/// handler 完成认证/配额/请求体准备后调用此函数。
pub async fn proxy_stream(
    state: Arc<AppState>,
    ctx: StreamContext,
) -> Result<Response, ErrorTuple> {
    let trace_id = &ctx.trace_id;
    let start_time = ctx.start_time;
    let service_key = &ctx.service_key;
    let model_name = &ctx.model_name;
    let endpoint = ctx.endpoint;

    // ── 1. 路由解析 ───────────────────────────────────────────────
    let failover = state.failover_enabled.load(std::sync::atomic::Ordering::Relaxed);
    let candidates: Vec<ResolvedRoute> = {
        let cands = if failover {
            super::route::resolve_route_candidates(&state, model_name).await
        } else {
            resolve_route(&state, model_name).await.map(|r| vec![r])
        };
        match cands {
            Some(c) if !c.is_empty() => {
                info!(
                    trace_id = %trace_id,
                    candidates = c.len(),
                    provider_kind = %c[0].provider_kind,
                    real_model = %c[0].real_model_id,
                    "Route resolved"
                );
                c
            }
            _ => {
                warn!(trace_id = %trace_id, model = %model_name, "Model not found or not available");
                return Err((
                    StatusCode::BAD_REQUEST,
                    HeaderMap::new(),
                    Json(json!({"error": {"type": "invalid_request_error", "message": "Model not found or not available"}})),
                ));
            }
        }
    };

    // ── 2. WebSearch 劫持 ────────────────────────────────────────
    let resolved = candidates[0].clone();
    let provider_is_anthropic = resolved.provider_kind == "anthropic";

    // websearch 需要客户端原始格式的请求体
    let client_body = match ctx.client_format {
        ClientFormat::Anthropic => &ctx.body_anthropic,
        ClientFormat::Openai => &ctx.body_openai,
    };

    if state.websearch_hijack.load(std::sync::atomic::Ordering::Relaxed)
        && has_websearch_tool(client_body)
    {
        info!(trace_id = %trace_id, anthropic_upstream = provider_is_anthropic, "web_search hijacked → local Bing loop");
        return run_websearch_loop(&state, client_body, &resolved, provider_is_anthropic, trace_id, service_key).await;
    }

    // ── 3. 预构造两种格式的请求体（故障转移可能混合 OpenAI/Anthropic） ──
    let mut body_anthropic = ctx.body_anthropic.clone();
    let mut body_openai = ctx.body_openai.clone();

    let client = state.http_client.clone();

    info!(
        trace_id = %trace_id,
        upstream_url = %candidates[0].upstream_url,
        "Calling upstream API (streaming)"
    );

    // ── 4. 双循环：外层 provider 候选，内层 key 轮换 ─────────────
    let mut last_resp: Option<reqwest::Response> = None;
    let mut last_key_id: Option<String> = None;
    let mut last_key_name: Option<String> = None;
    let mut last_key_masked: Option<String> = None;
    let mut last_candidate: ResolvedRoute = candidates[0].clone();
    let mut provider_failure: Option<ProviderFailure> = None;
    let mut failover_resp: Option<reqwest::Response> = None;
    let mut response: Option<reqwest::Response> = None;
    let mut winner: Option<(ResolvedRoute, super::route::PickedKey)> = None;

    'provider: for (ci, cand) in candidates.iter().enumerate() {
        if failover && super::failover::is_provider_cooling(&state, &cand.provider_id) {
            info!(trace_id = %trace_id, provider = %cand.provider_id, "provider cooling, skipping");
            continue;
        }
        last_candidate = cand.clone();
        let attempt_body = if cand.provider_kind == "anthropic" {
            &mut body_anthropic
        } else {
            &mut body_openai
        };
        if let Some(obj) = attempt_body.as_object_mut() {
            obj.insert("model".to_string(), json!(cand.real_model_id));
        }
        let max_attempts = state.keys.get_stats(&cand.provider_id).map(|s| s.total as u32).unwrap_or(1);
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
            let key_name = picked.name.clone();
            let key_masked = picked.key_masked.clone();

            let mut req_builder = client.post(&cand.upstream_url);
            if cand.provider_kind == "anthropic" {
                req_builder = req_builder
                    .header("x-api-key", &picked.key_hash)
                    .header("anthropic-version", "2023-06-01");
            } else {
                req_builder = req_builder.header("Authorization", format!("Bearer {}", picked.key_hash));
            }

            let resp = match tokio::time::timeout(
                Duration::from_secs(UPSTREAM_HEADER_TIMEOUT_SECS),
                req_builder
                    .header("Content-Type", "application/json")
                    .json(&attempt_body)
                    .send(),
            )
            .await
            {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    if failover && ci + 1 < candidates.len() {
                        provider_failure = Some(ProviderFailure::Network {
                            cand: cand.clone(),
                            key_id: picked.id.clone(),
                            key_name: picked.name.clone(),
                            key_masked: picked.key_masked.clone(),
                            msg: e.to_string(),
                        });
                        super::failover::mark_provider_failed(&state, &cand.provider_id);
                        continue 'provider;
                    }
                    let duration_ms = start_time.elapsed().as_millis() as i64;
                    error!(trace_id = %trace_id, duration_ms, error = %e, "Upstream call failed");
                    let _ = state.database.insert_usage_log(
                        chrono::Utc::now().timestamp(),
                        &cand.provider_id, cand.provider_name.as_str(), &cand.model_row_id, model_name.as_str(),
                        Some(&picked.id), key_name.as_str(), key_masked.as_str(),
                        Some(service_key.id.as_str()), service_key.name.as_str(), service_key.key_masked.as_str(),
                        endpoint,
                        0, 0, duration_ms, false, Some(&e.to_string()), 0,
                    );
                    return Err((
                        StatusCode::BAD_GATEWAY,
                        HeaderMap::new(),
                        Json(json!({"error": {"type": "api_error", "message": e.to_string()}})),
                    ));
                }
                Err(_) => {
                    if failover && ci + 1 < candidates.len() {
                        provider_failure = Some(ProviderFailure::HeaderTimeout {
                            cand: cand.clone(),
                            key_id: picked.id.clone(),
                            key_name: picked.name.clone(),
                            key_masked: picked.key_masked.clone(),
                            secs: UPSTREAM_HEADER_TIMEOUT_SECS,
                        });
                        super::failover::mark_provider_failed(&state, &cand.provider_id);
                        continue 'provider;
                    }
                    let duration_ms = start_time.elapsed().as_millis() as i64;
                    let msg = format!(
                        "upstream timed out after {}s waiting for response headers",
                        UPSTREAM_HEADER_TIMEOUT_SECS
                    );
                    warn!(trace_id = %trace_id, duration_ms, key_id = %picked.id, "{}", msg);
                    let _ = state.database.insert_usage_log(
                        chrono::Utc::now().timestamp(),
                        &cand.provider_id, cand.provider_name.as_str(), &cand.model_row_id, model_name.as_str(),
                        Some(&picked.id), key_name.as_str(), key_masked.as_str(),
                        Some(service_key.id.as_str()), service_key.name.as_str(), service_key.key_masked.as_str(),
                        endpoint,
                        0, 0, duration_ms, false, Some(&msg), 0,
                    );
                    return Err((
                        StatusCode::GATEWAY_TIMEOUT,
                        HeaderMap::new(),
                        Json(json!({"error": {"type": "api_error", "message": msg}})),
                    ));
                }
            };

            let status = resp.status().as_u16();
            update_key_health(&state.keys, &cand.provider_id, &picked.key_hash, status);

            if matches!(status, 401 | 402 | 403 | 429) {
                warn!(trace_id = %trace_id, status, key_id = %picked.id, "upstream rejected key, rotating");
                last_resp = Some(resp);
                continue;
            }
            if matches!(status, 500..=599) {
                if failover && ci + 1 < candidates.len() {
                    provider_failure = Some(ProviderFailure::Upstream5xx {
                        cand: cand.clone(),
                        key_id: picked.id.clone(),
                        key_name: picked.name.clone(),
                        key_masked: picked.key_masked.clone(),
                        status,
                    });
                    failover_resp = Some(resp);
                    super::failover::mark_provider_failed(&state, &cand.provider_id);
                    continue 'provider;
                }
                last_resp = Some(resp);
                break;
            }
            // 2xx：选中
            super::failover::mark_provider_ok(&state, &cand.provider_id);
            winner = Some((cand.clone(), picked));
            response = Some(resp);
            break;
        }
    }

    // ── 5. 错误处理 ─────────────────────────────────────────────
    let (resolved, _winner_key) = match winner {
        Some((r, k)) => (r, k),
        None => {
            match provider_failure {
                Some(ProviderFailure::Network { cand, key_id, key_name, key_masked, msg }) => {
                    let duration_ms = start_time.elapsed().as_millis() as i64;
                    error!(trace_id = %trace_id, duration_ms, error = %msg, "Upstream call failed");
                    let _ = state.database.insert_usage_log(
                        chrono::Utc::now().timestamp(),
                        &cand.provider_id, cand.provider_name.as_str(), &cand.model_row_id, model_name.as_str(),
                        Some(&key_id), key_name.as_str(), key_masked.as_str(),
                        Some(service_key.id.as_str()), service_key.name.as_str(), service_key.key_masked.as_str(),
                        endpoint,
                        0, 0, duration_ms, false, Some(&msg), 0,
                    );
                    return Err((
                        StatusCode::BAD_GATEWAY,
                        HeaderMap::new(),
                        Json(json!({"error": {"type": "api_error", "message": msg}})),
                    ));
                }
                Some(ProviderFailure::HeaderTimeout { cand, key_id, key_name, key_masked, secs }) => {
                    let duration_ms = start_time.elapsed().as_millis() as i64;
                    let msg = format!("upstream timed out after {}s waiting for response headers", secs);
                    warn!(trace_id = %trace_id, duration_ms, key_id = %key_id, "{}", msg);
                    let _ = state.database.insert_usage_log(
                        chrono::Utc::now().timestamp(),
                        &cand.provider_id, cand.provider_name.as_str(), &cand.model_row_id, model_name.as_str(),
                        Some(&key_id), key_name.as_str(), key_masked.as_str(),
                        Some(service_key.id.as_str()), service_key.name.as_str(), service_key.key_masked.as_str(),
                        endpoint,
                        0, 0, duration_ms, false, Some(&msg), 0,
                    );
                    return Err((
                        StatusCode::GATEWAY_TIMEOUT,
                        HeaderMap::new(),
                        Json(json!({"error": {"type": "api_error", "message": msg}})),
                    ));
                }
                Some(ProviderFailure::Upstream5xx { cand, key_id, key_name, key_masked, .. }) => {
                    if let Some(r) = failover_resp {
                        let s = r.status().as_u16();
                        return Ok(forward_upstream_error(
                            &state.database, &cand.provider_id, cand.provider_name.as_str(), &cand.model_row_id, model_name.as_str(),
                            Some(&key_id), Some(&key_name), Some(&key_masked),
                            Some(service_key.id.as_str()), service_key.name.as_str(), service_key.key_masked.as_str(),
                            endpoint,
                            r, s, trace_id, &start_time,
                        ).await);
                    }
                    return Err((
                        StatusCode::SERVICE_UNAVAILABLE,
                        HeaderMap::new(),
                        Json(json!({"error": {"type": "api_error", "message": "No available upstream keys"}})),
                    ));
                }
                None => {}
            }
            if let Some(r) = last_resp {
                let s = r.status().as_u16();
                return Ok(forward_upstream_error(
                    &state.database, &last_candidate.provider_id, last_candidate.provider_name.as_str(),
                    &last_candidate.model_row_id, model_name.as_str(),
                    last_key_id.as_deref(), last_key_name.as_deref(), last_key_masked.as_deref(),
                    Some(service_key.id.as_str()), service_key.name.as_str(), service_key.key_masked.as_str(),
                    endpoint,
                    r, s, trace_id, &start_time,
                ).await);
            }
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                HeaderMap::new(),
                Json(json!({"error": {"type": "api_error", "message": "No available upstream keys"}})),
            ));
        }
    };
    let response = response.unwrap();
    let upstream_status = response.status().as_u16();

    // ── 6. 重绑定 winner 的 provider 信息 ───────────────────────
    let provider_id = resolved.provider_id.clone();
    let model_row_id = resolved.model_row_id.clone();
    let provider_is_anthropic = resolved.provider_kind == "anthropic";

    if upstream_status >= 400 {
        return Ok(forward_upstream_error(
            &state.database, &provider_id, resolved.provider_name.as_str(), &model_row_id, model_name.as_str(),
            last_key_id.as_deref(), last_key_name.as_deref(), last_key_masked.as_deref(),
            Some(service_key.id.as_str()), service_key.name.as_str(), service_key.key_masked.as_str(),
            endpoint,
            response, upstream_status, trace_id, &start_time,
        ).await);
    }

    info!(
        trace_id = %trace_id,
        status = upstream_status,
        duration_ms = start_time.elapsed().as_millis(),
        "Upstream response received, starting stream"
    );

    // ── 7. 流式转发（4 种分支） ─────────────────────────────────
    // needs_translation: 客户端格式 ≠ 上游格式时需要翻译
    let needs_translation = matches!(
        (&ctx.client_format, provider_is_anthropic),
        (ClientFormat::Anthropic, false) | (ClientFormat::Openai, true)
    );

    if needs_translation {
        if provider_is_anthropic {
            // ── Anthropic 上游 → OpenAI 客户端（翻译） ────────────
            spawn_translate_anthropic_to_openai(
                response, &state, trace_id, start_time,
                &provider_id, &resolved, model_name, &ctx.service_key,
                &last_key_id, &last_key_name, &last_key_masked, endpoint,
            )
        } else {
            // ── OpenAI 上游 → Anthropic 客户端（翻译） ────────────
            spawn_translate_openai_to_anthropic(
                response, &state, trace_id, start_time,
                &provider_id, &resolved, model_name, &ctx.service_key,
                &last_key_id, &last_key_name, &last_key_masked, endpoint,
                ctx.est_input, &ctx.body_anthropic,
            )
        }
    } else {
        // ── 同格式直通（passthrough） ─────────────────────────────
        spawn_passthrough(
            response, &state, trace_id, start_time,
            &provider_id, &resolved, model_name, &ctx.service_key,
            &last_key_id, &last_key_name, &last_key_masked, endpoint,
        )
    }
}

// ═══════════════════════════════════════════════════════════════════
// 流式分支实现
// ═══════════════════════════════════════════════════════════════════

/// Passthrough（同格式直通）：立即返回 Response + keepalive + 正确 SSE 头。
fn spawn_passthrough(
    response: reqwest::Response,
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
) -> Result<Response, ErrorTuple> {
    let provider_kind = resolved.provider_kind.clone();
    let provider_id_log = provider_id.to_string();
    let provider_name_log = resolved.provider_name.clone();
    let model_id_log = resolved.model_row_id.clone();
    let model_name_log = model_name.to_string();
    let key_id_log = last_key_id.clone();
    let key_name_log = last_key_name.clone();
    let key_masked_log = last_key_masked.clone();
    let service_key_id_log = service_key.id.clone();
    let service_key_name_log = service_key.name.clone();
    let service_key_masked_log = service_key.key_masked.clone();
    let db = state.database.clone();

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, reqwest::Error>>(100);

    tokio::spawn(async move {
        // 初始 keepalive：客户端立即知道连接存活
        let _ = tx.send(Ok(bytes::Bytes::from(":keepalive\n\n"))).await;

        // 后台 keepalive 心跳：上游等待期间保持连接存活
        let keepalive_tx = tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(SSE_KEEPALIVE_SECS));
            loop {
                interval.tick().await;
                if keepalive_tx.send(Ok(bytes::Bytes::from(":keepalive\n\n"))).await.is_err() {
                    break;
                }
            }
        });

        // 主转发循环
        let mut sniff = sniff::SniffStream::new(response.bytes_stream(), &provider_kind);
        loop {
            let item = match tokio::time::timeout(
                Duration::from_secs(UPSTREAM_CHUNK_TIMEOUT_SECS),
                sniff.next(),
            )
            .await
            {
                Ok(Some(i)) => i,
                Ok(None) => break,
                Err(_) => {
                    warn!("upstream stream silent for {}s, closing", UPSTREAM_CHUNK_TIMEOUT_SECS);
                    break;
                }
            };
            if tx.send(item).await.is_err() {
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
        let _ = db.insert_usage_log(
            chrono::Utc::now().timestamp(),
            &provider_id_log,
            provider_name_log.as_str(),
            &model_id_log,
            model_name_log.as_str(),
            key_id_log.as_deref(),
            key_name_log.as_deref().unwrap_or(""),
            key_masked_log.as_deref().unwrap_or(""),
            Some(service_key_id_log.as_str()),
            service_key_name_log.as_str(),
            service_key_masked_log.as_str(),
            endpoint,
            input_t,
            output_tokens,
            start_time.elapsed().as_millis() as i64,
            true,
            None,
            cr,
        );
    });

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .header("x-accel-buffering", "no")
        .body(axum::body::Body::from_stream(ReceiverStream::new(rx)))
        .unwrap())
}

/// Translation: OpenAI 上游 → Anthropic 客户端。
fn spawn_translate_openai_to_anthropic(
    response: reqwest::Response,
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
    body: &Value,
) -> Result<Response, ErrorTuple> {
    let mut stream = response.bytes_stream();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, std::io::Error>>(100);
    let model_name_clone = model_name.to_string();
    let trace_id_clone = trace_id.to_string();
    let db = state.database.clone();
    let provider_id_log = provider_id.to_string();
    let provider_name_log = resolved.provider_name.clone();
    let model_id_log = resolved.model_row_id.clone();
    let model_name_log = model_name.to_string();
    let key_id_log = last_key_id.clone();
    let key_name_log = last_key_name.clone();
    let key_masked_log = last_key_masked.clone();
    let service_key_id_log = service_key.id.clone();
    let service_key_name_log = service_key.name.clone();
    let service_key_masked_log = service_key.key_masked.clone();

    tokio::spawn(async move {
        // 初始 keepalive：客户端立即收到首个 SSE 事件
        let _ = tx.send(Ok(Event::default().comment("keepalive"))).await;

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
                Ok(Some(c)) => c,
                Ok(None) => break,
                Err(_) => {
                    warn!(
                        trace_id = %trace_id_clone,
                        "upstream stream silent for {}s, closing",
                        UPSTREAM_CHUNK_TIMEOUT_SECS
                    );
                    break;
                }
            };
            if let Ok(bytes) = chunk {
                buffer.push_str(&String::from_utf8_lossy(&bytes));

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
                                &model_name_clone,
                                &mut stream_state,
                            );
                            for ev in events {
                                if ev != Value::Null {
                                    chunk_count += 1;
                                    let event_type = ev["type"].as_str().unwrap_or("message");
                                    let json_str = serde_json::to_string(&ev).unwrap();
                                    let _ = tx.send(
                                        Ok(Event::default().event(event_type).data(json_str)),
                                    )
                                    .await;
                                }
                            }
                        }
                    }
                }
            }
        }

        for ev in translate::finalize_openai_to_anthropic(&mut stream_state) {
            let event_type = ev["type"].as_str().unwrap_or("message");
            let json_str = serde_json::to_string(&ev).unwrap();
            let _ = tx.send(
                Ok(Event::default().event(event_type).data(json_str)),
            )
            .await;
        }

        info!(
            trace_id = %trace_id_clone,
            total_chunks = chunk_count,
            done = saw_done,
            "Stream ended"
        );

        let output_tokens = if stream_state.output_tokens > 0 {
            stream_state.output_tokens as i64
        } else {
            (stream_state.output_chars / 4) as i64
        };
        let input_t = stream_state.input_tokens as i64;
        let cr = stream_state.cache_read_input_tokens as i64;
        let _ = db.insert_usage_log(
            chrono::Utc::now().timestamp(),
            &provider_id_log,
            provider_name_log.as_str(),
            &model_id_log,
            model_name_log.as_str(),
            key_id_log.as_deref(),
            key_name_log.as_deref().unwrap_or(""),
            key_masked_log.as_deref().unwrap_or(""),
            Some(service_key_id_log.as_str()),
            service_key_name_log.as_str(),
            service_key_masked_log.as_str(),
            endpoint,
            input_t,
            output_tokens,
            start_time.elapsed().as_millis() as i64,
            true,
            None,
            cr,
        );
    });

    let mut resp = Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::default())
        .into_response();
    resp.headers_mut().insert("x-accel-buffering", "no".parse().unwrap());
    Ok(resp)
}

/// Translation: Anthropic 上游 → OpenAI 客户端。
fn spawn_translate_anthropic_to_openai(
    response: reqwest::Response,
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
) -> Result<Response, ErrorTuple> {
    let mut stream = response.bytes_stream();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, std::io::Error>>(100);
    let trace_id_clone = trace_id.to_string();
    let db = state.database.clone();
    let provider_id_log = provider_id.to_string();
    let provider_name_log = resolved.provider_name.clone();
    let model_id_log = resolved.model_row_id.clone();
    let model_name_log = model_name.to_string();
    let key_id_log = last_key_id.clone();
    let key_name_log = last_key_name.clone();
    let key_masked_log = last_key_masked.clone();
    let service_key_id_log = service_key.id.clone();
    let service_key_name_log = service_key.name.clone();
    let service_key_masked_log = service_key.key_masked.clone();

    tokio::spawn(async move {
        // 初始 keepalive
        let _ = tx.send(Ok(Event::default().comment("keepalive"))).await;

        let mut buffer = String::new();
        let mut chunk_count = 0u64;
        let mut accum_input: u64 = 0;
        let mut accum_output: u64 = 0;
        let mut accum_cache_read: u64 = 0;
        let mut accum_chars: u64 = 0;
        let mut oa_state = translate::OaStreamState::new();

        let record_usage = |input_tokens: u64, output_tokens: u64, output_chars: u64, cache_read: u64| {
            let output_tokens = if output_tokens > 0 {
                output_tokens as i64
            } else {
                (output_chars / 4) as i64
            };
            let cr = cache_read as i64;
            let _ = db.insert_usage_log(
                chrono::Utc::now().timestamp(),
                &provider_id_log,
                provider_name_log.as_str(),
                &model_id_log,
                model_name_log.as_str(),
                key_id_log.as_deref(),
                key_name_log.as_deref().unwrap_or(""),
                key_masked_log.as_deref().unwrap_or(""),
                Some(service_key_id_log.as_str()),
                service_key_name_log.as_str(),
                service_key_masked_log.as_str(),
                endpoint,
                input_tokens as i64,
                output_tokens,
                start_time.elapsed().as_millis() as i64,
                true,
                None,
                cr,
            );
        };

        loop {
            let chunk = match tokio::time::timeout(
                Duration::from_secs(UPSTREAM_CHUNK_TIMEOUT_SECS),
                stream.next(),
            )
            .await
            {
                Ok(Some(c)) => c,
                Ok(None) => break,
                Err(_) => {
                    warn!(
                        trace_id = %trace_id_clone,
                        "upstream stream silent for {}s, closing",
                        UPSTREAM_CHUNK_TIMEOUT_SECS
                    );
                    break;
                }
            };
            if let Ok(bytes) = chunk {
                buffer.push_str(&String::from_utf8_lossy(&bytes));

                while let Some(newline_pos) = buffer.find("\n\n") {
                    let event = buffer[..newline_pos].to_string();
                    buffer = buffer[newline_pos + 2..].to_string();

                    if let Some(data) = event.strip_prefix("data: ") {
                        if data == "[DONE]" {
                            info!(
                                trace_id = %trace_id_clone,
                                total_chunks = chunk_count,
                                "Stream completed"
                            );
                            let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
                            record_usage(accum_input, accum_output, accum_chars, accum_cache_read);
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
                                let _ = tx.send(Ok(Event::default().data(json_str))).await;
                            }
                        }
                    }
                }
            }
        }
        info!(
            trace_id = %trace_id_clone,
            total_chunks = chunk_count,
            "Stream ended (no [DONE] received)"
        );
        let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
        record_usage(accum_input, accum_output, accum_chars, accum_cache_read);
    });

    let mut resp = Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::default())
        .into_response();
    resp.headers_mut().insert("x-accel-buffering", "no".parse().unwrap());
    Ok(resp)
}
