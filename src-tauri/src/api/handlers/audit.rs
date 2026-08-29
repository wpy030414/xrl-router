//! 对话审查 handler：列表查询 + 详情 + 删除。

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::Json;

use crate::gateway::server::AppState;

#[derive(serde::Deserialize)]
pub(crate) struct AuditQuery {
    page: Option<i64>,
    page_size: Option<i64>,
    service_key_id: Option<String>,
}

/// GET /api/audit — 对话列表（按最近活动排序，分页）
pub(crate) async fn get_conversations(
    State(state): State<Arc<AppState>>,
    Query(params): Query<AuditQuery>,
) -> impl IntoResponse {
    let page = params.page.unwrap_or(1).max(1);
    let page_size = params.page_size.unwrap_or(20).clamp(1, 100);
    let (total, data) = state
        .database
        .get_conversations_page(page, page_size, params.service_key_id.as_deref())
        .unwrap_or_else(|_| (0, Vec::new()));
    Json(serde_json::json!({
        "total": total,
        "page": page,
        "page_size": page_size,
        "data": data,
    }))
}

/// GET /api/audit/:id — 单条对话详情（完整消息历史）
pub(crate) async fn get_conversation(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> impl IntoResponse {
    match state.database.get_conversation(id) {
        Ok(Some(conv)) => Json(conv),
        _ => Json(serde_json::json!({"error": "not found"})),
    }
}

/// DELETE /api/audit/:id — 删除单条对话
pub(crate) async fn delete_conversation(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> impl IntoResponse {
    let _ = state.database.delete_conversation(id);
    Json(serde_json::json!({"status": "ok"}))
}
