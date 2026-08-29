//! 三个代理入口 handler：Anthropic / OpenAI Chat / OpenAI Responses 流式代理 + 模型列表。
//!
//! handler 是薄入口层：提取 API key → authenticate_and_stream() → 委托给
//! `stream::proxy_stream()` 完成路由解析、上游连接、密钥轮换、流式转发。
//!
//! 认证 / 路由 / 密钥轮换分别下沉到 `auth` / `route` / `key_rotation`。
//! 搜索工具剔除（MCP 模式）在 `stream.rs`；本地搜索/抓取能力由
//! `src-tauri/src/mcp/`（`/mcp` 端点）提供。
//! `ir` / `stream` / `forward` 为既有子模块。

use std::sync::Arc;

use axum::extract::{Json, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json as AxumJson;
use serde_json::{json, Value};
use tracing::{info, warn};
use uuid::Uuid;

use crate::gateway::server::AppState;

use super::auth::verify_service_key;
use super::ir;
use super::ir::types::IrRequest;
use super::quota::check_quota;
use super::stream::{ClientFormat, StreamContext};

/// POST /v1/messages - Anthropic Messages API proxy (streaming only).
pub async fn proxy_messages(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, (StatusCode, HeaderMap, AxumJson<Value>)> {
    let trace_id = Uuid::new_v4().to_string();
    let start_time = std::time::Instant::now();

    // Anthropic 协议：x-api-key 优先，Authorization Bearer 备选
    let api_key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            headers
                .get("Authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
        })
        .unwrap_or("");

    let ir_request = ir::from_messages::messages_req_to_ir(&body);
    authenticate_and_stream(
        state,
        api_key,
        ir_request,
        trace_id,
        start_time,
        ClientFormat::Messages,
        "/v1/messages",
    )
    .await
}

/// POST /v1/chat/completions - OpenAI Chat Completions API proxy (streaming only).
pub async fn proxy_chat_completions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, (StatusCode, HeaderMap, AxumJson<Value>)> {
    let trace_id = Uuid::new_v4().to_string();
    let start_time = std::time::Instant::now();

    // OpenAI 协议：Authorization Bearer 优先，x-api-key 备选
    let api_key = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| headers.get("x-api-key").and_then(|v| v.to_str().ok()))
        .unwrap_or("");

    let ir_request = ir::from_chat_completions::chat_completions_req_to_ir(&body);
    authenticate_and_stream(
        state,
        api_key,
        ir_request,
        trace_id,
        start_time,
        ClientFormat::ChatCompletions,
        "/v1/chat/completions",
    )
    .await
}

/// POST /v1/responses - OpenAI Responses API proxy (streaming only).
pub async fn proxy_responses(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, (StatusCode, HeaderMap, AxumJson<Value>)> {
    let trace_id = Uuid::new_v4().to_string();
    let start_time = std::time::Instant::now();

    // OpenAI 协议：Authorization Bearer 优先，x-api-key 备选
    let api_key = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| headers.get("x-api-key").and_then(|v| v.to_str().ok()))
        .unwrap_or("");

    let ir_request = ir::from_responses::responses_req_to_ir(&body);
    authenticate_and_stream(
        state,
        api_key,
        ir_request,
        trace_id,
        start_time,
        ClientFormat::Responses,
        "/v1/responses",
    )
    .await
}

/// 认证 + 请求体准备 + 委托 stream::proxy_stream()。
///
/// 三个 handler 共享此流程：
/// 1. 验证 service key
/// 2. 检查配额
/// 3. 检查模型白名单
/// 4. 委托流式引擎
async fn authenticate_and_stream(
    state: Arc<AppState>,
    api_key: &str,
    mut ir_request: IrRequest,
    trace_id: String,
    start_time: std::time::Instant,
    client_format: ClientFormat,
    endpoint: &'static str,
) -> Result<Response, (StatusCode, HeaderMap, AxumJson<Value>)> {
    let model_name = ir_request.model.clone();

    info!(
        trace_id = %trace_id,
        model = %model_name,
        endpoint = endpoint,
        "Proxy request received"
    );

    // ── 认证 ────────────────────────────────────────────────────
    let service_key = match verify_service_key(&state, api_key).await {
        Some(info) => {
            info!(trace_id = %trace_id, service_key_id = %info.id, "Service key verified");
            info
        }
        None => {
            warn!(trace_id = %trace_id, "Authentication failed: invalid API key");
            return Err((
                StatusCode::UNAUTHORIZED,
                HeaderMap::new(),
                AxumJson(json!({"error": {"type": "authentication_error", "message": "Invalid API key"}})),
            ));
        }
    };

    // ── 配额检查 ────────────────────────────────────────────────
    if let Err((code, headers, body)) = check_quota(&state, &service_key).await {
        warn!(trace_id = %trace_id, service_key_id = %service_key.id, "Quota exceeded for service key");
        return Err((code, headers, body));
    }

    // ── 模型白名单 ──────────────────────────────────────────────
    if !service_key.allowed_models.is_empty()
        && !service_key.allowed_models.iter().any(|m| m == &model_name)
    {
        warn!(trace_id = %trace_id, model = %model_name, "Model not allowed for this service key");
        return Err((
            StatusCode::FORBIDDEN,
            HeaderMap::new(),
            AxumJson(json!({"error": {"type": "forbidden", "message": "Model not allowed for this service key"}})),
        ));
    }

    // ── 保存客户端原始 stream 偏好 ──────────────────────────────────
    // 上游永远走流式（简化实现），但客户端非流式时需要收集所有事件后返回 JSON
    let client_wants_stream = ir_request.stream;

    // ── 强制 stream=true（上游始终走流式） ────────────────────────────
    ir_request.stream = true;

    // ── 会话注入：非空时前置到系统提示词 ─────────────────────────────
    {
        let inject = state.session_inject.read().unwrap().clone();
        if !inject.is_empty() {
            match &mut ir_request.system {
                Some(ir::types::IrSystemContent::Text(ref mut t)) => {
                    *t = format!("{}\n{}", t, inject);
                }
                Some(ir::types::IrSystemContent::Blocks(ref mut blocks)) => {
                    blocks.push(ir::types::IrSystemBlock {
                        text: inject,
                        cache_control: None,
                    });
                }
                None => {
                    ir_request.system = Some(ir::types::IrSystemContent::Text(inject));
                }
            }
        }
    }

    // ── 输入 token 估算（translation 路径 message_start 占位用） ─
    let est_input = ir::usage::estimate_input_tokens(&ir_request);
    // 大上下文（缓存恢复）首字节延迟高，按输入规模放宽响应头超时
    let header_timeout_secs = super::header_timeout_for(est_input);

    // ── 对话审查上下文（仅 audit_enabled 开启时构建） ─────────────
    // 审查在流式转发完成后执行（forward.rs / non_stream.rs），
    // 这样每条记录都包含完整的请求消息 + 助手回复。
    let audit = if state.audit_enabled.load(std::sync::atomic::Ordering::Relaxed) {
        Some(super::stream::AuditCapture {
            db: state.database.clone(),
            sk_id: service_key.id.clone(),
            sk_name: service_key.name.clone(),
            request_messages: ir_request.messages.clone(),
        })
    } else {
        None
    };

    // ── 委托流式引擎 ────────────────────────────────────────────────
    super::stream::proxy_stream(
        state,
        StreamContext {
            trace_id,
            start_time,
            service_key,
            model_name,
            endpoint,
            ir_request,
            client_format,
            est_input,
            header_timeout_secs,
            client_wants_stream,
            audit,
        },
    )
    .await
}

/// GET /v1/models - List available models.
pub async fn proxy_list_models(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<AxumJson<Value>, (StatusCode, HeaderMap, AxumJson<Value>)> {
    let api_key = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| headers.get("x-api-key").and_then(|v| v.to_str().ok()))
        .unwrap_or("");

    let service_key = match verify_service_key(&state, api_key).await {
        Some(info) => info,
        None => {
            return Err((
                StatusCode::UNAUTHORIZED,
                HeaderMap::new(),
                AxumJson(json!({"error": {"type": "authentication_error", "message": "Invalid API key"}})),
            ));
        }
    };
    // 列表端点同样受配额约束：超限时模型列表不可用。
    if let Err((code, headers, body)) = check_quota(&state, &service_key).await {
        return Err((code, headers, body));
    }

    let conn = state.database.conn();

    let mut stmt = conn
        .prepare(
            "SELECT m.model_id, m.display_name, m.tier, p.name, m.context_window, m.max_output_tokens, m.capabilities
             FROM models m
             JOIN providers p ON m.provider_id = p.id
             WHERE m.enabled = 1 AND p.enabled = 1
             ORDER BY m.tier, m.display_name",
        )
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                HeaderMap::new(),
                AxumJson(json!({"error": {"type": "api_error", "message": e.to_string()}})),
            )
        })?;

    let models: Vec<Value> = stmt
        .query_map([], |row| {
            // capabilities 存的是 JSON 数组字符串（如 '["text","tools"]'），
            // 解析成真实数组；解析失败时回退到空数组，避免破坏模型列表。
            let caps: String = row.get(6)?;
            let caps: Vec<String> = serde_json::from_str(&caps).unwrap_or_default();
            Ok(json!({
                "id": row.get::<_, String>(1)?,
                "object": "model",
                "created": 1699000000,
                "owned_by": row.get::<_, String>(3)?,
                "display_name": row.get::<_, String>(1)?,
                "tier": row.get::<_, String>(2)?,
                "context_window": row.get::<_, i64>(4)?,
                "max_output_tokens": row.get::<_, i64>(5)?,
                "capabilities": caps,
            }))
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                HeaderMap::new(),
                AxumJson(json!({"error": {"type": "api_error", "message": e.to_string()}})),
            )
        })?
        .filter_map(|r| r.ok())
        .collect();

    // 组合别名（enabled）追加为可用模型条目，客户端可发现并直接使用。
    // 不校验运行时可解析性（只列 enabled 组合），解析失败时调用方在请求时收到 400。
    // 注意：必须复用上方同一个 conn 守卫——std::sync::Mutex 不可重入，
    // 函数末尾才 drop，这里再 conn() 会自死锁（tokio 单线程 runtime 直接冻结）。
    let combo_rows: Vec<Value> = {
        let mut stmt = conn
            .prepare("SELECT id, name, created_at FROM combos WHERE enabled = 1 ORDER BY created_at ASC")
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    HeaderMap::new(),
                    AxumJson(json!({"error": {"type": "api_error", "message": e.to_string()}})),
                )
            })?;
        let rows = stmt
            .query_map([], |row| {
                Ok(json!({
                    "id": row.get::<_, String>(1)?,
                    "object": "model",
                    "created": row.get::<_, i64>(2)?,
                    "owned_by": "combo",
                    "display_name": row.get::<_, String>(1)?,
                    "tier": "combo",
                    "context_window": 0,
                    "max_output_tokens": 0,
                    "capabilities": [],
                }))
            })
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    HeaderMap::new(),
                    AxumJson(json!({"error": {"type": "api_error", "message": e.to_string()}})),
                )
            })?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };
    let mut models = models;
    models.extend(combo_rows);

    // Apply allowed_models whitelist (empty = return all)
    let data: Vec<Value> = if service_key.allowed_models.is_empty() {
        models
    } else {
        models
            .into_iter()
            .filter(|m| {
                m["display_name"]
                    .as_str()
                    .map(|dn| service_key.allowed_models.iter().any(|a| a == dn))
                    .unwrap_or(false)
            })
            .collect()
    };

    Ok(AxumJson(json!({
        "object": "list",
        "data": data,
    })))
}
