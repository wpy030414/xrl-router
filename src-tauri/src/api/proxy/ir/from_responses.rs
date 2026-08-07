//! OpenAI Responses API → IR 方向的格式翻译：请求体与流式 chunk。
//!
//! Responses API 与 Chat Completions 的主要差异：
//! - 请求用 `input` 而非 `messages`
//! - 内容类型是 `input_text`/`input_image`/`output_text` 等
//! - 流式事件是 `response.created`/`response.output_item.added` 等
//! - 工具定义用 `function` 类型

use serde_json::Value;

use super::types::*;

/// 将 OpenAI Responses 请求体解析为 IR。
pub fn responses_req_to_ir(req: &Value) -> IrRequest {
    let model = req["model"].as_str().unwrap_or("").to_string();
    let stream = req.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    // 解析 input（可能是字符串或数组）
    let (system, messages) = parse_responses_input(req);

    // Tools
    let tools = req
        .get("tools")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    // Responses 格式：{"type": "function", "name": ..., "parameters": ...}
                    if t["type"].as_str() != Some("function") {
                        return None;
                    }
                    let name = t.get("name")?.as_str()?;
                    Some(IrTool {
                        name: name.to_string(),
                        description: t
                            .get("description")
                            .and_then(|d| d.as_str())
                            .map(String::from),
                        input_schema: t
                            .get("parameters")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}})),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    // Tool choice
    let tool_choice = req.get("tool_choice").map(parse_responses_tool_choice);

    // Reasoning config
    let thinking = req.get("reasoning").and_then(|r| {
        let effort = r.get("effort").and_then(|v| v.as_str()).unwrap_or("medium");
        let budget = match effort {
            "low" => Some(1024),
            "medium" => Some(4096),
            "high" => Some(16384),
            _ => None,
        };
        Some(IrThinkingConfig {
            enabled: true,
            budget_tokens: budget,
        })
    });

    // max_output_tokens
    let max_tokens = req
        .get("max_output_tokens")
        .or_else(|| req.get("max_tokens"))
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

/// 解析 Responses input 字段。
fn parse_responses_input(req: &Value) -> (Option<IrSystemContent>, Vec<IrMessage>) {
    let input = match req.get("input") {
        Some(Value::String(s)) => {
            // 简单字符串 input → 单条 user 消息
            return (
                None,
                vec![IrMessage {
                    role: IrRole::User,
                    content: vec![IrContentBlock::Text {
                        text: s.clone(),
                        cache_control: None,
                    }],
                }],
            );
        }
        Some(Value::Array(arr)) => arr,
        _ => return (None, vec![]),
    };

    let mut system_parts: Vec<String> = vec![];
    let mut messages: Vec<IrMessage> = vec![];

    for item in input {
        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("user");

        match item_type {
            "message" => {
                let ir_role = match role {
                    "system" => {
                        // System 消息
                        if let Some(content) = item.get("content") {
                            match content {
                                Value::String(s) => system_parts.push(s.clone()),
                                Value::Array(parts) => {
                                    for part in parts {
                                        if let Some(text) =
                                            part.get("text").and_then(|t| t.as_str())
                                        {
                                            system_parts.push(text.to_string());
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        continue;
                    }
                    "assistant" => IrRole::Assistant,
                    _ => IrRole::User,
                };

                let content = parse_responses_content(item);
                if !content.is_empty() {
                    messages.push(IrMessage {
                        role: ir_role,
                        content,
                    });
                }
            }
            "function_call" => {
                // Function call → ToolUse
                let id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let arguments = item
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}");
                let input: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);

                // 添加到最后的 assistant 消息，或创建新的
                if let Some(last) = messages.last_mut() {
                    if last.role == IrRole::Assistant {
                        last.content.push(IrContentBlock::ToolUse { id, name, input });
                        continue;
                    }
                }
                messages.push(IrMessage {
                    role: IrRole::Assistant,
                    content: vec![IrContentBlock::ToolUse { id, name, input }],
                });
            }
            "function_call_output" => {
                // Function call output → ToolResult
                let call_id = item
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let output = item
                    .get("output")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                messages.push(IrMessage {
                    role: IrRole::User,
                    content: vec![IrContentBlock::ToolResult {
                        tool_use_id: call_id,
                        content: IrToolResultContent::Text(output),
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
        Some(IrSystemContent::Text(
            system_parts.into_iter().next().unwrap(),
        ))
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

    (system, messages)
}

/// 解析 Responses message 的 content。
fn parse_responses_content(item: &Value) -> Vec<IrContentBlock> {
    let mut blocks = vec![];

    if let Some(content) = item.get("content").and_then(|v| v.as_array()) {
        for part in content {
            let part_type = part.get("type").and_then(|v| v.as_str()).unwrap_or("");

            match part_type {
                "input_text" | "output_text" | "text" => {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        blocks.push(IrContentBlock::Text {
                            text: text.to_string(),
                            cache_control: None,
                        });
                    }
                }
                "input_image" => {
                    // 可能是 URL 或 base64
                    if let Some(url) = part.get("image_url").and_then(|v| v.as_str()) {
                        if url.starts_with("data:") {
                            let parts: Vec<&str> = url.splitn(2, ',').collect();
                            if parts.len() == 2 {
                                let meta = parts[0];
                                let data = parts[1].to_string();
                                let media_type = meta
                                    .strip_prefix("data:")
                                    .and_then(|s| s.split(';').next())
                                    .unwrap_or("image/png")
                                    .to_string();
                                blocks.push(IrContentBlock::Image {
                                    source: IrImageSource::Base64 { media_type, data },
                                });
                            }
                        } else {
                            blocks.push(IrContentBlock::Image {
                                source: IrImageSource::Url { url: url.to_string() },
                            });
                        }
                    }
                }
                "reasoning" => {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        blocks.push(IrContentBlock::Thinking {
                            thinking: text.to_string(),
                            signature: None,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    blocks
}

/// 解析 Responses tool_choice。
fn parse_responses_tool_choice(tc: &Value) -> IrToolChoice {
    match tc {
        Value::String(s) => match s.as_str() {
            "auto" => IrToolChoice::Auto,
            "none" => IrToolChoice::None,
            "required" => IrToolChoice::Any,
            _ => IrToolChoice::Auto,
        },
        Value::Object(obj) => {
            if let Some(name) = obj.get("name").and_then(|v| v.as_str()) {
                IrToolChoice::Tool {
                    name: name.to_string(),
                }
            } else {
                IrToolChoice::Auto
            }
        }
        _ => IrToolChoice::Auto,
    }
}

// ═══════════════════════════════════════════════════════════════════
// 流式 chunk 解析
// ═══════════════════════════════════════════════════════════════════

/// Responses chunk 解析状态。
pub struct ResponsesParseState {
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
    /// 消息 ID
    msg_id: String,
    /// 模型名
    model: String,
}

impl ResponsesParseState {
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

impl Default for ResponsesParseState {
    fn default() -> Self {
        Self::new()
    }
}

/// 将 OpenAI Responses 流式 chunk 解析为 IR 事件序列。
///
/// Responses API 的流式事件：
/// - `response.created` → MessageStart
/// - `response.output_item.added` → ContentBlockStart
/// - `response.content_part.added` → 内容部分开始
/// - `response.output_text.delta` → TextDelta
/// - `response.reasoning.delta` → ThinkingDelta
/// - `response.function_call_arguments.delta` → InputJsonDelta
/// - `response.content_part.done` → ContentBlockStop
/// - `response.output_item.done` → ContentBlockStop
/// - `response.completed` → MessageDelta + MessageStop
pub fn responses_chunk_to_ir(
    chunk: &Value,
    state: &mut ResponsesParseState,
) -> Vec<IrStreamEvent> {
    let mut events = vec![];

    let event_type = match chunk.get("type").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return events,
    };

    match event_type {
        "response.created" => {
            // 提取 ID 和模型
            if let Some(response) = chunk.get("response") {
                if let Some(id) = response.get("id").and_then(|v| v.as_str()) {
                    state.msg_id = id.to_string();
                }
                if let Some(model) = response.get("model").and_then(|v| v.as_str()) {
                    state.model = model.to_string();
                }
            }

            events.push(IrStreamEvent::MessageStart {
                id: state.msg_id.clone(),
                model: state.model.clone(),
            });
        }

        "response.output_item.added" => {
            // 新的输出项（message/function_call/reasoning）
            if let Some(item) = chunk.get("item") {
                let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");

                match item_type {
                    "function_call" => {
                        let id = item
                            .get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();

                        // 如果 text 已开启但未关闭，先关闭
                        if state.text_started && state.tool_count == 0 {
                            events.push(IrStreamEvent::ContentBlockStop {
                                index: state.text_index,
                            });
                        }

                        let block_index = if state.thinking_started || state.text_started {
                            state.text_index + 1 + state.tool_count
                        } else {
                            state.tool_count
                        };

                        events.push(IrStreamEvent::ContentBlockStart {
                            index: block_index,
                            block: IrContentBlockStart::ToolUse { id, name },
                        });
                        state.tool_count += 1;
                    }
                    "reasoning" => {
                        if !state.thinking_started {
                            events.push(IrStreamEvent::ContentBlockStart {
                                index: 0,
                                block: IrContentBlockStart::Thinking,
                            });
                            state.thinking_started = true;
                        }
                    }
                    _ => {}
                }
            }
        }

        "response.output_text.delta" => {
            // 文本增量
            if let Some(delta) = chunk.get("delta").and_then(|v| v.as_str()) {
                // 如果 thinking 已开启但未关闭，先关闭
                if state.thinking_started && !state.text_started {
                    events.push(IrStreamEvent::ContentBlockStop { index: 0 });
                    state.text_index = 1;
                }

                if !state.text_started {
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
                    delta: IrContentDelta::TextDelta(delta.to_string()),
                });
            }
        }

        "response.reasoning.delta" => {
            // Reasoning 增量
            if let Some(delta) = chunk.get("delta").and_then(|v| v.as_str()) {
                if !state.thinking_started {
                    events.push(IrStreamEvent::ContentBlockStart {
                        index: 0,
                        block: IrContentBlockStart::Thinking,
                    });
                    state.thinking_started = true;
                }

                events.push(IrStreamEvent::ContentBlockDelta {
                    index: 0,
                    delta: IrContentDelta::ThinkingDelta(delta.to_string()),
                });
            }
        }

        "response.function_call_arguments.delta" => {
            // Function call 参数增量
            if let Some(delta) = chunk.get("delta").and_then(|v| v.as_str()) {
                let block_index = if state.thinking_started || state.text_started {
                    state.text_index + state.tool_count
                } else {
                    state.tool_count - 1
                };

                events.push(IrStreamEvent::ContentBlockDelta {
                    index: block_index,
                    delta: IrContentDelta::InputJsonDelta(delta.to_string()),
                });
            }
        }

        "response.content_part.done" | "response.output_item.done" => {
            // 内容部分或输出项完成
            // 这里不发送 ContentBlockStop，因为 response.completed 会统一处理
        }

        "response.completed" => {
            // 流结束
            // 关闭所有未关闭的 block
            if state.thinking_started && !state.text_started {
                events.push(IrStreamEvent::ContentBlockStop { index: 0 });
            }
            if state.text_started {
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

            // 提取 usage
            let usage = super::usage::extract_responses_usage(chunk);
            if usage.input_tokens > 0 || usage.output_tokens > 0 {
                state.usage.input_tokens = state.usage.input_tokens.max(usage.input_tokens);
                state.usage.output_tokens = state.usage.output_tokens.max(usage.output_tokens);
                state.usage.cache_read_input_tokens =
                    state.usage.cache_read_input_tokens.max(usage.cache_read_input_tokens);
            }

            // 提取 stop_reason
            let stop_reason = chunk
                .get("response")
                .and_then(|r| r.get("status"))
                .and_then(|v| v.as_str())
                .map(|s| match s {
                    "completed" => IrStopReason::EndTurn,
                    "incomplete" => IrStopReason::MaxTokens,
                    _ => IrStopReason::EndTurn,
                })
                .unwrap_or(IrStopReason::EndTurn);

            events.push(IrStreamEvent::MessageDelta {
                stop_reason: Some(stop_reason),
                usage: Some(state.usage.clone()),
            });
            events.push(IrStreamEvent::MessageStop);
        }

        _ => {}
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_basic_request_string_input() {
        let req = json!({
            "model": "gpt-4o",
            "input": "Hello",
            "stream": true
        });
        let ir = responses_req_to_ir(&req);
        assert_eq!(ir.model, "gpt-4o");
        assert!(ir.stream);
        assert_eq!(ir.messages.len(), 1);
        assert_eq!(ir.messages[0].role, IrRole::User);
        assert!(matches!(&ir.messages[0].content[0], IrContentBlock::Text { text, .. } if text == "Hello"));
    }

    #[test]
    fn test_basic_request_array_input() {
        let req = json!({
            "model": "gpt-4o",
            "input": [
                {
                    "type": "message",
                    "role": "system",
                    "content": [{"type": "input_text", "text": "You are helpful."}]
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "Hello"}]
                }
            ],
            "max_output_tokens": 4096
        });
        let ir = responses_req_to_ir(&req);
        assert!(matches!(ir.system, Some(IrSystemContent::Text(ref t)) if t == "You are helpful."));
        assert_eq!(ir.messages.len(), 1);
        assert_eq!(ir.max_tokens, Some(4096));
    }

    #[test]
    fn test_function_call_and_output() {
        let req = json!({
            "model": "gpt-4o",
            "input": [
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "search",
                    "arguments": "{\"q\":\"test\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "result text"
                }
            ],
            "tools": [{
                "type": "function",
                "name": "search",
                "description": "Search the web",
                "parameters": {"type": "object", "properties": {"q": {"type": "string"}}}
            }],
            "tool_choice": "auto"
        });
        let ir = responses_req_to_ir(&req);
        // Function call → ToolUse
        assert!(matches!(&ir.messages[0].content[0], IrContentBlock::ToolUse { id, name, .. } if id == "call_1" && name == "search"));
        // Function call output → ToolResult
        assert!(matches!(&ir.messages[1].content[0], IrContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "call_1"));
        // Tool definition
        assert_eq!(ir.tools.len(), 1);
        assert_eq!(ir.tools[0].name, "search");
    }

    #[test]
    fn test_reasoning_config() {
        let req = json!({
            "model": "o1",
            "input": "Hello",
            "reasoning": {"effort": "high"}
        });
        let ir = responses_req_to_ir(&req);
        let thinking = ir.thinking.unwrap();
        assert!(thinking.enabled);
        assert_eq!(thinking.budget_tokens, Some(16384));
    }

    #[test]
    fn test_image_input() {
        let req = json!({
            "model": "gpt-4o",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "What is this?"},
                    {"type": "input_image", "image_url": "https://example.com/img.png"}
                ]
            }]
        });
        let ir = responses_req_to_ir(&req);
        assert_eq!(ir.messages[0].content.len(), 2);
        assert!(matches!(&ir.messages[0].content[1], IrContentBlock::Image { source: IrImageSource::Url { url } } if url == "https://example.com/img.png"));
    }

    // ── 流式 chunk 测试 ──

    #[test]
    fn test_chunk_response_created() {
        let mut state = ResponsesParseState::new();
        let chunk = json!({
            "type": "response.created",
            "response": {
                "id": "resp_123",
                "model": "gpt-4o"
            }
        });
        let events = responses_chunk_to_ir(&chunk, &mut state);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], IrStreamEvent::MessageStart { id, model } if id == "resp_123" && model == "gpt-4o"));
    }

    #[test]
    fn test_chunk_text_delta() {
        let mut state = ResponsesParseState::new();
        state.msg_id = "resp_1".to_string();
        state.model = "gpt-4o".to_string();

        let chunk = json!({
            "type": "response.output_text.delta",
            "delta": "Hello"
        });
        let events = responses_chunk_to_ir(&chunk, &mut state);
        // 应该发送 ContentBlockStart + ContentBlockDelta
        assert!(events.iter().any(|e| matches!(e, IrStreamEvent::ContentBlockStart { index: 0, block: IrContentBlockStart::Text })));
        assert!(events.iter().any(|e| matches!(e, IrStreamEvent::ContentBlockDelta { index: 0, delta: IrContentDelta::TextDelta(t) } if t == "Hello")));
    }

    #[test]
    fn test_chunk_reasoning_then_text() {
        let mut state = ResponsesParseState::new();
        state.msg_id = "resp_1".to_string();
        state.model = "o1".to_string();

        // reasoning delta
        let chunk1 = json!({
            "type": "response.reasoning.delta",
            "delta": "thinking..."
        });
        let events1 = responses_chunk_to_ir(&chunk1, &mut state);
        assert!(events1.iter().any(|e| matches!(e, IrStreamEvent::ContentBlockStart { index: 0, block: IrContentBlockStart::Thinking })));
        assert!(events1.iter().any(|e| matches!(e, IrStreamEvent::ContentBlockDelta { index: 0, delta: IrContentDelta::ThinkingDelta(t) } if t == "thinking...")));

        // text delta (should close thinking first)
        let chunk2 = json!({
            "type": "response.output_text.delta",
            "delta": "answer"
        });
        let events2 = responses_chunk_to_ir(&chunk2, &mut state);
        assert!(events2.iter().any(|e| matches!(e, IrStreamEvent::ContentBlockStop { index: 0 })));
        assert!(events2.iter().any(|e| matches!(e, IrStreamEvent::ContentBlockStart { index: 1, block: IrContentBlockStart::Text })));
        assert!(events2.iter().any(|e| matches!(e, IrStreamEvent::ContentBlockDelta { index: 1, delta: IrContentDelta::TextDelta(t) } if t == "answer")));
    }

    #[test]
    fn test_chunk_function_call() {
        let mut state = ResponsesParseState::new();
        state.msg_id = "resp_1".to_string();
        state.model = "gpt-4o".to_string();

        // function_call added
        let chunk1 = json!({
            "type": "response.output_item.added",
            "item": {
                "type": "function_call",
                "call_id": "call_1",
                "name": "search"
            }
        });
        let events1 = responses_chunk_to_ir(&chunk1, &mut state);
        assert!(events1.iter().any(|e| matches!(e, IrStreamEvent::ContentBlockStart { index: 0, block: IrContentBlockStart::ToolUse { id, name } } if id == "call_1" && name == "search")));

        // function_call arguments delta
        let chunk2 = json!({
            "type": "response.function_call_arguments.delta",
            "delta": "{\"q\":"
        });
        let events2 = responses_chunk_to_ir(&chunk2, &mut state);
        assert!(events2.iter().any(|e| matches!(e, IrStreamEvent::ContentBlockDelta { index: 0, delta: IrContentDelta::InputJsonDelta(p) } if p == "{\"q\":")));
    }

    #[test]
    fn test_chunk_response_completed() {
        let mut state = ResponsesParseState::new();
        state.msg_id = "resp_1".to_string();
        state.model = "gpt-4o".to_string();
        state.text_started = true;
        state.text_index = 0;

        let chunk = json!({
            "type": "response.completed",
            "response": {
                "status": "completed",
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "input_tokens_details": {"cached_tokens": 80}
                }
            }
        });
        let events = responses_chunk_to_ir(&chunk, &mut state);
        // 应该关闭 text block，发送 MessageDelta + MessageStop
        assert!(events.iter().any(|e| matches!(e, IrStreamEvent::ContentBlockStop { index: 0 })));
        assert!(events.iter().any(|e| matches!(e, IrStreamEvent::MessageDelta { stop_reason: Some(IrStopReason::EndTurn), .. })));
        assert!(events.iter().any(|e| matches!(e, IrStreamEvent::MessageStop)));
        assert_eq!(state.usage.input_tokens, 100);
        assert_eq!(state.usage.output_tokens, 50);
        assert_eq!(state.usage.cache_read_input_tokens, 80);
    }
}
