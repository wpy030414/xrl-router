//! IR → OpenAI Chat Completions 方向的格式翻译：请求体与流式事件渲染。
//!
//! Chat Completions 的 SSE 格式与 Anthropic 差异较大：
//! - 没有 content_block_start/stop，所有 delta 在 choices[0].delta 中
//! - tool_calls 有独立的 index 和 id
//! - finish_reason 在最后一个 chunk 的 choices[0] 中
//! - usage 在最后一个 chunk 的顶层（需 stream_options.include_usage）

use bytes::Bytes;
use serde_json::{json, Value};

use super::types::*;

/// 将 IR 请求体序列化为 OpenAI Chat Completions 格式。
pub fn ir_req_to_chat_completions(req: &IrRequest) -> Value {
    let mut out = json!({
        "model": req.model,
        "stream": req.stream,
    });

    // 构建 messages 数组
    let mut messages: Vec<Value> = vec![];

    // System prompt → messages[0]
    if let Some(ref system) = req.system {
        let system_content = match system {
            IrSystemContent::Text(t) => t.clone(),
            IrSystemContent::Blocks(blocks) => blocks
                .iter()
                .map(|b| b.text.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        };
        messages.push(json!({"role": "system", "content": system_content}));
    }

    // Messages
    for msg in &req.messages {
        let role = match msg.role {
            IrRole::User => "user",
            IrRole::Assistant => "assistant",
        };

        let mut msg_obj = json!({"role": role});
        let mut text_parts: Vec<String> = vec![];
        let mut tool_calls: Vec<Value> = vec![];
        let mut reasoning_content: Option<String> = None;

        for block in &msg.content {
            match block {
                IrContentBlock::Text { text, .. } => {
                    text_parts.push(text.clone());
                }
                IrContentBlock::Thinking { thinking, .. } => {
                    reasoning_content = Some(thinking.clone());
                }
                IrContentBlock::ToolUse { id, name, input } => {
                    tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": serde_json::to_string(input).unwrap_or_else(|_| "{}".to_string())
                        }
                    }));
                }
                IrContentBlock::ToolResult { tool_use_id, content, .. } => {
                    // ToolResult 作为独立的 tool role 消息
                    let result_text = match content {
                        IrToolResultContent::Text(t) => t.clone(),
                        IrToolResultContent::Blocks(blocks) => blocks
                            .iter()
                            .filter_map(|b| {
                                if let IrContentBlock::Text { text, .. } = b {
                                    Some(text.clone())
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    };
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": tool_use_id,
                        "content": result_text
                    }));
                }
                IrContentBlock::Image { source } => {
                    // Image → content parts
                    let image_url = match source {
                        IrImageSource::Url { url } => url.clone(),
                        IrImageSource::Base64 { media_type, data } => {
                            format!("data:{};base64,{}", media_type, data)
                        }
                    };
                    // 如果有文本，先添加文本
                    if !text_parts.is_empty() {
                        let text = text_parts.join("\n");
                        msg_obj["content"] = json!([
                            {"type": "text", "text": text},
                            {"type": "image_url", "image_url": {"url": image_url}}
                        ]);
                        text_parts.clear();
                    } else {
                        msg_obj["content"] = json!([
                            {"type": "image_url", "image_url": {"url": image_url}}
                        ]);
                    }
                }
            }
        }

        // 设置 content
        if !text_parts.is_empty() {
            let text = text_parts.join("\n");
            if msg_obj.get("content").is_none() {
                msg_obj["content"] = json!(text);
            }
        } else if msg_obj.get("content").is_none() && tool_calls.is_empty() {
            msg_obj["content"] = json!("");
        }

        // 设置 reasoning_content
        if let Some(reasoning) = reasoning_content {
            msg_obj["reasoning_content"] = json!(reasoning);
        }

        // 设置 tool_calls
        if !tool_calls.is_empty() {
            msg_obj["tool_calls"] = json!(tool_calls);
            if msg_obj.get("content").is_none() {
                msg_obj["content"] = Value::Null;
            }
        }

        messages.push(msg_obj);
    }

    out["messages"] = json!(messages);

    // Tools
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut obj = json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "parameters": t.input_schema,
                    }
                });
                if let Some(ref desc) = t.description {
                    obj["function"]["description"] = json!(desc);
                }
                obj
            })
            .collect();
        out["tools"] = json!(tools);
    }

    // Tool choice
    if let Some(ref tc) = req.tool_choice {
        out["tool_choice"] = match tc {
            IrToolChoice::Auto => json!("auto"),
            IrToolChoice::Any => json!("required"),
            IrToolChoice::None => json!("none"),
            IrToolChoice::Tool { name } => {
                json!({"type": "function", "function": {"name": name}})
            }
        };
    }

    // Thinking config → reasoning_effort
    if let Some(ref thinking) = req.thinking {
        if thinking.enabled {
            let effort = match thinking.budget_tokens {
                Some(b) if b <= 2048 => "low",
                Some(b) if b <= 8192 => "medium",
                _ => "high",
            };
            out["reasoning_effort"] = json!(effort);
        }
    }

    // Pass through scalar params
    if let Some(max_tokens) = req.max_tokens {
        out["max_tokens"] = json!(max_tokens);
    }
    if let Some(temperature) = req.temperature {
        out["temperature"] = json!(temperature);
    }
    if let Some(top_p) = req.top_p {
        out["top_p"] = json!(top_p);
    }

    out
}

// ═══════════════════════════════════════════════════════════════════
// 流式事件渲染
// ═══════════════════════════════════════════════════════════════════

/// Chat Completions SSE 渲染状态机。
///
/// Chat 的 SSE 格式特点：
/// - 每个 chunk 都有完整的 id/model/choices 结构
/// - delta 中只有变化的字段
/// - tool_calls 有独立的 index
/// - finish_reason 在最后一个 chunk
pub struct ChatCompletionsRenderState {
    msg_id: String,
    model: String,
    created: i64,
    /// 已发送的 tool_call 数量
    tool_count: usize,
    /// 是否已发送第一个 chunk（需要包含 role）
    first_chunk_sent: bool,
}

impl ChatCompletionsRenderState {
    pub fn new() -> Self {
        Self {
            msg_id: String::new(),
            model: String::new(),
            created: chrono::Utc::now().timestamp(),
            tool_count: 0,
            first_chunk_sent: false,
        }
    }

    /// 将 IR 流式事件渲染为 Chat Completions SSE 字节段。
    ///
    /// 返回 `None` 表示该事件不产生输出（如 ContentBlockStart/Stop）。
    pub fn render_event(&mut self, ev: &IrStreamEvent) -> Option<Bytes> {
        let mk = |payload: Value| -> Bytes {
            let data = serde_json::to_string(&payload).unwrap_or_default();
            Bytes::from(format!("data: {}\n\n", data))
        };

        match ev {
            IrStreamEvent::MessageStart { id, model } => {
                self.msg_id = id.clone();
                self.model = model.clone();
                // Chat 不需要单独的 start 事件，第一个 delta 会包含 role
                None
            }
            IrStreamEvent::ContentBlockStart { block, .. } => {
                match block {
                    IrContentBlockStart::ToolUse { id, name } => {
                        // tool_use start 需要发送 tool_call header
                        let tool_index = self.tool_count;
                        self.tool_count += 1;
                        let mut delta_obj = json!({});
                        if !self.first_chunk_sent {
                            delta_obj["role"] = json!("assistant");
                            self.first_chunk_sent = true;
                        }
                        delta_obj["tool_calls"] = json!([{
                            "index": tool_index,
                            "id": id,
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": ""
                            }
                        }]);

                        Some(mk(json!({
                            "id": self.msg_id,
                            "object": "chat.completion.chunk",
                            "created": self.created,
                            "model": self.model,
                            "choices": [{
                                "index": 0,
                                "delta": delta_obj,
                                "finish_reason": null
                            }]
                        })))
                    }
                    _ => {
                        // text/thinking: Chat 没有 content_block_start
                        None
                    }
                }
            }
            IrStreamEvent::ContentBlockDelta { index, delta } => {
                let mut delta_obj = json!({});

                // 第一个 chunk 需要包含 role
                if !self.first_chunk_sent {
                    delta_obj["role"] = json!("assistant");
                    self.first_chunk_sent = true;
                }

                match delta {
                    IrContentDelta::TextDelta(text) => {
                        delta_obj["content"] = json!(text);
                    }
                    IrContentDelta::ThinkingDelta(thinking) => {
                        delta_obj["reasoning_content"] = json!(thinking);
                    }
                    IrContentDelta::InputJsonDelta(partial) => {
                        // tool_calls arguments delta
                        // 需要计算 tool_call index（减去 thinking 和 text 的 block）
                        let tool_index = *index as i64 - (self.tool_count as i64 - 1).max(0);
                        delta_obj["tool_calls"] = json!([{
                            "index": tool_index.max(0),
                            "function": {
                                "arguments": partial
                            }
                        }]);
                    }
                }

                Some(mk(json!({
                    "id": self.msg_id,
                    "object": "chat.completion.chunk",
                    "created": self.created,
                    "model": self.model,
                    "choices": [{
                        "index": 0,
                        "delta": delta_obj,
                        "finish_reason": null
                    }]
                })))
            }
            IrStreamEvent::ContentBlockStop { .. } => {
                // Chat 没有 content_block_stop
                None
            }
            IrStreamEvent::MessageDelta { stop_reason, usage } => {
                let finish_reason = stop_reason.map(|sr| sr.as_chat_finish_reason());

                let mut chunk = json!({
                    "id": self.msg_id,
                    "object": "chat.completion.chunk",
                    "created": self.created,
                    "model": self.model,
                    "choices": [{
                        "index": 0,
                        "delta": {},
                        "finish_reason": finish_reason
                    }]
                });

                // 添加 usage（如果有）
                if let Some(u) = usage {
                    let output_tokens = if u.output_tokens > 0 {
                        u.output_tokens
                    } else {
                        u.output_chars / 4
                    };
                    chunk["usage"] = json!({
                        "prompt_tokens": u.input_tokens + u.cache_read_input_tokens,
                        "completion_tokens": output_tokens,
                        "total_tokens": u.input_tokens + u.cache_read_input_tokens + output_tokens,
                        "prompt_tokens_details": {
                            "cached_tokens": u.cache_read_input_tokens
                        }
                    });
                }

                Some(mk(chunk))
            }
            IrStreamEvent::MessageStop => {
                // Chat 的流结束标记
                Some(Bytes::from("data: [DONE]\n\n"))
            }
        }
    }

    /// 流结束时渲染收尾（[DONE]）。
    ///
    /// Chat 的 finalize 很简单，因为 MessageDelta 已经包含了 finish_reason。
    pub fn finalize(&mut self, _usage: &IrUsage) -> Vec<Bytes> {
        // 如果还没有发送 [DONE]，现在发送
        vec![Bytes::from("data: [DONE]\n\n")]
    }
}

impl Default for ChatCompletionsRenderState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ir_req_to_chat_completions_basic() {
        let req = IrRequest {
            model: "gpt-4o".to_string(),
            system: Some(IrSystemContent::Text("Be helpful.".to_string())),
            messages: vec![IrMessage {
                role: IrRole::User,
                content: vec![IrContentBlock::Text {
                    text: "Hello".to_string(),
                    cache_control: None,
                }],
            }],
            tools: vec![],
            tool_choice: None,
            max_tokens: Some(4096),
            temperature: None,
            top_p: None,
            thinking: None,
            stream: true,
        };
        let v = ir_req_to_chat_completions(&req);
        assert_eq!(v["model"], "gpt-4o");
        assert_eq!(v["messages"][0]["role"], "system");
        assert_eq!(v["messages"][0]["content"], "Be helpful.");
        assert_eq!(v["messages"][1]["role"], "user");
        assert_eq!(v["messages"][1]["content"], "Hello");
        assert_eq!(v["max_tokens"], 4096);
        assert_eq!(v["stream"], true);
    }

    #[test]
    fn test_ir_req_to_chat_completions_with_tools() {
        let req = IrRequest {
            model: "gpt-4o".to_string(),
            system: None,
            messages: vec![],
            tools: vec![IrTool {
                name: "search".to_string(),
                description: Some("Search the web".to_string()),
                input_schema: json!({"type": "object", "properties": {"q": {"type": "string"}}}),
            }],
            tool_choice: Some(IrToolChoice::Any),
            max_tokens: Some(1024),
            temperature: Some(0.7),
            top_p: None,
            thinking: Some(IrThinkingConfig {
                enabled: true,
                budget_tokens: Some(16384),
            }),
            stream: true,
        };
        let v = ir_req_to_chat_completions(&req);
        assert_eq!(v["tools"][0]["type"], "function");
        assert_eq!(v["tools"][0]["function"]["name"], "search");
        assert_eq!(v["tools"][0]["function"]["description"], "Search the web");
        assert_eq!(v["tool_choice"], "required");
        assert_eq!(v["reasoning_effort"], "high");
        assert_eq!(v["temperature"], 0.7);
    }

    #[test]
    fn test_ir_req_to_chat_completions_tool_use_and_result() {
        let req = IrRequest {
            model: "gpt-4o".to_string(),
            system: None,
            messages: vec![
                IrMessage {
                    role: IrRole::Assistant,
                    content: vec![IrContentBlock::ToolUse {
                        id: "call_1".to_string(),
                        name: "search".to_string(),
                        input: json!({"q": "test"}),
                    }],
                },
                IrMessage {
                    role: IrRole::User,
                    content: vec![IrContentBlock::ToolResult {
                        tool_use_id: "call_1".to_string(),
                        content: IrToolResultContent::Text("result text".to_string()),
                        is_error: false,
                    }],
                },
            ],
            tools: vec![],
            tool_choice: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            thinking: None,
            stream: false,
        };
        let v = ir_req_to_chat_completions(&req);
        // Assistant message with tool_calls
        assert_eq!(v["messages"][0]["role"], "assistant");
        assert_eq!(v["messages"][0]["tool_calls"][0]["id"], "call_1");
        assert_eq!(v["messages"][0]["tool_calls"][0]["function"]["name"], "search");
        // Tool result message
        assert_eq!(v["messages"][1]["role"], "tool");
        assert_eq!(v["messages"][1]["tool_call_id"], "call_1");
        assert_eq!(v["messages"][1]["content"], "result text");
    }

    #[test]
    fn test_render_event_text_delta() {
        let mut state = ChatCompletionsRenderState::new();
        state.msg_id = "chatcmpl-1".to_string();
        state.model = "gpt-4o".to_string();

        let ev = IrStreamEvent::ContentBlockDelta {
            index: 0,
            delta: IrContentDelta::TextDelta("Hello".to_string()),
        };
        let bytes = state.render_event(&ev).unwrap();
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("data: "));
        assert!(s.contains("\"content\":\"Hello\""));
        assert!(s.contains("\"role\":\"assistant\"")); // 第一个 chunk 包含 role
    }

    #[test]
    fn test_render_event_reasoning_delta() {
        let mut state = ChatCompletionsRenderState::new();
        state.msg_id = "chatcmpl-1".to_string();
        state.model = "qwen-max".to_string();

        let ev = IrStreamEvent::ContentBlockDelta {
            index: 0,
            delta: IrContentDelta::ThinkingDelta("thinking...".to_string()),
        };
        let bytes = state.render_event(&ev).unwrap();
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("\"reasoning_content\":\"thinking...\""));
    }

    #[test]
    fn test_render_event_message_delta_with_finish() {
        let mut state = ChatCompletionsRenderState::new();
        state.msg_id = "chatcmpl-1".to_string();
        state.model = "gpt-4o".to_string();

        let ev = IrStreamEvent::MessageDelta {
            stop_reason: Some(IrStopReason::EndTurn),
            usage: Some(IrUsage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_input_tokens: 80,
                ..Default::default()
            }),
        };
        let bytes = state.render_event(&ev).unwrap();
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("\"finish_reason\":\"stop\""));
        assert!(s.contains("\"prompt_tokens\":180")); // 100 + 80
        assert!(s.contains("\"completion_tokens\":50"));
        assert!(s.contains("\"cached_tokens\":80"));
    }

    #[test]
    fn test_render_event_message_stop() {
        let mut state = ChatCompletionsRenderState::new();
        let ev = IrStreamEvent::MessageStop;
        let bytes = state.render_event(&ev).unwrap();
        let s = String::from_utf8_lossy(&bytes);
        assert_eq!(s.trim(), "data: [DONE]");
    }

    #[test]
    fn test_render_event_tool_use_start() {
        let mut state = ChatCompletionsRenderState::new();
        state.msg_id = "chatcmpl-1".to_string();
        state.model = "gpt-4o".to_string();
        state.tool_count = 1;

        let ev = IrStreamEvent::ContentBlockStart {
            index: 0,
            block: IrContentBlockStart::ToolUse {
                id: "call_1".to_string(),
                name: "search".to_string(),
            },
        };
        let bytes = state.render_event(&ev).unwrap();
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("\"tool_calls\""));
        assert!(s.contains("\"id\":\"call_1\""));
        assert!(s.contains("\"name\":\"search\""));
    }
}
