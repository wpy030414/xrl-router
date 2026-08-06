//! 全部 Axum 路由的定义。
//!
//! 单 listener 绑 `0.0.0.0:port`，通过路径级 IP 中间件控制访问权限：
//! - 公开路径：`/health`、`/ws`、`/ws/plugin`、`/install`、`/fm/*`（广播电台）、
//!   `/v1/*` 代理（套 rate_limit，128 req/min）。
//! - 管理路径：`/api/*`（CRUD + 密钥 + 数据导出等）——仅允许 loopback IP 访问，
//!   非本机请求被 `admin_ip_guard` 中间件拦截返回 403。
//!
//! `build_router` 保留为兼容入口（`lib.rs` 与冒烟测试沿用）。

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{get, post, put};
use axum::Router;

use crate::gateway::server::AppState;
use crate::middleware::rate_limit::rate_limit_middleware;

use super::handlers;
use super::proxy;

/// /v1/* 代理端点（套 rate_limit，128 req/min）。
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

/// 单 listener 全量 Router：公开路径 + 管理路径（IP 限制）。
///
/// 公开路径（无需 loopback）：`/health`、`/ws`、`/ws/plugin`、`/install`、`/fm/*`、`/v1/*`。
/// 管理路径（仅 loopback，非本机返回 403）：`/api/*`。
pub fn build_router(state: Arc<AppState>) -> Router {
    // /api/* 管理路由：仅 loopback IP 可访问（admin_ip_guard 中间件）
    let api_routes = Router::new()
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
        .route("/api/plugins", get(handlers::list_plugins))
        .route("/api/plugins/:id", get(handlers::get_plugin).delete(handlers::delete_plugin))
        .route("/api/plugins/:id/confirm", post(handlers::confirm_plugin))
        // 本机局域网 IP 查询（供 UI 拼分发链接）
        .route("/api/install/local-ip", get(handlers::get_local_ip))
        // admin_ip_guard：非 loopback IP → 403 Forbidden
        .layer(middleware::from_fn(crate::middleware::admin_ip_guard));

    Router::new()
        // Health check
        .route("/health", get(handlers::health_check))
        .route("/", get(handlers::health_check))
        // WebSocket endpoints (no rate limiting)
        .route("/ws", get(handlers::ws_handler))
        .route("/ws/plugin", get(handlers::plugin_ws_handler))
        // Install 静态页（局域网设备访问）
        .route("/install", get(handlers::serve_install_page))
        // Claude FM 广播电台直播流（公开路径，局域网设备可访问）
        .route("/fm/live", get(handlers::fm_live))
        .route("/fm/meta", get(handlers::fm_current_meta))
        // /api/* 管理路由（IP 限制）
        .merge(api_routes)
        // /v1/* 代理（套 rate_limit，128 req/min）
        .merge(proxy_routes(&state))
        .with_state(state)
}
