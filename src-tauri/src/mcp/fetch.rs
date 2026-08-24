//! WebFetch 渲染层：复用本机 Chrome/Edge headless 执行页面 JS 后取正文（Markdown）。
//!
//! 设计取舍（见 docs/DECISIONS.md）：
//! - **不自动下载浏览器**——Windows 自带 Edge，探测命中率基本 100%；探测失败回退
//!   静态抓取（`crate::http::http_client()`，继承系统代理）并在输出里明确说明。
//! - **同步 headless_chrome 进 `spawn_blocking`**——CDP 交互是同步 API，
//!   项目里同步代码进阻塞池是既有模式（rusqlite）。
//! - **浏览器实例懒启动 + 全局保活**——每次抓取新开 Tab、用完即关；进程不主动退出，
//!   下次抓取免 ~500ms 冷启动。单用户场景用 Mutex 串行化（一次只渲染一页）。

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, Context};
use headless_chrome::{Browser, LaunchOptions};

/// 页面加载（等待导航稳定）超时。
const NAVIGATE_TIMEOUT: Duration = Duration::from_secs(30);
/// 输出正文上限（按字符计），超出截断并附注。
const MAX_OUTPUT_CHARS: usize = 60_000;

/// 全局浏览器实例（懒启动，进程级保活）。
static BROWSER: OnceLock<Mutex<Option<Browser>>> = OnceLock::new();

fn browser_slot() -> &'static Mutex<Option<Browser>> {
    BROWSER.get_or_init(|| Mutex::new(None))
}

/// 抓取页面正文（Markdown）。
///
/// 探测到本机浏览器 → headless 渲染（JS 执行，SPA 内容可得）；
/// 否则静态抓取并在开头注明「可能不含 JS 渲染结果」。
pub(super) async fn fetch_url(url: &str) -> anyhow::Result<String> {
    let normalized = normalize_url(url)?;

    if browser_executable().is_some() {
        let rendered = tokio::task::spawn_blocking({
            let u = normalized.clone();
            move || render_page(&u)
        })
        .await
        .map_err(|e| anyhow!("render task failed: {e}"))??;
        return Ok(html_to_markdown(&rendered));
    }

    // 回退：静态抓取（本机没有可用的 Chrome/Edge）。
    let client = crate::http::build_http_client()
        .timeout(NAVIGATE_TIMEOUT)
        .build()
        .map_err(|e| anyhow!("failed to build http client: {e}"))?;
    let html = client
        .get(&normalized)
        .send()
        .await
        .with_context(|| format!("fetch failed for {normalized}"))?
        .error_for_status()
        .map_err(|e| anyhow!("fetch failed: {e}"))?
        .text()
        .await
        .map_err(|e| anyhow!("read body failed: {e}"))?;
    Ok(format!(
        "（未检测到本机 Chrome/Edge，以下为静态抓取内容，可能不含 JS 渲染结果）\n{}",
        html_to_markdown(&html)
    ))
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

/// headless 渲染单页：懒启动浏览器 → 新 Tab → 导航 → 等待 → 取渲染后 HTML → 关 Tab。
fn render_page(url: &str) -> anyhow::Result<String> {
    let slot = browser_slot();
    let mut guard = slot.lock().map_err(|_| anyhow!("browser lock poisoned"))?;

    let browser = match guard.as_ref() {
        Some(b) => b,
        None => {
            let b = launch_browser()?;
            guard.insert(b)
        }
    };

    let tab = browser.new_tab().map_err(|e| anyhow!("new_tab failed: {e}"))?;
    let result = (|| -> anyhow::Result<String> {
        tab.navigate_to(url)
            .map_err(|e| anyhow!("navigate failed: {e}"))?;
        tab.wait_until_navigated()
            .map_err(|e| anyhow!("page load timed out or failed: {e}"))?;
        tab.get_content()
            .map_err(|e| anyhow!("get_content failed: {e}"))
    })();
    // 无论成败都关掉 Tab，避免泄漏（浏览器进程保活复用）。
    let _ = tab.close(true);
    result
}

/// 启动 headless 浏览器（指向探测到的本机 Chrome/Edge）。
fn launch_browser() -> anyhow::Result<Browser> {
    let path = browser_executable().ok_or_else(|| anyhow!("no local Chrome/Edge found"))?;
    let no_first_run: &std::ffi::OsStr = std::ffi::OsStr::new("--no-first-run");
    let no_default_browser_check: &std::ffi::OsStr =
        std::ffi::OsStr::new("--no-default-browser-check");
    let options = LaunchOptions::default_builder()
        .path(Some(path.to_path_buf()))
        .headless(true)
        .window_size(Some((1280, 2000)))
        .args(vec![no_first_run, no_default_browser_check])
        .build()
        .map_err(|e| anyhow!("invalid launch options: {e}"))?;
    tracing::info!(browser = %path.display(), "mcp: launching headless browser");
    Browser::new(options).map_err(|e| anyhow!("launch browser failed: {e}"))
}

/// 本机浏览器可执行文件探测（结果缓存，进程内只探测一次）。
///
/// 顺序：Windows 优先 Edge（系统自带），其次 Chrome；
/// macOS 对应 .app 内路径；Linux 常见发行版路径。
fn browser_executable() -> Option<&'static std::path::Path> {
    static CANDIDATE: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    CANDIDATE
        .get_or_init(detect_browser_executable)
        .as_deref()
}

/// 纯探测逻辑：按平台给出候选路径列表，取第一个存在的。
fn detect_browser_executable() -> Option<std::path::PathBuf> {
    browser_candidates().into_iter().find(|p| p.exists())
}

#[cfg(target_os = "windows")]
fn browser_candidates() -> Vec<std::path::PathBuf> {
    [
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
    ]
    .iter()
    .map(std::path::PathBuf::from)
    .collect()
}

#[cfg(target_os = "macos")]
fn browser_candidates() -> Vec<std::path::PathBuf> {
    [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ]
    .iter()
    .map(std::path::PathBuf::from)
    .collect()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn browser_candidates() -> Vec<std::path::PathBuf> {
    [
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
    ]
    .iter()
    .map(std::path::PathBuf::from)
    .collect()
}

/// 渲染后 HTML → Markdown → 截断。htmd 失败时退化为原始 HTML 截断。
fn html_to_markdown(html: &str) -> String {
    let md = htmd::convert(html).unwrap_or_else(|_| html.to_string());
    trim_output(md)
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
        let md = html_to_markdown("<html><body><h1>标题</h1><p>正文内容</p></body></html>");
        assert!(md.contains("标题"));
        assert!(md.contains("正文内容"));
    }

    #[test]
    fn test_browser_candidates_non_empty() {
        assert!(!browser_candidates().is_empty());
    }
}
