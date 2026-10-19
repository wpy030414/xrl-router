//! 域名权威度分级与垃圾站黑名单（硬编码常量表，保守起步）。
//!
//! 语义对齐 DeepSeek Search API 的 `authority` 字段（-1~4，≥3 高度权威），
//! 仅作为 rerank_score 的一个分量（上限 0.8/3.0），单因子误判不致命。

/// 权威度 4：政府 / 国际组织 / 官方标准与官方文档。
const TIER4: &[&str] = &[
    "gov.cn",
    "gov",
    "edu.cn",
    "edu",
    "mil.cn",
    "w3.org",
    "ietf.org",
    "kernel.org",
    "unicode.org",
    "iana.org",
    "docs.rs",
    "rust-lang.org",
    "python.org",
    "golang.org",
    "openjdk.org",
    "developer.mozilla.org",
    "learn.microsoft.com",
    "developer.apple.com",
    "go.dev",
    "nginx.org",
    "postgresql.org",
    "nodejs.org",
];

/// 权威度 3：权威技术社区 / 百科 / 预印本 / 国际通讯社。
const TIER3: &[&str] = &[
    "github.com",
    "gitlab.com",
    "stackoverflow.com",
    "stackexchange.com",
    "wikipedia.org",
    "wikimedia.org",
    "arxiv.org",
    "doi.org",
    "mozilla.org",
    "reactjs.org",
    "react.dev",
    "vuejs.org",
    "angular.dev",
    "redis.io",
    "mysql.com",
    "sqlite.org",
    "mozilla.com",
    "reuters.com",
    "bbc.com",
    "bbc.co.uk",
    "apnews.com",
    "nature.com",
    "science.org",
    "acm.org",
    "ieee.org",
    "springer.com",
    "sciencedirect.com",
];

/// 权威度 2：成熟中文技术社区 / 主流媒体。
const TIER2: &[&str] = &[
    "zhihu.com",
    "juejin.cn",
    "segmentfault.com",
    "cnblogs.com",
    "oschina.net",
    "infoq.cn",
    "v2ex.com",
    "jianshu.com",
    "csdn.net",
    "51cto.com",
    "36kr.com",
    "ifeng.com",
    "thepaper.cn",
    "news.qq.com",
    "news.sina.com.cn",
    "163.com",
    "sohu.com",
    "sina.com.cn",
    "baidu.com",
    "medium.com",
    "habr.com",
    "solidot.org",
    "solidot.cn",
    "ourcoders.com",
    "linux.do",
    "rustcc.cn",
    "gitee.com",
    "aibase.com",
];

/// 黑名单：文档倒卖 / 内容农场 / 低质聚合站，命中整条结果丢弃。
const BLACKLIST: &[&str] = &[
    "wenku.baidu.com",
    "doc88.com",
    "docin.com",
    "book118.com",
    "renrendoc.com",
    "taodocs.com",
    "gaozhongw.com",
    "max.book118.com",
    "wenku.csdn.net",
];

/// URL → 域名权威度：
/// 4=政府/官方文档，3=权威站，2=成熟社区，1=默认，-1=无信号（解析失败或 IP 字面量）。
pub(super) fn authority_of(url: &str) -> i8 {
    let Some(host) = host_of(url) else {
        return -1;
    };
    if matches_suffix(&host, TIER4) {
        4
    } else if matches_suffix(&host, TIER3) {
        3
    } else if matches_suffix(&host, TIER2) {
        2
    } else {
        1
    }
}

/// 垃圾站黑名单命中判定。
pub(super) fn is_blacklisted(url: &str) -> bool {
    match host_of(url) {
        Some(host) => matches_suffix(&host, BLACKLIST),
        None => false,
    }
}

/// 提取规范化 host：小写、剥 `www.` 前缀；解析失败或 IP 字面量 → None。
fn host_of(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    // IP 字面量（v4/v6）无域名权威信号
    if matches!(
        parsed.host(),
        Some(url::Host::Ipv4(_)) | Some(url::Host::Ipv6(_))
    ) {
        return None;
    }
    let host = parsed.host_str()?;
    let host = host.trim_start_matches("www.").to_lowercase();
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

/// 后缀匹配：host 等于 entry，或 host 以 `.entry` 结尾（按段比较，
/// `github.com.evil.com` 不会匹配 `github.com`）。
fn matches_suffix(host: &str, list: &[&str]) -> bool {
    list.iter()
        .any(|entry| host == *entry || host.ends_with(&format!(".{entry}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_authority_tiers() {
        assert_eq!(authority_of("https://github.com/tokio-rs/tokio"), 3);
        assert_eq!(authority_of("https://gist.github.com/x"), 3); // 子域继承
        assert_eq!(authority_of("https://www.stackoverflow.com/q/1"), 3); // www 剥离
        assert_eq!(authority_of("https://docs.rs/tokio/latest/tokio/"), 4);
        assert_eq!(
            authority_of("https://learn.microsoft.com/en-us/windows/"),
            4
        );
        assert_eq!(authority_of("https://zhuanlan.zhihu.com/p/123"), 2);
        assert_eq!(authority_of("https://example.com/article"), 1); // 默认
    }

    #[test]
    fn test_authority_invalid_and_ip() {
        assert_eq!(authority_of("not a url"), -1);
        assert_eq!(authority_of("https://192.168.1.1/index.html"), -1);
        assert_eq!(authority_of("https://[::1]/"), -1);
    }

    #[test]
    fn test_authority_suffix_spoof_resistant() {
        // 伪造后缀不继承权威度
        assert_eq!(authority_of("https://github.com.evil.com/x"), 1);
        assert_eq!(authority_of("https://evil-github.com/x"), 1);
        // 后缀必须按段：agov 不匹配 gov
        assert_eq!(authority_of("https://www.agov.cn/"), 1);
    }

    #[test]
    fn test_blacklist() {
        assert!(is_blacklisted("https://wenku.baidu.com/view/abc.html"));
        assert!(is_blacklisted("https://max.book118.com/html/xxx.htm"));
        assert!(!is_blacklisted("https://baike.baidu.com/item/x")); // 同主站不同子站不受牵连
        assert!(!is_blacklisted("https://github.com/x"));
        assert!(!is_blacklisted("garbage"));
    }
}
