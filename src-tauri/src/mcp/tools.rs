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
    CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorCode,
    ErrorData, Implementation, JsonObject, ListToolsResult, PaginatedRequestParams,
    ProtocolVersion, ServerCapabilities, ServerInfo, Tool, ToolAnnotations,
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

/// 工具列表的缓存提示（SEP-2549）：不缓存（`ttlMs = 0`）+ 仅请求方可见（`private`）。
///
/// MCP 协议修订版 `2026-07-28` 把 `ttlMs` / `cacheScope` 列为 `tools/list` 等
/// 列表结果的**必填**字段；rmcp 3.x 只在 `#[tool_router]` 宏生成的路径上补发
/// 它们，手写 `ServerHandler`（本项目为按开关动态过滤而手写）不会补，字段直接
/// 缺席。严格校验该修订版的客户端（Claude Code ≥2.1.x）会对缺字段的 `tools/list`
/// 报 schema 错误，三个工具全部不可用。
///
/// 取值必须是 0 + Private，不能学宏的默认 Public：工具列表受运行时开关控制，
/// 任何缓存都会让客户端在开关切换后读到过期清单。rmcp 自身对「不可缓存」的编码
/// 与此一致（`DiscoverResult::new` 用 0 / Private）。
const LIST_TOOLS_TTL_MS: u64 = 0;

/// 判断协商出的协议版本是否要求缓存提示（≥ 2026-07-28）。
///
/// 收 `Option<&ProtocolVersion>` 而非 `RequestContext`，以便单测直接构造版本、
/// 无需伪造会话上下文。ISO `YYYY-MM-DD` 版本串按字典序比较即时序比较，与 rmcp
/// 内部判定一致（`src/handler/server.rs` 的 `sep_2322_supported`）。
fn version_requires_cache_hints(version: Option<&ProtocolVersion>) -> bool {
    version
        .map(|v| v.as_str() >= ProtocolVersion::V_2026_07_28.as_str())
        .unwrap_or(false)
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
        context: RequestContext<RoleServer>,
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
        let mut result = ListToolsResult::with_all_items(filter_tools(ws, wf, n));
        // 与 rmcp 宏同构：仅在客户端按新修订版校验时补发，旧版本保持 legacy 线格式。
        let negotiated = context.protocol_version();
        if version_requires_cache_hints(negotiated.as_ref()) {
            result = with_no_cache_hint(result);
        }
        Ok(result)
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

/// 给列表结果补上「不缓存」的缓存提示（纯函数，便于单测锁定线上契约）。
fn with_no_cache_hint(mut result: ListToolsResult) -> ListToolsResult {
    result.ttl_ms = Some(LIST_TOOLS_TTL_MS);
    result.cache_scope = Some(CacheScope::Private);
    result
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
        "Search the web for current information using local Bing search, then \
         enrich each hit with an extractive summary, a rerank_score (0-3, where \
         >=1.5 means relevant), publish date and site metadata. Prefer results \
         with higher rerank_score; ignore those near 0. Pass ori_question in \
         multi-turn conversations so ranking and summaries track the user's \
         original intent. Use this when the question requires up-to-date facts, \
         recent events, or information not in the training data.",
        Arc::new(web_search_schema()),
    )
    .with_title("Web Search")
    .with_annotations(ToolAnnotations::new().read_only(true))
}

/// web_search 的 JSON Schema：`query`（required）+ `ori_question`（optional）。
fn web_search_schema() -> JsonObject {
    json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "The search query"
            },
            "ori_question": {
                "type": "string",
                "description": "Optional. The user's original question or intent that \
                    led to this search (multi-turn context). Improves relevance ranking \
                    and summary extraction."
            }
        },
        "required": ["query"],
        "additionalProperties": false
    })
    .as_object()
    .unwrap()
    .clone()
}

fn web_fetch_tool() -> Tool {
    Tool::new(
        "web_fetch",
        "Fetch a web page and return its content as clean Markdown. Static requests \
         are tried first; JavaScript-heavy pages are automatically re-rendered in a \
         local webview. Supports HTML, plain text, JSON, XML, CSV and PDF. Long pages \
         are paginated: the header reports the total length and the range returned, \
         and start_index reads the remainder. Private/loopback addresses are refused.",
        Arc::new(web_fetch_schema()),
    )
    .with_title("Web Fetch")
    .with_annotations(ToolAnnotations::new().read_only(true).open_world(true))
}

/// web_fetch 的 JSON Schema：`url`（required）+ 分段读取参数（optional）。
fn web_fetch_schema() -> JsonObject {
    json!({
        "type": "object",
        "properties": {
            "url": {
                "type": "string",
                "description": "The URL of the page to fetch (http or https)"
            },
            "start_index": {
                "type": "integer",
                "minimum": 0,
                "description": "Optional. Character offset to start reading from (default 0). \
                    Use it when a previous call reported truncated content."
            },
            "max_chars": {
                "type": "integer",
                "minimum": 1000,
                "maximum": 200000,
                "description": "Optional. Maximum characters to return in this call (default 60000)."
            }
        },
        "required": ["url"],
        "additionalProperties": false
    })
    .as_object()
    .unwrap()
    .clone()
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

/// web_search 执行：本地 Bing 搜索 → 内容增强层（摘要/重排/元数据）。
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
    // ori_question 宽松解析（多轮对话的原始意图，空串视为未提供）
    let ori_question = args
        .get("ori_question")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let Some(state) = app_state() else {
        return tool_unavailable();
    };

    info!(query = %query, ori = ?ori_question, "mcp: web_search executing");
    match crate::search::bing::search(&state.search_http, &query).await {
        Ok(results) if results.is_empty() => CallToolResult::success(vec![ContentBlock::text(
            format!("No web search results found for: {query}"),
        )])
        .into(),
        Ok(results) => {
            let raw_count = results.len();
            let enriched = crate::content::enrich_search_results(
                &state.http_client,
                results,
                &query,
                ori_question.as_deref(),
            )
            .await;
            info!(query = %query, raw = raw_count, results = enriched.len(), "mcp: web_search succeeded");
            CallToolResult::success(vec![ContentBlock::text(format_search_text(&enriched))]).into()
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

    // 分段参数宽松解析：负数/非整数视为未提供（schema 已声明 minimum）
    let start_index = args
        .get("start_index")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(0);
    let max_chars = args
        .get("max_chars")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(super::fetch::DEFAULT_MAX_CHARS);

    match super::fetch::fetch_url(&url, start_index, max_chars).await {
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

/// 把增强后的搜索结果格式化成喂给 LLM 的文本。
///
/// 格式（面向 LLM 消费）：标题、URL、元数据行（日期/站点/权威度/相关度）、摘要。
/// 摘要优先用内容增强层生成的长摘要，未抓到正文时退化为 Bing snippet。
fn format_search_text(results: &[crate::content::EnrichedResult]) -> String {
    results
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let date = r.date.as_deref().unwrap_or("未知");
            let site = r.site_name.as_deref().unwrap_or("未知");
            // fetched 标记摘要来源：正文提取成功用长摘要，否则退化为 Bing snippet
            let summary = if r.fetched && !r.summary.trim().is_empty() {
                &r.summary
            } else {
                &r.snippet
            };
            format!(
                "[{}] {}\nURL: {}\n日期: {} | 站点: {} | 权威度: {} | 相关度: {:.1}/3\n摘要: {}",
                i + 1,
                r.title,
                r.url,
                date,
                site,
                r.authority,
                r.rerank_score,
                summary,
            )
        })
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
    fn test_web_search_schema_has_optional_ori_question() {
        let schema = web_search_tool().input_schema;
        // query 必填；ori_question 可选（在 properties 但不在 required）
        assert_eq!(schema["required"].as_array().unwrap().len(), 1);
        assert_eq!(schema["required"][0].as_str(), Some("query"));
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("query"));
        assert!(props.contains_key("ori_question"), "missing optional ori_question");
        assert_eq!(schema["additionalProperties"].as_bool(), Some(false));
    }

    #[test]
    fn test_web_fetch_schema_has_pagination_params() {
        let schema = web_fetch_tool().input_schema;
        // 仅 url 必填；start_index / max_chars 可选
        assert_eq!(schema["required"][0].as_str(), Some("url"));
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("url"));
        assert!(props.contains_key("start_index"));
        assert!(props.contains_key("max_chars"));
        assert_eq!(schema["additionalProperties"].as_bool(), Some(false));
    }

    #[test]
    fn test_format_search_text() {
        let results = vec![
            crate::content::EnrichedResult {
                title: "标题一".to_string(),
                url: "https://example.com/a".to_string(),
                snippet: "短摘要一".to_string(),
                summary: "长摘要一".to_string(),
                rerank_score: 2.6,
                date: Some("2026-03-12".to_string()),
                site_name: Some("Example".to_string()),
                authority: 3,
                fetched: true,
            },
            crate::content::EnrichedResult {
                title: "标题二".to_string(),
                url: "https://example.com/b".to_string(),
                snippet: "短摘要二".to_string(),
                summary: String::new(), // 未抓到正文：退化为 snippet
                rerank_score: 0.32,
                date: None,
                site_name: None,
                authority: 1,
                fetched: false,
            },
        ];
        let text = format_search_text(&results);
        assert!(text.contains("[1] 标题一"));
        assert!(text.contains("URL: https://example.com/a"));
        assert!(text.contains("日期: 2026-03-12 | 站点: Example | 权威度: 3 | 相关度: 2.6/3"));
        assert!(text.contains("摘要: 长摘要一"));
        // 第二条：缺失元数据占位「未知」，空 summary 退化为 snippet
        assert!(text.contains("[2] 标题二"));
        assert!(text.contains("日期: 未知 | 站点: 未知 | 权威度: 1 | 相关度: 0.3/3"));
        assert!(text.contains("摘要: 短摘要二"));
    }

    /// 2026-07-28 修订版要求列表结果必带 `ttlMs` / `cacheScope`，取值判定。
    #[test]
    fn test_version_requires_cache_hints() {
        // 无协商版本（legacy 会话）不发。
        assert!(!version_requires_cache_hints(None));
        // 旧修订版保持 legacy 线格式。
        for old in [
            ProtocolVersion::V_2024_11_05,
            ProtocolVersion::V_2025_03_26,
            ProtocolVersion::V_2025_06_18,
            ProtocolVersion::V_2025_11_25,
        ] {
            assert!(
                !version_requires_cache_hints(Some(&old)),
                "{old} 不应要求缓存提示"
            );
        }
        assert!(version_requires_cache_hints(Some(
            &ProtocolVersion::V_2026_07_28
        )));
    }

    /// 补发缓存提示后的线格式必须满足 Claude Code 的 schema：
    /// `ttlMs` 为非负整数、`cacheScope` ∈ {public,private}、`resultType` 存在。
    #[test]
    fn test_list_tools_result_wire_shape_with_cache_hints() {
        let result = with_no_cache_hint(ListToolsResult::with_all_items(filter_tools(
            true, true, true,
        )));
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["ttlMs"].as_u64(), Some(0));
        assert_eq!(value["cacheScope"].as_str(), Some("private"));
        assert_eq!(value["resultType"].as_str(), Some("complete"));
        assert_eq!(value["tools"].as_array().unwrap().len(), 3);
    }

    /// 开关全关（空列表）也要带上缓存提示——校验对空 tools 同样生效。
    #[test]
    fn test_empty_list_tools_still_carries_cache_hints() {
        let result = with_no_cache_hint(ListToolsResult::with_all_items(filter_tools(
            false, false, false,
        )));
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["ttlMs"].as_u64(), Some(0));
        assert_eq!(value["cacheScope"].as_str(), Some("private"));
        assert_eq!(value["tools"].as_array().unwrap().len(), 0);
    }

    /// 未补发时字段缺席——正是回归的成因，锁定 skip_serializing 行为不被误改。
    #[test]
    fn test_raw_list_tools_result_omits_cache_hints() {
        let value = serde_json::to_value(ListToolsResult::with_all_items(filter_tools(
            true, false, false,
        )))
        .unwrap();
        assert!(value.get("ttlMs").is_none());
        assert!(value.get("cacheScope").is_none());
    }

    /// tools/call 结果不受影响：新修订版只要求 resultType 存在。
    #[test]
    fn test_call_tool_result_carries_result_type() {
        for value in [
            serde_json::to_value(CallToolResult::success(vec![ContentBlock::text(
                "ok".to_string(),
            )]))
            .unwrap(),
            serde_json::to_value(CallToolResult::error(vec![ContentBlock::text(
                "bad".to_string(),
            )]))
            .unwrap(),
        ] {
            assert_eq!(value["resultType"].as_str(), Some("complete"));
        }
    }
}
