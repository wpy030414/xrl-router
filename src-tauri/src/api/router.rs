//! 全部 Axum 路由的定义。
//!
//! 拆分为管理（admin，绑 127.0.0.1）与公共（public，绑 0.0.0.0）两套：
//! - admin：所有 `/api/*` 管理 CRUD + `/health` + `/ws` + `/ws/plugin` +
//!   `/api/install/local-ip`（本机 IP 查询，供 UI 拼分发链接）。
//! - public：`/v1/*` 代理（套 rate_limit）+ `/install` 静态页，供局域网设备访问。
//!
//! `build_router` 保留为 admin 的兼容入口（`lib.rs` 与冒烟测试沿用）。

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{get, post, put};
use axum::Router;

use crate::gateway::server::AppState;
use crate::middleware::rate_limit::rate_limit_middleware;

use super::handlers;
use super::proxy;

/// /v1/* 代理端点（套 rate_limit）——admin 与 public 两个 listener 都挂。
/// 拆双端口前本就在 19068 上；拆后 admin 必须保留，否则本机既有客户端
/// （CC Switch 等直连 19068）会因 /v1/* 404 而坏（模型列表/余额检查失效）。
/// 注意：返回 `Router<AppState>`（未 with_state），由调用方 `.with_state` 统一收敛。
fn proxy_routes(state: &Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/chat/completions", post(proxy::proxy_openai_chat))
        .route("/v1/messages", post(proxy::proxy_anthropic_messages))
        .route("/v1/models", get(proxy::proxy_list_models))
        .route("/v1/user/balance", get(proxy::user_balance))
        .layer(middleware::from_fn_with_state(
            state.rate_limiter.clone(),
            rate_limit_middleware,
        ))
        // axum 默认只放行 2MiB 请求体；超长会话（多轮历史 + base64 截图）
        // 会被 413 拒绝，正是「输入太大」报错的成因之一。放宽到 64MiB。
        .layer(DefaultBodyLimit::max(super::proxy::MAX_REQUEST_BODY_BYTES))
}

/// 管理 Router：/api/* + /health + /ws + /v1/* 代理（绑 127.0.0.1）。
pub fn build_admin_router(state: Arc<AppState>) -> Router {
    Router::new()
        // Health check
        .route("/health", get(handlers::health_check))
        .route("/", get(handlers::health_check))
        // WebSocket endpoint (no rate limiting)
        .route("/ws", get(handlers::ws_handler))
        // Provider management
        .route("/api/providers", get(handlers::list_providers).post(handlers::create_provider))
        .route(
            "/api/providers/:id",
            get(handlers::get_provider).put(handlers::update_provider).delete(handlers::delete_provider),
        )
        // 供应商拖拽排序（顺序即优先级，撞名时靠前的优先）
        .route("/api/providers/reorder", put(handlers::reorder_providers))
        // API Key management
        .route("/api/keys", get(handlers::list_keys).post(handlers::create_key))
        .route(
            "/api/keys/:id",
            get(handlers::get_key).put(handlers::update_key).delete(handlers::delete_key),
        )
        // Model management
        .route("/api/models", get(handlers::list_models).post(handlers::create_model))
        .route(
            "/api/models/:id",
            get(handlers::get_model).put(handlers::update_model).delete(handlers::delete_model),
        )
        // Fetch upstream models (proxy to avoid CORS and inject API key)
        .route("/api/proxy/models", get(handlers::proxy_fetch_models))
        // Statistics
        .route("/api/stats", get(handlers::get_stats))
        // 请求日志分页（时间逆序）
        .route("/api/stats/requests", get(handlers::get_stats_requests))
        // Service Key management (argon2 hashed)
        .route("/api/service-keys", get(handlers::list_service_keys).post(handlers::create_service_key))
        .route("/api/service-keys/:id", put(handlers::update_service_key).delete(handlers::delete_service_key))
        // App settings
        .route("/api/settings", get(handlers::get_settings).put(handlers::update_settings))
        // Data management (export/import/reset)
        .route("/api/data/export", get(handlers::export_data))
        .route("/api/data/import", post(handlers::import_data))
        .route("/api/data/reset", post(handlers::reset_data))
        // Plugin management
        .route("/ws/plugin", get(handlers::plugin_ws_handler))
        .route("/api/plugins", get(handlers::list_plugins))
        .route("/api/plugins/:id", get(handlers::get_plugin).delete(handlers::delete_plugin))
        .route("/api/plugins/:id/confirm", post(handlers::confirm_plugin))
        // 本机局域网 IP 查询（供 UI 拼分发链接）
        .route("/api/install/local-ip", get(handlers::get_local_ip))
        // /v1/* 代理（本机既有客户端直连 19068 的兼容入口）
        .merge(proxy_routes(&state))
        .with_state(state)
}

/// 公共 Router：/v1/* 代理 + /install 静态页（绑 0.0.0.0，供局域网设备访问）。
pub fn build_public_router(state: Arc<AppState>) -> Router {
    // install 静态页（无 state 需求，但 merge 后整体 with_state）
    let install_routes = Router::new().route("/install", get(handlers::serve_install_page));

    Router::new()
        .merge(install_routes)
        .merge(proxy_routes(&state))
        .with_state(state)
}

/// 兼容入口：等价于 admin router（lib.rs 与冒烟测试沿用）。
pub fn build_router(state: Arc<AppState>) -> Router {
    build_admin_router(state)
}
