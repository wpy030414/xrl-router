//! API Key 管理 handler。`ProviderFilter` 同时被 models handler 复用。

use std::sync::Arc;

use axum::extract::{Json, Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::Utc;
use serde::Deserialize;

use crate::gateway::server::AppState;
use crate::types::ApiKey;

#[derive(Deserialize)]
pub(crate) struct ProviderFilter {
    pub(crate) provider_id: Option<String>,
}

pub(crate) async fn list_keys(
    State(state): State<Arc<AppState>>,
    Query(filter): Query<ProviderFilter>,
) -> impl IntoResponse {
    let keys = state.database.list_all_keys().unwrap_or_default();

    let filtered = match &filter.provider_id {
        Some(id) => keys.into_iter().filter(|k| k.provider_id == *id).collect::<Vec<_>>(),
        None => keys,
    };

    let keys_with_plain: Vec<serde_json::Value> = filtered
        .into_iter()
        .map(|k| {
            let plain = crate::crypto::decrypt(&k.key_hash, &state.master_key).ok();
            let mut val = serde_json::to_value(&k).unwrap_or_default();
            if let Some(obj) = val.as_object_mut() {
                // 可用性走内存：用 pool 实时状态覆盖 DB status 残留。
                // 池内查不到（如解密失败被跳过的 key）显示 unknown 而非
                // 回退 DB 旧值——DB status 自 V5 起不再被读写，回退展示
                // 会误报 green（Bug 1 的误导性来源之一）。
                let live = state
                    .keys
                    .get_key_status(&k.id)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                obj.insert("status".to_string(), serde_json::json!(live));
                obj.insert("key_plain".to_string(), serde_json::json!(plain));
            }
            val
        })
        .collect();
    Json(keys_with_plain)
}

#[derive(Deserialize)]
pub(crate) struct CreateKeyRequest {
    provider_id: String,
    name: String,
    key: String,
}

pub(crate) async fn create_key(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateKeyRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    if !state.providers.contains(&req.provider_id) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Provider not found"})),
        ));
    }

    let masked = if req.key.len() > 8 {
        format!("{}...{}", &req.key[..4], &req.key[req.key.len() - 4..])
    } else {
        "***".to_string()
    };

    // Encrypt the raw key at rest (AES-256-GCM); key_hash stores ciphertext.
    let key_cipher = match crate::crypto::encrypt(&req.key, &state.master_key) {
        Ok(c) => c,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            ))
        }
    };

    let api_key = ApiKey {
        id: uuid::Uuid::new_v4().to_string(),
        provider_id: req.provider_id,
        name: req.name,
        key_hash: key_cipher,
        key_masked: masked,
        key_plain: None,
        status: "green".to_string(),
        last_error: None,
        last_error_code: None,
        last_error_time: None,
        last_used_at: None,
        balance: None,
        balance_updated_at: None,
        total_requests: 0,
        total_tokens: 0,
        created_at: Utc::now().timestamp(),
        updated_at: Utc::now().timestamp(),
    };

    if let Err(e) = state.database.save_api_key(&api_key) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ));
    }

    // 热生效：同步进内存池（与 delete_key 对称；池内 key_hash 存明文，
    // 与 load_all_keys_from_db 装池口径一致）。不重启即可被代理轮询到。
    let added = state.keys.add_key(crate::keys::pool::KeyEntry {
        id: api_key.id.clone(),
        provider_id: api_key.provider_id.clone(),
        name: api_key.name.clone(),
        key_hash: req.key.clone(),
        key_masked: api_key.key_masked.clone(),
        status: crate::types::KeyStatus::Green,
        last_error_time: None,
        total_requests: 0,
        total_tokens: 0,
    });
    if !added {
        tracing::warn!(key_id = %api_key.id, "key saved to DB but pool already holds same id");
    }

    Ok((StatusCode::CREATED, Json(api_key)))
}

pub(crate) async fn get_key(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    match state.database.get_api_key(&id) {
        Ok(Some(key)) => Ok(Json(key)),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Key not found"})),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )),
    }
}

#[derive(Deserialize)]
pub(crate) struct UpdateKeyRequest {
    name: Option<String>,
    status: Option<String>,
}

pub(crate) async fn update_key(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateKeyRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let key = match state.database.get_api_key(&id) {
        Ok(Some(k)) => k,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Key not found"})),
            ))
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            ))
        }
    };

    let mut updated = key.clone();
    let mut name_changed = false;
    if let Some(name) = req.name {
        name_changed = name != updated.name;
        updated.name = name;
    }
    if let Some(status) = req.status {
        updated.status = status;
    }
    updated.updated_at = Utc::now().timestamp();

    if let Err(e) = state.database.save_api_key(&updated) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ));
    }

    // 热生效：改名同步内存池（PickedKey.name 供 usage_log 快照用）。
    // status 不同步——可用性纯内存（V5），由运行时按请求结果探测。
    if name_changed {
        state
            .keys
            .rename_key(&updated.provider_id, &updated.id, &updated.name);
    }

    Ok(Json(updated))
}

pub(crate) async fn delete_key(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    // 先查 provider_id（KeyPool 的 remove_key 需要它），再删 DB + 同步内存。
    let provider_id = match state.database.get_api_key(&id) {
        Ok(Some(k)) => k.provider_id,
        _ => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Key not found"})),
            ))
        }
    };

    if let Err(e) = state.database.delete_api_key(&id) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ));
    }

    // 同步 KeyPool 内存：移除 key 并修正轮询指针（越界自动回退 0）。
    state.keys.remove_key(&provider_id, &id);

    Ok(Json(serde_json::json!({"status": "deleted"})))
}
