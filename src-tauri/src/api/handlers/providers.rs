//! Provider 管理 handler（CRUD + KeyPool/registry 内存同步）。

use std::sync::Arc;

use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::Utc;
use serde::Deserialize;

use crate::gateway::server::AppState;
use crate::types::{Provider, ProviderKind};

pub(crate) async fn list_providers(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // 云智能列表剔除本地引擎注册的 local-* provider（私有智能独立管理页维护）
    // 注意：/api/models 和 /api/keys 不过滤，因为组合成员选择器和密钥白名单弹窗需要看到私有模型
    let providers: Vec<Provider> = state
        .providers
        .list_all()
        .into_iter()
        .filter(|p| !p.id.starts_with("local-"))
        .collect();
    Json(providers)
}

#[derive(Deserialize)]
pub(crate) struct CreateProviderRequest {
    name: String,
    kind: String,
    base_url: String,
    api_path: Option<String>,
    config: Option<serde_json::Value>,
}

pub(crate) async fn create_provider(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateProviderRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    // 新供应商排在队尾：取当前最大 sort_order + 1，历史数据（V13 前全为 0）
    // 不会挤到已拖拽排序的供应商前面。
    let sort_order = match state.database.next_sort_order() {
        Ok(v) => v,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            ))
        }
    };
    let provider = Provider {
        id: uuid::Uuid::new_v4().to_string(),
        name: req.name,
        kind: ProviderKind::from_str(&req.kind),
        base_url: req.base_url,
        api_path: req.api_path.unwrap_or_else(|| "/v1/chat/completions".to_string()),
        config: req.config.unwrap_or_else(|| serde_json::json!({})),
        enabled: true,
        created_at: Utc::now().timestamp(),
        updated_at: Utc::now().timestamp(),
        sort_order,
    };

    state.providers.insert(provider.clone());

    if let Err(e) = state.database.save_provider(&provider) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ));
    }

    Ok((StatusCode::CREATED, Json(provider)))
}

pub(crate) async fn get_provider(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    match state.providers.get(&id) {
        Some(provider) => Ok(Json(provider)),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Provider not found"})),
        )),
    }
}

#[derive(Deserialize)]
pub(crate) struct UpdateProviderRequest {
    name: Option<String>,
    kind: Option<String>,
    base_url: Option<String>,
    api_path: Option<String>,
    config: Option<serde_json::Value>,
    enabled: Option<bool>,
}

pub(crate) async fn update_provider(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateProviderRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let provider = match state.providers.get(&id) {
        Some(p) => p,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Provider not found"})),
            ))
        }
    };

    let mut updated = provider.clone();
    if let Some(name) = req.name {
        updated.name = name;
    }
    if let Some(kind) = req.kind {
        updated.kind = ProviderKind::from_str(&kind);
    }
    if let Some(base_url) = req.base_url {
        updated.base_url = base_url;
    }
    if let Some(api_path) = req.api_path {
        updated.api_path = api_path;
    }
    if let Some(config) = req.config {
        updated.config = config;
    }
    if let Some(enabled) = req.enabled {
        updated.enabled = enabled;
    }
    updated.updated_at = Utc::now().timestamp();

    state.providers.insert(updated.clone());

    if let Err(e) = state.database.save_provider(&updated) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ));
    }

    Ok(Json(updated))
}

pub(crate) async fn delete_provider(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    if !state.providers.contains(&id) {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Provider not found"})),
        ));
    }

    state.providers.remove(&id);
    // 同步 KeyPool 内存：移除该 provider 的密钥 + 轮询指针。
    state.keys.remove_provider(&id);

    if let Err(e) = state.database.delete_provider(&id) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ));
    }

    Ok(Json(serde_json::json!({"status": "deleted"})))
}

#[derive(Deserialize)]
pub(crate) struct ReorderProvidersRequest {
    ids: Vec<String>,
}

/// PUT /api/providers/reorder — 拖拽后的全量顺序（靠前的供应商优先级更高）。
pub(crate) async fn reorder_providers(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ReorderProvidersRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    if req.ids.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "ids must not be empty"})),
        ));
    }
    // 校验：传入 id 必须都存在，且无重复，避免把不存在的行写进 sort_order。
    let mut seen = std::collections::HashSet::new();
    for id in &req.ids {
        if !seen.insert(id) {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("duplicate id: {}", id)})),
            ));
        }
        if !state.providers.contains(id) {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("unknown provider id: {}", id)})),
            ));
        }
    }

    if let Err(e) = state.providers.reorder(&req.ids) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ));
    }
    Ok(Json(serde_json::json!({"status": "ok"})))
}
