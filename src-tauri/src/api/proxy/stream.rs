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
use axum::response::Response;
use axum::Json;
use bytes::Bytes;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info, warn};

use crate::gateway::server::AppState;

use super::auth::ServiceKeyInfo;
use super::forward::ForwardOutcome;
use super::ir;
use super::ir::types::{IrRequest, IrSystemBlock, IrSystemContent, IrToolChoice};
use super::key_rotation::{pick_key_for, update_key_health};
use super::route::{resolve_route, ResolvedRoute};

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
    /// 客户端原始 stream 偏好（true=流式，false=非流式）。
    /// 上游始终走流式，但客户端非流式时需要收集所有事件后返回 JSON。
    pub(super) client_wants_stream: bool,
    /// 对话审查上下文（仅 audit_enabled 时存在）。
    /// 由 handler 构建，forward/non_stream 消费：流式转发完成后，
    /// 将请求消息 + 累积的助手回复一起 upsert 到 conversations 表。
    pub(super) audit: Option<AuditCapture>,
}

/// 对话审查捕获上下文：handler 构建，forward/non_stream 消费。
/// 在流式转发完成后，将请求消息 + 累积的助手回复一起 upsert 到 conversations 表。
pub(super) struct AuditCapture {
    pub(super) db: crate::db::Database,
    pub(super) sk_id: String,
    pub(super) sk_name: String,
    pub(super) request_messages: Vec<super::ir::types::IrMessage>,
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
    /// HTTP 200 但流内错误事件（SSE error / 非 SSE JSON 错误体），
    /// 密钥级（欠费/限流/认证）轮换耗尽后透传。
    StreamError {
        cand: ResolvedRoute,
        key_id: String,
        key_name: String,
        key_masked: String,
        status: u16,
        message: String,
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
    mut ctx: StreamContext,
) -> Result<Response, ErrorTuple> {
    // MCP WebSearch 模式：开关开启时剔除请求自带的搜索类工具（客户端内置
    // `WebSearch` / 上游 server-side `web_search_*`），防止上游官方搜索生效——
    // 模型需要联网搜索时走客户端注册的本地 MCP（/mcp 的 web_search 工具）。
    // 开关关闭则完全不碰工具定义。
    if state.mcp_websearch.load(std::sync::atomic::Ordering::Relaxed) {
        strip_search_tools(&mut ctx.ir_request);
    }

    // MCP WebFetch 模式：开关开启时剔除请求自带的网页抓取类工具（客户端内置
    // `WebFetch` / 上游 server-side `web_fetch_*`），防止上游官方抓取生效——
    // 模型需要抓取网页时走客户端注册的本地 MCP（/mcp 的 web_fetch 工具）。
    // 开关关闭则完全不碰工具定义。
    if state.mcp_webfetch.load(std::sync::atomic::Ordering::Relaxed) {
        strip_fetch_tools(&mut ctx.ir_request);
    }

    // MCP Notify 模式：开关开启时剔除请求自带的原生通知工具（如 Claude Code 的
    // `PushNotification`、OpenAI 的 `send_notification`），并注入系统提示告知模型
    // 必须使用本地 MCP 的 `notify` 工具（跨平台、支持声音等宽松参数）。
    // 同时告知模型在任务完成、需要用户决策、响应完毕等场景主动调用通知。
    if state.mcp_notify.load(std::sync::atomic::Ordering::Relaxed) {
        strip_notify_tools(&mut ctx.ir_request);
        inject_notify_hint(&mut ctx.ir_request);
    }

    // ── 分支：客户端非流式 → 走收集路径 ──────────────────────────────
    // 上游始终走流式（复用所有流式基础设施），但客户端期望 JSON 响应时，
    // 收集所有 IR 事件后组装成完整的非流式 JSON 返回。
    if !ctx.client_wants_stream {
        return super::non_stream::proxy_non_stream(state, ctx).await;
    }

    let trace_id = &ctx.trace_id;
    let model_name = &ctx.model_name;
    let header_timeout_secs = ctx.header_timeout_secs;

    // ── 1. 路由解析（同步段，~1ms；与非流式路径共享） ────────────────
    let (candidates, failover, is_combo_req) =
        resolve_candidates(&state, model_name, trace_id).await?;

    // ── 2. 上下文超限预警（同步段，纯内存判断） ────────────────────
    // 估算输入 token 超过模型 context_window 时仅记 warn，不阻断请求。
    // 原因：
    //   1. chars/4 估算口径偏保守，中文/代码实际 token 数通常低于估算；
    //   2. 硬拒绝会阻断客户端 auto-compact（/compact 自身也需走代理），
    //      形成死锁——客户端永远无法拿到真实 usage 来触发压缩。
    // 让上游自行判断是否超限并返回准确错误，客户端可据此 auto-compact。
    let est_input = ctx.est_input;
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
    let audit_capture = ctx.audit;

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
        // 400 时响应体（resp 被 text() 消费后无法保留，body 单独存）
        let mut last_resp_body: Option<String> = None;
        let mut last_key_id: Option<String> = None;
        let mut last_key_name: Option<String> = None;
        let mut last_key_masked: Option<String> = None;
        let mut last_candidate: ResolvedRoute = candidates[0].clone();
        let mut provider_failure: Option<ProviderFailure> = None;
        let mut winner: Option<(ResolvedRoute, super::route::PickedKey)> = None;

        'provider: for (ci, cand) in candidates.iter().enumerate() {
            if failover && super::failover::is_provider_cooling(&state, &cand.provider_id) {
                info!(trace_id = %trace_id, provider = %cand.provider_id, "provider cooling, skipping");
                continue;
            }
            last_candidate = cand.clone();

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

                // 400 + 配额耗尽：视为密钥级错误，轮换到下一把密钥重试。
                // 某些上游（如 OpenAI）在密钥配额耗尽时返回 400 而非 402/429，
                // 例如 {"error":{"code":"quotaExceeded","message":"Insufficient quota"}}。
                // 这类错误不应透传给客户端，应换密钥重试，除非所有密钥都失败。
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
                        last_resp_body = Some(body_str);
                        last_resp = None;
                        continue;
                    }
                    // 普通 400：透传给客户端（以 SSE error event 表达）
                    update_key_health(&state.keys, &cand.provider_id, &picked.key_hash, status);
                    warn!(trace_id = %trace_id, status, upstream_body = %body_str, "upstream 400");
                    // 清掉更早 attempt 的陈旧 last_resp（401/5xx 残留会让兜底链透传错内容）
                    last_resp = None;
                    last_resp_body = Some(body_str);
                    if is_combo_req {
                        // 组合语义：普通 400 立即透传，不试下一成员（请求级错误换成员也白搭）
                        break 'provider;
                    }
                    break;
                }

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
                // 2xx：选中 → 立即流式转发（搜索工具已在入口剔除，模型经客户端
                // 注册的 MCP 工具完成搜索，代理不再跑 tool-calling 循环）
                super::failover::mark_provider_ok(&state, &cand.provider_id);
                info!(
                    trace_id = %trace_id,
                    status = status,
                    duration_ms = start_time.elapsed().as_millis(),
                    "Upstream response received, starting stream"
                );
                let outcome = super::forward::forward_stream_ir(
                    resp, &tx, &state, trace_id, start_time,
                    &cand.provider_id, cand, model_name, &service_key,
                    &last_key_id, &last_key_name, &last_key_masked, endpoint,
                    &cand.provider_kind, client_format, est_input,
                    audit_capture.as_ref(),
                ).await;
                match outcome {
                    // 流内密钥级错误（200 + SSE error event / 非 SSE JSON 错误体）：
                    // 视为欠费/限流/认证失败，换下一把密钥重试。
                    ForwardOutcome::UpstreamKeyError { status: key_status, message } => {
                        warn!(
                            trace_id = %trace_id,
                            status = key_status,
                            key_id = %picked.id,
                            upstream_message = %message,
                            "upstream 200 with SSE key error, rotating key"
                        );
                        update_key_health(&state.keys, &cand.provider_id, &picked.key_hash, key_status);
                        provider_failure = Some(ProviderFailure::StreamError {
                            cand: cand.clone(),
                            key_id: picked.id.clone(),
                            key_name: picked.name.clone(),
                            key_masked: picked.key_masked.clone(),
                            status: key_status,
                            message,
                        });
                        // 清掉更早 attempt 的陈旧错误体，避免全部耗尽后透传错内容
                        last_resp = None;
                        last_resp_body = None;
                        continue;
                    }
                    // Completed / ErrorDelivered：流已结束（正常完成或错误已透传）
                    _ => {
                        winner = Some((cand.clone(), picked));
                        break 'provider;
                    }
                }
            }
        }

        // ── 错误处理：通过 SSE error event 告知客户端 ─────────────
        // 正常完成（Completed / ErrorDelivered）时 winner 已设置——流已在
        // 双循环内转发完毕，直接结束 spawn。
        if let Some((_r, _k)) = winner {
            return;
        }
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
                    0, 0, duration_ms, false,
                    Some(&format!("upstream 5xx: {} | body: {}", msg, body.chars().take(200).collect::<String>())),
                    0,
                );
                send_error_event(&tx, client_format, "api_error", &msg);
                return;
            }
            // 200 + 流内密钥级错误在所有密钥上轮换耗尽后的兜底透传
            Some(ProviderFailure::StreamError { cand, key_id, key_name, key_masked, status, message }) => {
                let duration_ms = start_time.elapsed().as_millis() as i64;
                let msg = format!("upstream {}: {}", status, message);
                warn!(trace_id = %trace_id, duration_ms, key_id = %key_id, "{}", msg);
                let _ = state.database.insert_usage_log(
                    chrono::Utc::now().timestamp(),
                    &cand.provider_id, cand.provider_name.as_str(), &cand.model_row_id, model_name.as_str(),
                    Some(&key_id), key_name.as_str(), key_masked.as_str(),
                    Some(service_key.id.as_str()), service_key.name.as_str(), service_key.key_masked.as_str(),
                    endpoint,
                    0, 0, duration_ms, false,
                    Some(&format!("upstream stream error {}: {}", status, message)),
                    0,
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
            warn!(trace_id = %trace_id, upstream_status = s, duration_ms, upstream_body = %body_str, "Upstream error forwarded");
            let _ = state.database.insert_usage_log(
                chrono::Utc::now().timestamp(),
                &last_candidate.provider_id, last_candidate.provider_name.as_str(),
                &last_candidate.model_row_id, model_name.as_str(),
                last_key_id.as_deref(), last_key_name.as_deref().unwrap_or(""), last_key_masked.as_deref().unwrap_or(""),
                Some(service_key.id.as_str()), service_key.name.as_str(), service_key.key_masked.as_str(),
                endpoint,
                0, 0, duration_ms, false,
                Some(&format!("upstream status {}: {}", s, body_str.chars().take(200).collect::<String>())),
                0,
            );
            send_error_event(&tx, client_format, "api_error", &msg);
            return;
        }
        if let Some(body_str) = last_resp_body {
            let msg = extract_error_message(&body_str);
            let duration_ms = start_time.elapsed().as_millis() as i64;
            warn!(trace_id = %trace_id, duration_ms, upstream_body = %body_str, "Upstream 400 forwarded (body-only)");
            let _ = state.database.insert_usage_log(
                chrono::Utc::now().timestamp(),
                &last_candidate.provider_id, last_candidate.provider_name.as_str(),
                &last_candidate.model_row_id, model_name.as_str(),
                last_key_id.as_deref(), last_key_name.as_deref().unwrap_or(""), last_key_masked.as_deref().unwrap_or(""),
                Some(service_key.id.as_str()), service_key.name.as_str(), service_key.key_masked.as_str(),
                endpoint,
                0, 0, duration_ms, false,
                Some(&format!("upstream status 400: {}", body_str.chars().take(200).collect::<String>())),
                0,
            );
            send_error_event(&tx, client_format, "api_error", &msg);
            return;
        }
        send_error_event(&tx, client_format, "api_error", "No available upstream keys");
        return;
    });

    // ── 立即返回 Response（客户端毫秒级收到首字节） ────────────────
    Ok(sse_response(rx))
}

/// 路由解析（组合别名展开 + failover 候选），流式与非流式两条路径共享。
///
/// 返回 `(候选列表, failover 开关, 是否组合请求)`；模型不可解析时返回 400
/// 错误元组（两条路径都以 HTTP 400 表达，此时尚未向客户端发送任何字节）。
pub(super) async fn resolve_candidates(
    state: &AppState,
    model_name: &str,
    trace_id: &str,
) -> Result<(Vec<ResolvedRoute>, bool, bool), ErrorTuple> {
    let global_failover = state.failover_enabled.load(std::sync::atomic::Ordering::Relaxed);
    // 组合别名优先：命中 enabled 组合 → 展开为多候选；组合强制 failover 语义
    // （成员间回退不受全局开关影响），普通别名保持现有行为。
    let is_combo = super::route::resolve_combo(state, model_name).await;
    // is_combo 会被下方 if let 移动消费，这里先取出「是否组合请求」标记。
    let is_combo_req = is_combo.is_some();
    let candidates: Vec<ResolvedRoute> = if let Some(cands) = is_combo {
        if cands.is_empty() {
            warn!(trace_id = %trace_id, model = %model_name, "Combo has no resolvable members");
            return Err((
                StatusCode::BAD_REQUEST,
                HeaderMap::new(),
                Json(json!({"error": {"type": "invalid_request_error", "message": "Model not found or not available"}})),
            ));
        }
        info!(
            trace_id = %trace_id,
            combo = %model_name,
            candidates = cands.len(),
            "Combo resolved"
        );
        cands
    } else {
        let cands = if global_failover {
            super::route::resolve_route_candidates(state, model_name).await
        } else {
            resolve_route(state, model_name).await.map(|r| vec![r])
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
    // 组合强制 failover：外层循环的冷却跳过/密钥耗尽/网络错误/超时/5xx 换成员门
    // 全部以 failover 为条件，组合命中后强制为 true（普通别名不受影响）。
    let failover = global_failover || is_combo_req;
    Ok((candidates, failover, is_combo_req))
}

/// 用 mpsc rx 构造标准 SSE Response（含 keepalive 用的响应头集合）。
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

/// 判断工具名是否属于搜索类（代理应剔除的范畴）。
///
/// 覆盖两种来源：
/// - Anthropic 服务端内置的 `web_search_*`（IR 层已归一化为 `web_search`）
/// - Claude Code 客户端的 `WebSearch`（PascalCase）
fn is_search_tool_name(name: &str) -> bool {
    name.starts_with("web_search") || name.eq_ignore_ascii_case("WebSearch")
}

/// 判断工具名是否属于网页抓取类（代理应剔除的范畴）。
///
/// 覆盖两种来源：
/// - Anthropic 服务端内置的 `web_fetch_*`（IR 层已归一化为 `web_fetch`）
/// - Claude Code 客户端的 `WebFetch`（PascalCase）
fn is_fetch_tool_name(name: &str) -> bool {
    name.starts_with("web_fetch") || name.eq_ignore_ascii_case("WebFetch")
}

/// 判断工具名是否属于"客户端/上游原生通知工具"（代理应剔除的范畴）。
///
/// 覆盖多种来源：
/// - Claude Code 内置的 `PushNotification`
/// - OpenAI Codex / ChatGPT 内置的 `send_notification`
/// - 通用名 `notify` / `notification`（上游 server-side 或客户端自定义）
/// - 以 `notify_` 开头或 `_notification` 结尾的派生名
///
/// **显式排除所有 MCP 工具**（`mcp__` 前缀）：本网关的本地 MCP 通知工具
/// （`mcp__xrl-tools__notify`）必须保留，不能被误剔除。
fn is_notify_tool_name(name: &str) -> bool {
    if name.starts_with("mcp__") {
        return false;
    }
    name.eq_ignore_ascii_case("PushNotification")
        || name.eq_ignore_ascii_case("send_notification")
        || name.eq_ignore_ascii_case("notify")
        || name.eq_ignore_ascii_case("notification")
        || name.starts_with("notify_")
        || name.ends_with("_notification")
}

/// MCP WebSearch 模式下的工具剔除。
///
/// 移除请求自带的全部搜索类工具（客户端内置 `WebSearch` / 上游 server-side
/// `web_search_*`），避免上游官方搜索生效——模型需要联网搜索时应走客户端注册的
/// 本地 MCP（`/mcp` 的 `web_search` 工具）。非搜索类工具不受影响。
///
/// `tool_choice` 若强制指向被移除的搜索工具，改写为 `Auto`
/// （代理不再注入自己的工具，无可改写的目标名）。
pub(super) fn strip_search_tools(ir_request: &mut IrRequest) {
    let before = ir_request.tools.len();
    ir_request.tools.retain(|t| {
        let dominated = is_search_tool_name(&t.name);
        if dominated {
            info!(tool = %t.name, "mcp: removing search tool from proxied request");
        }
        !dominated
    });
    let removed = before - ir_request.tools.len();

    if let Some(IrToolChoice::Tool { name }) = &ir_request.tool_choice {
        if is_search_tool_name(name) {
            info!(from = %name, "mcp: rewriting tool_choice target to auto");
            ir_request.tool_choice = Some(IrToolChoice::Auto);
        }
    }

    if removed > 0 {
        info!(removed, "mcp: stripped search tools from proxied request");
    }
}

/// MCP WebFetch 模式下的工具剔除。
///
/// 移除请求自带的全部网页抓取类工具（客户端内置 `WebFetch` / 上游 server-side
/// `web_fetch_*`），避免上游官方抓取生效——模型需要抓取网页时应走客户端注册的
/// 本地 MCP（`/mcp` 的 `web_fetch` 工具）。非抓取类工具不受影响。
///
/// `tool_choice` 若强制指向被移除的抓取工具，改写为 `Auto`
/// （代理不再注入自己的工具，无可改写的目标名）。
pub(super) fn strip_fetch_tools(ir_request: &mut IrRequest) {
    let before = ir_request.tools.len();
    ir_request.tools.retain(|t| {
        let dominated = is_fetch_tool_name(&t.name);
        if dominated {
            info!(tool = %t.name, "mcp: removing fetch tool from proxied request");
        }
        !dominated
    });
    let removed = before - ir_request.tools.len();

    if let Some(IrToolChoice::Tool { name }) = &ir_request.tool_choice {
        if is_fetch_tool_name(name) {
            info!(from = %name, "mcp: rewriting tool_choice target to auto");
            ir_request.tool_choice = Some(IrToolChoice::Auto);
        }
    }

    if removed > 0 {
        info!(removed, "mcp: stripped fetch tools from proxied request");
    }
}

/// MCP Notify 模式下的原生通知工具剔除。
///
/// 移除请求自带的全部"客户端/上游原生"通知工具（如 Claude Code 的
/// `PushNotification`、OpenAI 的 `send_notification` 等），避免模型走
/// 原生通道——原生通知受限于客户端实现（无声音、参数少、部分平台静默），
/// 应强制走本地 MCP 的 `notify` 工具（经 `notify-rust` 跨平台实现，
/// 支持 `sound` 等宽松参数）。
///
/// **保留所有 MCP 工具**（`mcp__` 前缀）：本地 MCP 的 `mcp__xrl-tools__notify`
/// 是模型唯一可用的通知通道，绝不能被误剔除。
///
/// `tool_choice` 若强制指向被移除的通知工具，改写为 `Auto`。
pub(super) fn strip_notify_tools(ir_request: &mut IrRequest) {
    let before = ir_request.tools.len();
    ir_request.tools.retain(|t| {
        let dominated = is_notify_tool_name(&t.name);
        if dominated {
            info!(tool = %t.name, "mcp: removing native notify tool from proxied request");
        }
        !dominated
    });
    let removed = before - ir_request.tools.len();

    if let Some(IrToolChoice::Tool { name }) = &ir_request.tool_choice {
        if is_notify_tool_name(name) {
            info!(from = %name, "mcp: rewriting tool_choice target to auto");
            ir_request.tool_choice = Some(IrToolChoice::Auto);
        }
    }

    if removed > 0 {
        info!(removed, "mcp: stripped native notify tools from proxied request");
    }
}

/// MCP Notify 模式下的系统提示注入。
///
/// 在系统提示末尾追加指令，告知模型：原生通知工具已被禁用，
/// 必须通过 MCP `notify` 工具发送通知，并在以下场景**主动调用**：
/// - 任务完成（长时间运行结束、构建/部署/下载等）
/// - 需要用户决策（等待用户输入、选择方案等）
/// - 响应完毕且用户可能已离开终端
///
/// 仅在请求中确实存在 MCP 通知工具（`mcp__*notify*`）时才注入提示，
/// 避免提示指向一个不存在的工具。
pub(super) fn inject_notify_hint(ir_request: &mut IrRequest) {
    // 检查是否存在 MCP 通知工具（`mcp__` 前缀 + 名称含 `notify`）
    let has_mcp_notify = ir_request
        .tools
        .iter()
        .any(|t| t.name.starts_with("mcp__") && t.name.to_lowercase().contains("notify"));
    if !has_mcp_notify {
        return;
    }

    // 找到第一个可用的 MCP notify 工具名（用于提示中精确引用）
    let mcp_notify_name = ir_request
        .tools
        .iter()
        .find(|t| t.name.starts_with("mcp__") && t.name.to_lowercase().contains("notify"))
        .map(|t| t.name.as_str())
        .unwrap_or("notify");

    const HINT: &str = "\n\n[Desktop Notification Policy] \
        Native notification tools (e.g. PushNotification, send_notification) are DISABLED. \
        You MUST use the `__TOOL__` MCP tool to send desktop notifications. \
        Proactively call it whenever: \
        (1) a long-running task completes (build, deploy, download, scan, etc.); \
        (2) you need the user's decision or answer and the user may not be watching the terminal; \
        (3) your response is finished and the user may have stepped away. \
        Keep the notification message concise (under 200 characters) and lead with the actionable fact.";

    let hint = HINT.replace("__TOOL__", mcp_notify_name);

    match &mut ir_request.system {
        Some(IrSystemContent::Text(ref mut s)) => {
            s.push_str(&hint);
        }
        Some(IrSystemContent::Blocks(ref mut blocks)) => {
            blocks.push(IrSystemBlock {
                text: hint,
                cache_control: None,
            });
        }
        None => {
            ir_request.system = Some(IrSystemContent::Text(hint));
        }
    }

    info!(tool = %mcp_notify_name, "mcp: injected notify hint into system prompt");
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
                "response": {
                    "id": null,
                    "object": "response",
                    "status": "failed",
                    "output": [],
                    "error": { "type": error_type, "message": message }
                }
            }))
            .unwrap_or_default();
            Bytes::from(format!("event: response.failed\ndata: {}\n\n", payload))
        }
    };
    // 注意：不能用 tx.send()（async）——本函数是同步的，未 await 的 future
    // 会被丢弃，错误事件永远发不出去，客户端只看到 keepalive 后 EOF
    // （表现为 "stream closed before response.completed"）。
    // 用 try_send（同步）：channel 容量 100，错误场景下不可能满。
    let _ = tx.try_send(Ok(bytes));
}

/// 检测上游 400 响应体是否为密钥配额耗尽错误。
///
/// 某些上游（如 OpenAI）在密钥配额耗尽时返回 HTTP 400 而非 402/429：
/// ```json
/// {"error": {"code": "quotaExceeded", "message": "Insufficient quota"}}
/// ```
///
/// 检测策略（任一命中即视为密钥级配额错误）：
/// 1. `error.code` 包含 "quota"（不区分大小写）
/// 2. `error.type` 包含 "quota" 或 "insufficient_quota"
/// 3. `error.message` 包含 "quota" 或 "insufficient"
pub(super) fn is_key_quota_error(body: &str) -> bool {
    let v = match serde_json::from_str::<Value>(body) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let err = &v["error"];
    if err.is_null() {
        return false;
    }

    // 检查 error.code
    if let Some(code) = err["code"].as_str() {
        let lower = code.to_lowercase();
        if lower.contains("quota") || lower.contains("insufficient") {
            return true;
        }
    }

    // 检查 error.type
    if let Some(ty) = err["type"].as_str() {
        let lower = ty.to_lowercase();
        if lower.contains("quota") || lower.contains("insufficient") {
            return true;
        }
    }

    // 检查 error.message
    if let Some(msg) = err["message"].as_str() {
        let lower = msg.to_lowercase();
        if lower.contains("quota") || lower.contains("insufficient") {
            return true;
        }
    }

    false
}

/// 从上游错误 body 中提取可读错误信息。
pub(super) fn extract_error_message(body: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_key_quota_error_quota_exceeded_code() {
        // OpenAI 风格：error.code = "quotaExceeded"
        let body = r#"{"error":{"code":"quotaExceeded","message":"Insufficient quota"}}"#;
        assert!(is_key_quota_error(body));
    }

    #[test]
    fn test_is_key_quota_error_insufficient_quota_type() {
        // error.type = "insufficient_quota"
        let body = r#"{"error":{"type":"insufficient_quota","message":"You exceeded your current quota"}}"#;
        assert!(is_key_quota_error(body));
    }

    #[test]
    fn test_is_key_quota_error_quota_in_message() {
        // error.message 包含 "quota"
        let body = r#"{"error":{"message":"Rate limit reached for requests, quota exceeded"}}"#;
        assert!(is_key_quota_error(body));
    }

    #[test]
    fn test_is_key_quota_error_insufficient_in_message() {
        // error.message 包含 "insufficient"
        let body = r#"{"error":{"message":"Insufficient funds"}}"#;
        assert!(is_key_quota_error(body));
    }

    #[test]
    fn test_is_key_quota_error_not_quota_400() {
        // 普通 400 错误（如参数错误）不应被误判
        let body = r#"{"error":{"type":"invalid_request_error","message":"model is required"}}"#;
        assert!(!is_key_quota_error(body));
    }

    #[test]
    fn test_is_key_quota_error_empty_body() {
        assert!(!is_key_quota_error(""));
    }

    #[test]
    fn test_is_key_quota_error_non_json() {
        assert!(!is_key_quota_error("<html>502 Bad Gateway</html>"));
    }

    #[test]
    fn test_is_key_quota_error_no_error_field() {
        let body = r#"{"message":"some other error"}"#;
        assert!(!is_key_quota_error(body));
    }

    #[test]
    fn test_is_key_quota_error_case_insensitive() {
        // 大小写不敏感
        let body = r#"{"error":{"code":"QUOTA_EXCEEDED","message":"INSUFFICIENT QUOTA"}}"#;
        assert!(is_key_quota_error(body));
    }
}
