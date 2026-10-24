//! 搜索层：引擎抽象 + 多后端并发融合 + 查询缓存。
//!
//! 后端分两类：
//! - **Bing**：零配置默认（`bing.rs`，裸 HTTP + cookie 预热 + 降级检测）；
//! - **JSON API**：SearXNG（自建实例）/ Tavily / Exa / Brave（`engines.rs`）。
//!
//! `auto` 模式把已配置的后端**并发**执行，按 RRF（Reciprocal Rank Fusion，
//! k=60）融合名次，再做 URL 归一化去重与域名过滤；全失败时回退 Bing。
//! 单一后端模式不做融合，直接返回其排序。
//!
//! 缓存：`(backend, query, num_results, freshness, domains)` 为键，TTL 15 分钟，
//! 避免模型在同一会话里反复打同一查询。

pub mod bing;
mod config;
mod engines;

pub use bing::SearchHttp;
pub use config::{persist_backend, persist_key, persist_searxng_url, SearchConfig, BACKENDS};

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::bail;
use serde::Serialize;
use url::Url;

/// 多引擎总预算（各引擎自身 15s 超时）。
const TOTAL_TIMEOUT: Duration = Duration::from_secs(20);
/// 查询缓存 TTL。
const CACHE_TTL: Duration = Duration::from_secs(15 * 60);
/// 查询缓存容量。
const CACHE_CAPACITY: usize = 64;
/// RRF 常数（社区惯例 60）。
const RRF_K: f64 = 60.0;

/// 单条搜索结果（三种客户端格式/各引擎统一结构）。
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// 结果新鲜度窗口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    Day,
    Week,
    Month,
    Year,
}

impl Freshness {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "day" | "d" | "24h" => Some(Self::Day),
            "week" | "w" => Some(Self::Week),
            "month" | "m" => Some(Self::Month),
            "year" | "y" => Some(Self::Year),
            _ => None,
        }
    }

    pub(super) fn searx_label(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
        }
    }

    pub(super) fn tavily_label(self) -> &'static str {
        self.searx_label()
    }

    pub(super) fn brave_label(self) -> &'static str {
        match self {
            Self::Day => "pd",
            Self::Week => "pw",
            Self::Month => "pm",
            Self::Year => "py",
        }
    }

    /// Bing 的相对时间过滤（分钟阈值，best-effort）。
    pub(super) fn bing_qft(self) -> &'static str {
        match self {
            Self::Day => "+filterui:age-lt1440",
            Self::Week => "+filterui:age-lt10080",
            Self::Month => "+filterui:age-lt43200",
            Self::Year => "+filterui:age-lt525600",
        }
    }

    /// Exa 的 `startPublishedDate`（ISO 8601）。
    pub(super) fn since_iso(self) -> String {
        let days = match self {
            Self::Day => 1,
            Self::Week => 7,
            Self::Month => 30,
            Self::Year => 365,
        };
        (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339()
    }
}

/// 搜索参数（工具层解析后传入）。
#[derive(Debug, Clone)]
pub struct SearchParams {
    pub query: String,
    pub num_results: usize,
    pub freshness: Option<Freshness>,
    pub include_domains: Vec<String>,
    pub exclude_domains: Vec<String>,
}

impl SearchParams {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            num_results: 8,
            freshness: None,
            include_domains: Vec::new(),
            exclude_domains: Vec::new(),
        }
    }

    /// 派生一个替换查询串的副本（Bing 降级重试用）。
    pub fn with_query(&self, query: &str) -> Self {
        let mut next = self.clone();
        next.query = query.trim().to_string();
        next
    }

    /// 归一化：条数夹到 [1, 20]、域名小写去空白、去重。
    pub fn normalized(mut self) -> Self {
        self.query = self.query.trim().to_string();
        self.num_results = self.num_results.clamp(1, 20);
        self.include_domains = normalize_domains(self.include_domains);
        self.exclude_domains = normalize_domains(self.exclude_domains);
        self
    }

    fn cache_key(&self) -> String {
        let mut include = self.include_domains.clone();
        include.sort();
        let mut exclude = self.exclude_domains.clone();
        exclude.sort();
        format!(
            "{}|{}|{:?}|{}|{}",
            self.query,
            self.num_results,
            self.freshness,
            include.join(","),
            exclude.join(",")
        )
    }
}

fn normalize_domains(domains: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = domains
        .into_iter()
        .map(|d| {
            let lower = d.trim().to_ascii_lowercase();
            lower
                .trim_start_matches("http://")
                .trim_start_matches("https://")
                .trim_end_matches('/')
                .to_string()
        })
        .filter(|d| !d.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// 引擎查询串：无原生域名参数的引擎用 `site:` 操作符表达（Bing/SearXNG 均支持）。
pub(crate) fn build_engine_query(params: &SearchParams) -> String {
    let mut query = params.query.clone();
    if !params.include_domains.is_empty() {
        let sites: Vec<String> = params
            .include_domains
            .iter()
            .map(|d| format!("site:{d}"))
            .collect();
        query.push_str(&format!(" ({})", sites.join(" OR ")));
    }
    for domain in &params.exclude_domains {
        query.push_str(&format!(" -site:{domain}"));
    }
    query
}

/// 搜索后端。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Bing,
    Searxng,
    Tavily,
    Exa,
    Brave,
}

impl Engine {
    pub fn name(self) -> &'static str {
        match self {
            Self::Bing => "bing",
            Self::Searxng => "searxng",
            Self::Tavily => "tavily",
            Self::Exa => "exa",
            Self::Brave => "brave",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "bing" => Some(Self::Bing),
            "searxng" => Some(Self::Searxng),
            "tavily" => Some(Self::Tavily),
            "exa" => Some(Self::Exa),
            "brave" => Some(Self::Brave),
            _ => None,
        }
    }
}

/// 单次搜索的共享依赖。
pub struct SearchCtx<'a> {
    pub config: &'a SearchConfig,
    pub bing: &'a SearchHttp,
    pub client: &'a reqwest::Client,
}

/// 搜索结果（`engines` 为实际参与融合的后端名）。
#[derive(Debug, Clone)]
pub struct SearchOutcome {
    pub results: Vec<SearchResult>,
    pub engines: Vec<String>,
}

/// 搜索入口：按配置选择引擎 → 并发执行 → RRF 融合 → 域名过滤 → 缓存。
pub async fn search(ctx: &SearchCtx<'_>, params: &SearchParams) -> anyhow::Result<SearchOutcome> {
    let params = params.clone().normalized();
    if params.query.is_empty() {
        bail!("empty query");
    }
    let cache_key = format!("{}|{}", ctx.config.backend, params.cache_key());
    if let Some(hit) = cache_get(&cache_key) {
        return Ok(hit);
    }

    let engines = ctx.config.plan();
    if engines.is_empty() {
        bail!("未配置任何搜索来源");
    }

    let mut groups: Vec<Vec<SearchResult>> = Vec::new();
    let mut used: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    if engines.len() == 1 {
        collect_result(
            engines[0],
            run_engine(ctx, engines[0], &params).await,
            &mut groups,
            &mut used,
            &mut errors,
        );
    } else {
        // 显式借用：`async move` 若捕获 owned params 会被 FnMut 闭包拒绝
        let params_ref = &params;
        let futures = engines
            .iter()
            .map(|&engine| async move { (engine, run_engine(ctx, engine, params_ref).await) });
        match tokio::time::timeout(TOTAL_TIMEOUT, futures::future::join_all(futures)).await {
            Ok(items) => {
                for (engine, result) in items {
                    collect_result(engine, result, &mut groups, &mut used, &mut errors);
                }
            }
            Err(_) => bail!("搜索超时（{}s）", TOTAL_TIMEOUT.as_secs()),
        }
    }

    // auto 模式全部失败 → Bing 兜底（Bing 已在计划中则不重复）
    if groups.is_empty() && !engines.contains(&Engine::Bing) {
        collect_result(
            Engine::Bing,
            run_engine(ctx, Engine::Bing, &params).await,
            &mut groups,
            &mut used,
            &mut errors,
        );
    }

    if groups.is_empty() {
        bail!("所有搜索来源均失败：{}", errors.join("；"));
    }

    let merged = if groups.len() == 1 {
        groups.pop().unwrap_or_default()
    } else {
        merge_rrf(groups)
    };
    let results: Vec<SearchResult> = merged
        .into_iter()
        .map(|mut result| {
            result.url = normalize_result_url(&result.url);
            result
        })
        .filter(|r| domain_allowed(&r.url, &params))
        .take(params.num_results)
        .collect();
    let outcome = SearchOutcome {
        results,
        engines: used,
    };
    cache_put(&cache_key, outcome.clone());
    Ok(outcome)
}

fn collect_result(
    engine: Engine,
    result: anyhow::Result<Vec<SearchResult>>,
    groups: &mut Vec<Vec<SearchResult>>,
    used: &mut Vec<String>,
    errors: &mut Vec<String>,
) {
    match result {
        Ok(items) if !items.is_empty() => {
            used.push(engine.name().to_string());
            groups.push(items);
        }
        Ok(_) => tracing::debug!(engine = engine.name(), "search: empty result"),
        Err(e) => {
            tracing::warn!(engine = engine.name(), error = %e, "search: engine failed");
            errors.push(format!("{}: {e}", engine.name()));
        }
    }
}

async fn run_engine(
    ctx: &SearchCtx<'_>,
    engine: Engine,
    params: &SearchParams,
) -> anyhow::Result<Vec<SearchResult>> {
    match engine {
        Engine::Bing => bing::search(ctx.bing, params).await,
        Engine::Searxng => engines::searxng(ctx.client, ctx.config, params).await,
        Engine::Tavily => engines::tavily(ctx.client, ctx.config, params).await,
        Engine::Exa => engines::exa(ctx.client, ctx.config, params).await,
        Engine::Brave => engines::brave(ctx.client, ctx.config, params).await,
    }
}

/// RRF 融合：同一 URL 的名次贡献累加；保留摘要更长的那条。
pub(crate) fn merge_rrf(groups: Vec<Vec<SearchResult>>) -> Vec<SearchResult> {
    let mut scored: HashMap<String, (f64, SearchResult)> = HashMap::new();
    for group in groups {
        for (rank, mut result) in group.into_iter().enumerate() {
            let key = normalize_result_url(&result.url);
            result.url = key.clone();
            let add = 1.0 / (RRF_K + rank as f64 + 1.0);
            match scored.get_mut(&key) {
                Some((score, existing)) => {
                    *score += add;
                    if result.snippet.chars().count() > existing.snippet.chars().count() {
                        *existing = result;
                    }
                }
                None => {
                    scored.insert(key, (add, result));
                }
            }
        }
    }
    let mut merged: Vec<(f64, SearchResult)> = scored.into_values().collect();
    merged.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    merged.into_iter().map(|(_, result)| result).collect()
}

/// URL 归一化（去 fragment、小写 host、去尾斜杠、去跟踪参数）。
pub(crate) fn normalize_result_url(raw: &str) -> String {
    let Ok(mut url) = Url::parse(raw) else {
        return raw.to_string();
    };
    url.set_fragment(None);
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    let _ = url.set_host(Some(&host));
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| !is_tracking_param(k))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    url.set_query(None);
    if !pairs.is_empty() {
        let mut query = url.query_pairs_mut();
        for (k, v) in &pairs {
            query.append_pair(k, v);
        }
    }
    let mut out = url.to_string();
    while out.ends_with('/') && out.len() > "https://x".len() {
        out.pop();
    }
    out
}

fn is_tracking_param(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.starts_with("utm_")
        || matches!(
            key.as_str(),
            "gclid" | "fbclid" | "yclid" | "msclkid" | "spm" | "_hsenc" | "_hsmi" | "ref_src"
        )
}

fn domain_allowed(url: &str, params: &SearchParams) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return params.include_domains.is_empty();
    };
    let Some(host) = parsed.host_str().map(|h| h.to_ascii_lowercase()) else {
        return params.include_domains.is_empty();
    };
    let matches_domain = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
    if params.exclude_domains.iter().any(|d| matches_domain(d)) {
        return false;
    }
    if params.include_domains.is_empty() {
        true
    } else {
        params.include_domains.iter().any(|d| matches_domain(d))
    }
}

// ── 查询缓存 ──────────────────────────────────────────────────────────────

type CacheEntry = (Instant, SearchOutcome);
type CacheMap = Mutex<HashMap<String, CacheEntry>>;

static CACHE: OnceLock<CacheMap> = OnceLock::new();

fn cache_map() -> &'static CacheMap {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_get(key: &str) -> Option<SearchOutcome> {
    let now = Instant::now();
    let guard = cache_map().lock().ok()?;
    match guard.get(key) {
        Some((ts, outcome)) if now.duration_since(*ts) < CACHE_TTL => Some(outcome.clone()),
        _ => None,
    }
}

fn cache_put(key: &str, outcome: SearchOutcome) {
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
    guard.insert(key.to_string(), (now, outcome));
}

/// 供设置页展示「当前生效来源」。
pub fn configured_engines(config: &SearchConfig) -> Vec<&'static str> {
    config.plan().iter().map(|e| e.name()).collect()
}

impl SearchConfig {
    /// 按配置生成执行计划。
    pub(crate) fn plan(&self) -> Vec<Engine> {
        if let Some(engine) = Engine::from_name(&self.backend) {
            return vec![engine];
        }
        let mut plan = Vec::new();
        if self.searxng_url.is_some() {
            plan.push(Engine::Searxng);
        }
        if self.tavily_key.is_some() {
            plan.push(Engine::Tavily);
        }
        if self.exa_key.is_some() {
            plan.push(Engine::Exa);
        }
        if self.brave_key.is_some() {
            plan.push(Engine::Brave);
        }
        if plan.is_empty() {
            plan.push(Engine::Bing);
        }
        plan
    }
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

    #[test]
    fn test_params_normalized() {
        let mut p = SearchParams::new("  rust tokio  ");
        p.num_results = 99;
        p.include_domains = vec![
            " HTTPS://Docs.rs/ ".to_string(),
            "docs.rs".to_string(),
            "".to_string(),
        ];
        let p = p.normalized();
        assert_eq!(p.query, "rust tokio");
        assert_eq!(p.num_results, 20);
        assert_eq!(p.include_domains, vec!["docs.rs".to_string()]);
    }

    #[test]
    fn test_engine_query_with_domain_operators() {
        let mut p = SearchParams::new("tokio timeout");
        p.include_domains = vec!["docs.rs".into(), "github.com".into()];
        p.exclude_domains = vec!["csdn.net".into()];
        let q = build_engine_query(&p);
        assert!(q.starts_with("tokio timeout"));
        assert!(q.contains("(site:docs.rs OR site:github.com)"));
        assert!(q.contains("-site:csdn.net"));
    }

    #[test]
    fn test_merge_rrf_dedupes_and_ranks() {
        let groups = vec![
            vec![
                result("A", "https://a.com/page", "short"),
                result("B", "https://b.com/", "b snippet"),
            ],
            vec![
                result(
                    "A2",
                    "https://a.com/page/?utm_source=x",
                    "a much longer snippet",
                ),
                result("C", "https://c.com/", "c snippet"),
            ],
        ];
        let merged = merge_rrf(groups);
        // A 在两个引擎都出现 → 融合分最高
        assert_eq!(merged[0].url, "https://a.com/page");
        assert_eq!(merged[0].snippet, "a much longer snippet");
        let urls: Vec<&str> = merged.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(urls.len(), 3);
        assert!(urls.contains(&"https://b.com"));
        assert!(urls.contains(&"https://c.com"));
    }

    #[test]
    fn test_domain_filter() {
        let mut p = SearchParams::new("q");
        p.include_domains = vec!["docs.rs".into()];
        assert!(domain_allowed("https://docs.rs/tokio", &p));
        assert!(domain_allowed("https://api.docs.rs/x", &p));
        assert!(!domain_allowed("https://evil-docs.rs/x", &p));

        let mut p2 = SearchParams::new("q");
        p2.exclude_domains = vec!["csdn.net".into()];
        assert!(!domain_allowed("https://blog.csdn.net/x", &p2));
        assert!(domain_allowed("https://docs.rs/x", &p2));
    }

    #[test]
    fn test_freshness_labels() {
        assert_eq!(Freshness::parse("week"), Some(Freshness::Week));
        assert_eq!(Freshness::parse("W"), Some(Freshness::Week));
        assert_eq!(Freshness::parse("decade"), None);
        assert_eq!(Freshness::Day.brave_label(), "pd");
        assert_eq!(Freshness::Month.searx_label(), "month");
        assert_eq!(Freshness::Year.bing_qft(), "+filterui:age-lt525600");
    }

    #[test]
    fn test_plan_auto_and_explicit() {
        let mut config = SearchConfig::default();
        assert_eq!(config.plan(), vec![Engine::Bing]);
        config.searxng_url = Some("http://localhost:8080".into());
        config.tavily_key = Some("k".into());
        assert_eq!(config.plan(), vec![Engine::Searxng, Engine::Tavily]);
        config.backend = "brave".into();
        assert_eq!(config.plan(), vec![Engine::Brave]);
    }
}
