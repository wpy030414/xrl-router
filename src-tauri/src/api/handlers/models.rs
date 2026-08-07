//! Model 管理 handler + 上游 /models 拉取代理（避免 CORS、服务端注入 key）。

use std::sync::Arc;

use axum::extract::{Json, Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::Utc;
use serde::Deserialize;

use crate::gateway::server::AppState;
use crate::types::Model;

use super::keys::ProviderFilter;

pub(crate) async fn list_models(
    State(state): State<Arc<AppState>>,
    Query(filter): Query<ProviderFilter>,
) -> impl IntoResponse {
    let models = state.database.list_all_models().unwrap_or_default();

    let filtered = match &filter.provider_id {
        Some(id) => models
            .into_iter()
            .filter(|m| m.provider_id == *id)
            .collect::<Vec<_>>(),
        None => models,
    };

    Json(filtered)
}

#[derive(Deserialize)]
pub(crate) struct CreateModelRequest {
    provider_id: String,
    model_id: String,
    display_name: String,
    tier: String,
    context_window: Option<i64>,
    max_output_tokens: Option<i64>,
    capabilities: Option<String>,
}

pub(crate) async fn create_model(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateModelRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    if !state.providers.contains(&req.provider_id) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Provider not found"})),
        ));
    }

    let model = Model {
        id: uuid::Uuid::new_v4().to_string(),
        provider_id: req.provider_id,
        model_id: req.model_id,
        display_name: req.display_name,
        tier: req.tier,
        context_window: req.context_window.unwrap_or(128000),
        max_output_tokens: req.max_output_tokens.unwrap_or(4096),
        capabilities: req.capabilities.unwrap_or_else(|| "[\"text\"]".to_string()),
        enabled: true,
        created_at: Utc::now().timestamp(),
        updated_at: Utc::now().timestamp(),
    };

    if let Err(e) = state.database.save_model(&model) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ));
    }

    Ok((StatusCode::CREATED, Json(model)))
}

pub(crate) async fn get_model(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    match state.database.get_model(&id) {
        Ok(Some(model)) => Ok(Json(model)),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Model not found"})),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )),
    }
}

#[derive(Deserialize)]
pub(crate) struct UpdateModelRequest {
    display_name: Option<String>,
    tier: Option<String>,
    context_window: Option<i64>,
    max_output_tokens: Option<i64>,
    enabled: Option<bool>,
}

pub(crate) async fn update_model(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateModelRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let model = match state.database.get_model(&id) {
        Ok(Some(m)) => m,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Model not found"})),
            ))
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            ))
        }
    };

    let mut updated = model.clone();
    if let Some(display_name) = req.display_name {
        updated.display_name = display_name;
    }
    if let Some(tier) = req.tier {
        updated.tier = tier;
    }
    if let Some(context_window) = req.context_window {
        updated.context_window = context_window;
    }
    if let Some(max_output_tokens) = req.max_output_tokens {
        updated.max_output_tokens = max_output_tokens;
    }
    if let Some(enabled) = req.enabled {
        updated.enabled = enabled;
    }
    updated.updated_at = Utc::now().timestamp();

    if let Err(e) = state.database.save_model(&updated) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ));
    }

    Ok(Json(updated))
}

pub(crate) async fn delete_model(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    if let Err(e) = state.database.delete_model(&id) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ));
    }

    Ok(Json(serde_json::json!({"status": "deleted"})))
}

#[derive(Deserialize)]
pub(crate) struct FetchModelsParams {
    url: String,
    #[serde(rename = "type")]
    kind: String,
    key: Option<String>,
}

/// GET /api/proxy/models?url=&type=&key= — proxy an upstream /models request,
/// avoiding browser CORS and injecting the API key server-side.
pub(crate) async fn proxy_fetch_models(Query(params): Query<FetchModelsParams>) -> axum::response::Response {
    let client = crate::http::http_client();
    let mut req = client.get(&params.url);
    if let Some(key) = params.key {
        if !key.is_empty() {
            if params.kind == "messages" {
                req = req
                    .header("x-api-key", &key)
                    .header("anthropic-version", "2023-06-01");
            } else {
                req = req.header("Authorization", format!("Bearer {}", key));
            }
        }
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                match resp.json::<serde_json::Value>().await {
                    Ok(body) => {
                        let models: Vec<String> = body
                            .get("data")
                            .and_then(|d| d.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|m| m["id"].as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default();
                        Json(serde_json::json!({"models": models})).into_response()
                    }
                    Err(e) => (
                        StatusCode::BAD_GATEWAY,
                        Json(serde_json::json!({"error": e.to_string()})),
                    )
                        .into_response(),
                }
            } else {
                (
                    StatusCode::BAD_GATEWAY,
                    Json(serde_json::json!({"error": format!("upstream returned {}", status)})),
                )
                    .into_response()
            }
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}
