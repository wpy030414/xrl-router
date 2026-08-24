//! 本地 MCP 工具服务器：以 Streamable HTTP 端点（`/mcp`）挂在网关单 listener 上。
//!
//! 提供三个工具，分别由设置开关控制（`tools/list` 按请求实时过滤，开关切换后
//! 客户端重新连接即可看到最新列表）：
//!
//! - `web_search`（`mcp_websearch`）：本地 Bing 搜索，复用 `crate::search::bing::search`。
//!   开关同时控制代理层剔除请求自带的搜索类工具（见 `api/proxy/stream.rs`），
//!   防止上游官方搜索生效。
//! - `web_fetch`（`mcp_webfetch`）：Tauri 内置 WebView 渲染（隐藏窗口执行页面 JS
//!   后提取正文 Markdown），渲染不可用时回退静态抓取（见 `fetch.rs`）。
//! - `web_vision`（`mcp_vision`）：用设置页指定的「视觉专用模型」识别图片
//!   （http(s) URL 或本地路径，网关取图后 base64 上送），返回描述文本（见 `vision.rs`）。
//!
//! 鉴权与 `/v1/*` 代理一致：`Authorization: Bearer <service-key>`（argon2 校验）。
//! 会话模式为无状态（`NeverSessionManager`）——工具只有三个且无服务端推送，
//! 不需要 MCP 会话，客户端每次请求独立处理。

mod fetch;
mod tools;
mod vision;

use std::sync::{Arc, OnceLock};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, Request, StatusCode};
use axum::response::{IntoResponse, Response};

use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};

use crate::gateway::server::AppState;
use tauri::AppHandle;

type McpService = StreamableHttpService<tools::XrlMcpTools, NeverSessionManager>;

/// 注入全局 AppState + AppHandle（`lib.rs` setup 创建 AppState 后调用；
/// AppHandle 供 web_fetch 的 WebView 渲染层创建隐藏窗口）。
pub(crate) fn init(state: Arc<AppState>, app: AppHandle) {
    tools::init(state);
    fetch::init(app);
}

/// 全局 MCP 服务单例（懒加载）。`NeverSessionManager` 无状态、`XrlMcpTools` 无字段，
/// 服务本身不持有请求间状态，可安全跨请求复用。
static MCP_SERVICE: OnceLock<McpService> = OnceLock::new();

fn mcp_service() -> &'static McpService {
    MCP_SERVICE.get_or_init(|| {
        let mut config = StreamableHttpServerConfig::default();
        // 无状态模式：不为每个客户端建立会话（工具只读、无服务端推送，会话纯属开销）。
        config.legacy_session_mode = false;
        // 关闭 Host 白名单校验：rmcp 默认只放行 loopback（防 DNS rebinding），
        // 但本网关公开区（同 /v1/*）允许局域网客户端以 LAN IP 的 Host 访问，
        // 安全边界由 Service Key 鉴权承担（与既有安全模型一致，见 AGENTS.md）。
        config = config.disable_allowed_hosts();
        StreamableHttpService::new(
            || Ok(tools::XrlMcpTools),
            Arc::new(NeverSessionManager::default()),
            config,
        )
    })
}

/// `POST/GET/DELETE /mcp` 入口：鉴权 → 委托 rmcp Streamable HTTP 服务。
///
/// 鉴权失败 401；两个开关都关闭时仍正常应答协议请求（`tools/list` 返回空），
/// 保证已注册的客户端连接不报错。
pub(crate) async fn handle_mcp_request(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
) -> Response {
    // Service Key 鉴权（与 /v1/* 同一套），从 Authorization: Bearer 提取。
    let key = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if crate::api::proxy::auth::verify_service_key(&state, key).await.is_none() {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"error":{"type":"authentication_error","message":"invalid or missing service key"}}"#,
        )
            .into_response();
    }

    // rmcp 的 handle() 接受任意 http_body::Body，返回 BoxBody<Bytes, Infallible>；
    // axum 0.7 的 Body::new 直接接受任何 http_body::Body<Data = Bytes>，无需升级 0.8。
    let resp = mcp_service().handle(req).await;
    let (parts, body) = resp.into_parts();
    Response::from_parts(parts, Body::new(body))
}
