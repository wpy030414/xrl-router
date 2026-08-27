//! 统一 HTTP 客户端工厂：自动继承系统代理（环境变量 → Windows 注册表）。

use std::sync::OnceLock;

/// 当前解析出的系统代理 URL（如 `http://127.0.0.1:7897`）。无代理时为 None。
///
/// 用 OnceLock 缓存一次：代理在应用运行期间几乎不会变（Clash 端口固定），
/// 省掉每次建 client 都读注册表。
pub fn system_proxy() -> Option<&'static str> {
    static PROXY: OnceLock<Option<String>> = OnceLock::new();
    PROXY.get_or_init(resolve_system_proxy).as_deref()
}

fn resolve_system_proxy() -> Option<String> {
    // 1. 环境变量优先（跨平台标准，也便于开发时覆盖）。
    for var in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    // 2. Windows：读注册表 Internet Settings 的系统代理。
    //    ProxyEnable=1 且 ProxyServer 非空才生效；跳过 PAC (AutoConfigURL)。
    #[cfg(windows)]
    if let Some(proxy) = resolve_windows_registry_proxy() {
        return Some(proxy);
    }
    // 3. macOS：读 scutil --proxy 的系统代理（所有网络接口汇总，不依赖接口名）。
    #[cfg(target_os = "macos")]
    if let Some(proxy) = resolve_macos_proxy() {
        return Some(proxy);
    }
    None
}

/// macOS: 从 `scutil --proxy` 读取系统代理（HTTP/HTTPS）。
///
/// 为什么用 scutil 而非 networksetup：`networksetup -getwebproxy` 需要指定
/// 网络接口名（Wi-Fi/Ethernet…），接口名因机器而异（USB 网卡、热点等），
/// 猜错就返回 None。`scutil --proxy` 输出**当前生效**的代理配置（系统按
/// 服务顺序聚合），不依赖接口名，更可靠。
#[cfg(target_os = "macos")]
fn resolve_macos_proxy() -> Option<String> {
    let output = std::process::Command::new("scutil")
        .args(["--proxy"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut http_enabled = false;
    let mut http_server = String::new();
    let mut http_port = String::new();
    let mut https_enabled = false;
    let mut https_server = String::new();
    let mut https_port = String::new();

    for line in stdout.lines() {
        let line = line.trim();
        // 形如: HTTPEnable : 1
        if let Some(v) = line.strip_prefix("HTTPEnable :") {
            http_enabled = v.trim() == "1";
        } else if let Some(v) = line.strip_prefix("HTTPSEnable :") {
            https_enabled = v.trim() == "1";
        } else if let Some(v) = line.strip_prefix("HTTPProxy :") {
            http_server = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("HTTPPort :") {
            http_port = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("HTTPSProxy :") {
            https_server = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("HTTPSPort :") {
            https_port = v.trim().to_string();
        }
    }

    // HTTPS 代理优先（多数场景 HTTPS 流量走独立代理），HTTP 兜底。
    if https_enabled && !https_server.is_empty() && !https_port.is_empty() {
        return Some(format!("http://{}:{}", https_server, https_port));
    }
    if http_enabled && !http_server.is_empty() && !http_port.is_empty() {
        return Some(format!("http://{}:{}", http_server, http_port));
    }
    None
}

#[cfg(windows)]
fn resolve_windows_registry_proxy() -> Option<String> {
    const HKCU: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    let query = |name: &str| -> Option<String> {
        let out = std::process::Command::new("reg")
            .args(["query", HKCU, "/v", name])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8_lossy(&out.stdout).lines().find_map(|l| {
            let s = l.trim();
            let (_, v) = s.split_once("REG_SZ")?;
            Some(v.trim().trim_matches('"').to_string())
        })
    };

    let enabled = query("ProxyEnable")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0)
        != 0;
    if !enabled {
        return None;
    }
    let server = query("ProxyServer")?;
    if server.is_empty() {
        return None;
    }
    // 形如 "127.0.0.1:7897" 或 "http://127.0.0.1:7897"。
    Some(if server.contains("://") {
        server
    } else {
        format!("http://{}", server)
    })
}

/// 构建带系统代理的 reqwest 客户端。
///
/// 调用方可继续链式覆盖 timeout / cookie_store 等。
pub fn build_http_client() -> reqwest::ClientBuilder {
    let mut builder = reqwest::Client::builder();
    if let Some(proxy) = system_proxy() {
        if let Ok(p) = reqwest::Proxy::all(proxy) {
            let p = if no_proxy_list().is_empty() {
                p
            } else {
                p.no_proxy(reqwest::NoProxy::from_string(&no_proxy_list()))
            };
            builder = builder.proxy(p);
        }
    }
    builder
}

/// NO_PROXY 列表：默认豁免本机回环（插件系统的 upstream 可能在 localhost），
/// 并附加环境变量 NO_PROXY / no_proxy 的额外项。
fn no_proxy_list() -> String {
    let mut list: Vec<String> = ["localhost", "127.0.0.1", "[::1]"]
        .into_iter()
        .map(String::from)
        .collect();
    if let Ok(extra) = std::env::var("NO_PROXY").or_else(|_| std::env::var("no_proxy")) {
        for part in extra.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if !list.iter().any(|p| p == part) {
                list.push(part.to_string());
            }
        }
    }
    list.join(",")
}

/// 便捷方法：带系统代理 + 默认构建。
pub fn http_client() -> reqwest::Client {
    build_http_client()
        .build()
        .expect("failed to build http client")
}

