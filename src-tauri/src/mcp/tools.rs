//! MCP 工具定义与执行：`web_search`（本地 Bing）+ `web_fetch`（Tauri WebView 渲染）
//! + `notify`（系统桌面通知）。
//!
//! 手写 `ServerHandler`（不用 `#[tool_router]` 宏）——工具只有三个，且 `tools/list`
//! 必须按运行时开关（`mcp_websearch` / `mcp_webfetch` / `mcp_notify`）动态过滤，
//! 宏生成的静态列表做不到。
//!
//! 工具实现需要 `AppState`（SearchHttp / 开关原子量），而 `ServerHandler`
//! 方法深处 rmcp 内部拿不到 axum State，故启动时经 `init()` 注入全局引用
//! （Tauri 单实例，`OnceLock` 无风险）。

use std::sync::{Arc, OnceLock};

use serde_json::json;
use tracing::info;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorCode, ErrorData,
    Implementation, JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
    ServerInfo, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler};

use crate::gateway::server::AppState;

/// 全局 AppState 引用（Tauri setup 中注入一次）。
static APP_STATE: OnceLock<Arc<AppState>> = OnceLock::new();

/// 注入全局 AppState（`lib.rs` 创建 AppState 后调用）。
pub(super) fn init(state: Arc<AppState>) {
    let _ = APP_STATE.set(state);
}

fn app_state() -> Option<Arc<AppState>> {
    APP_STATE.get().cloned()
}

/// 无状态标记结构：实现 `ServerHandler`，所有状态经全局 `APP_STATE` 访问。
pub(super) struct XrlMcpTools;

impl ServerHandler for XrlMcpTools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new("XRL Router", env!("CARGO_PKG_VERSION"))
                .with_title("XRL Router Tools")
                .with_description("本地网页搜索与抓取工具（Bing 搜索 + 浏览器渲染抓取）"),
        )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        // 开关实时读取：客户端（重新）连接时即拿到最新工具列表。
        let (ws, wf, n) = match app_state() {
            Some(s) => (
                s.mcp_websearch.load(std::sync::atomic::Ordering::Relaxed),
                s.mcp_webfetch.load(std::sync::atomic::Ordering::Relaxed),
                s.mcp_notify.load(std::sync::atomic::Ordering::Relaxed),
            ),
            None => (false, false, false),
        };
        Ok(ListToolsResult::with_all_items(filter_tools(ws, wf, n)))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let args = request.arguments.unwrap_or_default();
        match request.name.as_ref() {
            "web_search" => Ok(run_web_search(&args).await),
            "web_fetch" => Ok(run_web_fetch(&args).await),
            "notify" => Ok(run_notify(&args).await),
            other => Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                format!("unknown tool: {other}"),
                None,
            )),
        }
    }
}

/// 按开关组合过滤工具列表（纯函数，便于单测）。
fn filter_tools(websearch: bool, webfetch: bool, notify: bool) -> Vec<Tool> {
    let mut tools = Vec::with_capacity(3);
    if websearch {
        tools.push(web_search_tool());
    }
    if webfetch {
        tools.push(web_fetch_tool());
    }
    if notify {
        tools.push(notify_tool());
    }
    tools
}

fn web_search_tool() -> Tool {
    Tool::new(
        "web_search",
        "Search the web for current information using local Bing search. \
         Use this when the question requires up-to-date facts, recent events, \
         or information not in the training data.",
        Arc::new(schema_with_single_field(
            "query",
            "The search query",
        )),
    )
    .with_title("Web Search")
    .with_annotations(ToolAnnotations::new().read_only(true))
}

fn web_fetch_tool() -> Tool {
    Tool::new(
        "web_fetch",
        "Fetch a web page and return its content as Markdown. The page is rendered \
         with a local webview (JavaScript executes, so SPA/JS content is included). \
         Use this to read full article content from a URL.",
        Arc::new(schema_with_single_field(
            "url",
            "The URL of the page to fetch (http or https)",
        )),
    )
    .with_title("Web Fetch")
    .with_annotations(ToolAnnotations::new().read_only(true))
}

fn notify_tool() -> Tool {
    Tool::new(
        "notify",
        "Send a desktop notification to the user. Use this to alert the user \
         when a long-running task is complete, when an important event occurs, \
         or when the user needs to be notified about something.",
        Arc::new(
            json!({
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "The notification title (required)"
                    },
                    "body": {
                        "type": "string",
                        "description": "The notification body text (optional)"
                    },
                    "sound": {
                        "type": "string",
                        "description": "Notification sound (optional, defaults to \"default\"). \
                            Use \"default\" for the system default notification sound. \
                            Or specify a platform-specific sound name: \
                            macOS: \"Glass\", \"Basso\", \"Frog\", \"Hero\", \"Submarine\", \"Pop\", etc. \
                            Linux: freedesktop sound names like \"message-new-email\", \"bell\". \
                            Windows: toast sound names."
                    }
                },
                "required": ["title"],
                "additionalProperties": false
            })
            .as_object()
            .unwrap()
            .clone(),
        ),
    )
    .with_title("Notify")
    .with_annotations(ToolAnnotations::new().read_only(true))
}

/// 单字段工具的 JSON Schema（object + 一个 required string 字段）。
fn schema_with_single_field(field: &str, field_desc: &str) -> JsonObject {
    json!({
        "type": "object",
        "properties": {
            field: {
                "type": "string",
                "description": field_desc
            }
        },
        "required": [field],
        "additionalProperties": false
    })
    .as_object()
    .unwrap()
    .clone()
}

/// web_search 执行：本地 Bing 搜索（复用代理劫持时代的同一套搜索实现）。
async fn run_web_search(args: &JsonObject) -> CallToolResponse {
    let query = match args
        .get("query")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        Some(q) => q.to_string(),
        None => {
            return CallToolResult::error(vec![ContentBlock::text(
                "Error: missing required argument `query`".to_string(),
            )])
            .into()
        }
    };

    let Some(state) = app_state() else {
        return tool_unavailable();
    };

    info!(query = %query, "mcp: web_search executing");
    match crate::search::bing::search(&state.search_http, &query).await {
        Ok(results) if results.is_empty() => CallToolResult::success(vec![ContentBlock::text(
            format!("No web search results found for: {query}"),
        )])
        .into(),
        Ok(results) => {
            info!(query = %query, results = results.len(), "mcp: web_search succeeded");
            CallToolResult::success(vec![ContentBlock::text(format_search_text(&results))]).into()
        }
        Err(e) => {
            tracing::warn!(query = %query, error = %e, "mcp: web_search failed");
            CallToolResult::error(vec![ContentBlock::text(format!(
                "Web search unavailable: {e}. Do NOT make up information."
            ))])
            .into()
        }
    }
}

/// web_fetch 执行：Tauri WebView 渲染后取正文（Markdown）。
async fn run_web_fetch(args: &JsonObject) -> CallToolResponse {
    let url = match args
        .get("url")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        Some(u) => u.to_string(),
        None => {
            return CallToolResult::error(vec![ContentBlock::text(
                "Error: missing required argument `url`".to_string(),
            )])
            .into()
        }
    };

    match super::fetch::fetch_url(&url).await {
        Ok(markdown) => CallToolResult::success(vec![ContentBlock::text(markdown)]).into(),
        Err(e) => {
            tracing::warn!(url = %url, error = %e, "mcp: web_fetch failed");
            CallToolResult::error(vec![ContentBlock::text(format!(
                "Web fetch failed: {e}. Do NOT make up information."
            ))])
            .into()
        }
    }
}

/// notify 执行：发送系统桌面通知（宽松参数：title 必填，body / sound 可选）。
async fn run_notify(args: &JsonObject) -> CallToolResponse {
    let title = match args
        .get("title")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        Some(t) => t.to_string(),
        None => {
            return CallToolResult::error(vec![ContentBlock::text(
                "Error: missing required argument `title`".to_string(),
            )])
            .into()
        }
    };
    let body = args
        .get("body")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    // sound 宽松解析：缺失 / null / 空字符串 → 使用 "default"；否则原样透传给底层
    // （"default" 或平台特定声音名，由 notify-rust 在各平台处理）。
    let sound = args
        .get("sound")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());

    info!(title = %title, sound = ?sound, "mcp: notify executing");
    let params = super::notify::NotifyParams {
        title: &title,
        body: body.map(|s| s.as_ref()),
        sound: sound.map(|s| s.as_ref()),
    };
    match super::notify::send_notification(&params) {
        Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]).into(),
        Err(e) => {
            tracing::warn!(title = %title, error = %e, "mcp: notify failed");
            CallToolResult::error(vec![ContentBlock::text(format!(
                "Failed to send notification: {e}"
            ))])
            .into()
        }
    }
}

fn tool_unavailable() -> CallToolResponse {
    CallToolResult::error(vec![ContentBlock::text(
        "Tool unavailable: gateway state not initialized.".to_string(),
    )])
    .into()
}

/// 把 Bing 结果格式化成喂给 LLM 的文本（与旧劫持循环同款格式）。
fn format_search_text(results: &[crate::search::SearchResult]) -> String {
    results
        .iter()
        .enumerate()
        .map(|(i, r)| format!("[{}] {}\n{}\n摘要: {}", i + 1, r.title, r.url, r.snippet))
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_tools_by_switches() {
        assert_eq!(filter_tools(false, false, false).len(), 0);
        let ws = filter_tools(true, false, false);
        assert_eq!(ws.len(), 1);
        assert_eq!(ws[0].name.as_ref(), "web_search");
        let wf = filter_tools(false, true, false);
        assert_eq!(wf.len(), 1);
        assert_eq!(wf[0].name.as_ref(), "web_fetch");
        let n = filter_tools(false, false, true);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].name.as_ref(), "notify");
        let all = filter_tools(true, true, true);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].name.as_ref(), "web_search");
        assert_eq!(all[1].name.as_ref(), "web_fetch");
        assert_eq!(all[2].name.as_ref(), "notify");
    }

    #[test]
    fn test_tool_schemas_required_fields() {
        for t in [web_search_tool(), web_fetch_tool(), notify_tool()] {
            let schema = &t.input_schema;
            assert_eq!(schema.get("type").and_then(|v| v.as_str()), Some("object"));
            let required = schema["required"].as_array().unwrap();
            assert_eq!(required.len(), 1);
        }
    }

    #[test]
    fn test_format_search_text() {
        let results = vec![
            crate::search::SearchResult {
                title: "标题一".to_string(),
                url: "https://example.com/a".to_string(),
                snippet: "摘要一".to_string(),
            },
            crate::search::SearchResult {
                title: "标题二".to_string(),
                url: "https://example.com/b".to_string(),
                snippet: "摘要二".to_string(),
            },
        ];
        let text = format_search_text(&results);
        assert!(text.contains("[1] 标题一"));
        assert!(text.contains("https://example.com/a"));
        assert!(text.contains("[2] 标题二"));
        assert!(text.contains("摘要: 摘要一"));
    }
}
