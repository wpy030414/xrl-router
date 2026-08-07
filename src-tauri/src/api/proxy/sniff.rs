//! Passthrough stream sniffer.
//!
//! The two same-format proxy paths (`/v1/messages` -> Anthropic upstream,
//! `/v1/chat/completions` -> OpenAI upstream) forward the upstream SSE byte
//! stream to the client verbatim. To record token usage for these paths we
//! wrap the byte stream in [`SniffStream`]: every chunk is forwarded unchanged
//! while a lightweight SSE parser accumulates token usage visible in `data:`
//! payloads. The parsing reuses the same buffer-then-split-on-`\n\n` pattern as
//! the translation loops in `proxy.rs`, so SSE frames split across byte chunks
//! are handled correctly.

use bytes::Bytes;
use futures::stream::Stream;
use serde_json::Value;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Token usage accumulated while sniffing a passthrough SSE stream.
#[derive(Debug, Clone, Default)]
pub struct SniffedUsage {
    /// 全部「新输入」token：未缓存输入 + 首次写缓存的输入。
    /// 写缓存只是首次处理输入，本质属于输入，不单列。
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Anthropic prompt caching: tokens read from cache（命中复用，真正的「缓存」）。
    pub cache_read_input_tokens: u64,
    /// Emitted text/thinking char count, used as a fallback (chars / 4) when
    /// the upstream reports no token counts.
    pub output_chars: u64,
}

/// Wraps a `reqwest` byte stream to sniff token usage without modifying bytes.
pub struct SniffStream<S> {
    inner: S,
    buffer: String,
    usage: SniffedUsage,
    provider_kind: String, // "messages" or "chat_completions"
}

impl<S> SniffStream<S>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
{
    pub fn new(inner: S, provider_kind: &str) -> Self {
        Self {
            inner,
            buffer: String::new(),
            usage: SniffedUsage::default(),
            provider_kind: provider_kind.to_string(),
        }
    }

    /// Consume the wrapper and return the accumulated usage. Call only after
    /// the stream has ended.
    pub fn into_usage(self) -> SniffedUsage {
        self.usage
    }

    /// Parse every complete SSE frame (`\n\n`-delimited) currently buffered.
    /// A frame may contain several `data:` lines; each line is parsed once.
    fn process_buffer(&mut self) {
        while let Some(pos) = self.buffer.find("\n\n") {
            let frame = self.buffer[..pos].to_string();
            self.buffer = self.buffer[pos + 2..].to_string();
            for line in frame.lines() {
                if let Some(data) = line.strip_prefix("data: ") {
                    if data == "[DONE]" {
                        continue;
                    }
                    if let Ok(json) = serde_json::from_str::<Value>(data) {
                        self.extract_usage(&json);
                    }
                }
            }
        }
    }

    fn extract_usage(&mut self, json: &Value) {
        match self.provider_kind.as_str() {
            "messages" => match json["type"].as_str().unwrap_or("") {
                "message_start" => {
                    let usage = &json["message"]["usage"];
                    // input_tokens（未缓存）+ cache_creation（首次写缓存）= 全部新输入。
                    // 写缓存只是首次处理输入，并入 input，不单列。
                    let it = usage["input_tokens"].as_u64().unwrap_or(0);
                    let cw = usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                    self.usage.input_tokens = it + cw;
                    if let Some(cr) = usage["cache_read_input_tokens"].as_u64() {
                        self.usage.cache_read_input_tokens = cr;
                    }
                }
                "message_delta" => {
                    let usage = &json["usage"];
                    if let Some(ot) = usage["output_tokens"].as_u64() {
                        self.usage.output_tokens = ot;
                    }
                    // cache_read 在 message_delta 给出（命中读取）
                    if let Some(cr) = usage["cache_read_input_tokens"].as_u64() {
                        self.usage.cache_read_input_tokens = cr;
                    }
                }
                "content_block_delta" => {
                    let delta = &json["delta"];
                    match delta["type"].as_str() {
                        Some("text_delta") => {
                            self.usage.output_chars += delta["text"]
                                .as_str()
                                .map(|s| s.chars().count() as u64)
                                .unwrap_or(0);
                        }
                        Some("thinking_delta") => {
                            self.usage.output_chars += delta["thinking"]
                                .as_str()
                                .map(|s| s.chars().count() as u64)
                                .unwrap_or(0);
                        }
                        _ => {}
                    }
                }
                _ => {}
            },
            // OpenAI / compatible: the final chunk carries a top-level usage.
            _ => {
                if let Some(usage) = json.get("usage") {
                    if let Some(pt) = usage.get("prompt_tokens").and_then(|v| v.as_u64()) {
                        self.usage.input_tokens = pt;
                    }
                    if let Some(ct) = usage.get("completion_tokens").and_then(|v| v.as_u64()) {
                        self.usage.output_tokens = ct;
                    }
                }
                if let Some(content) = json
                    .get("choices")
                    .and_then(|c| c.as_array())
                    .and_then(|a| a.first())
                    .and_then(|c| c.get("delta"))
                    .and_then(|d| d.get("content"))
                    .and_then(|c| c.as_str())
                {
                    self.usage.output_chars += content.chars().count() as u64;
                }
            }
        }
    }
}

impl<S> Stream for SniffStream<S>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
{
    type Item = Result<Bytes, reqwest::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.inner).poll_next(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                self.buffer.push_str(&String::from_utf8_lossy(&bytes));
                self.process_buffer();
                // Forward the original bytes unmodified.
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => {
                self.process_buffer();
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}
