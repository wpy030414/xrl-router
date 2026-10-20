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
//! - `notify`（`mcp_notify`）：发送系统桌面通知（见 `notify.rs`）。
//!
//! 鉴权与 `/v1/*` 代理一致：`Authorization: Bearer <service-key>`（argon2 校验）。
//! 会话模式为无状态（`NeverSessionManager`）——工具只有三个且无服务端推送，
//! 不需要 MCP 会话，客户端每次请求独立处理。

mod fetch;
mod notify;
mod tools;

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
    fetch::init(app.clone());
    notify::init(app);
}

/// 全局 MCP 服务单例（懒加载）。`NeverSessionManager` 无状态、`XrlMcpTools` 无字段，
/// 服务本身不持有请求间状态，可安全跨请求复用。
static MCP_SERVICE: OnceLock<McpService> = OnceLock::new();

/// 按生产配置构造 MCP 服务（无状态 + 关闭 Host 白名单）。
///
/// 独立成函数供单测直接驱动 `handle()` 走真实传输层（见 `tests` 模块）。
fn build_service() -> McpService {
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
}

fn mcp_service() -> &'static McpService {
    MCP_SERVICE.get_or_init(build_service)
}

/// `POST/GET/DELETE /mcp` 入口：鉴权 → 委托 rmcp Streamable HTTP 服务。
///
/// 鉴权失败 401；全部开关都关闭时仍正常应答协议请求（`tools/list` 返回空），
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    /// 直接向生产配置的服务实例投递 JSON-RPC，取回响应体里的 JSON 文本。
    ///
    /// 走 `build_service()`（与 `mcp_service()` 同一套配置），因此覆盖的是真实的
    /// Streamable HTTP 传输层：无状态派发、协议版本判定、结果序列化全部生效。
    /// `APP_STATE` 未注入时 `tools/list` 返回空数组——本测试只关心结果的字段形状。
    async fn post_json_rpc(headers: &[(&str, String)], body: &str) -> serde_json::Value {
        let service = build_service();
        let mut builder = axum::http::Request::builder()
            .method(axum::http::Method::POST)
            .uri("/mcp")
            .header(header::HOST, "127.0.0.1:19068")
            .header(header::CONTENT_TYPE, "application/json")
            .header(
                header::ACCEPT,
                "application/json, text/event-stream",
            );
        for (k, v) in headers {
            builder = builder.header(*k, v.clone());
        }
        let req = builder.body(Body::from(body.to_string())).unwrap();
        let resp = service.handle(req).await;
        let (parts, body) = resp.into_parts();
        let bytes = to_bytes(Body::new(body), 64 * 1024).await.unwrap();
        let text = String::from_utf8_lossy(&bytes).to_string();
        assert_eq!(
            parts.status,
            StatusCode::OK,
            "tools/list 应返回 200，响应体: {text}"
        );
        // 传输层可能以 SSE（data: {...}）或纯 JSON 应答，统一取出 JSON 对象。
        let start = text.find('{').unwrap_or_else(|| panic!("响应体无 JSON: {text}"));
        let end = text.rfind('}').unwrap();
        serde_json::from_str(&text[start..=end])
            .unwrap_or_else(|e| panic!("JSON 解析失败 {e}: {text}"))
    }

    const TOOLS_LIST: &str = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;

    /// `tools/list` + SEP-2575 内联 `_meta`（声明 2026-07-28 修订版）。
    fn tools_list_with_inline_meta() -> String {
        let meta = r#""_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{},"io.modelcontextprotocol/clientInfo":{"name":"xrl-probe","version":"1.0.0"}}"#;
        format!(r#"{{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{{{meta}}}}}"#)
    }

    /// 回归（真实故障）：客户端声明 2026-07-28 修订版时，`tools/list` **必须**带
    /// `ttlMs` / `cacheScope`。
    ///
    /// Claude Code ≥2.1.x 的 schema 把两者列为必填，缺席即判定结果非法
    /// （`Invalid result for tools/list`），三个内置工具全部不可用。
    ///
    /// 覆盖完整协议信号路径：版本既进 `RequestContext`（`_meta`）也进传输层校验
    /// （请求头），确认补发逻辑在真实协商结果下确实触发。
    #[tokio::test]
    async fn tools_list_carries_cache_hints_for_modern_client() {
        // 完整的 2026-07-28 请求：SEP-2575 内联 `_meta` + SEP-2243 标准头
        // （二者必须同时在场——只有 `_meta` 版本时缺请求头被拒，只有请求头时缺
        // `Mcp-Method` 被拒。现网 Claude Code 两者都发，故请求能到达 list_tools）。
        let headers = vec![
            ("mcp-protocol-version", "2026-07-28".to_string()),
            ("mcp-method", "tools/list".to_string()),
        ];
        let json = post_json_rpc(&headers, &tools_list_with_inline_meta()).await;
        let result = &json["result"];
        assert!(
            result["ttlMs"].is_number(),
            "2026-07-28 客户端必须拿到数字 ttlMs，实际: {result}"
        );
        assert_eq!(result["ttlMs"].as_u64(), Some(0), "工具列表不应被缓存");
        assert_eq!(
            result["cacheScope"].as_str(),
            Some("private"),
            "开关驱动的列表不应声明为可公共缓存"
        );
        assert_eq!(result["resultType"].as_str(), Some("complete"));
        assert!(result["tools"].is_array(), "tools 应为数组: {result}");
    }

    /// 未声明新修订版的客户端保持 legacy 线格式（**不带**这两个字段）。
    ///
    /// 与 rmcp 的 `#[tool_router]` 宏同构：字段按协商版本条件发出，避免向旧客户端
    /// 塞入其 schema 未定义的键。无 `MCP-Protocol-Version` 请求头时传输层把版本
    /// 回落到 2025-03-26，因此「无请求头」与「显式 2025-03-26」是同一条路径。
    #[tokio::test]
    async fn tools_list_omits_cache_hints_for_legacy_clients() {
        for (label, headers) in [
            ("无请求头（回落 2025-03-26）", Vec::new()),
            (
                "显式 2025-03-26",
                vec![("mcp-protocol-version", "2025-03-26".to_string())],
            ),
        ] {
            let json = post_json_rpc(&headers, TOOLS_LIST).await;
            let result = &json["result"];
            assert!(
                result["tools"].is_array(),
                "{label}: tools 仍应正常返回，实际: {result}"
            );
            assert!(
                result.get("ttlMs").is_none(),
                "{label}: 旧客户端不应收到新修订版字段，实际: {result}"
            );
            assert!(
                result.get("cacheScope").is_none(),
                "{label}: 旧客户端不应收到新修订版字段，实际: {result}"
            );
        }
    }

    /// SEP-2243 边界：以请求头声明 2026-07-28 时，传输层强校验 `Mcp-Method` 标准头。
    ///
    /// 缺标准头 → 400 / -32020，**早于** handler 派发。记录该行为，说明为什么上面的
    /// 现网故障复现走 `params._meta`：这类请求根本到不了 `list_tools`。
    #[tokio::test]
    async fn standard_headers_gate_reject_incomplete_modern_request() {
        let req = axum::http::Request::builder()
            .method(axum::http::Method::POST)
            .uri("/mcp")
            .header(header::HOST, "127.0.0.1:19068")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header("mcp-protocol-version", "2026-07-28")
            .body(Body::from(TOOLS_LIST.to_string()))
            .unwrap();
        let resp = build_service().handle(req).await;
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "缺 Mcp-Method 标准头的 2026-07-28 请求应被传输层拒绝"
        );
    }
}
