//! WebFetch 渲染层：复用 Tauri 自带 WebView（macOS WKWebView / Windows WebView2 /
//! Linux WebKitGTK）执行页面 JS 后取正文（Markdown）。
//!
//! 设计取舍（见 docs/DECISIONS.md ADR-038）：
//! - **不探测本机 Chrome/Edge**——网关进程即 Tauri 进程（窗口关闭只隐藏到托盘），
//!   WebView 可用性 ≈ 应用可用性：三端全覆盖，无额外浏览器进程，无需 headless_chrome。
//! - **隐藏窗口 + Rust 单向 eval**——远程页面拿不到 Tauri IPC（不配 remote 域
//!   capability，也无 `withGlobalTauri`），HTML 提取完全由 Rust 发起（eval_with_callback）。
//! - **懒创建 + 全局保活**——首次抓取建隐藏窗口，之后复用；抓取间 `tokio::sync::Mutex`
//!   串行（跨 await 持有，一次渲染一页）。
//! - **渲染等待**——轮询 `readyState == "complete"` 且资源计数稳定（SPA 渲染完成信号），
//!   不是固定 sleep；导航失败（DNS/TLS 等）时 readyState 停驻 → 超时报错。
//! - **降级路径**——WebView 创建失败/超时/eval 异常 → 静态抓取（`crate::http`
//!   客户端，继承系统代理）并在输出开头注明「可能不含 JS 渲染结果」。

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use tauri::{AppHandle, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use url::Url;

/// 页面加载（等待导航稳定）超时。
const NAVIGATE_TIMEOUT: Duration = Duration::from_secs(30);
/// 输出正文上限（按字符计），超出截断并附注。
const MAX_OUTPUT_CHARS: usize = 60_000;
/// JS 侧回传 HTML 上限（防 WebView2 `ExecuteScriptWithResult` 结果体积限制）。
const MAX_JS_RETURN_CHARS: usize = 1_500_000;
/// 隐藏渲染窗口的 label。
const RENDER_WINDOW_LABEL: &str = "fetcher";

/// 全局 AppHandle（`mcp::init` 注入，`lib.rs` setup 调用）。
static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();
/// 全局渲染窗口（懒创建，进程级保活）。
static RENDER_WINDOW: OnceLock<Mutex<Option<WebviewWindow>>> = OnceLock::new();
/// 抓取串行锁：跨 await 持有，须 tokio Mutex（一次渲染一页）。
static FETCH_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

/// 注入 AppHandle（`lib.rs` setup 创建 AppState 后调用）。
pub(crate) fn init(app: AppHandle) {
    let _ = APP_HANDLE.set(app);
}

/// 抓取页面正文（Markdown）。
///
/// 优先 Tauri WebView 渲染（JS 执行，SPA 内容可得）；
/// 渲染不可用/失败 → 静态抓取，开头注明「可能不含 JS 渲染结果」。
pub(super) async fn fetch_url(url: &str) -> anyhow::Result<String> {
    let normalized = normalize_url(url)?;
    match render_with_webview(&normalized).await {
        Ok(html) => Ok(html_to_markdown(html, normalized.clone()).await),
        Err(render_err) => {
            tracing::warn!(
                url = %normalized,
                error = %render_err,
                "mcp: webview render failed, falling back to static fetch"
            );
            let html = static_fetch(&normalized).await?;
            let body = html_to_markdown(html, normalized.clone()).await;
            Ok(format!(
                "（WebView 渲染不可用：{render_err}。以下为静态抓取内容，可能不含 JS 渲染结果）\n{body}"
            ))
        }
    }
}

/// URL 归一化：接受 `http`/`https`；无协议时补 `https://`。
fn normalize_url(url: &str) -> anyhow::Result<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("empty url"));
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return Ok(trimmed.to_string());
    }
    Ok(format!("https://{trimmed}"))
}

/// WebView 渲染单页：懒创建隐藏窗口 → 导航 → 轮询加载完成 → eval 取渲染后 HTML。
async fn render_with_webview(url: &str) -> anyhow::Result<String> {
    let _guard = FETCH_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let app = APP_HANDLE.get().context("webview renderer not initialized")?;
    let window = get_or_create_window(app)?;
    window.navigate(Url::parse(url).context("invalid url")?)?;
    wait_for_load(&window).await?;
    extract_html(&window).await
}

/// 懒创建隐藏渲染窗口（进程级保活复用）。
fn get_or_create_window(app: &AppHandle) -> anyhow::Result<WebviewWindow> {
    let slot = RENDER_WINDOW.get_or_init(|| Mutex::new(None));
    let mut guard = slot
        .lock()
        .map_err(|_| anyhow!("render window slot poisoned"))?;
    if let Some(w) = guard.as_ref() {
        return Ok(w.clone());
    }
    let window = WebviewWindowBuilder::new(
        app,
        RENDER_WINDOW_LABEL,
        WebviewUrl::External("about:blank".parse().expect("about:blank is a valid url")),
    )
    .title("")
    .visible(false)
    .inner_size(1280.0, 2000.0)
    .build()
    .map_err(|e| anyhow!("create render window failed: {e}"))?;
    tracing::info!("mcp: web_fetch render window created (hidden)");
    let _ = guard.insert(window.clone());
    Ok(window)
}

/// 轮询等待页面加载完成：`readyState == "complete"` 且资源计数稳定（SPA 渲染完成信号）。
///
/// 初始页 about:blank 的 readyState 恒为 `complete`，必须等 `location.href`
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
        let n = match eval_string(window, "performance.getEntriesByType('resource').length").await
        {
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

/// eval 取渲染后 HTML（JS 侧先截断，防回传体积过大）。
async fn extract_html(window: &WebviewWindow) -> anyhow::Result<String> {
    const MAX: usize = MAX_JS_RETURN_CHARS;
    let js = format!(
        r#"(() => {{
  const root = document.documentElement || document;
  let html = root.outerHTML || '';
  if (html.length > {MAX}) html = html.slice(0, {MAX});
  return html;
}})()"#
    );
    // 同 wait_for_load：刚就绪的窗口 eval 可能被排队丢回调，重试到截止时间。
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match eval_string(window, &js).await {
            Ok(Some(html)) => return Ok(html),
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

/// 回退：静态抓取（继承系统代理，不执行 JS）。
async fn static_fetch(url: &str) -> anyhow::Result<String> {
    let client = crate::http::build_http_client()
        .timeout(NAVIGATE_TIMEOUT)
        .build()
        .map_err(|e| anyhow!("failed to build http client: {e}"))?;
    let html = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("fetch failed for {url}"))?
        .error_for_status()
        .map_err(|e| anyhow!("fetch failed: {e}"))?
        .text()
        .await
        .map_err(|e| anyhow!("read body failed: {e}"))?;
    Ok(html)
}

/// 渲染后 HTML → readability 正文提取 → Markdown → 截断。
///
/// 提取质量过低（`quality < EXTRACTION_QUALITY_THRESHOLD`）或正文转出的 Markdown
/// 太短时，降级为整页 `htmd::convert`（现状行为兜底，两层降级链保留）。
const EXTRACTION_QUALITY_THRESHOLD: f32 = 0.35;

/// 同步转换（在 blocking 线程执行）。
fn html_to_markdown_sync(html: &str, url: &str) -> String {
    let page = crate::content::extract(html, url);
    if page.quality >= EXTRACTION_QUALITY_THRESHOLD && !page.html.is_empty() {
        if let Ok(md) = htmd::convert(&page.html) {
            if md.chars().count() >= 80 {
                let title = page.title.trim();
                let full = if title.is_empty() {
                    md
                } else {
                    format!("# {title}\n\n{md}")
                };
                return trim_output(full);
            }
        }
    }
    // 降级：整页转换，htmd 失败再退化为原始 HTML
    trim_output(htmd::convert(html).unwrap_or_else(|_| html.to_string()))
}

/// 异步入口：`scraper::Html` 非 Send 且大页面解析 CPU 密集，整体卸载到
/// blocking 线程池。join 失败（极罕见）返回降级提示——原始 html 已 move 进闭包，
/// 且此处 panic 意味着提取器崩溃，不宜再走重逻辑。
async fn html_to_markdown(html: String, url: String) -> String {
    match tokio::task::spawn_blocking(move || html_to_markdown_sync(&html, &url)).await {
        Ok(md) => md,
        Err(e) => {
            tracing::warn!(error = %e, "mcp: html_to_markdown blocking task failed");
            "（网页内容处理失败，未能提取正文。）".to_string()
        }
    }
}

/// 按字符截断到 `MAX_OUTPUT_CHARS`，截断处附注。
fn trim_output(text: String) -> String {
    let count = text.chars().count();
    if count <= MAX_OUTPUT_CHARS {
        return text;
    }
    let truncated: String = text.chars().take(MAX_OUTPUT_CHARS).collect();
    format!("{truncated}\n\n…（内容过长，已截断 {} 字符）", count - MAX_OUTPUT_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_url() {
        assert_eq!(normalize_url("https://a.com").unwrap(), "https://a.com");
        assert_eq!(normalize_url("http://a.com").unwrap(), "http://a.com");
        assert_eq!(normalize_url("example.com/x").unwrap(), "https://example.com/x");
        assert_eq!(normalize_url("  example.com  ").unwrap(), "https://example.com");
        assert!(normalize_url("").is_err());
        assert!(normalize_url("   ").is_err());
    }

    #[test]
    fn test_trim_output_noop_under_limit() {
        let s = "abc".to_string();
        assert_eq!(trim_output(s.clone()), s);
        let exact: String = "字".repeat(MAX_OUTPUT_CHARS);
        assert_eq!(trim_output(exact.clone()), exact);
    }

    #[test]
    fn test_trim_output_truncates_over_limit() {
        let s = "x".repeat(MAX_OUTPUT_CHARS + 100);
        let out = trim_output(s);
        assert!(out.starts_with('x'));
        assert!(out.contains("已截断"));
        // 截断后总长度 = 上限 + 附注
        assert!(out.chars().count() > MAX_OUTPUT_CHARS);
    }

    #[test]
    fn test_html_to_markdown_basic() {
        let md = html_to_markdown_sync(
            "<html><body><h1>标题</h1><p>这是一段足够长的正文内容，用于让 readability \
             提取器判定为有效正文而非空壳页面。</p></body></html>",
            "https://example.com/a",
        );
        assert!(md.contains("标题"));
        assert!(md.contains("这是一段足够长的正文内容"));
    }

    #[test]
    fn test_html_to_markdown_extracts_title_and_drops_nav() {
        let html = r#"<html><head><title>文章标题 - 示例站点</title></head><body>
            <nav><a href="/">首页</a><a href="/about">关于我们</a><a href="/x">导航链接</a></nav>
            <article>
                <p>Tokio JoinSet 是并发任务管理器，允许同时 spawn 多个任务并统一收集结果。</p>
                <p>配合 timeout 可实现整体超时控制，JoinSet drop 时会自动 abort 残留任务。</p>
                <p>总体而言，它适合扇出型的并发场景，完成顺序即 join_next 的返回顺序。</p>
            </article>
            <footer><p>版权所有 保留一切权利 备案号某某某</p></footer>
        </body></html>"#;
        let md = html_to_markdown_sync(html, "https://example.com/article");
        // 输出以 `# {title}` 开头，且标题已剥站点名后缀
        assert!(
            md.starts_with("# 文章标题"),
            "md head: {}",
            md.chars().take(30).collect::<String>()
        );
        // 正文保留
        assert!(md.contains("并发任务管理器"));
        // 导航/页脚被剔除
        assert!(!md.contains("导航链接"), "nav leaked");
        assert!(!md.contains("关于我们"), "nav leaked");
        assert!(!md.contains("版权所有"), "footer leaked");
    }

    #[test]
    fn test_html_to_markdown_low_quality_falls_back_to_fullpage() {
        // 几乎无正文的空壳页：quality 低 → 降级整页转换（仍返回内容，不报错）
        let html = "<html><body><div id=\"root\"><span>a</span></div></body></html>";
        let md = html_to_markdown_sync(html, "https://spa.example.com/");
        // 降级路径不强制 `# title` 头；此处内容极少，断言不 panic 即可
        assert!(md.chars().count() < 100);
    }
}
