//! 代理层入口：三种 LLM 协议格式互转 + 上游转发 + 搜索工具剔除（MCP 模式）。
//!
//! 三个 pub handler 在 `handler`；认证 / 路由解析 / 密钥轮换分别下沉到
//! `auth` / `route` / `key_rotation`。`ir` / `stream` / `forward` 为子模块。
//! 搜索能力由本地 MCP 端点（`/mcp`，见 `src-tauri/src/mcp/`）提供，
//! 代理仅在 `mcp_websearch` 开启时剔除请求自带的搜索类工具。

pub mod auth;
pub mod audit;
pub mod failover;
pub mod forward;
pub mod handler;
pub mod ir;
pub mod key_rotation;
pub mod non_stream;
pub mod quota;
pub mod route;
pub mod stream;

pub use handler::{
    proxy_chat_completions, proxy_list_models, proxy_messages, proxy_responses,
};
pub use quota::user_balance;

/// 等待上游返回响应头的基准超时（秒）。
pub(crate) const UPSTREAM_HEADER_TIMEOUT_SECS: u64 = 300;
/// 流式响应中相邻 chunk 之间的最大间隔。
pub(crate) const UPSTREAM_CHUNK_TIMEOUT_SECS: u64 = 120;

/// /v1/* 代理入口的请求体上限（64MiB）。
pub(crate) const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024 * 1024;

/// 按估算输入规模放宽「等待上游响应头」的超时。
pub(crate) fn header_timeout_for(est_input_tokens: u64) -> u64 {
    if est_input_tokens >= 100_000 {
        600
    } else if est_input_tokens >= 50_000 {
        480
    } else {
        UPSTREAM_HEADER_TIMEOUT_SECS
    }
}
