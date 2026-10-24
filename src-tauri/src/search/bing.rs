//! Bing 搜索引擎（纯 Rust HTTP 请求 + 双域名 fallback）。
//!
//! 背景：早期 HTTP 爬虫（裸 reqwest）被 Bing 识别为非浏览器请求，对中文查询
//! 返回降级「热门站点推荐」结果（实测查询「张雪峰 2026」只返回「张」字的字典
//! 释义）。曾改用隐藏 WebView（WKWebView）执行，实测发现**关键在完整浏览器头**
//! （尤其 `sec-ch-ua` 系列 + UA + Accept-Language），而非 TLS 指纹或 JS 执行——
//! reqwest 带完整浏览器头 + `cookie_store(true)` + 预热（先 GET 主页建 cookie）
//! 即可拿到与 WebView 完全相同的正常结果。
//!
//! 策略：
//! - **专用搜索 client**：完整浏览器头 + `cookie_store(true)` + 直连（不走系统
//!   代理——Bing 对代理出口 IP 会降级）
//! - **懒预热**：首次搜索前 GET `cn.bing.com/` 建 cookie（复用会话），后续搜索
//!   直接复用，不再降级
//! - **www.bing.com 优先**，空壳/失败/降级 → fallback cn.bing.com
//! - **降级检测 + 简化重试**：结果与查询不相关（字典释义页）时用首词重搜
//! - **ck/a 重定向解码**：Bing 结果链接可能是 `www.bing.com/ck/a?u=a1<base64url>`，
//!   base64url 解码还原真实 URL（SearXNG bing.py 同款逻辑）

use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine as _;
use reqwest::header::{
    HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE, UPGRADE_INSECURE_REQUESTS, USER_AGENT,
};
use scraper::{Html, Selector};
use tokio::sync::Mutex;

use super::{SearchParams, SearchResult};

/// 单次 HTTP 搜索的超时（秒）。
const SEARCH_TIMEOUT_SECS: u64 = 20;

/// 搜索 HTTP 客户端：完整浏览器头 + cookie_store 复用 + 懒预热。
///
/// 放在 AppState 中全局复用：
/// - `client` 带 `cookie_store(true)`——cookie 会话持续，后续搜索不降级
/// - `prewarmed` 标记首次预热（GET 主页建 cookie）是否完成
/// - `prewarm_lock` 保证并发首搜时只预热一次
pub struct SearchHttp {
    client: reqwest::Client,
    prewarmed: AtomicBool,
    prewarm_lock: Mutex<()>,
}

impl SearchHttp {
    /// 构建搜索专用 client（完整浏览器头 + cookie_store + 直连）。
    ///
    /// 注意：**不**用 `http::build_http_client()`——它会继承系统代理，Bing 对
    /// 代理出口 IP（海外）返回降级结果。搜索必须直连。
    pub fn new() -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
            ),
        );
        headers.insert(
            ACCEPT,
            HeaderValue::from_static(
                "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8",
            ),
        );
        headers.insert(
            ACCEPT_LANGUAGE,
            HeaderValue::from_static("zh-CN,zh;q=0.9,en;q=0.8"),
        );
        headers.insert(UPGRADE_INSECURE_REQUESTS, HeaderValue::from_static("1"));
        // Client Hints：真实 Chrome 会发送，Bing 据此识别浏览器会话
        headers.insert(
            "sec-ch-ua",
            HeaderValue::from_static("\"Chromium\";v=\"131\", \"Not_A Brand\";v=\"24\""),
        );
        headers.insert("sec-ch-ua-mobile", HeaderValue::from_static("?0"));
        headers.insert("sec-ch-ua-platform", HeaderValue::from_static("\"macOS\""));

        let client = reqwest::Client::builder()
            .cookie_store(true) // 复用 cookie 会话
            .default_headers(headers)
            .build()
            .expect("failed to build search http client");
        Self {
            client,
            prewarmed: AtomicBool::new(false),
            prewarm_lock: Mutex::new(()),
        }
    }

    /// 确保 cookie 会话已建立（首次搜索前 GET 主页预热）。
    ///
    /// 幂等：已预热直接返回；并发首搜由 Mutex 串行化，只预热一次。
    /// 预热失败不阻断搜索（降级检测兜底），仅记 warn。
    pub async fn ensure_prewarmed(&self) {
        if self.prewarmed.load(Ordering::Relaxed) {
            return;
        }
        let _guard = self.prewarm_lock.lock().await;
        if self.prewarmed.load(Ordering::Relaxed) {
            return;
        }
        match self.client.get("https://cn.bing.com/").send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                tracing::debug!(status, "bing: prewarm page fetched (cookie established)");
                self.prewarmed.store(true, Ordering::Relaxed);
            }
            Err(e) => {
                tracing::warn!(error = %e, "bing: prewarm failed (will retry next search)");
            }
        }
    }

    /// 执行一次搜索请求，返回响应文本。
    async fn get(&self, url: url::Url) -> anyhow::Result<String> {
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(SEARCH_TIMEOUT_SECS),
            self.client.get(url).send(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("bing: search timed out ({}s)", SEARCH_TIMEOUT_SECS))?
        .map_err(|e| anyhow::anyhow!("bing: request failed: {}", e))?;

        if !resp.status().is_success() {
            anyhow::bail!("bing: HTTP {}", resp.status().as_u16());
        }
        let text = resp
            .text()
            .await
            .map_err(|e| anyhow::anyhow!("bing: read body failed: {}", e))?;
        Ok(text)
    }
}

impl Default for SearchHttp {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for SearchHttp {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            prewarmed: AtomicBool::new(self.prewarmed.load(Ordering::Relaxed)),
            prewarm_lock: Mutex::new(()),
        }
    }
}

/// 构造 Bing 搜索 URL：查询操作符（site: 过滤）+ count + 相对时间过滤。
///
/// `qft` 是 Bing 的「按时间筛选」参数（分钟阈值），属 best-effort——Bing 可能
/// 忽略它，因此结果侧仍以日期元数据为准。
fn build_search_url(domain: &str, params: &SearchParams) -> url::Url {
    let query = super::build_engine_query(params);
    let count = params.num_results.to_string();
    let mut url = url::Url::parse_with_params(
        &format!("https://{}/search", domain),
        &[
            ("q", query.as_str()),
            ("adlt", "off"),
            ("mkt", "zh-CN"),
            ("count", count.as_str()),
        ],
    )
    .expect("static URL with params cannot fail");
    if let Some(freshness) = params.freshness {
        url.query_pairs_mut()
            .append_pair("qft", freshness.bing_qft());
    }
    url
}

/// 对一个域名执行 HTTP 搜索。
async fn search_domain(
    search: &SearchHttp,
    domain: &str,
    params: &SearchParams,
) -> anyhow::Result<Vec<SearchResult>> {
    let url = build_search_url(domain, params);
    let html = search.get(url).await?;
    tracing::debug!(
        domain,
        query = %params.query,
        html_len = html.len(),
        "bing: http html received"
    );

    Ok(parse_results(&html, params.num_results))
}

/// Bing 结果链接解码：`https://www.bing.com/ck/a?u=a1<base64url>` → 真实 URL。
fn decode_ck_href(href: &str) -> String {
    if let Some(qs) = href.strip_prefix("https://www.bing.com/ck/a?") {
        if let Some(u) = qs.split('&').find_map(|kv| kv.strip_prefix("u=")) {
            if let Some(encoded) = u.strip_prefix("a1") {
                // base64url without padding → 补 padding 后解码
                let padded = format!("{}{}", encoded, "=".repeat((4 - encoded.len() % 4) % 4));
                if let Ok(decoded) =
                    base64::engine::general_purpose::URL_SAFE.decode(padded.as_bytes())
                {
                    if let Ok(s) = String::from_utf8(decoded) {
                        return s;
                    }
                }
            }
        }
    }
    href.to_string()
}

/// 结果 URL 过滤：去掉 Bing 内部跳转/服务页链接。
fn is_external_url(href: &str) -> bool {
    href.starts_with("http")
        && !href.contains("bing.com")
        && !href.contains("microsoft.com")
        && !href.contains("msn.com")
}

/// 解析 Bing 搜索结果：`ol#b_results > li.b_algo` 容器。
///
/// 输入既可以是完整 HTML 文档，也可以是 JS 回传的 `li.b_algo` 容器片段
/// （`Html::parse_document` 对片段同样成立，`ol#b_results li.b_algo` 选择器命中
/// 包含 ol 的片段；实际回传即片段集合）。
fn parse_results(html: &str, limit: usize) -> Vec<SearchResult> {
    let document = Html::parse_document(html);
    let algo_sel = Selector::parse("ol#b_results li.b_algo").unwrap();
    let link_sel = Selector::parse("h2 a").unwrap();
    let p_sel = Selector::parse("p").unwrap();

    let mut results = Vec::new();
    for item in document.select(&algo_sel) {
        let link = match item.select(&link_sel).next() {
            Some(l) => l,
            None => continue,
        };

        let href = link.value().attr("href").unwrap_or("").to_string();
        let title = link.text().collect::<String>().trim().to_string();
        if href.is_empty() || title.is_empty() {
            continue;
        }

        // 解码 Bing 重定向链接为真实 URL
        let url = decode_ck_href(&href);
        if !is_external_url(&url) {
            continue;
        }

        // 摘要：取全部 <p> 文本拼接
        let snippet = item
            .select(&p_sel)
            .map(|p| p.text().collect::<String>())
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string();

        results.push(SearchResult {
            title,
            url,
            snippet,
        });
        if results.len() >= limit {
            break;
        }
    }

    results
}

/// 搜索入口：预热 cookie → www.bing.com 优先，空壳/失败/降级时 fallback cn.bing.com。
pub async fn search(
    search: &SearchHttp,
    params: &SearchParams,
) -> anyhow::Result<Vec<SearchResult>> {
    let query = params.query.as_str();
    // 0. 预热：首次搜索前 GET 主页建 cookie（复用会话，后续不再降级）
    search.ensure_prewarmed().await;

    // 1. www.bing.com（国际版，质量更高）
    match search_domain(search, "www.bing.com", params).await {
        Ok(results) if !results.is_empty() && !is_degraded_results(&results, query) => {
            tracing::info!(query = %query, results_count = results.len(), "bing: www http results");
            return Ok(results);
        }
        Ok(results) if !results.is_empty() => {
            tracing::warn!(query = %query, results_count = results.len(), "bing: www degraded results, retrying with simplified query");
        }
        Ok(_) => {
            tracing::debug!(query = %query, "bing: www empty (shell page?), fallback to cn");
        }
        Err(e) => {
            tracing::debug!(query = %query, error = %e, "bing: www failed, fallback to cn");
        }
    }

    // 1.5 降级页检测：结果全是字典/百科释义（Bing 对热点人名+附加词的风控），
    // 改用简化查询（仅第一个词）重搜——实测「张雪峰 高考志愿」降级，
    // 但「张雪峰」单搜正常
    if let Some(simplified) = simplify_query(query) {
        tracing::info!(query = %query, simplified = %simplified, "bing: retrying with simplified query");
        match search_domain(search, "www.bing.com", &params.with_query(&simplified)).await {
            Ok(results) if !results.is_empty() && !is_degraded_results(&results, query) => {
                tracing::info!(query = %simplified, results_count = results.len(), "bing: www simplified results");
                return Ok(results);
            }
            Ok(_) => {
                tracing::debug!(query = %simplified, "bing: www simplified also degraded/empty");
            }
            Err(e) => {
                tracing::debug!(query = %simplified, error = %e, "bing: www simplified failed");
            }
        }
    }

    // 2. cn.bing.com 兜底
    let results = search_domain(search, "cn.bing.com", params).await?;
    tracing::info!(query = %query, results_count = results.len(), "bing: cn http results");
    Ok(results)
}

/// 降级结果检测：结果与查询不相关时判定为 Bing 降级页。
///
/// 触发特征：热点人名 + 附加词的查询被 Bing 风控，返回与查询无关的
/// 单字字典释义页（查「张雪峰 高考志愿」返回「张（汉语汉字）」）。
/// 用「结果是否包含查询首词」判断相关性——正常搜索结果标题/摘要几乎
/// 都包含核心实体（首词），降级页则几乎都不包含。
fn is_degraded_results(results: &[SearchResult], query: &str) -> bool {
    if results.len() < 3 {
        return false;
    }
    let first_word = query.split_whitespace().next().unwrap_or("");
    // 首词太短（虚词/单字）不可靠，不判定
    let clean = first_word.trim_matches('"');
    if clean.chars().count() < 2 {
        return false;
    }
    // 检查结果标题/摘要里包含查询首词的比例（大小写不敏感）
    let containing = results
        .iter()
        .filter(|r| {
            r.title.to_lowercase().contains(&clean.to_lowercase())
                || r.snippet.to_lowercase().contains(&clean.to_lowercase())
        })
        .count();
    let ratio = containing as f64 / results.len() as f64;
    // 少于 30% 的结果包含查询首词 → 降级
    ratio < 0.3
}

/// 简化查询：取查询的第一个「词」（按空白/引号切分），用于降级重试。
///
/// 实测：Bing 对「张雪峰 高考志愿」这类热点人名+附加词降级，但单查人名
/// 「张雪峰」返回正常结果。返回 None 表示查询无需简化（单词或不可切分）。
fn simplify_query(query: &str) -> Option<String> {
    let first = query.split_whitespace().next()?;
    // 去掉可能的引号
    let clean = first.trim_matches('"').trim();
    if clean.is_empty() || clean == query.trim() {
        None
    } else {
        Some(clean.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_search_url() {
        let mut params = SearchParams::new("张雪峰 2026");
        params.num_results = 8;
        let u = build_search_url("www.bing.com", &params);
        assert_eq!(u.host_str(), Some("www.bing.com"));
        assert!(u.query_pairs().any(|(k, v)| k == "q" && v == "张雪峰 2026"));
        assert!(u.query_pairs().any(|(k, v)| k == "adlt" && v == "off"));
        assert!(u.query_pairs().any(|(k, v)| k == "mkt" && v == "zh-CN"));
        assert!(u.query_pairs().any(|(k, v)| k == "count" && v == "8"));
        assert!(!u.query_pairs().any(|(k, _)| k == "qft"));
    }

    #[test]
    fn test_build_search_url_with_freshness_and_domains() {
        let mut params = SearchParams::new("tokio");
        params.freshness = Some(super::super::Freshness::Week);
        params.include_domains = vec!["docs.rs".into()];
        let u = build_search_url("www.bing.com", &params);
        let q = u
            .query_pairs()
            .find(|(k, _)| k == "q")
            .map(|(_, v)| v.into_owned())
            .unwrap();
        assert!(q.contains("site:docs.rs"), "q: {q}");
        assert!(u
            .query_pairs()
            .any(|(k, v)| k == "qft" && v == "+filterui:age-lt10080"));
    }

    #[test]
    fn test_decode_ck_href() {
        // base64url("https://example.com/page?x=1") = aHR0cHM6Ly9leGFtcGxlLmNvbS9wYWdlP3g9MQ
        let href = "https://www.bing.com/ck/a?u=a1aHR0cHM6Ly9leGFtcGxlLmNvbS9wYWdlP3g9MQ&ntb=1";
        assert_eq!(decode_ck_href(href), "https://example.com/page?x=1");
        // 非 ck/a 链接原样返回
        assert_eq!(
            decode_ck_href("https://example.com/direct"),
            "https://example.com/direct"
        );
    }

    #[test]
    fn test_is_external_url() {
        assert!(is_external_url("https://example.com/page"));
        assert!(!is_external_url("https://www.bing.com/ck/a?u=xxx"));
        assert!(!is_external_url("https://www.microsoft.com"));
        assert!(!is_external_url("https://www.msn.com/zh-cn/news"));
    }

    #[test]
    fn test_parse_results() {
        let html = r#"
        <html><body>
        <ol id="b_results">
            <li class="b_algo">
                <h2><a href="https://example.com/page">Test Title</a></h2>
                <p>Test snippet here</p>
            </li>
            <li class="b_algo">
                <h2><a href="https://www.bing.com/ck/a?u=a1aHR0cHM6Ly9uZXdzLmV4YW1wbGUuY29tL2I">News Title</a></h2>
                <p>News snippet</p>
            </li>
            <li class="b_algo">
                <h2><a href="https://www.bing.com/images/search?q=x">Internal Link</a></h2>
            </li>
        </ol>
        </body></html>
        "#;
        let results = parse_results(html, 8);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Test Title");
        assert_eq!(results[0].url, "https://example.com/page");
        assert_eq!(results[0].snippet, "Test snippet here");
        // ck/a 重定向解码
        assert_eq!(results[1].url, "https://news.example.com/b");
    }

    #[test]
    fn test_parse_empty_html() {
        let html = "<html><body></body></html>";
        assert!(parse_results(html, 8).is_empty());
    }

    /// 结果上限：20 条候选只保留请求条数
    #[test]
    fn test_parse_results_limit() {
        let items: String = (0..20)
            .map(|i| {
                format!(
                    r#"<li class="b_algo"><h2><a href="https://example.com/p{i}">Title {i}</a></h2><p>Snippet {i}</p></li>"#
                )
            })
            .collect();
        let html = format!(r#"<html><body><ol id="b_results">{items}</ol></body></html>"#);
        let results = parse_results(&html, 12);
        assert_eq!(results.len(), 12);
        assert_eq!(results.last().unwrap().title, "Title 11");
    }
}

// ── 降级检测测试 ──

/// 真实降级场景：热点人名+附加词 → 全是「张」字字典释义（标题高度雷同）
#[test]
fn test_is_degraded_dictionary_results() {
    let results = vec![
        SearchResult {
            title: "张（汉语汉字）_百度百科".into(),
            url: "https://baike.baidu.com/item/张".into(),
            snippet: "".into(),
        },
        SearchResult {
            title: "张姓（中国姓氏）_百度百科".into(),
            url: "https://baike.baidu.com/item/张姓".into(),
            snippet: "".into(),
        },
        SearchResult {
            title: "张的意思,张的解释,张的拼音,张的部首,张的笔顺-汉语国学".into(),
            url: "https://www.hanyuguoxue.com/zidian/zi-24352".into(),
            snippet: "".into(),
        },
        SearchResult {
            title: "《张》的拼音,张字的意思、组词、部首、笔画、笔顺 - 汉语查".into(),
            url: "https://www.hgcha.com/zidian/6721ca45.html".into(),
            snippet: "".into(),
        },
        SearchResult {
            title: "张-字典-意思-解释-拼音-注音-读音-部首-出处".into(),
            url: "https://www.shidianguji.com/character/张".into(),
            snippet: "".into(),
        },
    ];
    assert!(
        is_degraded_results(&results, "张雪峰 高考志愿填报"),
        "字典释义页应判定为降级"
    );
}

/// 正常结果：标题各不相同（真实新闻/百科混合）→ 不降级
#[test]
fn test_is_degraded_normal_results() {
    let results = vec![
        SearchResult {
            title: "张雪峰报志愿逻辑及核心观点（完整详细版） - 知乎".into(),
            url: "https://zhuanlan.zhihu.com".into(),
            snippet: "".into(),
        },
        SearchResult {
            title: "2025张雪峰最全高考志愿填报指南！！！ - 知乎".into(),
            url: "https://zhuanlan.zhihu.com".into(),
            snippet: "".into(),
        },
        SearchResult {
            title: "张雪峰：高考是全世界最公平的考试".into(),
            url: "https://www.163.com".into(),
            snippet: "".into(),
        },
        SearchResult {
            title: "峰学蔚来教育科技有限公司 - 企查查".into(),
            url: "https://www.qcc.com".into(),
            snippet: "".into(),
        },
    ];
    assert!(
        !is_degraded_results(&results, "张雪峰 高考志愿填报"),
        "正常结果不应判定为降级"
    );
}

/// 少量结果（<3 条）不判定降级（可能是真没结果）
#[test]
fn test_is_degraded_few_results() {
    let results = vec![
        SearchResult {
            title: "张（汉语汉字）".into(),
            url: "https://baike.baidu.com".into(),
            snippet: "".into(),
        },
        SearchResult {
            title: "张姓".into(),
            url: "https://baike.baidu.com".into(),
            snippet: "".into(),
        },
    ];
    assert!(
        !is_degraded_results(&results, "张雪峰 高考志愿填报"),
        "少于 3 条不判定降级"
    );
}

/// 简化查询：多词查询取第一个词
#[test]
fn test_simplify_query_multi_word() {
    assert_eq!(
        simplify_query("张雪峰 高考志愿填报"),
        Some("张雪峰".to_string())
    );
    assert_eq!(
        simplify_query("张雪峰 高考 志愿 填报"),
        Some("张雪峰".to_string())
    );
    assert_eq!(simplify_query("今天北京天气"), None, "无空白不可简化");
}

/// 简化查询：带引号
#[test]
fn test_simplify_query_quoted() {
    assert_eq!(
        simplify_query("\"张雪峰\" 最新消息"),
        Some("张雪峰".to_string())
    );
    assert_eq!(simplify_query("张雪峰"), None, "单词查询无需简化");
}
