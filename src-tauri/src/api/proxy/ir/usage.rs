//! IR usage 提取 — 从三种上游格式的流式 chunk 中提取 token usage。
//!
//! 每个 `extract_xxx_usage` 函数从单个 chunk 中提取增量 usage，
//! 由调用方累积到 `IrUsage` 中。

use serde_json::Value;

use super::types::{IrRequest, IrSystemContent, IrUsage};

/// 从 Anthropic 流式 chunk 提取 usage 增量。
///
/// - `message_start`: input_tokens + cache_creation（写缓存并入输入）+ cache_read
/// - `message_delta`: output_tokens + cache_read
/// - `content_block_delta`: output_chars（text/thinking 字符数，回退估算用）
pub fn extract_anthropic_usage(chunk: &Value) -> IrUsage {
    let event_type = chunk["type"].as_str().unwrap_or("");
    let mut usage = IrUsage::default();

    match event_type {
        "message_start" => {
            let u = &chunk["message"]["usage"];
            usage.input_tokens = u["input_tokens"].as_u64().unwrap_or(0)
                + u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
            usage.cache_read_input_tokens =
                u["cache_read_input_tokens"].as_u64().unwrap_or(0);
            usage.cache_creation_input_tokens =
                u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
        }
        "message_delta" => {
            let u = &chunk["usage"];
            usage.output_tokens = u["output_tokens"].as_u64().unwrap_or(0);
            usage.cache_read_input_tokens =
                u["cache_read_input_tokens"].as_u64().unwrap_or(0);
        }
        "content_block_delta" => {
            let delta = &chunk["delta"];
            let chars = match delta["type"].as_str() {
                Some("text_delta") => delta["text"]
                    .as_str()
                    .map(|s| s.chars().count() as u64)
                    .unwrap_or(0),
                Some("thinking_delta") => delta["thinking"]
                    .as_str()
                    .map(|s| s.chars().count() as u64)
                    .unwrap_or(0),
                _ => 0,
            };
            usage.output_chars = chars;
        }
        _ => {}
    }

    usage
}

/// 从 OpenAI Chat Completions 流式 chunk 提取 usage 增量。
///
/// OpenAI 的 usage 在最后一个 chunk 中给出（需 `stream_options.include_usage`）。
/// 支持多种缓存字段：
/// - `prompt_cache_hit_tokens`（DeepSeek/Kimi）
/// - `prompt_tokens_details.cached_tokens`（OpenAI 标准）
/// - `cache_read_input_tokens`（部分兼容上游）
pub fn extract_chat_usage(chunk: &Value) -> IrUsage {
    let mut usage = IrUsage::default();

    if let Some(u) = chunk.get("usage") {
        if let Some(pt) = u.get("prompt_tokens").and_then(|v| v.as_u64()) {
            // 缓存命中字段统一处理
            let cache_hit = u
                .get("prompt_cache_hit_tokens")
                .and_then(|v| v.as_u64())
                .or_else(|| {
                    u.get("prompt_tokens_details")
                        .and_then(|d| d.get("cached_tokens"))
                        .and_then(|v| v.as_u64())
                });

            if let Some(hit) = cache_hit {
                usage.cache_read_input_tokens = hit;
                if let Some(miss) = u
                    .get("prompt_cache_miss_tokens")
                    .and_then(|v| v.as_u64())
                {
                    usage.input_tokens = miss;
                } else {
                    usage.input_tokens = pt.saturating_sub(hit);
                }
            } else {
                usage.input_tokens = pt;
            }
        }
        if let Some(ct) = u.get("completion_tokens").and_then(|v| v.as_u64()) {
            usage.output_tokens = ct;
        }
        // 透传上游自报的 cache_read（若有）
        if let Some(cr) = u
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
        {
            usage.cache_read_input_tokens = cr;
        }
    }

    // 字符数回退估算
    if let Some(content) = chunk
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("delta"))
        .and_then(|d| d.get("content"))
        .and_then(|c| c.as_str())
    {
        usage.output_chars = content.chars().count() as u64;
    }

    usage
}

/// 从 OpenAI Responses 流式 chunk 提取 usage 增量。
///
/// Responses API 的 usage 在 `response.completed` 事件中给出。
pub fn extract_responses_usage(chunk: &Value) -> IrUsage {
    let mut usage = IrUsage::default();

    // response.completed 事件携带 usage
    if chunk["type"].as_str() == Some("response.completed") {
        if let Some(u) = chunk.get("response").and_then(|r| r.get("usage")) {
            usage.input_tokens = u
                .get("input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            usage.output_tokens = u
                .get("output_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            // Responses API 缓存字段
            if let Some(details) = u.get("input_tokens_details") {
                usage.cache_read_input_tokens = details
                    .get("cached_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
            }
        }
    }

    // 字符数回退估算
    if let Some(delta) = chunk.get("delta").and_then(|d| d.as_str()) {
        usage.output_chars = delta.chars().count() as u64;
    }

    usage
}

/// 粗估 IR 请求的输入 token 数（chars / 4），用于 message_start 占位。
///
/// 估算口径：system + messages 的文本字符数 / 4（粗略，仅占位用）。
/// 至少返回 1，避免占位为 0。
pub fn estimate_input_tokens(req: &IrRequest) -> u64 {
    let mut chars: usize = 0;

    // System prompt
    if let Some(ref system) = req.system {
        match system {
            IrSystemContent::Text(t) => chars += t.chars().count(),
            IrSystemContent::Blocks(blocks) => {
                for b in blocks {
                    chars += b.text.chars().count();
                }
            }
        }
    }

    // Messages
    for msg in &req.messages {
        for block in &msg.content {
            match block {
                super::types::IrContentBlock::Text { text, .. } => {
                    chars += text.chars().count();
                }
                super::types::IrContentBlock::Thinking { thinking, .. } => {
                    chars += thinking.chars().count();
                }
                super::types::IrContentBlock::ToolResult { content, .. } => match content {
                    super::types::IrToolResultContent::Text(t) => chars += t.chars().count(),
                    super::types::IrToolResultContent::Blocks(blocks) => {
                        for b in blocks {
                            if let super::types::IrContentBlock::Text { text, .. } = b {
                                chars += text.chars().count();
                            }
                        }
                    }
                },
                _ => {}
            }
        }
    }

    // 4 字符 ≈ 1 token；至少返回 1
    ((chars / 4) as u64).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_extract_anthropic_usage_message_start() {
        let chunk = json!({
            "type": "message_start",
            "message": {
                "usage": {
                    "input_tokens": 200,
                    "cache_creation_input_tokens": 1500,
                    "cache_read_input_tokens": 8000,
                    "output_tokens": 0
                }
            }
        });
        let u = extract_anthropic_usage(&chunk);
        assert_eq!(u.input_tokens, 1700); // 200 + 1500
        assert_eq!(u.cache_read_input_tokens, 8000);
        assert_eq!(u.cache_creation_input_tokens, 1500);
    }

    #[test]
    fn test_extract_anthropic_usage_message_delta() {
        let chunk = json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 300, "cache_read_input_tokens": 8000}
        });
        let u = extract_anthropic_usage(&chunk);
        assert_eq!(u.output_tokens, 300);
        assert_eq!(u.cache_read_input_tokens, 8000);
    }

    #[test]
    fn test_extract_anthropic_usage_content_block_delta() {
        let chunk = json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "Hello world"}
        });
        let u = extract_anthropic_usage(&chunk);
        assert_eq!(u.output_chars, 11);
    }

    #[test]
    fn test_extract_chat_usage_basic() {
        let chunk = json!({
            "id": "chatcmpl-1",
            "choices": [],
            "usage": {
                "prompt_tokens": 9700,
                "completion_tokens": 300,
                "prompt_tokens_details": {"cached_tokens": 8000}
            }
        });
        let u = extract_chat_usage(&chunk);
        assert_eq!(u.input_tokens, 1700); // 9700 - 8000
        assert_eq!(u.output_tokens, 300);
        assert_eq!(u.cache_read_input_tokens, 8000);
    }

    #[test]
    fn test_extract_chat_usage_deepseek_cache() {
        let chunk = json!({
            "id": "chatcmpl-1",
            "choices": [],
            "usage": {
                "prompt_tokens": 9700,
                "completion_tokens": 300,
                "prompt_cache_hit_tokens": 8000,
                "prompt_cache_miss_tokens": 1700
            }
        });
        let u = extract_chat_usage(&chunk);
        assert_eq!(u.input_tokens, 1700);
        assert_eq!(u.cache_read_input_tokens, 8000);
    }

    #[test]
    fn test_extract_chat_usage_no_cache() {
        let chunk = json!({
            "id": "chatcmpl-1",
            "choices": [],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 50
            }
        });
        let u = extract_chat_usage(&chunk);
        assert_eq!(u.input_tokens, 100);
        assert_eq!(u.output_tokens, 50);
        assert_eq!(u.cache_read_input_tokens, 0);
    }

    #[test]
    fn test_extract_chat_usage_char_count() {
        let chunk = json!({
            "id": "chatcmpl-1",
            "choices": [{"index": 0, "delta": {"content": "Hello"}}]
        });
        let u = extract_chat_usage(&chunk);
        assert_eq!(u.output_chars, 5);
    }

    #[test]
    fn test_extract_responses_usage_completed() {
        let chunk = json!({
            "type": "response.completed",
            "response": {
                "usage": {
                    "input_tokens": 500,
                    "output_tokens": 200,
                    "input_tokens_details": {"cached_tokens": 300}
                }
            }
        });
        let u = extract_responses_usage(&chunk);
        assert_eq!(u.input_tokens, 500);
        assert_eq!(u.output_tokens, 200);
        assert_eq!(u.cache_read_input_tokens, 300);
    }

    #[test]
    fn test_extract_responses_usage_delta_chars() {
        let chunk = json!({
            "type": "response.output_text.delta",
            "delta": "Hello world"
        });
        let u = extract_responses_usage(&chunk);
        assert_eq!(u.output_chars, 11);
    }

    #[test]
    fn test_estimate_input_tokens_basic() {
        let req = IrRequest {
            model: "test".to_string(),
            system: Some(IrSystemContent::Text("You are helpful.".to_string())),
            messages: vec![super::super::types::IrMessage {
                role: super::super::types::IrRole::User,
                content: vec![super::super::types::IrContentBlock::Text {
                    text: "Hello world, this is a test.".to_string(),
                    cache_control: None,
                }],
            }],
            tools: vec![],
            tool_choice: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            thinking: None,
            stream: false,
        };
        let est = estimate_input_tokens(&req);
        // "You are helpful." (16) + "Hello world, this is a test." (28) = 44 chars / 4 = 11
        assert_eq!(est, 11);
    }

    #[test]
    fn test_estimate_input_tokens_minimum_one() {
        let req = IrRequest {
            model: "test".to_string(),
            system: None,
            messages: vec![],
            tools: vec![],
            tool_choice: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            thinking: None,
            stream: false,
        };
        assert_eq!(estimate_input_tokens(&req), 1);
    }
}
