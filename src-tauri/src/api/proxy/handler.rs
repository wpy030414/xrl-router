//! 三个代理入口 handler：Anthropic / OpenAI Chat / OpenAI Responses 流式代理 + 模型列表。
//!
//! handler 是薄入口层：提取 API key → authenticate_and_stream() → 委托给
//! `stream::proxy_stream()` 完成路由解析、上游连接、密钥轮换、流式转发。
//!
//! 认证 / 路由 / 密钥轮换 / WebSearch 劫持分别下沉到
//! `auth` / `route` / `key_rotation` / `websearch`。
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

use super::auth::{verify_service_key, ServiceKeyInfo};
use super::ir;
use super::ir::types::IrRequest;
use super::quota::check_quota;
use super::stream::{ClientFormat, StreamContext};

/// POST /v1/messages - Anthropic Messages API proxy (streaming only).
pub async fn proxy_anthropic_messages(
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

    let ir_request = ir::from_anthropic::anthropic_req_to_ir(&body);
    authenticate_and_stream(
        state,
        api_key,
        ir_request,
        trace_id,
        start_time,
        ClientFormat::Anthropic,
        "/v1/messages",
    )
    .await
}

/// POST /v1/chat/completions - OpenAI Chat API proxy (streaming only).
pub async fn proxy_openai_chat(
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

    let ir_request = ir::from_chat::chat_req_to_ir(&body);
    authenticate_and_stream(
        state,
        api_key,
        ir_request,
        trace_id,
        start_time,
        ClientFormat::Chat,
        "/v1/chat/completions",
    )
    .await
}

/// POST /v1/responses - OpenAI Responses API proxy (streaming only).
pub async fn proxy_openai_responses(
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

    // ── 强制 stream=true ────────────────────────────────────────
    ir_request.stream = true;

    // ── 输入 token 估算（translation 路径 message_start 占位用） ─
    let est_input = ir::usage::estimate_input_tokens(&ir_request);
    // 大上下文（缓存恢复）首字节延迟高，按输入规模放宽响应头超时
    let header_timeout_secs = super::header_timeout_for(est_input);

    // ── 委托流式引擎 ────────────────────────────────────────────
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
            "SELECT m.model_id, m.display_name, m.tier, p.name, m.context_window
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
            Ok(json!({
                "id": row.get::<_, String>(1)?,
                "object": "model",
                "created": 1699000000,
                "owned_by": row.get::<_, String>(3)?,
                "display_name": row.get::<_, String>(1)?,
                "tier": row.get::<_, String>(2)?,
                "context_window": row.get::<_, i64>(4)?,
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
