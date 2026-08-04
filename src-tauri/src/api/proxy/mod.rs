//! 代理层入口：Anthropic / OpenAI 格式互转 + 上游转发 + WebSearch 劫持。
//!
//! 三个 pub handler 在 `handler`；认证 / 路由解析 / 密钥轮换 / 上游错误
//! 转发 / WebSearch 劫持分别下沉到 `auth` / `route` / `key_rotation` /
//! `upstream` / `websearch`。`translate` / `sniff` 为既有子模块。

pub mod auth;
pub mod failover;
pub mod handler;
pub mod key_rotation;
pub mod quota;
pub mod route;
pub mod sniff;
pub mod stream;
pub mod translate;
pub mod upstream;
pub mod websearch;

pub use handler::{proxy_anthropic_messages, proxy_list_models, proxy_openai_chat};
pub use quota::user_balance;

/// 等待上游返回响应头的最大时长。上游建连后挂起不响应时，send() 会卡死，
/// 这里用超时兜底，避免整个重试循环被一次挂起的请求拖住。
pub(crate) const UPSTREAM_HEADER_TIMEOUT_SECS: u64 = 60;
/// 流式响应中相邻 chunk 之间的最大间隔。上游中途断流但不关连接时，
/// stream.next() 会永久挂起；超过该间隔即视为断流，正常收尾返回。
pub(crate) const UPSTREAM_CHUNK_TIMEOUT_SECS: u64 = 120;
