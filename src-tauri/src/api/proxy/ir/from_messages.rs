//! Anthropic Messages → IR 方向的格式翻译：请求体与流式 chunk。

use serde_json::Value;

use super::types::*;

/// 将 Anthropic Messages 请求体解析为 IR。
pub fn messages_req_to_ir(req: &Value) -> IrRequest {
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
                    // server-side 内置工具（web_search_20250305 等）：可能只有 type 没有 name，
                    // 归一化为 name="web_search"，保证 MCP 模式的搜索工具剔除对 Messages 客户端生效
                    let (name, is_websearch) = match t["name"].as_str() {
                        Some(n) => (n.to_string(), n.starts_with("web_search")),
                        None => {
                            let ty = t["type"].as_str().unwrap_or("");
                            if ty.starts_with("web_search") {
                                ("web_search".to_string(), true)
                            } else {
                                return None;
                            }
                        }
                    };
                    Some(IrTool {
                        name,
                        description: t.get("description").and_then(|d| d.as_str()).map(String::from),
                        input_schema: t
                            .get("input_schema")
                            .cloned()
                            .unwrap_or_else(|| {
                                if is_websearch {
                                    // web_search 工具需要合理的 schema，让上游 LLM 知道如何填写 query
                                    serde_json::json!({
                                        "type": "object",
                                        "properties": {
                                            "query": {
                                                "type": "string",
                                                "description": "The search query"
                                            }
                                        },
                                        "required": ["query"]
                                    })
                                } else {
                                    serde_json::json!({"type": "object", "properties": {}})
                                }
                            }),
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

/// Messages chunk 解析状态（Anthropic SSE 几乎 1:1 映射到 IR，状态很轻）。
pub struct MessagesParseState {
    /// 已捕获的 usage（message_start + message_delta 累积）。
    pub usage: IrUsage,
}

impl MessagesParseState {
    pub fn new() -> Self {
        Self {
            usage: IrUsage::default(),
        }
    }
}

impl Default for MessagesParseState {
    fn default() -> Self {
        Self::new()
    }
}

/// 将 Anthropic Messages 流式 chunk 解析为 IR 事件序列。
///
/// Anthropic SSE 事件与 IrStreamEvent 几乎同构，主要做：
/// - `message_start` → 提取 id/model/usage
/// - `content_block_start` → 映射块类型
/// - `content_block_delta` → 映射 delta 类型 + 累积 output_chars
/// - `message_delta` → 提取 stop_reason + usage
pub fn messages_chunk_to_ir(
    chunk: &Value,
    state: &mut MessagesParseState,
) -> Vec<IrStreamEvent> {
    let event_type = chunk["type"].as_str().unwrap_or("");
    let mut events = Vec::new();

    match event_type {
        "message_start" => {
            let msg = &chunk["message"];
            let id = msg["id"].as_str().unwrap_or("msg_unknown").to_string();
            let model = msg["model"].as_str().unwrap_or("").to_string();

            // 提取初始 usage。
            let usage = &msg["usage"];
            // IR 口径：input_tokens 保持「未缓存输入」口径（不含 cache_creation），
            // 渲染回 Anthropic 客户端时再合并（input_tokens + cache_creation）。
            let it = usage["input_tokens"].as_u64().unwrap_or(0);
            let cr = usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
            // 部分兼容上游（如智谱 GLM）message_start 的 input_tokens 恒为 0——
            // 此时保留 forward.rs 预填的估算占位，等 message_delta 的真实值覆盖，
            // 否则 usage_log 的输入列会永远是 0。
            if it > 0 {
                state.usage.input_tokens = it;
            }
            state.usage.cache_read_input_tokens = cr;
            state.usage.cache_creation_input_tokens =
                usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);

            events.push(IrStreamEvent::MessageStart {
                id,
                model,
                // 上游 message_start 携带的真实 usage（input 侧），
                // 供客户端上下文条感知；无真实值时由 forward.rs 预填的估算兜底
                usage: Some(state.usage.clone()),
            });
        }
        "content_block_start" => {
            let index = chunk["index"].as_u64().unwrap_or(0) as usize;
            let block = &chunk["content_block"];
            let ir_block = match block["type"].as_str().unwrap_or("") {
                "text" => IrContentBlockStart::Text,
                "thinking" => IrContentBlockStart::Thinking {
                    signature: block.get("signature").and_then(|v| v.as_str()).map(String::from),
                },
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
                .map(IrStopReason::from_messages);
            let usage_obj = &chunk["usage"];
            if let Some(ot) = usage_obj["output_tokens"].as_u64() {
                state.usage.output_tokens = ot;
            }
            // 部分兼容上游在 message_delta 才补报 input_tokens（同样 >0 才覆盖，
            // 避免 0 值抹掉估算占位）
            if let Some(it) = usage_obj["input_tokens"].as_u64() {
                if it > 0 {
                    state.usage.input_tokens = it;
                }
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
        let ir = messages_req_to_ir(&req);
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
        let ir = messages_req_to_ir(&req);
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
        let ir = messages_req_to_ir(&req);
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
        let ir = messages_req_to_ir(&req);
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
        let ir = messages_req_to_ir(&req);
        assert!(matches!(ir.tool_choice, Some(IrToolChoice::Any)));

        // "none" string
        let req = json!({"model": "x", "messages": [], "tool_choice": "none"});
        let ir = messages_req_to_ir(&req);
        assert!(matches!(ir.tool_choice, Some(IrToolChoice::None)));

        // Specific tool
        let req = json!({"model": "x", "messages": [], "tool_choice": {"type": "tool", "name": "search"}});
        let ir = messages_req_to_ir(&req);
        assert!(matches!(ir.tool_choice, Some(IrToolChoice::Tool { ref name }) if name == "search"));
    }

    #[test]
    fn test_thinking_config() {
        let req = json!({
            "model": "claude",
            "messages": [],
            "thinking": {"type": "enabled", "budget_tokens": 5000}
        });
        let ir = messages_req_to_ir(&req);
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
        let ir = messages_req_to_ir(&req);
        assert_eq!(ir.messages[0].content.len(), 2);
        assert!(matches!(&ir.messages[0].content[0], IrContentBlock::Thinking { thinking, signature } if thinking == "let me think" && signature.as_deref() == Some("sig_abc")));
    }

    // ── 流式 chunk 测试 ──

    #[test]
    fn test_chunk_message_start() {
        let mut state = MessagesParseState::new();
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
        let events = messages_chunk_to_ir(&chunk, &mut state);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], IrStreamEvent::MessageStart { id, model, usage: Some(u) } if id == "msg_123" && model == "claude-opus-4-8" && u.input_tokens == 100));
        // IR 口径：input_tokens 为纯 miss（100），cache_creation 独立存放（50）
        assert_eq!(state.usage.input_tokens, 100);
        assert_eq!(state.usage.cache_read_input_tokens, 8000);
        assert_eq!(state.usage.cache_creation_input_tokens, 50);
    }

    #[test]
    fn test_chunk_content_block_sequence() {
        let mut state = MessagesParseState::new();

        // content_block_start (text)
        let start = json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}});
        let events = messages_chunk_to_ir(&start, &mut state);
        assert!(matches!(&events[0], IrStreamEvent::ContentBlockStart { index: 0, block: IrContentBlockStart::Text }));

        // content_block_delta (text)
        let delta = json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hello"}});
        let events = messages_chunk_to_ir(&delta, &mut state);
        assert!(matches!(&events[0], IrStreamEvent::ContentBlockDelta { index: 0, delta: IrContentDelta::TextDelta(t) } if t == "Hello"));
        assert_eq!(state.usage.output_chars, 5);

        // content_block_stop
        let stop = json!({"type": "content_block_stop", "index": 0});
        let events = messages_chunk_to_ir(&stop, &mut state);
        assert!(matches!(&events[0], IrStreamEvent::ContentBlockStop { index: 0 }));
    }

    #[test]
    fn test_chunk_tool_use_sequence() {
        let mut state = MessagesParseState::new();

        let start = json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "call_1", "name": "search", "input": {}}});
        let events = messages_chunk_to_ir(&start, &mut state);
        assert!(matches!(&events[0], IrStreamEvent::ContentBlockStart { index: 1, block: IrContentBlockStart::ToolUse { id, name } } if id == "call_1" && name == "search"));

        let delta = json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"q\":"}});
        let events = messages_chunk_to_ir(&delta, &mut state);
        assert!(matches!(&events[0], IrStreamEvent::ContentBlockDelta { index: 1, delta: IrContentDelta::InputJsonDelta(p) } if p == "{\"q\":"));
    }

    #[test]
    fn test_chunk_message_delta_with_usage() {
        let mut state = MessagesParseState::new();
        state.usage.input_tokens = 150;

        let chunk = json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 300, "cache_read_input_tokens": 8000}
        });
        let events = messages_chunk_to_ir(&chunk, &mut state);
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
        let mut state = MessagesParseState::new();
        let chunk = json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "thinking_delta", "thinking": "思考中..."}
        });
        messages_chunk_to_ir(&chunk, &mut state);
        assert_eq!(state.usage.output_chars, 6); // "思考中..." = 3 汉字 + 3 点 = 6 chars
    }
}
