//! OpenAI Chat Completions → IR 方向的格式翻译：请求体与流式 chunk。
//!
//! Chat Completions 的消息模型与 IR 差异较大：
//! - system 是 messages[0] 而非独立字段
//! - content 可以是字符串或 content parts 数组
//! - tool_calls 是 message 的独立字段而非 content block
//! - tool role 消息对应 IR 的 ToolResult
//! - reasoning_content 是非标准字段（qwen/DeepSeek 等使用）

use serde_json::Value;

use super::types::*;

/// 将 OpenAI Chat Completions 请求体解析为 IR。
pub fn chat_completions_req_to_ir(req: &Value) -> IrRequest {
    let model = req["model"].as_str().unwrap_or("").to_string();
    let stream = req.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    // 解析 messages，提取 system 和对话消息
    let (system, messages) = parse_chat_messages(req);

    // Tools
    let tools = req
        .get("tools")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    // Chat 格式：{"type": "function", "function": {"name": ..., "parameters": ...}}
                    let func = t.get("function")?;
                    let name = func.get("name")?.as_str()?;
                    Some(IrTool {
                        name: name.to_string(),
                        description: func
                            .get("description")
                            .and_then(|d| d.as_str())
                            .map(String::from),
                        input_schema: func
                            .get("parameters")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}})),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    // Tool choice
    let tool_choice = req.get("tool_choice").map(parse_chat_tool_choice);

    // Reasoning effort → thinking config
    let thinking = req.get("reasoning_effort").and_then(|v| v.as_str()).map(|effort| {
        let budget = match effort {
            "low" => Some(1024),
            "medium" => Some(4096),
            "high" => Some(16384),
            _ => None,
        };
        IrThinkingConfig {
            enabled: true,
            budget_tokens: budget,
        }
    });

    // max_tokens 或 max_completion_tokens
    let max_tokens = req
        .get("max_tokens")
        .or_else(|| req.get("max_completion_tokens"))
        .and_then(|v| v.as_u64());

    IrRequest {
        model,
        system,
        messages,
        tools,
        tool_choice,
        max_tokens,
        temperature: req.get("temperature").and_then(|v| v.as_f64()),
        top_p: req.get("top_p").and_then(|v| v.as_f64()),
        thinking,
        stream,
    }
}

/// 解析 Chat messages 数组，分离 system 和对话消息。
fn parse_chat_messages(req: &Value) -> (Option<IrSystemContent>, Vec<IrMessage>) {
    let messages = req.get("messages").and_then(|m| m.as_array());
    let messages = match messages {
        Some(m) => m,
        None => return (None, vec![]),
    };

    let mut system_parts: Vec<String> = vec![];
    let mut ir_messages: Vec<IrMessage> = vec![];

    for msg in messages {
        let role = msg["role"].as_str().unwrap_or("user");

        match role {
            "system" => {
                // System 消息收集
                if let Some(content) = msg.get("content") {
                    match content {
                        Value::String(s) => system_parts.push(s.clone()),
                        Value::Array(parts) => {
                            for part in parts {
                                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                    system_parts.push(text.to_string());
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            "user" | "assistant" => {
                let ir_role = if role == "assistant" {
                    IrRole::Assistant
                } else {
                    IrRole::User
                };
                let content = parse_chat_content(msg);
                if !content.is_empty() {
                    ir_messages.push(IrMessage {
                        role: ir_role,
                        content,
                    });
                }
            }
            "tool" => {
                // Tool role → ToolResult
                let tool_call_id = msg
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let content = msg
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                ir_messages.push(IrMessage {
                    role: IrRole::User, // Tool result 作为 user 消息
                    content: vec![IrContentBlock::ToolResult {
                        tool_use_id: tool_call_id,
                        content: IrToolResultContent::Text(content),
                        is_error: false,
                    }],
                });
            }
            _ => {}
        }
    }

    let system = if system_parts.is_empty() {
        None
    } else if system_parts.len() == 1 {
        Some(IrSystemContent::Text(system_parts.into_iter().next().unwrap()))
    } else {
        Some(IrSystemContent::Blocks(
            system_parts
                .into_iter()
                .map(|text| IrSystemBlock {
                    text,
                    cache_control: None,
                })
                .collect(),
        ))
    };

    (system, ir_messages)
}

/// 解析 Chat message 的 content 字段（字符串或 content parts 数组）。
fn parse_chat_content(msg: &Value) -> Vec<IrContentBlock> {
    let mut blocks = vec![];

    // reasoning_content → Thinking block
    if let Some(reasoning) = msg.get("reasoning_content").and_then(|v| v.as_str()) {
        if !reasoning.is_empty() {
            blocks.push(IrContentBlock::Thinking {
                thinking: reasoning.to_string(),
                signature: None,
            });
        }
    }

    // content 字段
    if let Some(content) = msg.get("content") {
        match content {
            Value::String(s) => {
                if !s.is_empty() {
                    blocks.push(IrContentBlock::Text {
                        text: s.clone(),
                        cache_control: None,
                    });
                }
            }
            Value::Array(parts) => {
                for part in parts {
                    if let Some(block) = parse_chat_content_part(part) {
                        blocks.push(block);
                    }
                }
            }
            _ => {}
        }
    }

    // tool_calls → ToolUse blocks
    if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
        for tc in tool_calls {
            let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let func = tc.get("function");
            let name = func
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let arguments = func
                .and_then(|f| f.get("arguments"))
                .and_then(|v| v.as_str())
                .unwrap_or("{}");
            let input: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);

            blocks.push(IrContentBlock::ToolUse { id, name, input });
        }
    }

    blocks
}

/// 解析单个 Chat content part。
fn parse_chat_content_part(part: &Value) -> Option<IrContentBlock> {
    let part_type = part.get("type")?.as_str()?;

    match part_type {
        "text" => {
            let text = part.get("text")?.as_str()?.to_string();
            Some(IrContentBlock::Text {
                text,
                cache_control: None,
            })
        }
        "image_url" => {
            let url = part.get("image_url")?.get("url")?.as_str()?.to_string();
            // 判断是 base64 还是 URL
            if url.starts_with("data:") {
                // data:image/png;base64,xxxx
                let parts: Vec<&str> = url.splitn(2, ',').collect();
                if parts.len() == 2 {
                    let meta = parts[0];
                    let data = parts[1].to_string();
                    let media_type = meta
                        .strip_prefix("data:")
                        .and_then(|s| s.split(';').next())
                        .unwrap_or("image/png")
                        .to_string();
                    Some(IrContentBlock::Image {
                        source: IrImageSource::Base64 { media_type, data },
                    })
                } else {
                    None
                }
            } else {
                Some(IrContentBlock::Image {
                    source: IrImageSource::Url { url },
                })
            }
        }
        _ => None,
    }
}

/// 解析 Chat tool_choice 字段。
fn parse_chat_tool_choice(tc: &Value) -> IrToolChoice {
    match tc {
        Value::String(s) => match s.as_str() {
            "auto" => IrToolChoice::Auto,
            "none" => IrToolChoice::None,
            "required" => IrToolChoice::Any,
            _ => IrToolChoice::Auto,
        },
        Value::Object(obj) => {
            // {"type": "function", "function": {"name": "..."}}
            if let Some(func) = obj.get("function") {
                if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                    return IrToolChoice::Tool {
                        name: name.to_string(),
                    };
                }
            }
            IrToolChoice::Auto
        }
        _ => IrToolChoice::Auto,
    }
}

// ═══════════════════════════════════════════════════════════════════
// 流式 chunk 解析
// ═══════════════════════════════════════════════════════════════════

/// Chat Completions chunk 解析状态。
///
/// Chat 的流式状态机需要处理：
/// - reasoning_content 和 content 可能交错（qwen3.7 等）
/// - tool_calls 的 arguments 分多个 chunk 到达
/// - 确保 IR 的 content block 顺序正确（thinking → text → tools）
pub struct ChatCompletionsParseState {
    /// 已捕获的 usage
    pub usage: IrUsage,
    /// 当前是否已发送 thinking block start
    thinking_started: bool,
    /// 当前是否已发送 text block start
    text_started: bool,
    /// 当前 text block 的 index
    text_index: usize,
    /// 已发送的 tool_use block 数量
    tool_count: usize,
    /// 消息 ID（从第一个 chunk 捕获）
    msg_id: String,
    /// 模型名（从第一个 chunk 捕获）
    model: String,
}

impl ChatCompletionsParseState {
    pub fn new() -> Self {
        Self {
            usage: IrUsage::default(),
            thinking_started: false,
            text_started: false,
            text_index: 0,
            tool_count: 0,
            msg_id: String::new(),
            model: String::new(),
        }
    }
}

impl Default for ChatCompletionsParseState {
    fn default() -> Self {
        Self::new()
    }
}

/// 将 OpenAI Chat Completions 流式 chunk 解析为 IR 事件序列。
///
/// Chat 的 chunk 结构：
/// - `choices[0].delta.content` → text delta
/// - `choices[0].delta.reasoning_content` → thinking delta
/// - `choices[0].delta.tool_calls[i]` → tool_use start/delta
/// - `choices[0].finish_reason` → stop reason
/// - `usage` → token usage（通常在最后一个 chunk）
pub fn chat_completions_chunk_to_ir(chunk: &Value, state: &mut ChatCompletionsParseState) -> Vec<IrStreamEvent> {
    let mut events = vec![];

    // 提取 usage（可能在任何 chunk 中）
    let usage = super::usage::extract_chat_completions_usage(chunk);
    if usage.input_tokens > 0
        || usage.output_tokens > 0
        || usage.cache_read_input_tokens > 0
    {
        state.usage.input_tokens = state.usage.input_tokens.max(usage.input_tokens);
        state.usage.output_tokens = state.usage.output_tokens.max(usage.output_tokens);
        state.usage.cache_read_input_tokens =
            state.usage.cache_read_input_tokens.max(usage.cache_read_input_tokens);
    }
    state.usage.output_chars += usage.output_chars;

    // 提取消息 ID 和模型（第一个 chunk）
    if state.msg_id.is_empty() {
        if let Some(id) = chunk.get("id").and_then(|v| v.as_str()) {
            state.msg_id = id.to_string();
        }
    }
    if state.model.is_empty() {
        if let Some(model) = chunk.get("model").and_then(|v| v.as_str()) {
            state.model = model.to_string();
        }
    }

    // 解析 choices
    let choices = match chunk.get("choices").and_then(|v| v.as_array()) {
        Some(c) => c,
        None => return events,
    };

    for choice in choices {
        let delta = match choice.get("delta") {
            Some(d) => d,
            None => continue,
        };

        // reasoning_content → thinking delta
        if let Some(reasoning) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
            if !reasoning.is_empty() {
                if !state.thinking_started {
                    // 发送 thinking block start
                    events.push(IrStreamEvent::ContentBlockStart {
                        index: 0,
                        block: IrContentBlockStart::Thinking,
                    });
                    state.thinking_started = true;
                }
                events.push(IrStreamEvent::ContentBlockDelta {
                    index: 0,
                    delta: IrContentDelta::ThinkingDelta(reasoning.to_string()),
                });
            }
        }

        // content → text delta
        if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
            if !content.is_empty() {
                // 如果 thinking 已开启但未关闭，先关闭 thinking
                if state.thinking_started && !state.text_started {
                    events.push(IrStreamEvent::ContentBlockStop { index: 0 });
                    state.text_index = 1;
                }

                if !state.text_started {
                    // 发送 text block start
                    let index = if state.thinking_started {
                        state.text_index
                    } else {
                        0
                    };
                    events.push(IrStreamEvent::ContentBlockStart {
                        index,
                        block: IrContentBlockStart::Text,
                    });
                    state.text_started = true;
                    state.text_index = index;
                }

                events.push(IrStreamEvent::ContentBlockDelta {
                    index: state.text_index,
                    delta: IrContentDelta::TextDelta(content.to_string()),
                });
            }
        }

        // tool_calls → tool_use blocks
        if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            // 如果 text 已开启但未关闭，先关闭 text
            if state.text_started && state.tool_count == 0 {
                events.push(IrStreamEvent::ContentBlockStop {
                    index: state.text_index,
                });
            }

            for tc in tool_calls {
                let index = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let block_index = if state.thinking_started {
                    state.text_index + 1 + index
                } else if state.text_started {
                    state.text_index + 1 + index
                } else {
                    index
                };

                // 检查是否有 function.name（表示新的 tool_use）
                if let Some(func) = tc.get("function") {
                    if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                        // 新的 tool_use block
                        let id = tc
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        events.push(IrStreamEvent::ContentBlockStart {
                            index: block_index,
                            block: IrContentBlockStart::ToolUse {
                                id,
                                name: name.to_string(),
                            },
                        });
                        state.tool_count = state.tool_count.max(index + 1);
                    }

                    // arguments delta
                    if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                        events.push(IrStreamEvent::ContentBlockDelta {
                            index: block_index,
                            delta: IrContentDelta::InputJsonDelta(args.to_string()),
                        });
                    }
                }
            }
        }

        // finish_reason → stop reason
        if let Some(finish) = choice.get("finish_reason").and_then(|v| v.as_str()) {
            // 关闭所有未关闭的 block
            if state.thinking_started && !state.text_started {
                events.push(IrStreamEvent::ContentBlockStop { index: 0 });
            }
            if state.text_started && state.tool_count == 0 {
                events.push(IrStreamEvent::ContentBlockStop {
                    index: state.text_index,
                });
            }
            for i in 0..state.tool_count {
                let block_index = if state.thinking_started || state.text_started {
                    state.text_index + 1 + i
                } else {
                    i
                };
                events.push(IrStreamEvent::ContentBlockStop { index: block_index });
            }

            let stop_reason = match finish {
                "stop" => Some(IrStopReason::EndTurn),
                "tool_calls" => Some(IrStopReason::ToolUse),
                "length" => Some(IrStopReason::MaxTokens),
                _ => None,
            };

            events.push(IrStreamEvent::MessageDelta {
                stop_reason,
                usage: Some(state.usage.clone()),
            });
            events.push(IrStreamEvent::MessageStop);
        }
    }

    // 第一个 chunk 发送 MessageStart
    if !state.msg_id.is_empty() && !state.model.is_empty() && events.is_empty() {
        events.insert(
            0,
            IrStreamEvent::MessageStart {
                id: state.msg_id.clone(),
                model: state.model.clone(),
            },
        );
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
            "model": "gpt-4o",
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 4096,
            "stream": true
        });
        let ir = chat_completions_req_to_ir(&req);
        assert_eq!(ir.model, "gpt-4o");
        assert_eq!(ir.max_tokens, Some(4096));
        assert!(ir.stream);
        assert!(matches!(ir.system, Some(IrSystemContent::Text(ref t)) if t == "You are helpful."));
        assert_eq!(ir.messages.len(), 1);
        assert_eq!(ir.messages[0].role, IrRole::User);
    }

    #[test]
    fn test_multiple_system_messages() {
        let req = json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "system", "content": "Part 1"},
                {"role": "system", "content": "Part 2"},
                {"role": "user", "content": "Hello"}
            ]
        });
        let ir = chat_completions_req_to_ir(&req);
        match ir.system.unwrap() {
            IrSystemContent::Blocks(blocks) => {
                assert_eq!(blocks.len(), 2);
                assert_eq!(blocks[0].text, "Part 1");
                assert_eq!(blocks[1].text, "Part 2");
            }
            _ => panic!("expected Blocks"),
        }
    }

    #[test]
    fn test_content_parts_with_image() {
        let req = json!({
            "model": "gpt-4o",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "What is this?"},
                    {"type": "image_url", "image_url": {"url": "https://example.com/img.png"}},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,abc123"}}
                ]
            }]
        });
        let ir = chat_completions_req_to_ir(&req);
        assert_eq!(ir.messages[0].content.len(), 3);
        assert!(matches!(&ir.messages[0].content[0], IrContentBlock::Text { text, .. } if text == "What is this?"));
        assert!(matches!(&ir.messages[0].content[1], IrContentBlock::Image { source: IrImageSource::Url { url } } if url == "https://example.com/img.png"));
        assert!(matches!(&ir.messages[0].content[2], IrContentBlock::Image { source: IrImageSource::Base64 { media_type, data } } if media_type == "image/png" && data == "abc123"));
    }

    #[test]
    fn test_tool_calls_and_results() {
        let req = json!({
            "model": "gpt-4o",
            "messages": [
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "search",
                            "arguments": "{\"q\":\"test\"}"
                        }
                    }]
                },
                {
                    "role": "tool",
                    "tool_call_id": "call_1",
                    "content": "result text"
                }
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "search",
                    "description": "Search the web",
                    "parameters": {"type": "object", "properties": {"q": {"type": "string"}}}
                }
            }],
            "tool_choice": "auto"
        });
        let ir = chat_completions_req_to_ir(&req);
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
        // "required" string
        let req = json!({"model": "x", "messages": [], "tool_choice": "required"});
        let ir = chat_completions_req_to_ir(&req);
        assert!(matches!(ir.tool_choice, Some(IrToolChoice::Any)));

        // "none" string
        let req = json!({"model": "x", "messages": [], "tool_choice": "none"});
        let ir = chat_completions_req_to_ir(&req);
        assert!(matches!(ir.tool_choice, Some(IrToolChoice::None)));

        // Specific tool
        let req = json!({
            "model": "x",
            "messages": [],
            "tool_choice": {
                "type": "function",
                "function": {"name": "search"}
            }
        });
        let ir = chat_completions_req_to_ir(&req);
        assert!(matches!(ir.tool_choice, Some(IrToolChoice::Tool { ref name }) if name == "search"));
    }

    #[test]
    fn test_reasoning_content() {
        let req = json!({
            "model": "qwen-max",
            "messages": [{
                "role": "assistant",
                "reasoning_content": "let me think",
                "content": "answer"
            }]
        });
        let ir = chat_completions_req_to_ir(&req);
        assert_eq!(ir.messages[0].content.len(), 2);
        assert!(matches!(&ir.messages[0].content[0], IrContentBlock::Thinking { thinking, .. } if thinking == "let me think"));
        assert!(matches!(&ir.messages[0].content[1], IrContentBlock::Text { text, .. } if text == "answer"));
    }

    #[test]
    fn test_reasoning_effort() {
        let req = json!({
            "model": "o1",
            "messages": [],
            "reasoning_effort": "high"
        });
        let ir = chat_completions_req_to_ir(&req);
        let thinking = ir.thinking.unwrap();
        assert!(thinking.enabled);
        assert_eq!(thinking.budget_tokens, Some(16384));
    }

    #[test]
    fn test_max_completion_tokens() {
        let req = json!({
            "model": "o1",
            "messages": [],
            "max_completion_tokens": 8192
        });
        let ir = chat_completions_req_to_ir(&req);
        assert_eq!(ir.max_tokens, Some(8192));
    }

    // ── 流式 chunk 测试 ──

    #[test]
    fn test_chunk_message_start() {
        let mut state = ChatCompletionsParseState::new();
        let chunk = json!({
            "id": "chatcmpl-123",
            "model": "gpt-4o",
            "choices": []
        });
        let events = chat_completions_chunk_to_ir(&chunk, &mut state);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], IrStreamEvent::MessageStart { id, model } if id == "chatcmpl-123" && model == "gpt-4o"));
    }

    #[test]
    fn test_chunk_text_delta() {
        let mut state = ChatCompletionsParseState::new();
        state.msg_id = "chatcmpl-1".to_string();
        state.model = "gpt-4o".to_string();

        let chunk = json!({
            "id": "chatcmpl-1",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "delta": {"content": "Hello"},
                "finish_reason": null
            }]
        });
        let events = chat_completions_chunk_to_ir(&chunk, &mut state);
        // 应该发送 ContentBlockStart + ContentBlockDelta
        assert!(events.iter().any(|e| matches!(e, IrStreamEvent::ContentBlockStart { index: 0, block: IrContentBlockStart::Text })));
        assert!(events.iter().any(|e| matches!(e, IrStreamEvent::ContentBlockDelta { index: 0, delta: IrContentDelta::TextDelta(t) } if t == "Hello")));
    }

    #[test]
    fn test_chunk_reasoning_then_text() {
        let mut state = ChatCompletionsParseState::new();
        state.msg_id = "chatcmpl-1".to_string();
        state.model = "qwen-max".to_string();

        // reasoning_content chunk
        let chunk1 = json!({
            "id": "chatcmpl-1",
            "model": "qwen-max",
            "choices": [{
                "index": 0,
                "delta": {"reasoning_content": "thinking..."},
                "finish_reason": null
            }]
        });
        let events1 = chat_completions_chunk_to_ir(&chunk1, &mut state);
        assert!(events1.iter().any(|e| matches!(e, IrStreamEvent::ContentBlockStart { index: 0, block: IrContentBlockStart::Thinking })));
        assert!(events1.iter().any(|e| matches!(e, IrStreamEvent::ContentBlockDelta { index: 0, delta: IrContentDelta::ThinkingDelta(t) } if t == "thinking...")));

        // content chunk (should close thinking first)
        let chunk2 = json!({
            "id": "chatcmpl-1",
            "model": "qwen-max",
            "choices": [{
                "index": 0,
                "delta": {"content": "answer"},
                "finish_reason": null
            }]
        });
        let events2 = chat_completions_chunk_to_ir(&chunk2, &mut state);
        // 应该先关闭 thinking (index 0)，再开启 text (index 1)
        assert!(events2.iter().any(|e| matches!(e, IrStreamEvent::ContentBlockStop { index: 0 })));
        assert!(events2.iter().any(|e| matches!(e, IrStreamEvent::ContentBlockStart { index: 1, block: IrContentBlockStart::Text })));
        assert!(events2.iter().any(|e| matches!(e, IrStreamEvent::ContentBlockDelta { index: 1, delta: IrContentDelta::TextDelta(t) } if t == "answer")));
    }

    #[test]
    fn test_chunk_tool_calls() {
        let mut state = ChatCompletionsParseState::new();
        state.msg_id = "chatcmpl-1".to_string();
        state.model = "gpt-4o".to_string();

        // tool_calls start
        let chunk1 = json!({
            "id": "chatcmpl-1",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "search",
                            "arguments": ""
                        }
                    }]
                },
                "finish_reason": null
            }]
        });
        let events1 = chat_completions_chunk_to_ir(&chunk1, &mut state);
        assert!(events1.iter().any(|e| matches!(e, IrStreamEvent::ContentBlockStart { index: 0, block: IrContentBlockStart::ToolUse { id, name } } if id == "call_1" && name == "search")));

        // tool_calls arguments delta
        let chunk2 = json!({
            "id": "chatcmpl-1",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": {
                            "arguments": "{\"q\":"
                        }
                    }]
                },
                "finish_reason": null
            }]
        });
        let events2 = chat_completions_chunk_to_ir(&chunk2, &mut state);
        assert!(events2.iter().any(|e| matches!(e, IrStreamEvent::ContentBlockDelta { index: 0, delta: IrContentDelta::InputJsonDelta(p) } if p == "{\"q\":")));
    }

    #[test]
    fn test_chunk_finish_reason() {
        let mut state = ChatCompletionsParseState::new();
        state.msg_id = "chatcmpl-1".to_string();
        state.model = "gpt-4o".to_string();
        state.text_started = true;
        state.text_index = 0;

        let chunk = json!({
            "id": "chatcmpl-1",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 50
            }
        });
        let events = chat_completions_chunk_to_ir(&chunk, &mut state);
        // 应该关闭 text block，发送 MessageDelta + MessageStop
        assert!(events.iter().any(|e| matches!(e, IrStreamEvent::ContentBlockStop { index: 0 })));
        assert!(events.iter().any(|e| matches!(e, IrStreamEvent::MessageDelta { stop_reason: Some(IrStopReason::EndTurn), .. })));
        assert!(events.iter().any(|e| matches!(e, IrStreamEvent::MessageStop)));
        assert_eq!(state.usage.input_tokens, 100);
        assert_eq!(state.usage.output_tokens, 50);
    }
}
