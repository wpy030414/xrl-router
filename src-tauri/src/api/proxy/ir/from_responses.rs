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
                        // server-side 内置工具（web_search_preview 等）：无 name 字段，
                        // 归一化为 name="web_search"，保证 MCP 模式的搜索工具剔除对 Responses 客户端生效
                        let ty = t["type"].as_str().unwrap_or("");
                        if ty.starts_with("web_search") {
                            return Some(IrTool {
                                name: "web_search".to_string(),
                                description: Some("Search the web for information".to_string()),
                                // web_search 工具需要合理的 schema，让上游 LLM 知道如何填写 query
                                input_schema: serde_json::json!({
                                    "type": "object",
                                    "properties": {
                                        "query": {
                                            "type": "string",
                                            "description": "The search query"
                                        },
                                        "max_results": {
                                            "type": "integer",
                                            "description": "Maximum number of results to return",
                                            "default": 5
                                        }
                                    },
                                    "required": ["query"]
                                }),
                            });
                        }
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
    // 顶层 instructions 字段（Responses API 标准 system 载体）
    let mut system_parts: Vec<String> = vec![];
    if let Some(inst) = req.get("instructions") {
        match inst {
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

    let input = match req.get("input") {
        Some(Value::String(s)) => {
            // 简单字符串 input → 单条 user 消息
            let system = if system_parts.is_empty() {
                None
            } else if system_parts.len() == 1 {
                Some(IrSystemContent::Text(system_parts.into_iter().next().unwrap()))
            } else {
                Some(IrSystemContent::Blocks(
                    system_parts.into_iter().map(|text| IrSystemBlock { text, cache_control: None }).collect(),
                ))
            };
            return (
                system,
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
        _ => return (if system_parts.is_empty() { None } else { Some(IrSystemContent::Text(system_parts.join(" "))) }, vec![]),
    };

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
                // output 可能是字符串或数组（output_text parts），错误标记 is_error
                let is_error = item.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
                let output = match item.get("output") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Array(parts)) => parts
                        .iter()
                        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    _ => String::new(),
                };

                messages.push(IrMessage {
                    role: IrRole::User,
                    content: vec![IrContentBlock::ToolResult {
                        tool_use_id: call_id,
                        content: IrToolResultContent::Text(output),
                        is_error,
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
///
/// 以「output item 生命周期」为核心：每个 output item（reasoning/function_call/
/// message）分配一个连续 IR block index；文本/参数 delta 按 output_index 路由到
/// 对应块。subagent 高频场景（thinking → tool → 后续 text）因此不会混块或重复关块。
pub struct ResponsesParseState {
    /// 已捕获的 usage
    pub usage: IrUsage,
    /// 下一个 IR block index（thinking/text/tool 连续分配）
    next_index: usize,
    /// 是否已开过 thinking 块（避免重复 start）
    thinking_ever_started: bool,
    /// thinking 块当前是否打开
    thinking_open: bool,
    /// 当前打开的 text 块：(ir_index, 所属 output_index)
    text_open: Option<(usize, usize)>,
    /// output_index → IR block index（function_call 参数 delta 路由）
    tool_map: std::collections::HashMap<usize, usize>,
    /// 消息 ID
    msg_id: String,
    /// 模型名
    model: String,
}

impl ResponsesParseState {
    pub fn new() -> Self {
        Self {
            usage: IrUsage::default(),
            next_index: 0,
            thinking_ever_started: false,
            thinking_open: false,
            text_open: None,
            tool_map: std::collections::HashMap::new(),
            msg_id: String::new(),
            model: String::new(),
        }
    }

    /// 分配下一个 IR block index（单调递增）。
    fn alloc_index(&mut self) -> usize {
        let i = self.next_index;
        self.next_index += 1;
        i
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
                // Responses 的 usage 在 response.completed 才给出，
                // 这里先用估算值占位（forward.rs 已预填）
                usage: Some(state.usage.clone()),
            });
        }

        "response.output_item.added" => {
            // 新的输出项（message/function_call/reasoning）
            if let Some(item) = chunk.get("item") {
                let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let output_index = chunk["output_index"].as_u64().unwrap_or(0) as usize;

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

                        // 关闭当前打开的 text 块（工具调用意味着文本结束）
                        if let Some((t_idx, _)) = state.text_open.take() {
                            events.push(IrStreamEvent::ContentBlockStop { index: t_idx });
                        }
                        // 关闭当前打开的 thinking 块
                        if state.thinking_open {
                            events.push(IrStreamEvent::ContentBlockStop { index: 0 });
                            state.thinking_open = false;
                        }

                        // 分配独立 IR block index，并记录 output_index → IR index 映射
                        let block_index = state.alloc_index();
                        state.tool_map.insert(output_index, block_index);

                        events.push(IrStreamEvent::ContentBlockStart {
                            index: block_index,
                            block: IrContentBlockStart::ToolUse { id, name },
                        });
                    }
                    "reasoning" => {
                        if !state.thinking_ever_started {
                            // thinking 固定占 index 0；后续 text/tool 从 1 起
                            if state.next_index == 0 {
                                state.next_index = 1;
                            }
                            events.push(IrStreamEvent::ContentBlockStart {
                                index: 0,
                                block: IrContentBlockStart::Thinking { signature: None },
                            });
                            state.thinking_ever_started = true;
                            state.thinking_open = true;
                        }
                    }
                    "message" => {
                        // 新 message item 到达：关闭上一个 text 块（若开着），
                        // 为「工具后继续输出文本」的场景分配新的独立块
                        if let Some((t_idx, _)) = state.text_open.take() {
                            events.push(IrStreamEvent::ContentBlockStop { index: t_idx });
                        }
                    }
                    _ => {}
                }
            }
        }

        "response.output_text.delta" => {
            // 文本增量：按 output_index 路由到对应 text 块
            if let Some(delta) = chunk.get("delta").and_then(|v| v.as_str()) {
                let output_index = chunk["output_index"].as_u64().unwrap_or(0) as usize;

                // 若当前打开的 text 块不属于该 output item，先关闭它
                if let Some((t_idx, t_out)) = state.text_open {
                    if t_out != output_index {
                        events.push(IrStreamEvent::ContentBlockStop { index: t_idx });
                        state.text_open = None;
                    }
                }

                // thinking 块需要让位给文本
                if state.thinking_open && state.text_open.is_none() {
                    events.push(IrStreamEvent::ContentBlockStop { index: 0 });
                    state.thinking_open = false;
                }

                if state.text_open.is_none() {
                    // 分配新的 text 块（单调递增，紧接 thinking 之后）
                    let block_index = state.alloc_index();
                    state.text_open = Some((block_index, output_index));
                    events.push(IrStreamEvent::ContentBlockStart {
                        index: block_index,
                        block: IrContentBlockStart::Text,
                    });
                }

                let (t_idx, _) = state.text_open.unwrap();
                events.push(IrStreamEvent::ContentBlockDelta {
                    index: t_idx,
                    delta: IrContentDelta::TextDelta(delta.to_string()),
                });
            }
        }

        "response.reasoning.delta" => {
            // Reasoning 增量
            if let Some(delta) = chunk.get("delta").and_then(|v| v.as_str()) {
                if !state.thinking_ever_started {
                    if state.next_index == 0 {
                        state.next_index = 1;
                    }
                    events.push(IrStreamEvent::ContentBlockStart {
                        index: 0,
                        block: IrContentBlockStart::Thinking { signature: None },
                    });
                    state.thinking_ever_started = true;
                    state.thinking_open = true;
                }

                events.push(IrStreamEvent::ContentBlockDelta {
                    index: 0,
                    delta: IrContentDelta::ThinkingDelta(delta.to_string()),
                });
            }
        }

        "response.function_call_arguments.delta" => {
            // Function call 参数增量：按 output_index 查映射路由到 tool block
            if let Some(delta) = chunk.get("delta").and_then(|v| v.as_str()) {
                let output_index = chunk["output_index"].as_u64().unwrap_or(0) as usize;
                let block_index = *state
                    .tool_map
                    .get(&output_index)
                    .unwrap_or(&state.next_index.saturating_sub(1));

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
            // 流结束：关闭所有未关闭的 block（各只关一次）
            if state.thinking_open {
                events.push(IrStreamEvent::ContentBlockStop { index: 0 });
                state.thinking_open = false;
            }
            if let Some((t_idx, _)) = state.text_open.take() {
                events.push(IrStreamEvent::ContentBlockStop { index: t_idx });
            }
            for (_, &block_index) in state.tool_map.iter() {
                events.push(IrStreamEvent::ContentBlockStop { index: block_index });
            }

            // 提取 usage（覆盖语义：真实值覆盖 forward.rs 预填的估算占位，
            // 不能 max——估算值偏大会永久压住真实值，污染 usage_log 与上下文条）
            let usage = super::usage::extract_responses_usage(chunk);
            if usage.input_tokens > 0 || usage.output_tokens > 0 {
                if usage.input_tokens > 0 {
                    state.usage.input_tokens = usage.input_tokens;
                }
                if usage.output_tokens > 0 {
                    state.usage.output_tokens = usage.output_tokens;
                }
                if usage.cache_read_input_tokens > 0 {
                    state.usage.cache_read_input_tokens = usage.cache_read_input_tokens;
                }
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
    fn test_top_level_instructions_field() {
        // Responses API 标准：instructions 是顶层字段
        let req = json!({
            "model": "gpt-4o",
            "instructions": "You are a helpful assistant.",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Hello"}]}
            ],
            "stream": true
        });
        let ir = responses_req_to_ir(&req);
        assert!(matches!(ir.system, Some(IrSystemContent::Text(ref t)) if t == "You are a helpful assistant."));
        assert_eq!(ir.messages.len(), 1);

        // instructions + input 内 system 都应保留
        let req2 = json!({
            "model": "gpt-4o",
            "instructions": "Top-level instructions.",
            "input": [
                {"type": "message", "role": "system", "content": [{"type": "input_text", "text": "In-input system."}]},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Hello"}]}
            ],
            "stream": true
        });
        let ir2 = responses_req_to_ir(&req2);
        let combined = match ir2.system {
            Some(IrSystemContent::Text(ref t)) => t.clone(),
            Some(IrSystemContent::Blocks(ref blocks)) => blocks.iter().map(|b| b.text.clone()).collect::<Vec<_>>().join(" "),
            None => String::new(),
        };
        assert!(combined.contains("Top-level instructions."));
        assert!(combined.contains("In-input system."));
        assert_eq!(ir2.messages.len(), 1);

        // 字符串 input + instructions 也保留 system
        let req3 = json!({
            "model": "gpt-4o",
            "instructions": "Be brief.",
            "input": "Hello",
            "stream": true
        });
        let ir3 = responses_req_to_ir(&req3);
        assert!(matches!(ir3.system, Some(IrSystemContent::Text(ref t)) if t == "Be brief."));
        assert_eq!(ir3.messages.len(), 1);
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
    fn test_server_side_web_search_tool() {
        // Responses 客户端内置 web_search_preview：无 name 字段，应归一化为 web_search
        let req = json!({
            "model": "gpt-4o",
            "input": "What's the news?",
            "tools": [{
                "type": "web_search_preview",
                "search_context_size": "medium"
            }]
        });
        let ir = responses_req_to_ir(&req);
        assert_eq!(ir.tools.len(), 1);
        assert_eq!(ir.tools[0].name, "web_search", "内置 web_search_preview 应归一化为 web_search 以触发劫持");
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
        assert!(matches!(&events[0], IrStreamEvent::MessageStart { id, model, usage: Some(_) } if id == "resp_123" && model == "gpt-4o"));
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
        assert!(events1.iter().any(|e| matches!(e, IrStreamEvent::ContentBlockStart { index: 0, block: IrContentBlockStart::Thinking { .. } })));
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
        // 模拟已开过一个 text 块（index 0，output 0）
        state.text_open = Some((0, 0));

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
        assert_eq!(state.usage.input_tokens, 20);
        assert_eq!(state.usage.output_tokens, 50);
        assert_eq!(state.usage.cache_read_input_tokens, 80);
    }

    // ── subagent / 工具调用高频场景 ──

    /// 场景 A：thinking → function_call（推理模型直接调工具，无文本）
    #[test]
    fn test_subagent_thinking_then_direct_tool() {
        let mut state = ResponsesParseState::new();
        state.msg_id = "resp_1".to_string();
        state.model = "o1".to_string();

        let chunks = [
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"id": "r1", "type": "reasoning", "status": "in_progress", "content": [], "summary": []}}),
            json!({"type": "response.reasoning.delta", "item_id": "r1", "output_index": 0, "delta": "Need to check weather"}),
            json!({"type": "response.output_item.added", "output_index": 1, "item": {"id": "fc1", "type": "function_call", "call_id": "call_1", "name": "get_weather", "status": "in_progress"}}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc1", "output_index": 1, "delta": "{\"city\":\"Tokyo\"}"}),
            json!({"type": "response.completed", "response": {"id": "resp_1", "status": "completed"}}),
        ];
        let mut all = vec![];
        for c in &chunks {
            all.extend(responses_chunk_to_ir(c, &mut state));
        }
        // 事件序列：thinking start → thinking delta → thinking stop → tool start → args delta → tool stop → delta+stop
        let kinds: Vec<&str> = all.iter().map(|e| match e {
            IrStreamEvent::ContentBlockStart { block: IrContentBlockStart::Thinking { .. }, .. } => "t_start",
            IrStreamEvent::ContentBlockStart { block: IrContentBlockStart::ToolUse { .. }, .. } => "tool_start",
            IrStreamEvent::ContentBlockStop { .. } => "stop",
            _ => "other",
        }).collect();
        // 关键断言：thinking 在工具前被关闭（stop 总数 = 2：thinking + tool）
        let stops = kinds.iter().filter(|k| **k == "stop").count();
        assert_eq!(stops, 2, "thinking 与 tool 应各关一次: {:?}", kinds);
        // tool 的 start 与 args delta 应同 index（=1，thinking 占 0）
        let tool_start_idx = all.iter().find_map(|e| match e {
            IrStreamEvent::ContentBlockStart { index, block: IrContentBlockStart::ToolUse { .. } } => Some(*index),
            _ => None,
        }).unwrap();
        let args_idx = all.iter().find_map(|e| match e {
            IrStreamEvent::ContentBlockDelta { index, delta: IrContentDelta::InputJsonDelta(_) } => Some(*index),
            _ => None,
        }).unwrap();
        assert_eq!(tool_start_idx, 1);
        assert_eq!(args_idx, tool_start_idx, "参数 delta 应路由到 tool block");
    }

    /// 场景 B：thinking → text → function_call → completed（工具调用后结束）
    #[test]
    fn test_subagent_thinking_text_then_tool() {
        let mut state = ResponsesParseState::new();
        state.msg_id = "resp_1".to_string();
        state.model = "o1".to_string();

        let chunks = [
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"id": "r1", "type": "reasoning", "status": "in_progress", "content": [], "summary": []}}),
            json!({"type": "response.reasoning.delta", "item_id": "r1", "output_index": 0, "delta": "Reasoning"}),
            json!({"type": "response.output_item.added", "output_index": 1, "item": {"id": "m1", "type": "message", "status": "in_progress", "role": "assistant", "content": []}}),
            json!({"type": "response.output_text.delta", "item_id": "m1", "output_index": 1, "content_index": 0, "delta": "Let me check"}),
            json!({"type": "response.output_item.added", "output_index": 2, "item": {"id": "fc1", "type": "function_call", "call_id": "call_1", "name": "Bash", "status": "in_progress"}}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc1", "output_index": 2, "delta": "{\"command\":\"ls\"}"}),
            json!({"type": "response.completed", "response": {"id": "resp_1", "status": "completed"}}),
        ];
        let mut all = vec![];
        for c in &chunks {
            all.extend(responses_chunk_to_ir(c, &mut state));
        }
        // thinking(0) → text(1) → tool(2)；text 关闭后不得重复关闭
        let stops: Vec<usize> = all.iter().filter_map(|e| match e {
            IrStreamEvent::ContentBlockStop { index } => Some(*index),
            _ => None,
        }).collect();
        assert_eq!(stops, vec![0, 1, 2], "thinking/text/tool 各关一次，index 正确: {:?}", stops);
        // args delta 路由到 tool（index 2）
        let args_idx = all.iter().find_map(|e| match e {
            IrStreamEvent::ContentBlockDelta { index, delta: IrContentDelta::InputJsonDelta(_) } => Some(*index),
            _ => None,
        }).unwrap();
        assert_eq!(args_idx, 2);
    }

    /// 场景 C：text → function_call → 第二个 message（模型工具后继续总结，subagent 高频）
    #[test]
    fn test_subagent_tool_then_followup_text() {
        let mut state = ResponsesParseState::new();
        state.msg_id = "resp_1".to_string();
        state.model = "gpt-4o".to_string();

        let chunks = [
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"id": "m1", "type": "message", "status": "in_progress", "role": "assistant", "content": []}}),
            json!({"type": "response.output_text.delta", "item_id": "m1", "output_index": 0, "content_index": 0, "delta": "Checking"}),
            json!({"type": "response.output_item.added", "output_index": 1, "item": {"id": "fc1", "type": "function_call", "call_id": "call_1", "name": "Bash", "status": "in_progress"}}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc1", "output_index": 1, "delta": "{\"command\":\"ls\"}"}),
            json!({"type": "response.output_item.added", "output_index": 2, "item": {"id": "m2", "type": "message", "status": "in_progress", "role": "assistant", "content": []}}),
            json!({"type": "response.output_text.delta", "item_id": "m2", "output_index": 2, "content_index": 0, "delta": "Done"}),
            json!({"type": "response.completed", "response": {"id": "resp_1", "status": "completed"}}),
        ];
        let mut all = vec![];
        for c in &chunks {
            all.extend(responses_chunk_to_ir(c, &mut state));
        }
        // 两个 text 应各有一个 start（0 和 2），tool 在 1
        let starts: Vec<usize> = all.iter().filter_map(|e| match e {
            IrStreamEvent::ContentBlockStart { index, .. } => Some(*index),
            _ => None,
        }).collect();
        assert_eq!(starts, vec![0, 1, 2], "两个 text + 一个 tool 各有独立 start: {:?}", starts);
        // 文本内容：Checking + Done 分属 index 0 和 2
        let text0: String = all.iter().filter_map(|e| match e {
            IrStreamEvent::ContentBlockDelta { index: 0, delta: IrContentDelta::TextDelta(t) } => Some(t.clone()),
            _ => None,
        }).collect();
        let text2: String = all.iter().filter_map(|e| match e {
            IrStreamEvent::ContentBlockDelta { index: 2, delta: IrContentDelta::TextDelta(t) } => Some(t.clone()),
            _ => None,
        }).collect();
        assert_eq!(text0, "Checking");
        assert_eq!(text2, "Done");
        // stop 序列无重复
        let mut stops: Vec<usize> = all.iter().filter_map(|e| match e {
            IrStreamEvent::ContentBlockStop { index } => Some(*index),
            _ => None,
        }).collect();
        stops.sort_unstable();
        assert_eq!(stops, vec![0, 1, 2], "各块只关一次: {:?}", stops);
    }

    /// 场景 D：subagent 多轮历史（assistant tool_use → user tool_result → 新任务）
    #[test]
    fn test_subagent_multi_turn_history() {
        let req = json!({
            "model": "gpt-4o",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Do task"}]},
                {"type": "function_call", "call_id": "call_1", "name": "Bash", "arguments": "{\"command\":\"ls\"}"},
                {"type": "function_call_output", "call_id": "call_1", "output": [{"type": "output_text", "text": "file.txt"}]},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Now summarize"}]}
            ]
        });
        let ir = responses_req_to_ir(&req);
        // 消息顺序：user → assistant(tool_use) → user(tool_result) → user
        assert_eq!(ir.messages.len(), 4);
        assert_eq!(ir.messages[0].role, IrRole::User);
        assert!(matches!(&ir.messages[1].content[0], IrContentBlock::ToolUse { id, name, .. } if id == "call_1" && name == "Bash"));
        assert!(matches!(&ir.messages[2].content[0], IrContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "call_1"));
        assert_eq!(ir.messages[3].role, IrRole::User);
        // tool_result 的 output 数组应解析为文本
        let text = match &ir.messages[2].content[0] {
            IrContentBlock::ToolResult { content: IrToolResultContent::Text(t), .. } => t.clone(),
            _ => String::new(),
        };
        assert_eq!(text, "file.txt");
    }

    /// 场景 E：同一 output item 内 text → thinking 交错（部分推理模型）
    #[test]
    fn test_subagent_interleaved_text_and_thinking() {
        let mut state = ResponsesParseState::new();
        state.msg_id = "resp_1".to_string();
        state.model = "o1".to_string();

        let chunks = [
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"id": "r1", "type": "reasoning", "status": "in_progress", "content": [], "summary": []}}),
            json!({"type": "response.reasoning.delta", "item_id": "r1", "output_index": 0, "delta": "think part 1"}),
            json!({"type": "response.output_item.added", "output_index": 1, "item": {"id": "m1", "type": "message", "status": "in_progress", "role": "assistant", "content": []}}),
            json!({"type": "response.output_text.delta", "item_id": "m1", "output_index": 1, "content_index": 0, "delta": "answer part 1"}),
            json!({"type": "response.reasoning.delta", "item_id": "r1", "output_index": 0, "delta": "think part 2"}),  // 交错：thinking 又来了
            json!({"type": "response.output_text.delta", "item_id": "m1", "output_index": 1, "content_index": 0, "delta": " answer part 2"}),
            json!({"type": "response.completed", "response": {"id": "resp_1", "status": "completed"}}),
        ];
        let mut all = vec![];
        for c in &chunks {
            all.extend(responses_chunk_to_ir(c, &mut state));
        }
        // 文本全部在一个块里（output item 生命周期内）
        let text: String = all.iter().filter_map(|e| match e {
            IrStreamEvent::ContentBlockDelta { delta: IrContentDelta::TextDelta(t), .. } => Some(t.clone()),
            _ => None,
        }).collect();
        assert_eq!(text, "answer part 1 answer part 2");
        // 交错 thinking：已关的 thinking 不应重复开块（thinking_ever_started 守卫）
        let thinking_starts = all.iter().filter(|e| matches!(e, IrStreamEvent::ContentBlockStart { block: IrContentBlockStart::Thinking { .. }, .. })).count();
        assert_eq!(thinking_starts, 1, "thinking 只开一次");
    }

}
