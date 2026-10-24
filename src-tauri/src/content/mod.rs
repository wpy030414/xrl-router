//! 内容增强层：MCP `web_search` / `web_fetch` 的共用网页内容处理。
//!
//! 能力对齐 DeepSeek Search & Fetch API 的语义（自研实现，不接外部服务）：
//! - `summary`：600~800 字抽取式长摘要（并发抓取 top-N 结果页正文生成）
//! - `rerank_score`：0~3 语义重排分（相关性 × 域名权威 × 新鲜度 × 内容质量）
//! - 垃圾清洗：域名黑名单 + 空壳页检测
//!
//! 编排策略（全部后端常量，无设置项）：
//! - Bing 候选（≤12 条）先做零成本预排，并发静态抓取前 5 条：统一 HTTP 工厂 +
//!   浏览器 UA + 单页 8s 超时 + 整体 10s 预算（deadline 下放到 join 循环保留部分
//!   结果），慢页/失败页降级为仅 snippet
//! - 提取（scraper 解析是 CPU 密集且 `Html` 非 Send）整体放 `spawn_blocking`
//! - TTL 提取缓存只服务 enrichment 的重复搜索；**web_fetch 不读**——enrichment
//!   是静态抓取的降级内容，WebView 路径必须新渲染，防止降级内容污染

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures::StreamExt;

use crate::search::SearchResult;

mod authority;
mod rank;
mod readability;
mod summary;
mod text;

use rank::ScoreParts;
pub(crate) use readability::{decode_body, extract, ExtractedPage};
use text::QueryTerms;

/// 并发抓取的结果页数量。
const FETCH_TOP_N: usize = 5;
/// 单页抓取超时（连接 + 读体，reqwest per-request）。
const PER_PAGE_TIMEOUT: Duration = Duration::from_secs(8);
/// enrichment 整体预算（超时即返回已得部分结果，残余任务 abort）。
const ENRICH_BUDGET: Duration = Duration::from_secs(10);
/// 响应体字节上限（超出直接跳过；html5ever 容忍截断文档）。
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
/// 最终输出条数上限。
const MAX_RESULTS_OUT: usize = 10;
/// 提取缓存容量（条）。
const CACHE_CAPACITY: usize = 32;
/// 提取缓存 TTL。
const CACHE_TTL: Duration = Duration::from_secs(15 * 60);

const CHROME_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                         (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// 增强结果：`format_search_text` 的数据载体。
pub struct EnrichedResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    /// 长摘要（600~800 字抽取式；未抓到正文时退化为 snippet）。
    pub summary: String,
    /// 语义重排分 0~3（≥1.5 视为相关）。
    pub rerank_score: f32,
    pub date: Option<String>,
    pub site_name: Option<String>,
    pub authority: i8,
    /// 是否成功抓到并提取了正文。
    pub fetched: bool,
}

/// 搜索结果富化入口（`mcp/tools.rs` 调用）。
///
/// **永不失败**：全部抓取失败时退化为 snippet + title/snippet 打分。
pub async fn enrich_search_results(
    client: &reqwest::Client,
    results: Vec<SearchResult>,
    query: &str,
    ori_question: Option<&str>,
) -> Vec<EnrichedResult> {
    let started = Instant::now();
    let terms = QueryTerms::build(query, ori_question);
    if results.is_empty() {
        return Vec::new();
    }
    let now_utc = chrono::Utc::now();

    // 1. 零成本预排选抓取候选（黑名单/重复 URL 不浪费抓取预算）
    let candidates = pick_fetch_candidates(&results, &terms, FETCH_TOP_N);
    let candidate_count = candidates.len();

    // 2. 缓存命中的候选直取，其余并发抓取 + 提取
    //    （deadline 下放到 join 循环：超时保留已得部分结果）
    let mut pages: HashMap<usize, Arc<ExtractedPage>> = HashMap::new();
    let mut to_fetch: Vec<usize> = Vec::new();
    for idx in candidates {
        match cache_get(&results[idx].url, started) {
            Some(page) => {
                pages.insert(idx, page);
            }
            None => to_fetch.push(idx),
        }
    }
    if !to_fetch.is_empty() {
        let mut set: tokio::task::JoinSet<Option<(usize, String, Arc<ExtractedPage>)>> =
            tokio::task::JoinSet::new();
        for idx in to_fetch {
            let client = client.clone();
            let url = results[idx].url.clone();
            set.spawn(async move {
                fetch_page(&client, &url)
                    .await
                    .map(|(final_url, page)| (idx, final_url, page))
            });
        }
        let deadline = tokio::time::Instant::now() + ENRICH_BUDGET;
        loop {
            match tokio::time::timeout_at(deadline, set.join_next()).await {
                Ok(Some(Ok(Some((idx, final_url, page))))) => {
                    cache_put(&final_url, page.clone(), started);
                    pages.insert(idx, page);
                }
                Ok(Some(_)) => {}  // 单页失败或任务异常：跳过
                Ok(None) => break, // 全部完成
                Err(_) => break,   // 总预算耗尽（JoinSet drop 时 abort 残余任务）
            }
        }
    }

    let out = assemble_enriched(&results, &terms, &pages, now_utc);
    tracing::debug!(
        query = %query,
        candidates = candidate_count,
        fetched = pages.len(),
        output = out.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "content: search enrichment done"
    );
    out
}

/// 抓取并提取单页：成功返回 (最终 URL, 提取结果)。
///
/// 过滤链：2xx → content-type 仅 HTML/XHTML → 无意外 Content-Encoding →
/// Content-Length 预检 → 流式累积截断 2MB → spawn_blocking 提取。
///
/// 每个失败分支都记 debug 日志：抓取失败是本层最常见的降级路径（反爬/超时/
/// 非 HTML），静默失败会让「为什么没有长摘要」无法诊断。
async fn fetch_page(client: &reqwest::Client, url: &str) -> Option<(String, Arc<ExtractedPage>)> {
    // SSRF 闸：搜索结果是第三方内容，不能让它把请求引向本机/内网
    match url::Url::parse(url) {
        Ok(target) => {
            if let Err(reason) = crate::net_guard::ensure_public_url(&target).await {
                tracing::debug!(url = %url, reason = %reason, "content: blocked target");
                return None;
            }
        }
        Err(_) => return None,
    }
    let resp = match client
        .get(url)
        .header(reqwest::header::USER_AGENT, CHROME_UA)
        .header(
            reqwest::header::ACCEPT,
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
        // Client Hints：真实 Chrome 会发送，与 search/bing.rs 的 SearchHttp 保持一致
        // （该处实测「关键在完整浏览器头」而非 TLS 指纹或 JS 执行）。
        //
        // 注意：这些头**不能**绕过反爬。实测知乎/百度百科对本机直连仍返回 403
        // （走不走系统代理结果相同），属站点侧风控，只能靠 snippet 降级兜底。
        // 保留它们是为了不因请求头不完整而被识别为裸爬虫、拿到降级响应。
        .header(
            "sec-ch-ua",
            "\"Chromium\";v=\"131\", \"Not_A Brand\";v=\"24\"",
        )
        .header("sec-ch-ua-mobile", "?0")
        .header("sec-ch-ua-platform", "\"Windows\"")
        .timeout(PER_PAGE_TIMEOUT)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(url = %url, error = %e, "content: fetch request failed");
            return None;
        }
    };
    if !resp.status().is_success() {
        tracing::debug!(url = %url, status = resp.status().as_u16(), "content: non-2xx response");
        return None;
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if content_type.as_deref().is_some_and(|ct| !is_html_ct(ct)) {
        tracing::debug!(url = %url, content_type = ?content_type, "content: not HTML, skipped");
        return None;
    }
    // 防御：未请求压缩却回了压缩体（正常不会发生），解码交给 reqwest 但跳过异常声明
    if let Some(enc) = resp
        .headers()
        .get(reqwest::header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.eq_ignore_ascii_case("identity") && !v.is_empty())
    {
        tracing::debug!(url = %url, encoding = %enc, "content: unexpected content-encoding");
        return None;
    }
    if let Some(len) = resp
        .content_length()
        .filter(|len| *len as usize > MAX_BODY_BYTES)
    {
        tracing::debug!(url = %url, len, "content: body too large, skipped");
        return None;
    }
    let final_url = resp.url().to_string();

    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(url = %url, error = %e, "content: body stream error");
                return None;
            }
        };
        let remain = MAX_BODY_BYTES.saturating_sub(buf.len());
        if chunk.len() >= remain {
            buf.extend_from_slice(&chunk[..remain]);
            break;
        }
        buf.extend_from_slice(&chunk);
    }

    let url_for_extract = final_url.clone();
    let page = match tokio::task::spawn_blocking(move || {
        readability::extract_bytes(&buf, content_type.as_deref(), &url_for_extract)
    })
    .await
    {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!(url = %url, error = %e, "content: extraction task failed");
            return None;
        }
    };
    tracing::debug!(
        url = %url,
        text_len = page.text_len,
        quality = %format!("{:.2}", page.quality),
        "content: page extracted"
    );
    Some((final_url, Arc::new(page)))
}

fn is_html_ct(ct: &str) -> bool {
    let ct = ct.to_lowercase();
    ct.starts_with("text/html") || ct.starts_with("application/xhtml+xml")
}

/// 预排选抓取候选：title/snippet 覆盖度 + 域名权威度，去重、剔除黑名单。
fn pick_fetch_candidates(results: &[SearchResult], terms: &QueryTerms, n: usize) -> Vec<usize> {
    let mut seen = std::collections::HashSet::new();
    let mut scored: Vec<(f32, usize)> = results
        .iter()
        .enumerate()
        .filter(|(_, r)| !authority::is_blacklisted(&r.url))
        .filter(|(_, r)| seen.insert(r.url.trim_end_matches('/').to_string()))
        .map(|(i, r)| {
            let score = 0.6 * terms.coverage(&r.title)
                + 0.4 * terms.coverage(&r.snippet)
                + 0.1 * authority::authority_of(&r.url) as f32;
            (score, i)
        })
        .collect();
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    scored.into_iter().take(n).map(|(_, i)| i).collect()
}

/// 组装最终输出（纯函数，便于单测）：黑名单/空壳页处理 → 摘要 → 打分 → 排序裁剪。
fn assemble_enriched(
    results: &[SearchResult],
    terms: &QueryTerms,
    pages: &HashMap<usize, Arc<ExtractedPage>>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<EnrichedResult> {
    let total = results.len();
    let mut out: Vec<EnrichedResult> = Vec::with_capacity(total);
    for (i, r) in results.iter().enumerate() {
        if authority::is_blacklisted(&r.url) {
            continue;
        }
        // 空壳页（抓到但正文极少且全是链接）视作未抓到
        let page = pages
            .get(&i)
            .filter(|p| !(p.text_len < 120 && p.link_density > 0.6));
        let authority = authority::authority_of(&r.url);

        let (title, summary, body_text, quality, date, site_name) = match page {
            Some(p) => {
                let body = p.paragraphs.join("\n");
                let title = if p.title.is_empty() {
                    r.title.clone()
                } else {
                    p.title.clone()
                };
                let summary = summary::build_summary(&p.paragraphs, &title, &r.snippet, terms);
                (
                    title,
                    summary,
                    Some(body),
                    Some(p.quality),
                    p.date.clone(),
                    p.site_name.clone(),
                )
            }
            None => (r.title.clone(), r.snippet.clone(), None, None, None, None),
        };

        let parts = ScoreParts {
            title: &title,
            snippet: &r.snippet,
            body_text: body_text.as_deref(),
            authority,
            date: date.as_deref(),
            quality,
            order_bias: 1.0 - i as f32 / total as f32,
        };
        let score = rank::rerank_score(&parts, terms, now);
        out.push(EnrichedResult {
            title,
            url: r.url.clone(),
            snippet: r.snippet.clone(),
            summary,
            rerank_score: score,
            date,
            site_name,
            authority,
            fetched: page.is_some(),
        });
    }
    // 稳定排序：并列时保持 Bing 原序（order_bias 已提供弱先验）
    out.sort_by(|a, b| {
        b.rerank_score
            .partial_cmp(&a.rerank_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(MAX_RESULTS_OUT);
    out
}

// ── TTL 提取缓存（enrichment 专用） ───────────────────────────────────────

/// 缓存条目：写入时刻 + 提取结果（Arc 共享，命中时零拷贝 clone）。
type CacheEntry = (Instant, Arc<ExtractedPage>);
type CacheMap = Mutex<HashMap<String, CacheEntry>>;

static CACHE: OnceLock<CacheMap> = OnceLock::new();

fn cache_map() -> &'static CacheMap {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_get(url: &str, now: Instant) -> Option<Arc<ExtractedPage>> {
    let guard = cache_map().lock().ok()?;
    match guard.get(url) {
        Some((ts, page)) if now.duration_since(*ts) < CACHE_TTL => Some(page.clone()),
        _ => None,
    }
}

fn cache_put(url: &str, page: Arc<ExtractedPage>, now: Instant) {
    let map = cache_map();
    let Ok(mut guard) = map.lock() else { return };
    // 先清过期
    guard.retain(|_, (ts, _)| now.duration_since(*ts) < CACHE_TTL);
    // 仍超容则逐最旧
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
    guard.insert(url.to_string(), (now, page));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(title: &str, url: &str, snippet: &str) -> SearchResult {
        SearchResult {
            title: title.to_string(),
            url: url.to_string(),
            snippet: snippet.to_string(),
        }
    }

    fn page(
        paragraphs: Vec<String>,
        text_len: usize,
        link_density: f32,
        quality: f32,
    ) -> ExtractedPage {
        ExtractedPage {
            title: String::new(),
            site_name: None,
            date: Some("2026-10-01".to_string()),
            paragraphs,
            html: String::new(),
            text_len,
            link_density,
            quality,
        }
    }

    /// 全部抓取失败：退化为 snippet，格式完整仍有分。
    #[test]
    fn test_assemble_all_fetch_failed_degrades() {
        let results = vec![
            result(
                "tokio 指南",
                "https://github.com/tokio-rs/tokio",
                "tokio 是 Rust 的异步运行时",
            ),
            result("无关菜谱", "https://example.com/food", "今天吃什么"),
        ];
        let terms = QueryTerms::build("tokio 异步", None);
        let out = assemble_enriched(&results, &terms, &HashMap::new(), chrono::Utc::now());
        assert_eq!(out.len(), 2);
        assert!(out[0].summary.contains("tokio")); // snippet 退化
        assert!(!out[0].fetched);
        // 相关的排前面
        assert!(out[0].rerank_score > out[1].rerank_score);
        assert_eq!(out[0].title, "tokio 指南");
    }

    /// 黑名单整条丢弃。
    #[test]
    fn test_assemble_blacklist_dropped() {
        let results = vec![
            result("有用结果", "https://github.com/a", "内容 a"),
            result("倒卖文档", "https://wenku.baidu.com/view/1", "内容 b"),
        ];
        let terms = QueryTerms::build("内容", None);
        let out = assemble_enriched(&results, &terms, &HashMap::new(), chrono::Utc::now());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].url, "https://github.com/a");
    }

    /// 空壳页（text_len < 120 且 link_density > 0.6）视作未抓到。
    #[test]
    fn test_assemble_shell_page_treated_as_unfetched() {
        let results = vec![result("t", "https://example.com/x", "s")];
        let terms = QueryTerms::build("q", None);
        let mut pages = HashMap::new();
        pages.insert(
            0usize,
            Arc::new(page(vec!["链接一 链接二".to_string()], 50, 0.9, 0.1)),
        );
        let out = assemble_enriched(&results, &terms, &pages, chrono::Utc::now());
        assert!(!out[0].fetched);
        assert_eq!(out[0].summary, "s"); // 退化为 snippet
        assert_eq!(out[0].date, None);
    }

    /// 抓到正文的条目使用抽取式摘要 + 页面元数据。
    #[test]
    fn test_assemble_enriched_page_fields() {
        let results = vec![result("旧标题", "https://example.com/a", "旧 snippet")];
        let terms = QueryTerms::build("并发", None);
        let mut p = page(
            vec!["并发编程的第一段内容，讲述基本概念。".to_string()],
            2000,
            0.05,
            0.9,
        );
        p.title = "新标题".to_string();
        p.site_name = Some("Example".to_string());
        let mut pages = HashMap::new();
        pages.insert(0usize, Arc::new(p));
        let out = assemble_enriched(&results, &terms, &pages, chrono::Utc::now());
        assert!(out[0].fetched);
        assert_eq!(out[0].title, "新标题");
        assert_eq!(out[0].site_name.as_deref(), Some("Example"));
        assert_eq!(out[0].date.as_deref(), Some("2026-10-01"));
        assert!(out[0].summary.contains("并发编程"));
        // 高质量 + 高相关：分数显著
        assert!(out[0].rerank_score > 1.0, "score: {}", out[0].rerank_score);
    }

    /// 输出上限 10 条。
    #[test]
    fn test_assemble_truncates() {
        let results: Vec<SearchResult> = (0..12)
            .map(|i| {
                result(
                    &format!("标题{i}"),
                    &format!("https://example.com/{i}"),
                    "s",
                )
            })
            .collect();
        let terms = QueryTerms::build("标题", None);
        let out = assemble_enriched(&results, &terms, &HashMap::new(), chrono::Utc::now());
        assert_eq!(out.len(), 10);
    }

    /// 预排候选：黑名单不进、URL 去重、覆盖度高者优先。
    #[test]
    fn test_pick_fetch_candidates() {
        let results = vec![
            result(
                "tokio 异步运行时指南",
                "https://a.example.com/1",
                "tokio 异步",
            ),
            result("美食", "https://wenku.baidu.com/view/9", "吃什么"), // 黑名单
            result("tokio 相关", "https://a.example.com/1/", "tokio"),  // 与第 0 条同 URL（尾斜杠）
            result("无关", "https://b.example.com/2", "x"),
        ];
        let terms = QueryTerms::build("tokio 异步", None);
        let picked = pick_fetch_candidates(&results, &terms, 5);
        assert_eq!(picked.len(), 2); // 黑名单剔除 1 条 + 尾斜杠归一化去重剔除 1 条，剩 0 和 3
        assert_eq!(picked[0], 0); // 覆盖度最高的第一条
        assert!(!picked.contains(&1)); // 黑名单
        assert!(!picked.contains(&2)); // 与 0 同 URL（尾斜杠归一化后重复）
    }

    /// 缓存：命中 / TTL 过期 / 容量逐出。
    #[test]
    fn test_cache_hit_ttl_and_eviction() {
        let now = Instant::now();
        let p = Arc::new(page(vec!["x".to_string()], 1, 0.0, 0.0));
        cache_put("https://cache.test/1", p.clone(), now);
        assert!(cache_get("https://cache.test/1", now).is_some());
        // TTL 过期
        assert!(cache_get(
            "https://cache.test/1",
            now + CACHE_TTL + Duration::from_secs(1)
        )
        .is_none());
        // 未命中
        assert!(cache_get("https://cache.test/none", now).is_none());

        // 容量逐出：灌满后最旧的被逐
        cache_put("https://cache.test/old", p.clone(), now);
        let later = now + Duration::from_secs(1);
        for i in 0..CACHE_CAPACITY {
            cache_put(&format!("https://cache.test/n{i}"), p.clone(), later);
        }
        assert!(cache_get("https://cache.test/old", later).is_none());
        assert!(cache_get("https://cache.test/old", now).is_none());
        assert!(cache_get("https://cache.test/n0", later).is_some());
    }

    /// is_html_ct 边界。
    #[test]
    fn test_is_html_ct() {
        assert!(is_html_ct("text/html; charset=utf-8"));
        assert!(is_html_ct("application/xhtml+xml"));
        assert!(!is_html_ct("application/pdf"));
        assert!(!is_html_ct("application/json"));
        assert!(!is_html_ct("text/plain"));
    }

    /// 真实网络端到端 smoke test：Bing 搜索 → 并发抓取正文 → 抽取式摘要 →
    /// 重排打分全链路。默认 `#[ignore]`（依赖外网、受 Bing 风控影响），
    /// 手动触发：`cargo test --lib e2e_smoke -- --ignored --nocapture`。
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "需要真实网络访问 Bing 与结果页"]
    async fn e2e_smoke_real_network() {
        // 打开 debug 日志：抓取失败是本层最常见的降级路径，需要看到具体原因
        // （反爬 403 / 超时 / 非 HTML / 正文过短）。忽略已初始化的情况。
        let _ = tracing_subscriber::fmt()
            .with_env_filter("xrl_router_lib::content=debug")
            .with_test_writer()
            .try_init();

        let search_http = crate::search::bing::SearchHttp::new();
        let results = crate::search::bing::search(&search_http, "tokio JoinSet 用法")
            .await
            .expect("bing search should succeed");
        assert!(!results.is_empty(), "bing returned no results");

        let client = crate::http::http_client();
        let enriched = enrich_search_results(
            &client,
            results,
            "tokio JoinSet 用法",
            Some("Rust 里怎么给并发任务加总超时"),
        )
        .await;

        assert!(!enriched.is_empty(), "enrichment returned nothing");
        let fetched = enriched.iter().filter(|r| r.fetched).count();
        eprintln!("\n=== E2E: fetched {fetched}/{} pages ===", enriched.len());
        for r in enriched.iter().take(5) {
            eprintln!(
                "[{:.1}/3] {} | date={:?} site={:?} auth={} fetched={}\n    summary({} chars): {}",
                r.rerank_score,
                r.title,
                r.date,
                r.site_name,
                r.authority,
                r.fetched,
                r.summary.chars().count(),
                r.summary.chars().take(120).collect::<String>(),
            );
        }
        // 分数在 [0,3] 且降序排列
        assert!(enriched
            .iter()
            .all(|r| (0.0..=3.0).contains(&r.rerank_score)));
        let scores: Vec<f32> = enriched.iter().map(|r| r.rerank_score).collect();
        let mut sorted = scores.clone();
        sorted.sort_by(|a, b| b.partial_cmp(a).unwrap());
        assert_eq!(
            scores, sorted,
            "results must be sorted by rerank_score desc"
        );
        // 抓到正文的条目摘要应显著长于 snippet（长摘要语义）
        if let Some(r) = enriched.iter().find(|r| r.fetched) {
            assert!(
                r.summary.chars().count() > r.snippet.chars().count(),
                "summary should be longer than snippet for fetched pages"
            );
        }
    }
}
