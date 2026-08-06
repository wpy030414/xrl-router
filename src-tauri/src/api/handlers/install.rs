//! install 页面托管 + 本机局域网 IP 查询。
//!
//! `serve_install_page` 返回编译进二进制的静态 HTML（含生成安装命令的内联 JS），
//! 由单 listener（0.0.0.0:19068）暴露给局域网设备。`get_local_ip` 供主机 Tauri UI
//! 拼装分发链接，走管理 listener（127.0.0.1:19068）。

use std::sync::Arc;

use axum::extract::State;
use axum::response::Html;
use axum::Json;
use serde_json::json;

use crate::gateway::server::AppState;

/// install 页面静态 HTML（编译进二进制，零运行时文件依赖）。
static INSTALL_HTML: &str = include_str!("../../../assets/install.html");

/// GET /install — 返回 install 页面（公共端口）。
pub(crate) async fn serve_install_page() -> Html<&'static str> {
    Html(INSTALL_HTML)
}

/// GET /api/install/local-ip — 返回本机非 loopback 出口 IP（管理端口）。
/// UDP socket 连 8.8.8.8:80（不发数据）取本机出口地址，过滤回环。
pub(crate) async fn get_local_ip(State(_state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let ip = detect_local_ip();
    Json(json!({ "ip": ip }))
}

fn detect_local_ip() -> Option<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    sock.local_addr()
        .ok()
        .map(|a| a.ip().to_string())
        .filter(|s| !s.starts_with("127."))
}
