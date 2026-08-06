//! 代理层入口：Anthropic / OpenAI 格式互转 + 上游转发 + WebSearch 劫持。
//!
//! 三个 pub handler 在 `handler`；认证 / 路由解析 / 密钥轮换 /
//! WebSearch 劫持分别下沉到 `auth` / `route` / `key_rotation` /
//! `websearch`。`translate` / `sniff` / `stream` / `forward` 为既有子模块。

pub mod auth;
pub mod failover;
pub mod forward;
pub mod handler;
pub mod key_rotation;
pub mod quota;
pub mod route;
pub mod sniff;
pub mod stream;
pub mod translate;
pub mod websearch;

pub use handler::{proxy_anthropic_messages, proxy_list_models, proxy_openai_chat};
pub use quota::user_balance;

/// 等待上游返回响应头的基准超时（秒）。上游建连后挂起不响应时，send() 会卡死，
/// 这里用超时兜底，避免整个重试循环被一次挂起的请求拖住。
///
/// 历史值 60s 过短：大上下文（~80k token 缓存输入）+ 上游排队时，首字节常超
/// 60s。网关提前放弃并发 SSE error event 断流 → Claude Code 把「流中断」当作
/// 可回退错误，切换非流式重试（网关强制 stream=true，回退请求收到的是 SSE，
/// 无法解析为 Message JSON）→ 用户看到「API returned an empty or malformed
/// response (HTTP 200)」。基准提到 300s（对齐 Claude Code 的 API_TIMEOUT_MS
/// 默认值与 CLI 侧 SSE 空闲看门狗 90s：等待头期间网关每 15s 的 keepalive
/// 足以维持客户端连接），超大输入按 `header_timeout_for()` 再放宽。
pub(crate) const UPSTREAM_HEADER_TIMEOUT_SECS: u64 = 300;
/// 流式响应中相邻 chunk 之间的最大间隔。上游中途断流但不关连接时，
/// stream.next() 会永久挂起；超过该间隔即视为断流，正常收尾返回。
pub(crate) const UPSTREAM_CHUNK_TIMEOUT_SECS: u64 = 120;

/// /v1/* 代理入口的请求体上限（64MiB）。
///
/// axum 默认只放行 2MiB，超长会话（多轮历史 + 工具结果 + base64 截图）会被
/// 413 直接拒绝——这正是「输入太大」报错的另一半成因。Anthropic 对 base64
/// 图片本身上限 5MB/张，64MiB 足够覆盖多模态大会话。
pub(crate) const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024 * 1024;

/// 按估算输入规模放宽「等待上游响应头」的超时：上下文越大（尤其缓存命中），
/// 上游排队 + 首 token 前的处理时间越长。
/// 100k token 以上 → 600s；50k 以上 → 480s；其余 → 基准 300s。
pub(crate) fn header_timeout_for(est_input_tokens: u64) -> u64 {
    if est_input_tokens >= 100_000 {
        600
    } else if est_input_tokens >= 50_000 {
        480
    } else {
        UPSTREAM_HEADER_TIMEOUT_SECS
    }
}
