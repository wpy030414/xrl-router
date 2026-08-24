//! 组合别名（combo）管理 handler —— 多个模型 display_name 按顺序捆绑成新别名。
//!
//! 校验规则：
//! - name：trim 非空；不撞任何 models.display_name（组合名=模型名会歧义）；撞另一组合名 → 400。
//! - members：trim、非空、保序去重；全部必须是现存 models.display_name（未知成员 400 并列出）。

use std::sync::Arc;

use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::Utc;
use serde::Deserialize;

use crate::gateway::server::AppState;
use crate::types::Combo;

/// 把 `(Combo, members)` 组装成 API 响应 JSON。
fn combo_json(combo: &Combo, members: &[String]) -> serde_json::Value {
    serde_json::json!({
        "id": combo.id,
        "name": combo.name,
        "enabled": combo.enabled,
        "members": members,
        "created_at": combo.created_at,
        "updated_at": combo.updated_at,
    })
}

/// 名字校验：trim 非空 + 不撞任何 models.display_name。
fn validate_combo_name(state: &AppState, name: &str) -> Result<String, (StatusCode, Json<serde_json::Value>)> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Combo name must not be empty"})),
        ));
    }
    // display_name 在 models 里非唯一，判「至少一行存在」
    match state.database.model_display_name_exists(&name) {
        Ok(false) => {}
        Ok(true) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("Combo name '{}' conflicts with an existing model alias", name)})),
            ))
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            ))
        }
    }
    Ok(name)
}

/// 成员校验：trim、去空、保序去重、全部必须是现存 models.display_name。
fn validate_members(state: &AppState, members: &[String]) -> Result<Vec<String>, (StatusCode, Json<serde_json::Value>)> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for m in members {
        let m = m.trim().to_string();
        if m.is_empty() || !seen.insert(m.clone()) {
            continue;
        }
        out.push(m);
    }
    if out.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Combo must have at least one member"})),
        ));
    }
    let mut unknown: Vec<String> = Vec::new();
    for m in &out {
        match state.database.model_display_name_exists(m) {
            Ok(true) => {}
            Ok(false) => unknown.push(m.clone()),
            Err(e) => {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": e.to_string()})),
                ))
            }
        }
    }
    if !unknown.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("Members not found as model aliases: {}", unknown.join(", "))})),
        ));
    }
    Ok(out)
}

/// 映射 save_combo 的错误：UNIQUE(name) 违例（TOCTOU 兜底）→ 400，其余 → 500。
fn map_save_error(e: anyhow::Error, name: &str) -> (StatusCode, Json<serde_json::Value>) {
    let is_unique = e
        .downcast_ref::<rusqlite::Error>()
        .and_then(|e| e.sqlite_error_code())
        == Some(rusqlite::ErrorCode::ConstraintViolation);
    if is_unique {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("Combo name '{}' already exists", name)})),
        )
    } else {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
    }
}

pub(crate) async fn list_combos(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    match state.database.list_combos() {
        Ok(combos) => Ok(Json(
            combos
                .iter()
                .map(|(c, m)| combo_json(c, m))
                .collect::<Vec<_>>(),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )),
    }
}

pub(crate) async fn get_combo(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    match state.database.get_combo(&id) {
        Ok(Some((combo, members))) => Ok(Json(combo_json(&combo, &members))),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Combo not found"})),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )),
    }
}

#[derive(Deserialize)]
pub(crate) struct CreateComboRequest {
    name: String,
    enabled: Option<bool>,
    #[serde(default)]
    members: Vec<String>,
}

pub(crate) async fn create_combo(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateComboRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let name = validate_combo_name(&state, &req.name)?;
    let members = validate_members(&state, &req.members)?;

    let combo = Combo {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.clone(),
        enabled: req.enabled.unwrap_or(true),
        created_at: Utc::now().timestamp(),
        updated_at: Utc::now().timestamp(),
    };

    if let Err(e) = state.database.save_combo(&combo, &members) {
        return Err(map_save_error(e, &name));
    }

    Ok((StatusCode::CREATED, Json(combo_json(&combo, &members))))
}

#[derive(Deserialize)]
pub(crate) struct UpdateComboRequest {
    name: Option<String>,
    enabled: Option<bool>,
    members: Option<Vec<String>>,
}

pub(crate) async fn update_combo(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateComboRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let (mut combo, members) = match state.database.get_combo(&id) {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Combo not found"})),
            ))
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            ))
        }
    };
    let mut new_members = members;

    if let Some(name) = req.name {
        let validated = validate_combo_name(&state, &name)?;
        combo.name = validated;
    }
    if let Some(enabled) = req.enabled {
        combo.enabled = enabled;
    }
    if let Some(m) = req.members {
        new_members = validate_members(&state, &m)?;
    }
    combo.updated_at = Utc::now().timestamp();

    if let Err(e) = state.database.save_combo(&combo, &new_members) {
        return Err(map_save_error(e, &combo.name));
    }

    Ok(Json(combo_json(&combo, &new_members)))
}

pub(crate) async fn delete_combo(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    if let Err(e) = state.database.delete_combo(&id) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ));
    }

    Ok(Json(serde_json::json!({"status": "deleted"})))
}
