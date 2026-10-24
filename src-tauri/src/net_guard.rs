//! 出站 URL 安全守卫（SSRF 防护）。
//!
//! 触发面：`web_fetch` 接受模型生成的任意 URL，`web_search` 富化阶段也会抓取
//! Bing 返回的第三方链接。两者都在**网关进程内**发起请求——而管理 API
//! （`/api/*`）只信任 loopback 来源。不设防时，一条提示注入就能让模型把
//! `http://127.0.0.1:19068/api/...` 当成普通网页读进上下文。
//!
//! 两道闸：
//! 1. **字面量检查**（同步）：协议白名单 + 主机名黑名单 + IP 字面量私有段。
//!    重定向策略闭包里只能用这一道（reqwest 的 `Policy::custom` 是同步的）。
//! 2. **解析后检查**（异步）：域名先 DNS 解析，任一解析结果落在私有段即拒绝。
//!
//! 已知边界：检查与实际连接之间存在 DNS rebinding 窗口。桌面单用户场景下
//! 不引入「解析并钉死 IP」的复杂度，仅记录该取舍（见 ADR-056）。
//! 局域网地址（192.168/10/172.16 等）同样被拦截——web_fetch 不承担内网探测
//! 职责；需要访问内网时应在客户端侧用浏览器完成，而不是交给模型。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use url::{Host, Url};

/// DNS 解析超时（避免恶意域名拖慢整个抓取）。
const DNS_TIMEOUT: Duration = Duration::from_secs(3);

/// 同步字面量检查：命中返回人类可读原因。
///
/// 不查 DNS——供 reqwest 重定向策略等同步上下文使用。
pub(crate) fn blocked_literal_reason(url: &Url) -> Option<&'static str> {
    match url.scheme() {
        "http" | "https" => {}
        _ => return Some("仅允许 http/https 协议"),
    }
    match url.host()? {
        Host::Ipv4(ip) => blocked_ipv4_reason(ip),
        Host::Ipv6(ip) => blocked_ipv6_reason(ip),
        Host::Domain(domain) => blocked_domain_reason(domain),
    }
}

/// 完整校验：字面量检查 + DNS 解析后逐地址检查。返回 `Err(原因)` 表示拒绝。
pub(crate) async fn ensure_public_url(url: &Url) -> Result<(), String> {
    if let Some(reason) = blocked_literal_reason(url) {
        return Err(reason.to_string());
    }
    let Some(host) = url.host_str() else {
        return Err("URL 缺少主机名".to_string());
    };
    // 字面量 IP 已在上面判定，这里只剩域名需要解析
    if host.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    let port = url.port_or_known_default().unwrap_or(80);
    let addrs = match tokio::time::timeout(DNS_TIMEOUT, tokio::net::lookup_host((host, port))).await
    {
        Ok(Ok(addrs)) => addrs,
        Ok(Err(e)) => return Err(format!("DNS 解析失败：{e}")),
        Err(_) => return Err("DNS 解析超时".to_string()),
    };
    let mut any = false;
    for addr in addrs {
        any = true;
        if let Some(reason) = blocked_ip_reason(addr.ip()) {
            return Err(format!("目标解析到{reason}（{}）", addr.ip()));
        }
    }
    if !any {
        return Err("DNS 未解析出任何地址".to_string());
    }
    Ok(())
}

fn blocked_ip_reason(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => blocked_ipv4_reason(v4),
        IpAddr::V6(v6) => blocked_ipv6_reason(v6),
    }
}

fn blocked_ipv4_reason(ip: Ipv4Addr) -> Option<&'static str> {
    let o = ip.octets();
    if ip.is_loopback() {
        Some("回环地址")
    } else if ip.is_private() {
        Some("私有地址")
    } else if ip.is_link_local() {
        // 169.254.0.0/16：含 AWS/GCP/Azure 元数据服务 169.254.169.254
        Some("链路本地地址")
    } else if ip.is_unspecified() {
        Some("未指定地址")
    } else if ip.is_broadcast() {
        Some("广播地址")
    } else if ip.is_multicast() {
        Some("组播地址")
    } else if o[0] == 100 && (64..=127).contains(&o[1]) {
        // 100.64.0.0/10：运营商级 NAT
        Some("CGNAT 地址")
    } else if o[0] == 198 && matches!(o[1], 18 | 19) {
        // 198.18.0.0/15：基准测试保留段
        Some("基准测试保留地址")
    } else if (o[0] == 192 && o[1] == 0 && o[2] == 2)
        || (o[0] == 198 && o[1] == 51 && o[2] == 100)
        || (o[0] == 203 && o[1] == 0 && o[2] == 113)
    {
        // RFC 5737 文档示例段
        Some("文档保留地址")
    } else if o[0] >= 240 {
        // 240.0.0.0/4
        Some("保留地址")
    } else {
        None
    }
}

fn blocked_ipv6_reason(ip: Ipv6Addr) -> Option<&'static str> {
    // ::ffff:a.b.c.d（IPv4 映射）按内层 IPv4 判定
    if let Some(v4) = ip.to_ipv4_mapped() {
        return blocked_ipv4_reason(v4);
    }
    let seg = ip.segments();
    if ip.is_loopback() {
        Some("IPv6 回环地址")
    } else if ip.is_unspecified() {
        Some("IPv6 未指定地址")
    } else if seg[0] & 0xfe00 == 0xfc00 {
        // fc00::/7：唯一本地地址
        Some("IPv6 唯一本地地址")
    } else if seg[0] & 0xffc0 == 0xfe80 {
        // fe80::/10：链路本地
        Some("IPv6 链路本地地址")
    } else if ip.is_multicast() {
        Some("IPv6 组播地址")
    } else {
        None
    }
}

fn blocked_domain_reason(domain: &str) -> Option<&'static str> {
    let host = domain.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return Some("空主机名");
    }
    if host == "localhost" || host.ends_with(".localhost") {
        return Some("localhost");
    }
    // mDNS / 私有 DNS 后缀：解析结果通常指向内网
    if host.ends_with(".local") || host.ends_with(".internal") || host.ends_with(".home.arpa") {
        return Some("内网专用域名后缀");
    }
    // 云厂商元数据服务的域名形式
    if matches!(
        host.as_str(),
        "metadata.google.internal" | "metadata" | "instance-data" | "metadata.azure.com"
    ) {
        return Some("云元数据服务");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn test_blocks_loopback_and_private_literals() {
        for target in [
            "http://127.0.0.1:19068/api/settings",
            "http://127.0.0.1/",
            "http://10.0.0.1/",
            "http://172.16.0.1/",
            "http://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://0.0.0.0/",
            "http://100.64.0.1/",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[fc00::1]/",
            "http://[fe80::1]/",
        ] {
            assert!(
                blocked_literal_reason(&url(target)).is_some(),
                "should block {target}"
            );
        }
    }

    #[test]
    fn test_blocks_private_hostnames() {
        for target in [
            "http://localhost:19068/",
            "http://foo.localhost/",
            "http://router.local/",
            "http://metadata.google.internal/",
            "http://svc.internal/",
        ] {
            assert!(
                blocked_literal_reason(&url(target)).is_some(),
                "should block {target}"
            );
        }
    }

    #[test]
    fn test_allows_public_targets() {
        for target in [
            "https://example.com/",
            "https://github.com/tokio-rs/tokio",
            "http://8.8.8.8/",
            "https://[2001:4860:4860::8888]/",
        ] {
            assert!(
                blocked_literal_reason(&url(target)).is_none(),
                "should allow {target}"
            );
        }
    }

    #[test]
    fn test_rejects_non_http_schemes() {
        // Url::parse 能构造出的非 http 协议（file/data/ftp）一律拒绝
        assert!(blocked_literal_reason(&url("file:///etc/passwd")).is_some());
        assert!(blocked_literal_reason(&url("ftp://example.com/x")).is_some());
    }

    /// DNS 检查：`localhost` 已在字面量阶段拦下，这里覆盖「域名解析到内网」。
    /// 用 `.invalid` TLD 会解析失败——两条分支都必须返回 Err。
    #[tokio::test]
    async fn test_ensure_public_url_rejects_unresolvable() {
        let err = ensure_public_url(&url("https://does-not-exist.invalid/"))
            .await
            .unwrap_err();
        assert!(err.contains("DNS"), "unexpected error: {err}");
    }
}
