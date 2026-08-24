//! 全部 Axum 路由的定义。
//!
//! 单 listener 绑 `0.0.0.0:port`，通过路径级 IP 中间件控制访问权限：
//! - 公开路径：`/health`、`/ws`、`/ws/plugin`、
//!   `/v1/*` 代理（套 rate_limit，128 req/min）。
//! - 管理路径：`/api/*`（CRUD + 密钥 + 数据导出等）——仅允许 loopback IP 访问，
//!   非本机请求被 `admin_ip_guard` 中间件拦截返回 403。
//!
//! `build_router` 保留为兼容入口（`lib.rs` 与冒烟测试沿用）。

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::http::{header, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::Router;
use include_dir::{include_dir, Dir};

use crate::gateway::server::AppState;
use crate::middleware::rate_limit::rate_limit_middleware;

use super::handlers;
use super::proxy;

/// 编译期嵌入的前端构建产物（`dist/`）。
/// 消除生产环境的 cwd/DIST_DIR 路径解析问题：无论 exe 装在哪、cwd 是啥，
/// SPA fallback 都能从二进制自身读到 index.html。
static DIST_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../dist");

/// /v1/* 代理端点（套 rate_limit，128 req/min）。
/// 注意：返回 `Router<AppState>`（未 with_state），由调用方 `.with_state` 统一收敛。
fn proxy_routes(state: &Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/chat/completions", post(proxy::proxy_chat_completions))
        .route("/v1/messages", post(proxy::proxy_messages))
        .route("/v1/responses", post(proxy::proxy_responses))
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

/// SPA fallback：返回 `index.html`，让 Vue Router 处理前端路由。
/// 从编译期嵌入的 `DIST_DIR` 读取，消除生产环境 cwd 依赖。
async fn spa_fallback() -> impl IntoResponse {
    match DIST_DIR.get_file("index.html") {
        Some(file) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(axum::body::Body::from(file.contents()))
            .unwrap(),
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(axum::body::Body::from("index.html not found"))
            .unwrap(),
    }
}

/// /favicon.ico：从嵌入 dist 的根目录读取（vite 构建产物在 dist/ 根下）。
async fn serve_favicon() -> impl IntoResponse {
    match DIST_DIR.get_file("favicon.ico") {
        Some(file) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "image/x-icon")
            .header(header::CACHE_CONTROL, "public, max-age=86400")
            .body(axum::body::Body::from(file.contents()))
            .unwrap(),
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(axum::body::Body::from(""))
            .unwrap(),
    }
}

/// `/assets/*` 静态资源：从嵌入的 `DIST_DIR/assets/` 读取（与 spa_fallback 同源）。
/// 路径参数是 vite 构建时生成的 hash 文件名，带 `&str` 即可。
async fn serve_asset(axum::extract::Path(name): axum::extract::Path<String>) -> impl IntoResponse {
    // 防路径穿越：只允许安全字符（hash + 扩展名）
    if name.contains("..") || name.contains('\\') {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(axum::body::Body::from("invalid path"))
            .unwrap();
    }
    let asset_path = format!("assets/{}", name);
    match DIST_DIR.get_file(&asset_path) {
        Some(file) => {
            let ctype = mime_for_filename(&name);
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, ctype)
                // vite hash 文件名 + 短期缓存：文件名本身决定内容变化，安全缓存
                .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
                .body(axum::body::Body::from(file.contents()))
                .unwrap()
        }
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(axum::body::Body::from("asset not found"))
            .unwrap(),
    }
}

/// 按扩展名返回 MIME 类型（vite 构建只产出 JS/CSS/woff2/svg/png）。
fn mime_for_filename(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("");
    match ext {
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

/// 单 listener 全量 Router：公开路径 + 管理路径（IP 限制）。
///
/// 公开路径（无需 loopback）：`/health`、`/ws`、`/ws/plugin`、`/v1/*`。
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
        // WebSocket endpoints (no rate limiting)
        .route("/ws", get(handlers::ws_handler))
        .route("/ws/plugin", get(handlers::plugin_ws_handler))
        // MCP 工具端点（Streamable HTTP）：Service Key 鉴权 + 无状态会话。
        // 请求体上限 2MiB（JSON-RPC 帧很小，防滥用）。
        .route(
            "/mcp",
            axum::routing::any(crate::mcp::handle_mcp_request),
        )
        // UI settings (theme/hue/locale) — public for LAN install page
        .route("/api/ui-settings", get(handlers::get_ui_settings))
        // /api/* 管理路由（IP 限制）
        .merge(api_routes)
        // /v1/* 代理（套 rate_limit，128 req/min）
        .merge(proxy_routes(&state))
        // /favicon.ico
        .route("/favicon.ico", get(serve_favicon))
        // /assets/* 静态资源（从嵌入的 dist/assets/ 读取）
        .route("/assets/*name", get(serve_asset))
        // SPA fallback：未匹配 GET → 嵌入的 dist/index.html
        .fallback(get(spa_fallback))
        .with_state(state)
}
