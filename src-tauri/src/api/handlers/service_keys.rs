//! Service Key 管理 handler（argon2 哈希，哈希/校验函数见 `crypto`）。

use std::sync::Arc;

use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use crate::crypto::hash_service_key;
use crate::gateway::server::AppState;

#[derive(Deserialize)]
pub(crate) struct CreateServiceKeyRequest {
    name: String,
    allowed_models: Option<Vec<String>>,
}

#[derive(Serialize)]
pub(crate) struct CreateServiceKeyResponse {
    id: String,
    name: String,
    key: String,  // Only returned once at creation time
    key_masked: String,
}

/// Create a new service key with argon2 hashing
pub(crate) async fn create_service_key(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateServiceKeyRequest>,
) -> Result<(StatusCode, Json<CreateServiceKeyResponse>), (StatusCode, Json<serde_json::Value>)> {
    // Generate random key
    let raw_key = format!("xrl-{}", uuid::Uuid::new_v4().to_string().replace("-", ""));

    // Compute argon2 hash
    let key_hash = match hash_service_key(&raw_key) {
        Ok(h) => h,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            ))
        }
    };

    // Create masked version: **** + last 4 chars
    let key_masked = if raw_key.len() >= 4 {
        format!("****{}", &raw_key[raw_key.len() - 4..])
    } else {
        "****".to_string()
    };

    let id = uuid::Uuid::new_v4().to_string();

    let allowed_json = req
        .allowed_models
        .as_ref()
        .map(|m| serde_json::to_string(m).unwrap_or_else(|_| "[]".to_string()));

    // Save to database
    if let Err(e) = state
        .database
        .save_service_key(&id, &req.name, &key_hash, &key_masked, allowed_json.as_deref())
    {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ));
    }

    // Return the raw key (only time it's visible)
    Ok((
        StatusCode::CREATED,
        Json(CreateServiceKeyResponse {
            id,
            name: req.name,
            key: raw_key,
            key_masked,
        }),
    ))
}

/// List all service keys (masked)
pub(crate) async fn list_service_keys(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, Json<serde_json::Value>)> {
    match state.database.list_service_keys() {
        Ok(keys) => Ok(Json(keys)),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )),
    }
}

/// Delete a service key
pub(crate) async fn delete_service_key(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    if let Err(e) = state.database.delete_service_key(&id) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ));
    }

    Ok(Json(serde_json::json!({"status": "deleted"})))
}

#[derive(Deserialize)]
pub(crate) struct UpdateServiceKeyRequest {
    name: Option<String>,
    allowed_models: Option<Vec<String>>,
    /// 5h 滚动窗口 token 上限（0 = 不设限）。
    quota_5h: Option<i64>,
    /// 7d 滚动窗口 token 上限（0 = 不设限）。
    quota_7d: Option<i64>,
}

/// Update a service key (name / allowed_models / token quotas)
pub(crate) async fn update_service_key(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateServiceKeyRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    // 负值视为未设置，避免非法配置进入 DB。
    let q5 = req.quota_5h.filter(|v| *v >= 0);
    let q7 = req.quota_7d.filter(|v| *v >= 0);
    let allowed_json = req
        .allowed_models
        .as_ref()
        .map(|m| serde_json::to_string(m).unwrap_or_else(|_| "[]".to_string()));
    if let Err(e) = state
        .database
        .update_service_key(&id, req.name.as_deref(), allowed_json.as_deref(), q5, q7)
    {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ));
    }
    Ok(Json(serde_json::json!({"status": "ok"})))
}
