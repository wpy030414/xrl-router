//! JSON 搜索 API 后端：SearXNG（自建）/ Tavily / Exa / Brave。
//!
//! 统一纪律：
//! - 所有出站请求走调用方传入的 `reqwest::Client`（AppState 统一工厂，继承系统代理）；
//! - 单引擎 15s 超时；非 2xx 附带响应体前 200 字符便于排障；
//! - 解析函数与 HTTP 分离，便于用固定 JSON 做单测。

use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use reqwest::header::ACCEPT;
use serde_json::{json, Value};
use url::Url;

use super::{build_engine_query, SearchConfig, SearchParams, SearchResult};

const ENGINE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_SNIPPET_CHARS: usize = 600;

/// SearXNG：`GET {base}/search?q=...&format=json`。
pub(super) async fn searxng(
    client: &reqwest::Client,
    config: &SearchConfig,
    params: &SearchParams,
) -> anyhow::Result<Vec<SearchResult>> {
    let base = config
        .searxng_url
        .as_deref()
        .ok_or_else(|| anyhow!("未配置 SearXNG 实例地址"))?;
    let mut url = Url::parse(&format!("{}/search", base.trim_end_matches('/')))
        .context("SearXNG 地址无效（需形如 https://search.example.com）")?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("q", &build_engine_query(params));
        query.append_pair("format", "json");
        query.append_pair("categories", "general");
        query.append_pair("language", "zh-CN");
        query.append_pair("safesearch", "0");
        if let Some(freshness) = params.freshness {
            query.append_pair("time_range", freshness.searx_label());
        }
    }
    let request = client.get(url).header(ACCEPT, "application/json");
    let body = send_json(request, "SearXNG").await?;
    parse_searx(&body)
}

/// Tavily：`POST https://api.tavily.com/search`（Bearer 鉴权）。
pub(super) async fn tavily(
    client: &reqwest::Client,
    config: &SearchConfig,
    params: &SearchParams,
) -> anyhow::Result<Vec<SearchResult>> {
    let key = config
        .tavily_key
        .as_deref()
        .ok_or_else(|| anyhow!("未配置 Tavily API Key"))?;
    let mut body = json!({
        "query": build_engine_query(params),
        "max_results": params.num_results,
        "search_depth": "basic",
        "include_domains": params.include_domains,
        "exclude_domains": params.exclude_domains,
    });
    if let Some(freshness) = params.freshness {
        body["time_range"] = json!(freshness.tavily_label());
    }
    let request = client
        .post("https://api.tavily.com/search")
        .bearer_auth(key)
        .json(&body);
    let value = send_json(request, "Tavily").await?;
    parse_tavily(&value)
}

/// Exa：`POST https://api.exa.ai/search`（`x-api-key` 鉴权）。
pub(super) async fn exa(
    client: &reqwest::Client,
    config: &SearchConfig,
    params: &SearchParams,
) -> anyhow::Result<Vec<SearchResult>> {
    let key = config
        .exa_key
        .as_deref()
        .ok_or_else(|| anyhow!("未配置 Exa API Key"))?;
    let mut body = json!({
        "query": build_engine_query(params),
        "numResults": params.num_results,
        "type": "auto",
        "contents": { "text": { "maxCharacters": 1200 } },
    });
    if let Some(freshness) = params.freshness {
        body["startPublishedDate"] = json!(freshness.since_iso());
    }
    let request = client
        .post("https://api.exa.ai/search")
        .header("x-api-key", key)
        .json(&body);
    let value = send_json(request, "Exa").await?;
    parse_exa(&value)
}

/// Brave：`GET https://api.search.brave.com/res/v1/web/search`。
pub(super) async fn brave(
    client: &reqwest::Client,
    config: &SearchConfig,
    params: &SearchParams,
) -> anyhow::Result<Vec<SearchResult>> {
    let key = config
        .brave_key
        .as_deref()
        .ok_or_else(|| anyhow!("未配置 Brave API Key"))?;
    let mut url =
        Url::parse("https://api.search.brave.com/res/v1/web/search").expect("static brave url");
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("q", &build_engine_query(params));
        query.append_pair("count", &params.num_results.to_string());
        query.append_pair("safesearch", "off");
        if let Some(freshness) = params.freshness {
            query.append_pair("freshness", freshness.brave_label());
        }
    }
    let request = client
        .get(url)
        .header("X-Subscription-Token", key)
        .header(ACCEPT, "application/json");
    let value = send_json(request, "Brave").await?;
    parse_brave(&value)
}

async fn send_json(request: reqwest::RequestBuilder, engine: &str) -> anyhow::Result<Value> {
    let response = request
        .timeout(ENGINE_TIMEOUT)
        .send()
        .await
        .with_context(|| format!("{engine} 请求失败"))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let hint = match status.as_u16() {
            401 | 403 if engine == "SearXNG" => {
                "（SearXNG 需在 settings.yml 的 search.formats 中加入 json）"
            }
            401 | 403 => "（API Key 无效或无权限）",
            429 => "（触发限流）",
            _ => "",
        };
        bail!(
            "{engine} HTTP {}{hint}：{}",
            status.as_u16(),
            truncate(&text, 200)
        );
    }
    serde_json::from_str(&text)
        .with_context(|| format!("{engine} 响应不是合法 JSON：{}", truncate(&text, 200)))
}

// ── 解析（纯函数，便于单测） ──────────────────────────────────────────────

fn parse_searx(value: &Value) -> anyhow::Result<Vec<SearchResult>> {
    let items = value
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("SearXNG 响应缺少 results 数组"))?;
    Ok(items
        .iter()
        .filter_map(|item| {
            let title = item.get("title")?.as_str()?.trim();
            let url = item.get("url")?.as_str()?.trim();
            if title.is_empty() || url.is_empty() {
                return None;
            }
            Some(SearchResult {
                title: title.to_string(),
                url: url.to_string(),
                snippet: clean_text(item.get("content").and_then(Value::as_str).unwrap_or("")),
            })
        })
        .collect())
}

fn parse_tavily(value: &Value) -> anyhow::Result<Vec<SearchResult>> {
    let items = value
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Tavily 响应缺少 results 数组"))?;
    Ok(items
        .iter()
        .filter_map(|item| {
            let title = item.get("title")?.as_str()?.trim();
            let url = item.get("url")?.as_str()?.trim();
            if title.is_empty() || url.is_empty() {
                return None;
            }
            Some(SearchResult {
                title: title.to_string(),
                url: url.to_string(),
                snippet: clean_text(item.get("content").and_then(Value::as_str).unwrap_or("")),
            })
        })
        .collect())
}

fn parse_exa(value: &Value) -> anyhow::Result<Vec<SearchResult>> {
    let items = value
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Exa 响应缺少 results 数组"))?;
    Ok(items
        .iter()
        .filter_map(|item| {
            let url = item.get("url")?.as_str()?.trim();
            if url.is_empty() {
                return None;
            }
            let title = item
                .get("title")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .unwrap_or(url);
            Some(SearchResult {
                title: title.to_string(),
                url: url.to_string(),
                snippet: clean_text(item.get("text").and_then(Value::as_str).unwrap_or("")),
            })
        })
        .collect())
}

fn parse_brave(value: &Value) -> anyhow::Result<Vec<SearchResult>> {
    let items = value
        .get("web")
        .and_then(|web| web.get("results"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Brave 响应缺少 web.results 数组"))?;
    Ok(items
        .iter()
        .filter_map(|item| {
            let title = item.get("title")?.as_str()?.trim();
            let url = item.get("url")?.as_str()?.trim();
            if title.is_empty() || url.is_empty() {
                return None;
            }
            Some(SearchResult {
                title: title.to_string(),
                url: url.to_string(),
                snippet: clean_text(
                    item.get("description")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                ),
            })
        })
        .collect())
}

/// 折叠空白并截断。
fn clean_text(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(MAX_SNIPPET_CHARS).collect()
}

fn truncate(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        trimmed.to_string()
    } else {
        let head: String = trimmed.chars().take(max).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_searx() {
        let value = json!({
            "results": [
                {"title": "Rust 异步", "url": "https://docs.rs/tokio", "content": "tokio 是\n异步运行时"},
                {"title": "", "url": "https://skip.me", "content": "空标题被丢弃"},
                {"title": "无摘要", "url": "https://example.com/x"}
            ]
        });
        let results = parse_searx(&value).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Rust 异步");
        assert_eq!(results[0].snippet, "tokio 是 异步运行时");
        assert_eq!(results[1].snippet, "");
    }

    #[test]
    fn test_parse_tavily() {
        let value = json!({
            "results": [
                {"title": "T", "url": "https://a.com", "content": "内容一", "score": 0.9},
                {"title": "T2", "url": "https://b.com"}
            ]
        });
        let results = parse_tavily(&value).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].snippet, "内容一");
        assert_eq!(results[1].snippet, "");
    }

    #[test]
    fn test_parse_exa_uses_url_as_title_fallback() {
        let value = json!({
            "results": [
                {"url": "https://exa.ai/x", "text": "长文本"},
                {"title": "有标题", "url": "https://exa.ai/y", "text": ""}
            ]
        });
        let results = parse_exa(&value).unwrap();
        assert_eq!(results[0].title, "https://exa.ai/x");
        assert_eq!(results[0].snippet, "长文本");
        assert_eq!(results[1].title, "有标题");
    }

    #[test]
    fn test_parse_brave() {
        let value = json!({
            "web": {"results": [
                {"title": "Brave 结果", "url": "https://brave.com/x", "description": "描述文本"}
            ]}
        });
        let results = parse_brave(&value).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].snippet, "描述文本");
    }

    #[test]
    fn test_parse_missing_results_errors() {
        assert!(parse_searx(&json!({})).is_err());
        assert!(parse_tavily(&json!({})).is_err());
        assert!(parse_exa(&json!({})).is_err());
        assert!(parse_brave(&json!({})).is_err());
    }

    #[test]
    fn test_clean_text_collapses_and_truncates() {
        assert_eq!(clean_text(" a \n\t b  c "), "a b c");
        let long = "字".repeat(MAX_SNIPPET_CHARS + 50);
        assert_eq!(clean_text(&long).chars().count(), MAX_SNIPPET_CHARS);
    }
}
