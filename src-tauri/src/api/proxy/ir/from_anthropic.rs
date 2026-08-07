//! Anthropic Messages → IR 方向的格式翻译：请求体与流式 chunk。
//!
//! Anthropic 的 SSE 事件几乎 1:1 映射到 IrStreamEvent，
//! 主要工作是把 `Value` 解析为强类型 IR。

use serde_json::Value;

use super::types::*;

/// 将 Anthropic Messages 请求体解析为 IR。
pub fn anthropic_req_to_ir(req: &Value) -> IrRequest {
    let model = req["model"].as_str().unwrap_or("").to_string();
    let stream = req.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    // System prompt
    let system = req.get("system").map(|s| match s {
        Value::String(t) => IrSystemContent::Text(t.clone()),
        Value::Array(blocks) => {
            let parsed: Vec<IrSystemBlock> = blocks
                .iter()
                .map(|b| IrSystemBlock {
                    text: b["text"].as_str().unwrap_or("").to_string(),
                    cache_control: b.get("cache_control").cloned(),
                })
                .collect();
            IrSystemContent::Blocks(parsed)
        }
        _ => IrSystemContent::Text(String::new()),
    });

    // Messages
    let messages = parse_anthropic_messages(req);

    // Tools
    let tools = req
        .get("tools")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let name = t["name"].as_str()?;
                    Some(IrTool {
                        name: name.to_string(),
                        description: t.get("description").and_then(|d| d.as_str()).map(String::from),
                        input_schema: t
                            .get("input_schema")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}})),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    // Tool choice
    let tool_choice = req.get("tool_choice").map(parse_anthropic_tool_choice);

    // Thinking config
    let thinking = req.get("thinking").and_then(|t| {
        let enabled = t.get("type").and_then(|v| v.as_str()) == Some("enabled");
        if !enabled {
            return None;
        }
        Some(IrThinkingConfig {
            enabled: true,
            budget_tokens: t.get("budget_tokens").and_then(|v| v.as_u64()),
        })
    });

    IrRequest {
        model,
        system,
        messages,
        tools,
        tool_choice,
        max_tokens: req.get("max_tokens").and_then(|v| v.as_u64()),
        temperature: req.get("temperature").and_then(|v| v.as_f64()),
        top_p: req.get("top_p").and_then(|v| v.as_f64()),
        thinking,
        stream,
    }
}

/// 解析 Anthropic messages 数组。
fn parse_anthropic_messages(req: &Value) -> Vec<IrMessage> {
    req.get("messages")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|msg| {
                    let role = match msg["role"].as_str().unwrap_or("user") {
                        "assistant" => IrRole::Assistant,
                        _ => IrRole::User,
                    };
                    let content = parse_anthropic_content(&msg["content"]);
                    if content.is_empty() && role == IrRole::User {
                        // 纯 tool_result 消息会被展开为独立块，空消息跳过
                        return None;
                    }
                    Some(IrMessage { role, content })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 解析 Anthropic content 字段（字符串或块数组）。
fn parse_anthropic_content(content: &Value) -> Vec<IrContentBlock> {
    match content {
        Value::String(s) => {
            if s.is_empty() {
                vec![]
            } else {
                vec![IrContentBlock::Text {
                    text: s.clone(),
                    cache_control: None,
                }]
            }
        }
        Value::Array(blocks) => blocks.iter().filter_map(parse_anthropic_block).collect(),
        _ => vec![],
    }
}

/// 解析单个 Anthropic content block。
fn parse_anthropic_block(block: &Value) -> Option<IrContentBlock> {
    match block["type"].as_str()? {
        "text" => Some(IrContentBlock::Text {
            text: block["text"].as_str().unwrap_or("").to_string(),
            cache_control: block.get("cache_control").cloned(),
        }),
        "image" => {
            let source = &block["source"];
            let img_source = match source["type"].as_str().unwrap_or("") {
                "base64" => IrImageSource::Base64 {
                    media_type: source["media_type"]
                        .as_str()
                        .unwrap_or("image/png")
                        .to_string(),
                    data: source["data"].as_str().unwrap_or("").to_string(),
                },
                "url" => IrImageSource::Url {
                    url: source["url"].as_str().unwrap_or("").to_string(),
                },
                _ => return None,
            };
            Some(IrContentBlock::Image { source: img_source })
        }
        "thinking" => Some(IrContentBlock::Thinking {
            thinking: block["thinking"].as_str().unwrap_or("").to_string(),
            signature: block
                .get("signature")
                .and_then(|v| v.as_str())
                .map(String::from),
        }),
        "tool_use" => Some(IrContentBlock::ToolUse {
            id: block["id"].as_str().unwrap_or("").to_string(),
            name: block["name"].as_str().unwrap_or("").to_string(),
            input: block.get("input").cloned().unwrap_or(Value::Object(Default::default())),
        }),
        "tool_result" => {
            let tool_use_id = block["tool_use_id"].as_str().unwrap_or("").to_string();
            let is_error = block["is_error"].as_bool().unwrap_or(false);
            let content = match &block["content"] {
                Value::String(s) => IrToolResultContent::Text(s.clone()),
                Value::Array(blocks) => {
                    let parsed: Vec<IrContentBlock> =
                        blocks.iter().filter_map(parse_anthropic_block).collect();
                    IrToolResultContent::Blocks(parsed)
                }
                _ => IrToolResultContent::Text(String::new()),
            };
            Some(IrContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            })
        }
        _ => None,
    }
}

/// 解析 Anthropic tool_choice 字段。
fn parse_anthropic_tool_choice(tc: &Value) -> IrToolChoice {
    match tc {
        Value::String(s) => match s.as_str() {
            "any" => IrToolChoice::Any,
            "none" => IrToolChoice::None,
            _ => IrToolChoice::Auto,
        },
        Value::Object(obj) => match obj.get("type").and_then(|t| t.as_str()) {
            Some("any") => IrToolChoice::Any,
            Some("none") => IrToolChoice::None,
            Some("tool") => {
                let name = obj
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                IrToolChoice::Tool { name }
            }
            _ => IrToolChoice::Auto,
        },
        _ => IrToolChoice::Auto,
    }
}

// ═══════════════════════════════════════════════════════════════════
// 流式 chunk 解析
// ═══════════════════════════════════════════════════════════════════

/// Anthropic chunk 解析状态（Anthropic SSE 几乎 1:1 映射到 IR，状态很轻）。
pub struct AnthropicParseState {
    /// 已捕获的 usage（message_start + message_delta 累积）。
    pub usage: IrUsage,
}

impl AnthropicParseState {
    pub fn new() -> Self {
        Self {
            usage: IrUsage::default(),
        }
    }
}

impl Default for AnthropicParseState {
    fn default() -> Self {
        Self::new()
    }
}

/// 将 Anthropic 流式 chunk 解析为 IR 事件序列。
///
/// Anthropic SSE 事件与 IrStreamEvent 几乎同构，主要做：
/// - `message_start` → 提取 id/model/usage
/// - `content_block_start` → 映射块类型
/// - `content_block_delta` → 映射 delta 类型 + 累积 output_chars
/// - `message_delta` → 提取 stop_reason + usage
pub fn anthropic_chunk_to_ir(
    chunk: &Value,
    state: &mut AnthropicParseState,
) -> Vec<IrStreamEvent> {
    let event_type = chunk["type"].as_str().unwrap_or("");
    let mut events = Vec::new();

    match event_type {
        "message_start" => {
            let msg = &chunk["message"];
            let id = msg["id"].as_str().unwrap_or("msg_unknown").to_string();
            let model = msg["model"].as_str().unwrap_or("").to_string();

            // 提取初始 usage
            let usage = &msg["usage"];
            let it = usage["input_tokens"].as_u64().unwrap_or(0)
                + usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);
            let cr = usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
            state.usage.input_tokens = it;
            state.usage.cache_read_input_tokens = cr;
            state.usage.cache_creation_input_tokens =
                usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);

            events.push(IrStreamEvent::MessageStart { id, model });
        }
        "content_block_start" => {
            let index = chunk["index"].as_u64().unwrap_or(0) as usize;
            let block = &chunk["content_block"];
            let ir_block = match block["type"].as_str().unwrap_or("") {
                "text" => IrContentBlockStart::Text,
                "thinking" => IrContentBlockStart::Thinking,
                "tool_use" => IrContentBlockStart::ToolUse {
                    id: block["id"].as_str().unwrap_or("").to_string(),
                    name: block["name"].as_str().unwrap_or("").to_string(),
                },
                _ => return events, // 未知块类型跳过
            };
            events.push(IrStreamEvent::ContentBlockStart {
                index,
                block: ir_block,
            });
        }
        "content_block_delta" => {
            let index = chunk["index"].as_u64().unwrap_or(0) as usize;
            let delta = &chunk["delta"];
            let ir_delta = match delta["type"].as_str().unwrap_or("") {
                "text_delta" => {
                    let text = delta["text"].as_str().unwrap_or("").to_string();
                    state.usage.output_chars += text.chars().count() as u64;
                    IrContentDelta::TextDelta(text)
                }
                "thinking_delta" => {
                    let thinking = delta["thinking"].as_str().unwrap_or("").to_string();
                    state.usage.output_chars += thinking.chars().count() as u64;
                    IrContentDelta::ThinkingDelta(thinking)
                }
                "input_json_delta" => {
                    IrContentDelta::InputJsonDelta(
                        delta["partial_json"].as_str().unwrap_or("").to_string(),
                    )
                }
                _ => return events,
            };
            events.push(IrStreamEvent::ContentBlockDelta { index, delta: ir_delta });
        }
        "content_block_stop" => {
            let index = chunk["index"].as_u64().unwrap_or(0) as usize;
            events.push(IrStreamEvent::ContentBlockStop { index });
        }
        "message_delta" => {
            let stop_reason = chunk["delta"]["stop_reason"]
                .as_str()
                .map(IrStopReason::from_anthropic);
            let usage_obj = &chunk["usage"];
            if let Some(ot) = usage_obj["output_tokens"].as_u64() {
                state.usage.output_tokens = ot;
            }
            if let Some(cr) = usage_obj["cache_read_input_tokens"].as_u64() {
                state.usage.cache_read_input_tokens = cr;
            }
            events.push(IrStreamEvent::MessageDelta {
                stop_reason,
                usage: Some(state.usage.clone()),
            });
        }
        "message_stop" => {
            events.push(IrStreamEvent::MessageStop);
        }
        _ => {} // 忽略未知事件类型
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_basic_request() {
        let req = json!({
            "model": "claude-opus-4-8",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 4096,
            "system": "You are helpful.",
            "stream": true
        });
        let ir = anthropic_req_to_ir(&req);
        assert_eq!(ir.model, "claude-opus-4-8");
        assert_eq!(ir.max_tokens, Some(4096));
        assert!(ir.stream);
        assert!(matches!(ir.system, Some(IrSystemContent::Text(ref t)) if t == "You are helpful."));
        assert_eq!(ir.messages.len(), 1);
        assert_eq!(ir.messages[0].role, IrRole::User);
    }

    #[test]
    fn test_system_blocks() {
        let req = json!({
            "model": "claude",
            "messages": [],
            "system": [
                {"type": "text", "text": "Part 1", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "Part 2"}
            ]
        });
        let ir = anthropic_req_to_ir(&req);
        match ir.system.unwrap() {
            IrSystemContent::Blocks(blocks) => {
                assert_eq!(blocks.len(), 2);
                assert_eq!(blocks[0].text, "Part 1");
                assert!(blocks[0].cache_control.is_some());
                assert_eq!(blocks[1].text, "Part 2");
                assert!(blocks[1].cache_control.is_none());
            }
            _ => panic!("expected Blocks"),
        }
    }

    #[test]
    fn test_content_blocks_with_image() {
        let req = json!({
            "model": "claude",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "What is this?"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "abc"}},
                    {"type": "image", "source": {"type": "url", "url": "https://example.com/img.png"}}
                ]
            }]
        });
        let ir = anthropic_req_to_ir(&req);
        assert_eq!(ir.messages[0].content.len(), 3);
        assert!(matches!(&ir.messages[0].content[0], IrContentBlock::Text { text, .. } if text == "What is this?"));
        assert!(matches!(&ir.messages[0].content[1], IrContentBlock::Image { source: IrImageSource::Base64 { media_type, .. } } if media_type == "image/png"));
        assert!(matches!(&ir.messages[0].content[2], IrContentBlock::Image { source: IrImageSource::Url { url } } if url == "https://example.com/img.png"));
    }

    #[test]
    fn test_tool_use_and_result() {
        let req = json!({
            "model": "claude",
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "call_1", "name": "search", "input": {"q": "test"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "call_1", "content": "result text"}
                ]}
            ],
            "tools": [{
                "name": "search",
                "description": "Search the web",
                "input_schema": {"type": "object", "properties": {"q": {"type": "string"}}}
            }],
            "tool_choice": {"type": "auto"}
        });
        let ir = anthropic_req_to_ir(&req);
        // Tool use in assistant message
        assert!(matches!(&ir.messages[0].content[0], IrContentBlock::ToolUse { id, name, .. } if id == "call_1" && name == "search"));
        // Tool result in user message
        assert!(matches!(&ir.messages[1].content[0], IrContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "call_1"));
        // Tool definition
        assert_eq!(ir.tools.len(), 1);
        assert_eq!(ir.tools[0].name, "search");
        // Tool choice
        assert!(matches!(ir.tool_choice, Some(IrToolChoice::Auto)));
    }

    #[test]
    fn test_tool_choice_variants() {
        // "any" string
        let req = json!({"model": "x", "messages": [], "tool_choice": "any"});
        let ir = anthropic_req_to_ir(&req);
        assert!(matches!(ir.tool_choice, Some(IrToolChoice::Any)));

        // "none" string
        let req = json!({"model": "x", "messages": [], "tool_choice": "none"});
        let ir = anthropic_req_to_ir(&req);
        assert!(matches!(ir.tool_choice, Some(IrToolChoice::None)));

        // Specific tool
        let req = json!({"model": "x", "messages": [], "tool_choice": {"type": "tool", "name": "search"}});
        let ir = anthropic_req_to_ir(&req);
        assert!(matches!(ir.tool_choice, Some(IrToolChoice::Tool { ref name }) if name == "search"));
    }

    #[test]
    fn test_thinking_config() {
        let req = json!({
            "model": "claude",
            "messages": [],
            "thinking": {"type": "enabled", "budget_tokens": 5000}
        });
        let ir = anthropic_req_to_ir(&req);
        let thinking = ir.thinking.unwrap();
        assert!(thinking.enabled);
        assert_eq!(thinking.budget_tokens, Some(5000));
    }

    #[test]
    fn test_thinking_block_in_message() {
        let req = json!({
            "model": "claude",
            "messages": [{
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "let me think", "signature": "sig_abc"},
                    {"type": "text", "text": "answer"}
                ]
            }]
        });
        let ir = anthropic_req_to_ir(&req);
        assert_eq!(ir.messages[0].content.len(), 2);
        assert!(matches!(&ir.messages[0].content[0], IrContentBlock::Thinking { thinking, signature } if thinking == "let me think" && signature.as_deref() == Some("sig_abc")));
    }

    // ── 流式 chunk 测试 ──

    #[test]
    fn test_chunk_message_start() {
        let mut state = AnthropicParseState::new();
        let chunk = json!({
            "type": "message_start",
            "message": {
                "id": "msg_123",
                "model": "claude-opus-4-8",
                "role": "assistant",
                "content": [],
                "usage": {
                    "input_tokens": 100,
                    "cache_creation_input_tokens": 50,
                    "cache_read_input_tokens": 8000,
                    "output_tokens": 0
                }
            }
        });
        let events = anthropic_chunk_to_ir(&chunk, &mut state);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], IrStreamEvent::MessageStart { id, model } if id == "msg_123" && model == "claude-opus-4-8"));
        // input_tokens = 100 + 50 (cache_creation)
        assert_eq!(state.usage.input_tokens, 150);
        assert_eq!(state.usage.cache_read_input_tokens, 8000);
        assert_eq!(state.usage.cache_creation_input_tokens, 50);
    }

    #[test]
    fn test_chunk_content_block_sequence() {
        let mut state = AnthropicParseState::new();

        // content_block_start (text)
        let start = json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}});
        let events = anthropic_chunk_to_ir(&start, &mut state);
        assert!(matches!(&events[0], IrStreamEvent::ContentBlockStart { index: 0, block: IrContentBlockStart::Text }));

        // content_block_delta (text)
        let delta = json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hello"}});
        let events = anthropic_chunk_to_ir(&delta, &mut state);
        assert!(matches!(&events[0], IrStreamEvent::ContentBlockDelta { index: 0, delta: IrContentDelta::TextDelta(t) } if t == "Hello"));
        assert_eq!(state.usage.output_chars, 5);

        // content_block_stop
        let stop = json!({"type": "content_block_stop", "index": 0});
        let events = anthropic_chunk_to_ir(&stop, &mut state);
        assert!(matches!(&events[0], IrStreamEvent::ContentBlockStop { index: 0 }));
    }

    #[test]
    fn test_chunk_tool_use_sequence() {
        let mut state = AnthropicParseState::new();

        let start = json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "call_1", "name": "search", "input": {}}});
        let events = anthropic_chunk_to_ir(&start, &mut state);
        assert!(matches!(&events[0], IrStreamEvent::ContentBlockStart { index: 1, block: IrContentBlockStart::ToolUse { id, name } } if id == "call_1" && name == "search"));

        let delta = json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"q\":"}});
        let events = anthropic_chunk_to_ir(&delta, &mut state);
        assert!(matches!(&events[0], IrStreamEvent::ContentBlockDelta { index: 1, delta: IrContentDelta::InputJsonDelta(p) } if p == "{\"q\":"));
    }

    #[test]
    fn test_chunk_message_delta_with_usage() {
        let mut state = AnthropicParseState::new();
        state.usage.input_tokens = 150;

        let chunk = json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 300, "cache_read_input_tokens": 8000}
        });
        let events = anthropic_chunk_to_ir(&chunk, &mut state);
        assert_eq!(events.len(), 1);
        match &events[0] {
            IrStreamEvent::MessageDelta { stop_reason, usage } => {
                assert_eq!(*stop_reason, Some(IrStopReason::EndTurn));
                let u = usage.as_ref().unwrap();
                assert_eq!(u.output_tokens, 300);
                assert_eq!(u.cache_read_input_tokens, 8000);
            }
            _ => panic!("expected MessageDelta"),
        }
    }

    #[test]
    fn test_chunk_thinking_delta_counts_chars() {
        let mut state = AnthropicParseState::new();
        let chunk = json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "thinking_delta", "thinking": "思考中..."}
        });
        anthropic_chunk_to_ir(&chunk, &mut state);
        assert_eq!(state.usage.output_chars, 5); // 5 chars in "思考中..."
    }
}
