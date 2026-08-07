//! 流式代理引擎：上游连接 + 双循环重试 + 统一 IR 转发。
//!
//! `proxy_stream()` 是三个 handler（Anthropic / Chat / Responses）共享的核心逻辑。
//! handler 负责认证 + 请求体准备，本模块负责路由解析、上游连接、密钥轮换、
//! 故障转移、流式转发。
//!
//! SSE 即时响应：路由解析后立即返回 HTTP Response（含 `:keepalive` 首字节），
//! 上游连接 + 密钥轮换 + 流式转发全部在后台 spawn 中完成。客户端在毫秒级内
//! 收到首字节，上游等待期间每 15 秒发一次 keepalive 心跳。上游错误通过
//! SSE error event 传达（而非 HTTP status code），确保客户端始终有数据流动。

use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info, warn};

use crate::gateway::server::AppState;

use super::auth::ServiceKeyInfo;
use super::ir;
use super::ir::types::IrRequest;
use super::key_rotation::{pick_key_for, update_key_health};
use super::route::{resolve_route, ResolvedRoute};
use super::websearch::{has_websearch_tool_ir, run_websearch_loop};

/// HTTP error tuple returned by proxy handlers.
pub type ErrorTuple = (StatusCode, HeaderMap, Json<Value>);

/// 客户端期望的响应格式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientFormat {
    Messages,
    ChatCompletions,
    Responses,
}

/// 已认证的流式请求上下文（handler 构建后传入 `proxy_stream`）。
pub struct StreamContext {
    pub(super) trace_id: String,
    pub(super) start_time: Instant,
    pub(super) service_key: ServiceKeyInfo,
    pub(super) model_name: String,
    pub(super) endpoint: &'static str,
    /// IR 格式请求体（stream=true 已设置）。
    pub(super) ir_request: IrRequest,
    pub(super) client_format: ClientFormat,
    /// 输入 token 估算（translation 路径 message_start 占位用）。
    pub(super) est_input: u64,
    /// 按输入规模放宽后的「等待上游响应头」超时（秒），handler 预计算。
    pub(super) header_timeout_secs: u64,
}

/// provider 级失败（5xx/网络错误/响应头超时）：failover 切换后若无任何
/// provider 成功，通过 SSE error event 告知客户端。
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
        body: String,
    },
}

/// SSE keepalive 心跳间隔（秒）。上游等待期间（~8s）保持连接存活，
/// 防止客户端因无数据传输而超时断开。
pub(super) const SSE_KEEPALIVE_SECS: u64 = 15;

/// 流式代理引擎：路由解析 → 立即返回 Response → 后台 spawn 上游连接 + 流式转发。
///
/// handler 完成认证/配额/请求体准备后调用此函数。
/// 路由解析（~1ms）在同步段完成；上游连接 + 密钥轮换 + 流式转发全部在后台 spawn 中，
/// 不阻塞 HTTP Response 返回。客户端在毫秒级内收到 `:keepalive` 首字节。
pub async fn proxy_stream(
    state: Arc<AppState>,
    ctx: StreamContext,
) -> Result<Response, ErrorTuple> {
    let trace_id = &ctx.trace_id;
    let model_name = &ctx.model_name;
    let header_timeout_secs = ctx.header_timeout_secs;

    // ── 1. 路由解析（同步段，~1ms） ─────────────────────────────────
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

    // ── 2. WebSearch 劫持（同步段，纯内存判断） ─────────────────────
    let resolved = candidates[0].clone();

    if state.websearch_hijack.load(std::sync::atomic::Ordering::Relaxed)
        && has_websearch_tool_ir(&ctx.ir_request)
    {
        info!(trace_id = %trace_id, provider_kind = %resolved.provider_kind, "web_search hijacked → local Bing loop");
        return run_websearch_loop(
            state.clone(),
            ctx.ir_request.clone(),
            resolved.clone(),
            ctx.client_format,
            trace_id.clone(),
            ctx.service_key,
        )
        .await;
    }

    // ── 3. 预构造请求体（同步段，纯内存操作） ───────────────────────
    let ir_request = ctx.ir_request.clone();
    let client = state.http_client.clone();
    let client_format = ctx.client_format;
    let est_input = ctx.est_input;

    info!(
        trace_id = %trace_id,
        upstream_url = %candidates[0].upstream_url,
        "Calling upstream API (streaming)"
    );

    // ── 4. 立即返回 Response + 后台 spawn ──────────────────────────
    let (tx, rx) = mpsc::channel::<Result<Bytes, Infallible>>(100);

    // 初始 keepalive：客户端立即知道连接存活
    let _ = tx.send(Ok(Bytes::from(":keepalive\n\n"))).await;

    // 后台 spawn：上游连接 + 双循环重试 + 流式转发
    let trace_id_owned = ctx.trace_id.clone();
    let start_time = ctx.start_time;
    let service_key = ctx.service_key;
    let model_name_owned = ctx.model_name;
    let endpoint = ctx.endpoint;

    tokio::spawn(async move {
        let trace_id = &trace_id_owned;
        let model_name = &model_name_owned;

        // ── keepalive 心跳 + 取消信号 ─────────────────────────────
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
        let keepalive_tx = tx.clone();
        let keepalive_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(SSE_KEEPALIVE_SECS));
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

        // ── 双循环：外层 provider 候选，内层 key 轮换 ─────────────
        let ir_request = ir_request;
        let mut last_resp: Option<reqwest::Response> = None;
        let mut last_key_id: Option<String> = None;
        let mut last_key_name: Option<String> = None;
        let mut last_key_masked: Option<String> = None;
        let mut last_candidate: ResolvedRoute = candidates[0].clone();
        let mut provider_failure: Option<ProviderFailure> = None;
        let mut response: Option<reqwest::Response> = None;
        let mut winner: Option<(ResolvedRoute, super::route::PickedKey)> = None;

        'provider: for (ci, cand) in candidates.iter().enumerate() {
            if failover && super::failover::is_provider_cooling(&state, &cand.provider_id) {
                info!(trace_id = %trace_id, provider = %cand.provider_id, "provider cooling, skipping");
                continue;
            }
            last_candidate = cand.clone();

            // 根据 provider_kind 从 IR 生成上游请求体
            let mut attempt_body = match cand.provider_kind.as_str() {
                "messages" => ir::to_messages::ir_req_to_messages(&ir_request),
                "responses" => ir::to_responses::ir_req_to_responses(&ir_request),
                _ => ir::to_chat_completions::ir_req_to_chat_completions(&ir_request),
            };

            // 注入真实 model ID + stream 选项
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
                if cand.provider_kind == "messages" {
                    req_builder = req_builder
                        .header("x-api-key", &picked.key_hash)
                        .header("anthropic-version", "2023-06-01");
                } else {
                    req_builder = req_builder.header("Authorization", format!("Bearer {}", picked.key_hash));
                }

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
                        send_error_event(&tx, client_format, "api_error", &e.to_string());
                        return;
                    }
                    Err(_) => {
                        if failover && ci + 1 < candidates.len() {
                            provider_failure = Some(ProviderFailure::HeaderTimeout {
                                cand: cand.clone(),
                                key_id: picked.id.clone(),
                                key_name: picked.name.clone(),
                                key_masked: picked.key_masked.clone(),
                                secs: header_timeout_secs,
                            });
                            super::failover::mark_provider_failed(&state, &cand.provider_id);
                            continue 'provider;
                        }
                        let duration_ms = start_time.elapsed().as_millis() as i64;
                        let msg = format!(
                            "upstream timed out after {}s waiting for response headers",
                            header_timeout_secs
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
                        send_error_event(&tx, client_format, "api_error", &msg);
                        return;
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
                        let body_str = resp.text().await.unwrap_or_default();
                        provider_failure = Some(ProviderFailure::Upstream5xx {
                            cand: cand.clone(),
                            key_id: picked.id.clone(),
                            key_name: picked.name.clone(),
                            key_masked: picked.key_masked.clone(),
                            body: body_str,
                        });
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

        // ── 错误处理：通过 SSE error event 告知客户端 ─────────────
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
                        send_error_event(&tx, client_format, "api_error", &msg);
                        return;
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
                        send_error_event(&tx, client_format, "api_error", &msg);
                        return;
                    }
                    Some(ProviderFailure::Upstream5xx { cand, key_id, key_name, key_masked, body }) => {
                        let duration_ms = start_time.elapsed().as_millis() as i64;
                        let msg = extract_error_message(&body);
                        warn!(trace_id = %trace_id, duration_ms, key_id = %key_id, "Upstream 5xx: {}", msg);
                        let _ = state.database.insert_usage_log(
                            chrono::Utc::now().timestamp(),
                            &cand.provider_id, cand.provider_name.as_str(), &cand.model_row_id, model_name.as_str(),
                            Some(&key_id), key_name.as_str(), key_masked.as_str(),
                            Some(service_key.id.as_str()), service_key.name.as_str(), service_key.key_masked.as_str(),
                            endpoint,
                            0, 0, duration_ms, false, Some(&format!("upstream 5xx: {}", msg)), 0,
                        );
                        send_error_event(&tx, client_format, "api_error", &msg);
                        return;
                    }
                    None => {}
                }
                if let Some(r) = last_resp {
                    let s = r.status().as_u16();
                    let body_str = r.text().await.unwrap_or_default();
                    let msg = extract_error_message(&body_str);
                    let duration_ms = start_time.elapsed().as_millis() as i64;
                    warn!(trace_id = %trace_id, upstream_status = s, duration_ms, "Upstream error forwarded");
                    let _ = state.database.insert_usage_log(
                        chrono::Utc::now().timestamp(),
                        &last_candidate.provider_id, last_candidate.provider_name.as_str(),
                        &last_candidate.model_row_id, model_name.as_str(),
                        last_key_id.as_deref(), last_key_name.as_deref().unwrap_or(""), last_key_masked.as_deref().unwrap_or(""),
                        Some(service_key.id.as_str()), service_key.name.as_str(), service_key.key_masked.as_str(),
                        endpoint,
                        0, 0, duration_ms, false, Some(&format!("upstream status {}", s)), 0,
                    );
                    send_error_event(&tx, client_format, "api_error", &msg);
                    return;
                }
                send_error_event(&tx, client_format, "api_error", "No available upstream keys");
                return;
            }
        };
        let response = response.unwrap();
        let upstream_status = response.status().as_u16();

        // ── 重绑定 winner 的 provider 信息 ───────────────────────
        let provider_id = resolved.provider_id.clone();

        if upstream_status >= 400 {
            let body_str = response.text().await.unwrap_or_default();
            let msg = extract_error_message(&body_str);
            let duration_ms = start_time.elapsed().as_millis() as i64;
            warn!(trace_id = %trace_id, upstream_status, duration_ms, "Upstream error (post-winner)");
            let _ = state.database.insert_usage_log(
                chrono::Utc::now().timestamp(),
                &provider_id, resolved.provider_name.as_str(), &resolved.model_row_id, model_name.as_str(),
                last_key_id.as_deref(), last_key_name.as_deref().unwrap_or(""), last_key_masked.as_deref().unwrap_or(""),
                Some(service_key.id.as_str()), service_key.name.as_str(), service_key.key_masked.as_str(),
                endpoint,
                0, 0, duration_ms, false, Some(&format!("upstream status {}", upstream_status)), 0,
            );
            send_error_event(&tx, client_format, "api_error", &msg);
            return;
        }

        info!(
            trace_id = %trace_id,
            status = upstream_status,
            duration_ms = start_time.elapsed().as_millis(),
            "Upstream response received, starting stream"
        );

        // ── 统一 IR 流式转发 ──────────────────────────────────────
        super::forward::forward_stream_ir(
            response, &tx, &state, trace_id, start_time,
            &provider_id, &resolved, model_name, &service_key,
            &last_key_id, &last_key_name, &last_key_masked, endpoint,
            &resolved.provider_kind, client_format, est_input,
        ).await;
    });

    // ── 立即返回 Response（客户端毫秒级收到首字节） ────────────────
    Ok(sse_response(rx))
}

/// 用 mpsc rx 构造标准 SSE Response（含 keepalive 用的响应头集合）。
/// proxy_stream 与 websearch_loop 共用，确保两条路径响应头一致。
pub(super) fn sse_response(rx: mpsc::Receiver<Result<Bytes, Infallible>>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .header("x-accel-buffering", "no")
        .body(axum::body::Body::from_stream(ReceiverStream::new(rx)))
        .unwrap()
}

// ═══════════════════════════════════════════════════════════════════
// 辅助函数
// ═══════════════════════════════════════════════════════════════════

/// 向客户端发送 SSE error event（根据客户端格式构造）。
pub(super) fn send_error_event(
    tx: &mpsc::Sender<Result<Bytes, Infallible>>,
    client_format: ClientFormat,
    error_type: &str,
    message: &str,
) {
    let bytes = match client_format {
        ClientFormat::Messages => {
            let payload = serde_json::to_string(&json!({
                "type": "error",
                "error": { "type": error_type, "message": message }
            }))
            .unwrap_or_default();
            Bytes::from(format!("event: error\ndata: {}\n\n", payload))
        }
        ClientFormat::ChatCompletions => {
            let payload = serde_json::to_string(&json!({
                "error": { "message": message, "type": error_type, "code": null }
            }))
            .unwrap_or_default();
            Bytes::from(format!("data: {}\n\ndata: [DONE]\n\n", payload))
        }
        ClientFormat::Responses => {
            let payload = serde_json::to_string(&json!({
                "type": "response.failed",
                "error": { "type": error_type, "message": message }
            }))
            .unwrap_or_default();
            Bytes::from(format!("event: error\ndata: {}\n\n", payload))
        }
    };
    let _ = tx.send(Ok(bytes));
}

/// 从上游错误 body 中提取可读错误信息。
fn extract_error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v["error"]["message"]
                .as_str()
                .or_else(|| v["message"].as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| {
            if body.is_empty() {
                "upstream error".to_string()
            } else {
                body.to_string()
            }
        })
}
