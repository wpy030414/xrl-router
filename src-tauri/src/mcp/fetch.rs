//! WebFetch 抓取层：**静态优先，按需渲染**。
//!
//! 抓取链（`fetch_url`）：
//! 1. URL 归一化 + SSRF 守卫（`net_guard`：拒绝本机/内网/云元数据地址）；
//! 2. 内容缓存（归一化 URL 为键，TTL 15 分钟）——重复抓取零成本；
//! 3. **静态请求**（reqwest，浏览器头，继承系统代理）：HTML 走正文提取，
//!    text/json/xml/csv/pdf 走对应分支；
//! 4. 仅在「静态失败且值得重试」或「正文过薄且页面像 SPA」时升级到 WebView 渲染；
//! 5. 输出统一加元数据头，支持 `start_index` / `max_chars` 分段读取。
//!
//! 设计取舍：
//! - **静态优先**——绝大多数页面不需要 JS，静默降级不再污染输出；此前「一律
//!   WebView」让每页多付 3~10 秒，且全局串行。
//! - **渲染池**——隐藏窗口池（默认 2 个）+ 信号量限流，替代全局单窗口串行锁；
//!   静态路径完全不受渲染池影响，可任意并发。
//! - **导航白名单**——`on_navigation` 对每次（含重定向）导航做内网地址检查，
//!   防止 3xx 跳转到 loopback 管理端点。
//! - **显式截断**——输出头写明总长度与本次区间，尾部提示 `start_index` 续读，
//!   不再让模型在「已截断」三个字之后失去后半篇。
//!
//! WebView 资产（见 docs/DECISIONS.md ADR-038 / ADR-056）：不探测外部浏览器、
//! 隐藏窗口 + Rust 单向 eval、远程页面拿不到 Tauri IPC。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context};
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE, CONTENT_TYPE, USER_AGENT};
use tauri::{AppHandle, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use url::Url;

use crate::content::ExtractedPage;

/// 单次输出默认上限（字符）。
pub(super) const DEFAULT_MAX_CHARS: usize = 60_000;
/// 单次输出硬上限（防止模型用 max_chars 把整站灌进上下文）。
const HARD_MAX_CHARS: usize = 200_000;
/// 单次输出下限。
const MIN_MAX_CHARS: usize = 1_000;
/// 内容缓存 TTL。
const CACHE_TTL: Duration = Duration::from_secs(15 * 60);
/// 内容缓存容量。
const CACHE_CAPACITY: usize = 64;
/// 静态请求超时。
const STATIC_TIMEOUT: Duration = Duration::from_secs(20);
/// 响应体上限（HTML/PDF 通用）。
const MAX_BODY_BYTES: usize = 5 * 1024 * 1024;
/// 页面加载（等待导航稳定）超时。
const NAVIGATE_TIMEOUT: Duration = Duration::from_secs(30);
/// JS 侧回传 HTML 上限（防 WebView2 `ExecuteScriptWithResult` 结果体积限制）。
const MAX_JS_RETURN_CHARS: usize = 1_500_000;
/// 同时存在的隐藏渲染窗口数。
const RENDER_POOL_SIZE: usize = 2;
/// 正文低于此字符数时，若页面像 SPA 则升级渲染。
const SPA_TEXT_THRESHOLD: usize = 400;
/// 提取质量低于此值时退回整页转换。
const EXTRACTION_QUALITY_THRESHOLD: f32 = 0.35;

const CHROME_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                         (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// 全局 AppHandle（`mcp::init` 注入，`lib.rs` setup 调用）。
static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();

/// 注入 AppHandle（`lib.rs` setup 创建 AppState 后调用）。
pub(crate) fn init(app: AppHandle) {
    let _ = APP_HANDLE.set(app);
}

// ── 对外入口 ──────────────────────────────────────────────────────────────

/// 抓取页面正文（Markdown），按 `start_index` / `max_chars` 分段返回。
pub(super) async fn fetch_url(
    url: &str,
    start_index: usize,
    max_chars: usize,
) -> anyhow::Result<String> {
    let target = normalize_url(url)?;
    crate::net_guard::ensure_public_url(&target)
        .await
        .map_err(|e| anyhow!("该地址不允许抓取：{e}"))?;

    let key = cache_key(&target);
    let page = match cache_get(&key) {
        Some(page) => {
            tracing::debug!(url = %target, "mcp: web_fetch cache hit");
            page
        }
        None => {
            let page = Arc::new(load_page(&target).await?);
            cache_put(&key, page.clone());
            page
        }
    };
    Ok(frame_output(&page, start_index, max_chars))
}

/// URL 归一化：接受 `http`/`https`；无协议时补 `https://`。
fn normalize_url(input: &str) -> anyhow::Result<Url> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        bail!("empty url");
    }
    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let url = Url::parse(&candidate).map_err(|e| anyhow!("invalid url: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("unsupported scheme: {}", url.scheme());
    }
    if url.host_str().is_none() {
        bail!("url has no host");
    }
    Ok(url)
}

// ── 抓取编排 ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Source {
    Static,
    Rendered,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Source::Static => "静态抓取",
            Source::Rendered => "WebView 渲染",
        }
    }
}

/// 一页的最终内容（缓存单位）。
struct Page {
    title: Option<String>,
    final_url: String,
    source: Source,
    /// 附加说明（如「静态兜底，可能不含 JS 渲染结果」）。
    note: Option<String>,
    /// 正文 Markdown（不含标题 H1，标题由输出框架补）。
    markdown: String,
}

/// 抓取一页：静态优先，必要时升级渲染。
async fn load_page(url: &Url) -> anyhow::Result<Page> {
    let static_result = static_fetch(url).await;
    let needs_render = match &static_result {
        Ok((_, needs_render)) => {
            if *needs_render {
                tracing::debug!(url = %url, "mcp: static content thin, escalating to webview");
            }
            *needs_render
        }
        Err(e) if !e.try_render => return Err(anyhow!(e.message.clone())),
        Err(e) => {
            tracing::debug!(url = %url, error = %e.message, "mcp: static fetch failed, trying webview");
            true
        }
    };
    if !needs_render {
        return match static_result {
            Ok((page, _)) => Ok(page),
            Err(e) => Err(anyhow!(e.message)),
        };
    }

    match render_page(url).await {
        Ok(page) => Ok(page),
        Err(render_err) => match static_result {
            Ok((mut page, _)) => {
                tracing::warn!(url = %url, error = %render_err, "mcp: webview render failed, using static content");
                page.note = Some(
                    "WebView 渲染失败，以下为静态抓取内容（可能不含 JS 渲染结果）".to_string(),
                );
                Ok(page)
            }
            Err(e) => Err(anyhow!(
                "抓取失败：{}；WebView 渲染失败：{render_err}",
                e.message
            )),
        },
    }
}

// ── 静态抓取 ──────────────────────────────────────────────────────────────

struct StaticError {
    message: String,
    /// 是否值得再用 WebView 试一次（403/429/5xx/网络错误 → 值得）。
    try_render: bool,
}

impl StaticError {
    fn no_render(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            try_render: false,
        }
    }
}

/// 静态抓取客户端：继承系统代理 + 浏览器头 + 逐跳重定向守卫。
fn static_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(CHROME_UA));
        // 内容协商：优先向支持「Markdown for Agents」的站点拿现成 Markdown
        headers.insert(
            ACCEPT,
            HeaderValue::from_static(
                "text/markdown, text/html;q=0.9, application/xhtml+xml;q=0.8, \
                 text/plain;q=0.7, */*;q=0.5",
            ),
        );
        headers.insert(
            ACCEPT_LANGUAGE,
            HeaderValue::from_static("zh-CN,zh;q=0.9,en;q=0.8"),
        );
        crate::http::build_http_client()
            .default_headers(headers)
            .timeout(STATIC_TIMEOUT)
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 10 {
                    return attempt.error(std::io::Error::other("重定向次数过多"));
                }
                if let Some(reason) = crate::net_guard::blocked_literal_reason(attempt.url()) {
                    return attempt
                        .error(std::io::Error::other(format!("重定向目标被拒绝：{reason}")));
                }
                attempt.follow()
            }))
            .build()
            .expect("failed to build web_fetch http client")
    })
}

/// 静态抓取一页。返回 `(页面, 是否需要升级渲染)`。
async fn static_fetch(url: &Url) -> Result<(Page, bool), StaticError> {
    let resp = static_client()
        .get(url.clone())
        .send()
        .await
        .map_err(|e| StaticError {
            message: format!("请求失败：{e}"),
            try_render: true,
        })?;

    let status = resp.status();
    if !status.is_success() {
        return Err(StaticError {
            message: format!("HTTP {}", status.as_u16()),
            // 403/429/451 多为反爬或风控，WebView 有机会过；404/410 不必重试
            try_render: matches!(status.as_u16(), 403 | 429 | 451) || status.is_server_error(),
        });
    }

    let final_url = resp.url().to_string();
    let content_type = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let kind = classify_content_type(content_type.as_deref());
    if matches!(kind, ContentKind::Unsupported) {
        return Err(StaticError::no_render(format!(
            "不支持的内容类型：{}",
            content_type.as_deref().unwrap_or("未知")
        )));
    }

    let bytes = read_capped(resp, MAX_BODY_BYTES).await?;
    let ct = content_type.clone();
    let url_for_build = final_url.clone();
    let built = tokio::task::spawn_blocking(move || {
        build_page(kind, &bytes, ct.as_deref(), &url_for_build)
    })
    .await
    .map_err(|e| StaticError::no_render(format!("内容处理任务失败：{e}")))?
    .map_err(StaticError::no_render)?;
    Ok((built.page, built.needs_render))
}

/// 读取响应体，超过上限直接报错（截断 PDF 会得到损坏内容）。
async fn read_capped(resp: reqwest::Response, cap: usize) -> Result<Vec<u8>, StaticError> {
    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| StaticError {
            message: format!("读取响应体失败：{e}"),
            try_render: false,
        })?;
        if buf.len() + chunk.len() > cap {
            return Err(StaticError::no_render(format!(
                "响应体超过 {} MB 上限",
                cap / 1024 / 1024
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// 按内容类型分支构建页面（同步纯 CPU，调用方放 blocking 线程）。
fn build_page(
    kind: ContentKind,
    bytes: &[u8],
    content_type: Option<&str>,
    final_url: &str,
) -> Result<BuiltPage, String> {
    match kind {
        ContentKind::Html => {
            let html = crate::content::decode_body(bytes, content_type);
            let extracted = crate::content::extract(&html, final_url);
            let markdown = page_to_markdown(&extracted, &html);
            // 正文过薄且页面带 SPA 特征（或几乎没有文本）才升级渲染：
            // 真正短小的静态页不值得多花一次 3~10 秒的渲染。
            let thin = extracted.text_len < SPA_TEXT_THRESHOLD;
            let needs_render = thin && (looks_like_spa(&html) || extracted.text_len < 80);
            Ok(BuiltPage {
                page: Page {
                    title: Some(extracted.title),
                    final_url: final_url.to_string(),
                    source: Source::Static,
                    note: None,
                    markdown,
                },
                needs_render,
            })
        }
        ContentKind::PlainText | ContentKind::Json | ContentKind::Xml | ContentKind::Csv => {
            let text = crate::content::decode_body(bytes, content_type);
            let markdown = match kind {
                ContentKind::Json => format!("```json\n{}\n```", text.trim_end()),
                ContentKind::Xml => format!("```xml\n{}\n```", text.trim_end()),
                ContentKind::Csv => format!("```csv\n{}\n```", text.trim_end()),
                _ => text,
            };
            Ok(BuiltPage {
                page: Page {
                    title: None,
                    final_url: final_url.to_string(),
                    source: Source::Static,
                    note: None,
                    markdown,
                },
                needs_render: false,
            })
        }
        ContentKind::Pdf => {
            let text = pdf_extract::extract_text_from_mem(bytes)
                .map_err(|e| format!("PDF 解析失败：{e}"))?;
            if text.trim().is_empty() {
                return Err("PDF 未解析出文本（可能是扫描件）".to_string());
            }
            let title = final_url
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            Ok(BuiltPage {
                page: Page {
                    title,
                    final_url: final_url.to_string(),
                    source: Source::Static,
                    note: Some("内容由 PDF 文本层提取，可能丢失排版/表格结构".to_string()),
                    markdown: text,
                },
                needs_render: false,
            })
        }
        ContentKind::Unsupported => Err("不支持的内容类型".to_string()),
    }
}

struct BuiltPage {
    page: Page,
    needs_render: bool,
}

/// 正文提取 → Markdown。质量过低或正文太短时退回整页转换。
fn page_to_markdown(extracted: &ExtractedPage, full_html: &str) -> String {
    if extracted.quality >= EXTRACTION_QUALITY_THRESHOLD && !extracted.html.is_empty() {
        if let Ok(md) = htmd::convert(&extracted.html) {
            if md.chars().count() >= 80 {
                return md;
            }
        }
    }
    htmd::convert(full_html).unwrap_or_else(|_| full_html.to_string())
}

/// SPA 骨架特征（用于决定是否值得升级渲染）。
fn looks_like_spa(html: &str) -> bool {
    const MARKERS: &[&str] = &[
        "id=\"root\"",
        "id='root'",
        "id=\"app\"",
        "id='app'",
        "id=\"__next\"",
        "__NEXT_DATA__",
        "data-reactroot",
        "ng-version",
        "window.__NUXT__",
        "/_next/static/",
        "vite/modulepreload",
    ];
    MARKERS.iter().any(|m| html.contains(m))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ContentKind {
    Html,
    PlainText,
    Json,
    Xml,
    Csv,
    Pdf,
    Unsupported,
}

/// 内容类型分类。无 Content-Type 时按 HTML 试解析（兼容老站）。
fn classify_content_type(ct: Option<&str>) -> ContentKind {
    let Some(ct) = ct else {
        return ContentKind::Html;
    };
    let base = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if base.is_empty() {
        return ContentKind::Html;
    }
    match base.as_str() {
        "text/html" | "application/xhtml+xml" => ContentKind::Html,
        "application/xml" | "text/xml" => ContentKind::Xml,
        "application/json" | "text/json" => ContentKind::Json,
        "text/csv" => ContentKind::Csv,
        "application/pdf" => ContentKind::Pdf,
        _ => {
            if base.starts_with("text/") || base.contains("yaml") {
                ContentKind::PlainText
            } else {
                ContentKind::Unsupported
            }
        }
    }
}

// ── WebView 渲染池 ────────────────────────────────────────────────────────

static RENDER_POOL: OnceLock<RenderPool> = OnceLock::new();

struct RenderPool {
    /// 空闲窗口（`std::sync::Mutex` 只在同步切片内持有，绝不跨 await）。
    free: Mutex<Vec<WebviewWindow>>,
    permits: tokio::sync::Semaphore,
    next_label: AtomicUsize,
}

impl RenderPool {
    async fn acquire(&self) -> anyhow::Result<RenderSlot<'_>> {
        let permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| anyhow!("渲染池已关闭"))?;
        let reused = self.free.lock().map_err(|_| anyhow!("渲染池锁中毒"))?.pop();
        let window = match reused {
            Some(w) => w,
            None => {
                let app = APP_HANDLE
                    .get()
                    .context("webview renderer not initialized")?;
                let label = format!(
                    "fetcher-{}",
                    self.next_label.fetch_add(1, Ordering::Relaxed)
                );
                create_window(app, &label)?
            }
        };
        Ok(RenderSlot {
            pool: self,
            window: Some(window),
            _permit: permit,
        })
    }
}

fn render_pool() -> &'static RenderPool {
    RENDER_POOL.get_or_init(|| RenderPool {
        free: Mutex::new(Vec::new()),
        permits: tokio::sync::Semaphore::new(RENDER_POOL_SIZE),
        next_label: AtomicUsize::new(0),
    })
}

/// 借用中的渲染窗口；Drop 时归还池中。
struct RenderSlot<'a> {
    pool: &'a RenderPool,
    window: Option<WebviewWindow>,
    _permit: tokio::sync::SemaphorePermit<'a>,
}

impl RenderSlot<'_> {
    fn window(&self) -> &WebviewWindow {
        self.window.as_ref().expect("slot window taken")
    }
}

impl Drop for RenderSlot<'_> {
    fn drop(&mut self) {
        if let Some(window) = self.window.take() {
            if let Ok(mut free) = self.pool.free.lock() {
                free.push(window);
            }
        }
    }
}

fn create_window(app: &AppHandle, label: &str) -> anyhow::Result<WebviewWindow> {
    WebviewWindowBuilder::new(
        app,
        label,
        WebviewUrl::External("about:blank".parse().expect("about:blank is a valid url")),
    )
    .title("")
    .visible(false)
    .inner_size(1280.0, 2000.0)
    // 每次导航（含服务端 3xx）都过内网闸：防止跳转到 loopback 管理端点
    .on_navigation(|url| {
        if url.scheme() == "about" {
            return true;
        }
        match crate::net_guard::blocked_literal_reason(url) {
            Some(reason) => {
                tracing::warn!(url = %url, reason, "mcp: blocked private navigation in render window");
                false
            }
            None => true,
        }
    })
    .build()
    .map_err(|e| anyhow!("create render window failed: {e}"))
}

/// WebView 渲染单页 → Markdown。
async fn render_page(url: &Url) -> anyhow::Result<Page> {
    let pool = render_pool();
    let slot = pool.acquire().await?;
    let window = slot.window();
    window.navigate(url.clone()).context("navigate failed")?;
    wait_for_load(window).await?;
    let (final_url, html) = extract_page(window).await?;
    let extract_url = final_url.clone();
    let (markdown, title) = tokio::task::spawn_blocking(move || {
        let extracted = crate::content::extract(&html, &extract_url);
        let md = page_to_markdown(&extracted, &html);
        (md, extracted.title)
    })
    .await
    .map_err(|e| anyhow!("内容处理任务失败：{e}"))?;
    Ok(Page {
        title: Some(title).filter(|t| !t.trim().is_empty()),
        final_url,
        source: Source::Rendered,
        note: None,
        markdown,
    })
}

/// 轮询等待页面加载完成：`readyState == "complete"` 且资源计数稳定（SPA 渲染完成信号）。
///
/// 初始页 about:blank 的 `readyState` 恒为 `complete`，必须等 `location.href`
/// 离开初始页再进入资源稳定阶段，否则会在导航生效前误判完成。
///
/// 窗口刚创建时 wry 会把 eval 排队、**回调直接丢弃**（`pending_scripts` 机制，
/// macOS/WebKitGTK 同款），oneshot 立即关闭——因此 eval 失败一律视为「未就绪」
/// 继续轮询，直到截止时间。
async fn wait_for_load(window: &WebviewWindow) -> anyhow::Result<()> {
    let deadline = Instant::now() + NAVIGATE_TIMEOUT;

    // 阶段一：导航生效 + readyState complete。
    loop {
        let state = eval_string(window, "document.readyState").await;
        let href = eval_string(window, "location.href").await;
        if let (Ok(Some(state)), Ok(Some(href))) = (&state, &href) {
            if state == "complete" && href != "about:blank" {
                break;
            }
        }
        if Instant::now() > deadline {
            return Err(anyhow!(
                "page load timed out (readyState: {state:?}, href: {href:?})"
            ));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // 阶段二：资源计数连续三次一致视为渲染完成；到截止时间仍未稳定也继续（不强求）。
    let mut prev: Option<u32> = None;
    let mut stable = 0u32;
    while Instant::now() < deadline {
        let n = match eval_string(window, "performance.getEntriesByType('resource').length").await {
            Ok(v) => v.and_then(|s| s.parse::<u32>().ok()),
            Err(e) => {
                tracing::debug!(error = %e, "mcp: eval failed during settle poll, retrying");
                None
            }
        };
        if n == prev {
            stable += 1;
            if stable >= 3 {
                break;
            }
        } else {
            stable = 0;
        }
        prev = n;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Ok(())
}

/// eval 取渲染后 HTML 与最终 URL（JS 侧先截断，防回传体积过大）。
async fn extract_page(window: &WebviewWindow) -> anyhow::Result<(String, String)> {
    const MAX: usize = MAX_JS_RETURN_CHARS;
    let js = format!(
        r#"(() => {{
  const root = document.documentElement || document;
  let html = root.outerHTML || '';
  if (html.length > {MAX}) html = html.slice(0, {MAX});
  return JSON.stringify({{ href: location.href, html: html }});
}})()"#
    );
    // 同 wait_for_load：刚就绪的窗口 eval 可能被排队丢回调，重试到截止时间。
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match eval_string(window, &js).await {
            Ok(Some(raw)) => {
                let parsed: serde_json::Value =
                    serde_json::from_str(&raw).context("解析页面回传失败")?;
                let href = parsed
                    .get("href")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let html = parsed
                    .get("html")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                if html.is_empty() {
                    bail!("page returned no content");
                }
                return Ok((href, html));
            }
            Ok(None) => {}
            Err(e) => tracing::debug!(error = %e, "mcp: eval failed during extract, retrying"),
        }
        if Instant::now() > deadline {
            return Err(anyhow!("page returned no content"));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// eval 返回字符串的 JS 表达式：结果经 JSON 序列化回调回传（`eval_with_callback`）。
///
/// 回调在 Tauri 主线程触发，经 oneshot 桥回等待的调用方；导航途中 JS 上下文
/// 可能不可用（回调空串），返回 `None` 由轮询逻辑兜底。
async fn eval_string(window: &WebviewWindow, js: &str) -> anyhow::Result<Option<String>> {
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    // 回调签名是 Fn（可能被多次调用），oneshot send 消费自身 → Mutex<Option> 只发一次。
    let tx = std::sync::Mutex::new(Some(tx));
    window.eval_with_callback(js.to_string(), move |json| {
        if let Some(tx) = tx.lock().ok().and_then(|mut t| t.take()) {
            let _ = tx.send(json);
        }
    })?;
    let json = tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .map_err(|_| anyhow!("eval timed out"))?
        .map_err(|_| anyhow!("eval channel closed"))?;
    if json.is_empty() {
        return Ok(None);
    }
    // 字符串结果以 JSON 编码回传（如 `"complete"`）；非字符串（数字等）直接透传。
    match serde_json::from_str::<String>(&json) {
        Ok(v) => Ok(Some(v)),
        Err(_) => Ok(Some(json)),
    }
}

// ── 内容缓存 ──────────────────────────────────────────────────────────────

type CacheEntry = (Instant, Arc<Page>);
type CacheMap = Mutex<HashMap<String, CacheEntry>>;

static CACHE: OnceLock<CacheMap> = OnceLock::new();

fn cache_map() -> &'static CacheMap {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_get(key: &str) -> Option<Arc<Page>> {
    let now = Instant::now();
    let guard = cache_map().lock().ok()?;
    match guard.get(key) {
        Some((ts, page)) if now.duration_since(*ts) < CACHE_TTL => Some(page.clone()),
        _ => None,
    }
}

fn cache_put(key: &str, page: Arc<Page>) {
    let now = Instant::now();
    let Ok(mut guard) = cache_map().lock() else {
        return;
    };
    guard.retain(|_, (ts, _)| now.duration_since(*ts) < CACHE_TTL);
    while guard.len() >= CACHE_CAPACITY {
        let Some(oldest) = guard
            .iter()
            .min_by_key(|(_, (ts, _))| *ts)
            .map(|(k, _)| k.clone())
        else {
            break;
        };
        guard.remove(&oldest);
    }
    guard.insert(key.to_string(), (now, page));
}

/// 缓存键：去掉 fragment 与跟踪参数（`utm_*` 等），避免同一页面反复抓取。
fn cache_key(url: &Url) -> String {
    let mut normalized = url.clone();
    normalized.set_fragment(None);
    let pairs: Vec<(String, String)> = normalized
        .query_pairs()
        .filter(|(k, _)| !is_tracking_param(k))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    normalized.set_query(None);
    if !pairs.is_empty() {
        let mut query = normalized.query_pairs_mut();
        for (k, v) in &pairs {
            query.append_pair(k, v);
        }
    }
    normalized.to_string()
}

fn is_tracking_param(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.starts_with("utm_")
        || matches!(
            key.as_str(),
            "gclid" | "fbclid" | "yclid" | "msclkid" | "spm" | "_hsenc" | "_hsmi" | "ref_src"
        )
}

// ── 输出框架 ──────────────────────────────────────────────────────────────

/// 输出：标题 + 元数据头 + 指定区间正文 + 截断提示。
fn frame_output(page: &Page, start_index: usize, max_chars: usize) -> String {
    let total = page.markdown.chars().count();
    let start = start_index.min(total);
    let max = max_chars.clamp(MIN_MAX_CHARS, HARD_MAX_CHARS);
    let end = (start + max).min(total);
    let body: String = page
        .markdown
        .chars()
        .skip(start)
        .take(end - start)
        .collect();

    let mut out = String::new();
    if let Some(title) = page
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        out.push_str(&format!("# {title}\n\n"));
    }
    if let Some(note) = page.note.as_deref() {
        out.push_str(&format!("> 注意：{note}\n"));
    }
    out.push_str(&format!("> URL: {}\n", page.final_url));
    out.push_str(&format!("> 抓取方式: {}\n", page.source.label()));
    if total == 0 {
        out.push_str("> 正文: 0 字符\n");
    } else {
        out.push_str(&format!("> 正文: {total} 字符；本次返回 {start}–{end}\n"));
    }
    out.push('\n');
    out.push_str(&body);
    if total > 0 && start >= total {
        out.push_str("\n\n（start_index 超出正文长度，未返回内容）");
    } else if end < total {
        out.push_str(&format!(
            "\n\n…（已截断：剩余 {} 字符，可设置 start_index={end} 继续读取）",
            total - end
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(title: Option<&str>, body: &str) -> Page {
        Page {
            title: title.map(str::to_string),
            final_url: "https://example.com/a".to_string(),
            source: Source::Static,
            note: None,
            markdown: body.to_string(),
        }
    }

    #[test]
    fn test_normalize_url() {
        assert_eq!(
            normalize_url("https://a.com").unwrap().as_str(),
            "https://a.com/"
        );
        assert_eq!(
            normalize_url("http://a.com").unwrap().as_str(),
            "http://a.com/"
        );
        assert_eq!(
            normalize_url("example.com/x").unwrap().as_str(),
            "https://example.com/x"
        );
        assert_eq!(
            normalize_url("  example.com  ").unwrap().as_str(),
            "https://example.com/"
        );
        assert!(normalize_url("").is_err());
        assert!(normalize_url("   ").is_err());
        // 非 http(s) 协议一律拒绝
        assert!(normalize_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn test_classify_content_type() {
        assert_eq!(classify_content_type(None), ContentKind::Html);
        assert_eq!(
            classify_content_type(Some("text/html; charset=utf-8")),
            ContentKind::Html
        );
        assert_eq!(
            classify_content_type(Some("application/xhtml+xml")),
            ContentKind::Html
        );
        assert_eq!(
            classify_content_type(Some("application/json")),
            ContentKind::Json
        );
        assert_eq!(
            classify_content_type(Some("application/xml")),
            ContentKind::Xml
        );
        assert_eq!(classify_content_type(Some("text/csv")), ContentKind::Csv);
        assert_eq!(
            classify_content_type(Some("application/pdf")),
            ContentKind::Pdf
        );
        assert_eq!(
            classify_content_type(Some("text/plain")),
            ContentKind::PlainText
        );
        assert_eq!(
            classify_content_type(Some("application/octet-stream")),
            ContentKind::Unsupported
        );
    }

    #[test]
    fn test_cache_key_drops_tracking_and_fragment() {
        let a = Url::parse("https://example.com/a?utm_source=x&id=1#frag").unwrap();
        let b = Url::parse("https://example.com/a?id=1").unwrap();
        assert_eq!(cache_key(&a), cache_key(&b));
        // 非跟踪参数保留
        let c = Url::parse("https://example.com/a?id=2").unwrap();
        assert_ne!(cache_key(&a), cache_key(&c));
    }

    #[test]
    fn test_looks_like_spa() {
        assert!(looks_like_spa("<div id=\"root\"></div>"));
        assert!(looks_like_spa("<script id=\"__NEXT_DATA__\"></script>"));
        assert!(!looks_like_spa("<article><p>纯静态文章</p></article>"));
    }

    #[test]
    fn test_page_to_markdown_basic() {
        let html = "<html><body><h1>标题</h1><p>这是一段足够长的正文内容，用于让 readability \
                    提取器判定为有效正文而非空壳页面。</p></body></html>";
        let extracted = crate::content::extract(html, "https://example.com/a");
        let md = page_to_markdown(&extracted, html);
        assert!(md.contains("这是一段足够长的正文内容"));
    }

    #[test]
    fn test_page_to_markdown_drops_nav() {
        let html = r#"<html><head><title>文章标题 - 示例站点</title></head><body>
            <nav><a href="/">首页</a><a href="/about">关于我们</a><a href="/x">导航链接</a></nav>
            <article>
                <p>Tokio JoinSet 是并发任务管理器，允许同时 spawn 多个任务并统一收集结果。</p>
                <p>配合 timeout 可实现整体超时控制，JoinSet drop 时会自动 abort 残留任务。</p>
                <p>总体而言，它适合扇出型的并发场景，完成顺序即 join_next 的返回顺序。</p>
            </article>
            <footer><p>版权所有 保留一切权利 备案号某某某</p></footer>
        </body></html>"#;
        let extracted = crate::content::extract(html, "https://example.com/article");
        let md = page_to_markdown(&extracted, html);
        assert!(md.contains("并发任务管理器"));
        assert!(!md.contains("导航链接"), "nav leaked");
        assert!(!md.contains("关于我们"), "nav leaked");
        assert!(!md.contains("版权所有"), "footer leaked");
        assert!(
            extracted.title.starts_with("文章标题"),
            "title: {}",
            extracted.title
        );
    }

    #[test]
    fn test_frame_output_full() {
        let p = page(Some("标题"), "正文内容");
        let out = frame_output(&p, 0, DEFAULT_MAX_CHARS);
        assert!(out.starts_with("# 标题\n\n"));
        assert!(out.contains("> URL: https://example.com/a"));
        assert!(out.contains("> 抓取方式: 静态抓取"));
        assert!(out.contains("本次返回 0–4"));
        assert!(out.ends_with("正文内容"));
        assert!(!out.contains("已截断"));
    }

    #[test]
    fn test_frame_output_slices_and_notes_remaining() {
        let body: String = "0123456789".repeat(10); // 100 字符
        let p = page(None, &body);
        let out = frame_output(&p, 10, MIN_MAX_CHARS);
        // max_chars 被 clamp 到下限 1000 > 总长，返回 10..100
        assert!(out.contains("本次返回 10–100"));
        assert!(out.contains(&"0123456789".repeat(9)));
        assert!(!out.contains("已截断"));
        assert!(!out.starts_with('#'));

        // 缩小上限以触发截断提示：直接用总长以下的值走 clamp 下限以上的分支
        let long_body: String = "x".repeat(MIN_MAX_CHARS + 500);
        let p2 = page(None, &long_body);
        let out2 = frame_output(&p2, 0, MIN_MAX_CHARS);
        assert!(out2.contains(&format!("本次返回 0–{MIN_MAX_CHARS}")));
        assert!(out2.contains("剩余 500 字符"));
        assert!(out2.contains(&format!("start_index={MIN_MAX_CHARS}")));
    }

    #[test]
    fn test_frame_output_start_beyond_end() {
        let p = page(None, "abc");
        let out = frame_output(&p, 99, DEFAULT_MAX_CHARS);
        assert!(out.contains("start_index 超出正文长度"));
    }

    /// 真实网络端到端：静态抓取 → 正文提取 → 分段读取 → 缓存命中 → SSRF 拒绝。
    /// 默认 `#[ignore]`（依赖外网），手动触发：
    /// `cargo test --lib e2e_static_fetch -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "需要真实网络访问 example.com"]
    async fn e2e_static_fetch_real_network() {
        let out = fetch_url("https://example.com", 0, DEFAULT_MAX_CHARS)
            .await
            .expect("static fetch should succeed");
        assert!(out.contains("> 抓取方式: 静态抓取"), "output: {out}");
        assert!(out.contains("Example Domain"), "output: {out}");
        assert!(out.contains("> URL: https://example.com/"), "output: {out}");

        // 缓存命中：第二次调用不应改变内容
        let again = fetch_url("https://example.com", 0, DEFAULT_MAX_CHARS)
            .await
            .expect("cached fetch should succeed");
        assert_eq!(out, again);

        // 分段读取：从中间开始，头部标注区间
        let tail = fetch_url("https://example.com", 100, 1000)
            .await
            .expect("sliced fetch should succeed");
        assert!(tail.contains("本次返回 100–"), "output: {tail}");

        // SSRF：本机管理端点必须被拒绝
        let err = fetch_url("http://127.0.0.1:19068/api/settings", 0, 1000)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("不允许抓取"), "error: {err}");
    }

    #[test]
    fn test_cache_put_get_and_ttl() {
        let key = "https://cache.test/ttl";
        let p = Arc::new(page(None, "x"));
        cache_put(key, p);
        assert!(cache_get(key).is_some());
        // 过期条目在下次写入时被清理
        {
            let mut guard = cache_map().lock().unwrap();
            guard.insert(
                key.to_string(),
                (Instant::now() - CACHE_TTL * 2, Arc::new(page(None, "y"))),
            );
        }
        assert!(cache_get(key).is_none());
        cache_put("https://cache.test/other", Arc::new(page(None, "z")));
        assert!(cache_map().lock().unwrap().get(key).is_none());
    }
}
