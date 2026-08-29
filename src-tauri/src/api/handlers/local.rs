//! 本地模型（私有化）管理 API：模型 CRUD / 启动停止 / 后端检测 / HF 搜索。
//!
//! 全部挂在 `/api/local/*` 下，受 `admin_ip_guard` 保护（仅 loopback）。
//! 契约见 docs/specs/spec-local-models.md。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;

use crate::gateway::server::AppState;
use crate::local::{CreateLocalModelReq, EditLocalModelReq};

#[derive(serde::Deserialize)]
pub struct HfSearchQuery {
    pub q: String,
    #[serde(default = "default_filter")]
    pub filter: String,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_filter() -> String {
    "gguf".to_string()
}

fn default_limit() -> u32 {
    30
}

#[derive(serde::Deserialize)]
pub struct StartStopParams {
    /// 是否删除权重文件（仅 delete 使用）。
    #[serde(default)]
    pub remove_files: bool,
}

/// GET /api/local/models —— 列表
pub(crate) async fn list_local_models(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<crate::types::LocalModel>>, (StatusCode, Json<serde_json::Value>)> {
    state
        .local
        .list()
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))))
}

/// POST /api/local/models —— 创建（校验撞名 + 落库 + 后台下载）
pub(crate) async fn create_local_model(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateLocalModelReq>,
) -> Result<Json<crate::types::LocalModel>, (StatusCode, Json<serde_json::Value>)> {
    match state.local.create(req).await {
        Ok(m) => Ok(Json(m)),
        Err(e) => Err((StatusCode::CONFLICT, Json(serde_json::json!({"error": e})))),
    }
}

/// DELETE /api/local/models/:id —— 删除（停引擎 + 删 provider + 可选删文件）
pub(crate) async fn delete_local_model(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<StartStopParams>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    state
        .local
        .delete(&id, params.remove_files)
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e}))))
}

/// POST /api/local/models/:id/edit —— 编辑参数（ctx_size / n_gpu_layers / backend / autostart / thinking）
pub(crate) async fn edit_local_model(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<EditLocalModelReq>,
) -> Result<Json<crate::types::LocalModel>, (StatusCode, Json<serde_json::Value>)> {
    match state.local.edit(&id, req) {
        Ok(m) => Ok(Json(m)),
        Err(e) => Err((StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": e.to_string()})))),
    }
}

/// POST /api/local/models/:id/start —— 启动引擎并注册 provider
pub(crate) async fn start_local_model(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    state
        .local
        .start(&id)
        .await
        .map(|_| Json(serde_json::json!({"ok": true})))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e}))))
}

/// POST /api/local/models/:id/stop —— 停止引擎并下线 provider
pub(crate) async fn stop_local_model(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    state
        .local
        .stop(&id)
        .await
        .map(|_| Json(serde_json::json!({"ok": true})))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e}))))
}

/// POST /api/local/models/:id/cancel —— 取消下载
pub(crate) async fn cancel_local_download(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    state
        .local
        .cancel(&id)
        .map(|_| Json(serde_json::json!({"ok": true})))
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": e}))))
}

/// GET /api/local/backends —— 后端检测结果（平台/候选/可用性）
pub(crate) async fn get_local_backends(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let d = state.local.backends();
    Ok(Json(serde_json::json!(d)))
}

#[derive(serde::Deserialize)]
pub struct MirrorReq {
    pub mirror: bool,
}

/// POST /api/local/hf/mirror —— 切换 HF 镜像开关
pub(crate) async fn set_hf_mirror(
    State(state): State<Arc<AppState>>,
    Json(req): Json<MirrorReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    state
        .local
        .set_hf_mirror(req.mirror)
        .map(|_| Json(serde_json::json!({"ok": true, "mirror": req.mirror})))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e}))))
}

/// GET /api/local/hf/search —— HF 仓库搜索
pub(crate) async fn hf_search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HfSearchQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    match state.local.hf_search(&params.q, &params.filter, params.limit).await {
        Ok(repos) => Ok(Json(serde_json::json!({
            "results": repos,
            "mirror": state.local.hf_mirror_enabled(),
        }))),
        Err(e) => Err((StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": format!("HF 搜索失败: {}", e)})))),
    }
}

/// GET /api/local/hf/repo/:owner/:repo —— 仓库详情（文件列表 + 大小）
pub(crate) async fn hf_repo_detail(
    State(state): State<Arc<AppState>>,
    Path((owner, repo)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let full = format!("{}/{}", owner, repo);
    match state.local.hf_repo(&full).await {
        Ok(detail) => Ok(Json(serde_json::json!(detail))),
        Err(e) => Err((StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": format!("HF 仓库信息失败: {}", e)})))),
    }
}
