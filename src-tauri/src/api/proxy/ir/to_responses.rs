//! IR → OpenAI Responses API 方向的格式翻译：请求体与流式事件渲染。
//!
//! Responses API 的 SSE 事件序列：
//! - `response.created` → 响应开始
//! - `response.output_item.added` → 新的输出项
//! - `response.content_part.added` → 内容部分开始
//! - `response.output_text.delta` → 文本增量
//! - `response.reasoning.delta` → 推理增量
//! - `response.function_call_arguments.delta` → 函数参数增量
//! - `response.content_part.done` → 内容部分结束
//! - `response.output_item.done` → 输出项结束
//! - `response.completed` → 响应完成

use bytes::Bytes;
use serde_json::{json, Value};

use super::types::*;

/// 将 IR 请求体序列化为 OpenAI Responses 格式。
pub fn ir_req_to_responses(req: &IrRequest) -> Value {
    let mut out = json!({
        "model": req.model,
        "stream": req.stream,
    });

    // 构建 input 数组
    let mut input: Vec<Value> = vec![];

    // System prompt → system message
    if let Some(ref system) = req.system {
        let content = match system {
            IrSystemContent::Text(t) => {
                json!([{"type": "input_text", "text": t}])
            }
            IrSystemContent::Blocks(blocks) => {
                let parts: Vec<Value> = blocks
                    .iter()
                    .map(|b| json!({"type": "input_text", "text": b.text}))
                    .collect();
                json!(parts)
            }
        };
        input.push(json!({
            "type": "message",
            "role": "system",
            "content": content
        }));
    }

    // Messages
    for msg in &req.messages {
        let role = match msg.role {
            IrRole::User => "user",
            IrRole::Assistant => "assistant",
        };

        let mut text_parts: Vec<Value> = vec![];
        let mut function_calls: Vec<Value> = vec![];
        let mut function_outputs: Vec<Value> = vec![];

        for block in &msg.content {
            match block {
                IrContentBlock::Text { text, .. } => {
                    text_parts.push(json!({"type": "input_text", "text": text}));
                }
                IrContentBlock::Image { source } => match source {
                    IrImageSource::Url { url } => {
                        text_parts.push(json!({"type": "input_image", "image_url": url}));
                    }
                    IrImageSource::Base64 { media_type, data } => {
                        let url = format!("data:{};base64,{}", media_type, data);
                        text_parts.push(json!({"type": "input_image", "image_url": url}));
                    }
                },
                IrContentBlock::Thinking { thinking, .. } => {
                    text_parts.push(json!({"type": "reasoning", "text": thinking}));
                }
                IrContentBlock::ToolUse { id, name, input: args } => {
                    function_calls.push(json!({
                        "type": "function_call",
                        "call_id": id,
                        "name": name,
                        "arguments": serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string())
                    }));
                }
                IrContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => {
                    let output = match content {
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
                    function_outputs.push(json!({
                        "type": "function_call_output",
                        "call_id": tool_use_id,
                        "output": output
                    }));
                }
            }
        }

        // 添加 message（如果有文本/图像内容）
        if !text_parts.is_empty() {
            input.push(json!({
                "type": "message",
                "role": role,
                "content": text_parts
            }));
        }

        // 添加 function_call items
        for fc in function_calls {
            input.push(fc);
        }

        // 添加 function_call_output items
        for fo in function_outputs {
            input.push(fo);
        }
    }

    out["input"] = json!(input);

    // Instructions (system prompt 的另一种形式)
    // 已经在 input 中作为 system message 处理

    // Tools
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut obj = json!({
                    "type": "function",
                    "name": t.name,
                    "parameters": t.input_schema,
                });
                if let Some(ref desc) = t.description {
                    obj["description"] = json!(desc);
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
            IrToolChoice::Tool { name } => json!({"type": "function", "name": name}),
        };
    }

    // Thinking config → reasoning
    if let Some(ref thinking) = req.thinking {
        if thinking.enabled {
            let effort = match thinking.budget_tokens {
                Some(b) if b <= 2048 => "low",
                Some(b) if b <= 8192 => "medium",
                _ => "high",
            };
            out["reasoning"] = json!({"effort": effort});
        }
    }

    // Pass through scalar params
    if let Some(max_tokens) = req.max_tokens {
        out["max_output_tokens"] = json!(max_tokens);
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

/// Responses SSE 渲染状态机。
///
/// Responses API 的事件序列比 Chat Completions 更细粒度：
/// 每个 content block 都有 added → delta → done 的完整生命周期。
pub struct ResponsesRenderState {
    response_id: String,
    model: String,
    created: i64,
    /// 当前输出项的 index
    output_index: usize,
    /// 当前内容部分的 index
    content_part_index: usize,
    /// 是否已发送 response.created
    response_created: bool,
    /// 当前打开的 block 类型（Text / Thinking / ToolUse）
    current_block: Option<ResponsesBlockKind>,
    /// 当前 block 累积的文本（text / thinking / tool arguments）
    current_text: String,
    /// 当前 block 的 tool_use id/name（仅 ToolUse 时有效）
    current_tool_id: String,
    current_tool_name: String,
}

#[derive(Clone, Copy, PartialEq)]
enum ResponsesBlockKind {
    Text,
    Thinking,
    ToolUse,
}

impl ResponsesRenderState {
    pub fn new() -> Self {
        Self {
            response_id: String::new(),
            model: String::new(),
            created: chrono::Utc::now().timestamp(),
            output_index: 0,
            content_part_index: 0,
            response_created: false,
            current_block: None,
            current_text: String::new(),
            current_tool_id: String::new(),
            current_tool_name: String::new(),
        }
    }

    /// 将 IR 流式事件渲染为 Responses SSE 字节段。
    pub fn render_event(&mut self, ev: &IrStreamEvent) -> Option<Bytes> {
        let mk = |event_type: &str, payload: Value| -> Bytes {
            let data = serde_json::to_string(&payload).unwrap_or_default();
            Bytes::from(format!("event: {}\ndata: {}\n\n", event_type, data))
        };

        match ev {
            IrStreamEvent::MessageStart { id, model } => {
                self.response_id = id.clone();
                self.model = model.clone();
                self.response_created = true;

                Some(mk(
                    "response.created",
                    json!({
                        "type": "response.created",
                        "response": {
                            "id": id,
                            "object": "response",
                            "created_at": self.created,
                            "model": model,
                            "status": "in_progress",
                            "output": [],
                            "usage": {
                                "input_tokens": 0,
                                "output_tokens": 0,
                                "total_tokens": 0
                            }
                        }
                    }),
                ))
            }
            IrStreamEvent::ContentBlockStart { index, block } => {
                // 如果上一个 block 还开着，先关闭（字节暂存，与新增事件合并）
                let mut prelude: Vec<u8> = Vec::new();
                if self.current_block.is_some() {
                    if let Some(b) = self.close_block() {
                        prelude.extend_from_slice(&b);
                    }
                }

                match block {
                    IrContentBlockStart::Text => {
                        self.output_index = *index;
                        self.content_part_index = 0;
                        self.current_block = Some(ResponsesBlockKind::Text);
                        self.current_text = String::new();
                        let item_id = format!("item_{}", uuid::Uuid::new_v4().simple());

                        // output_item.added (message type)
                        let item_added = mk(
                            "response.output_item.added",
                            json!({
                                "type": "response.output_item.added",
                                "output_index": self.output_index,
                                "item": {
                                    "id": item_id,
                                    "type": "message",
                                    "status": "in_progress",
                                    "role": "assistant",
                                    "content": []
                                }
                            }),
                        );

                        // content_part.added
                        let part_added = mk(
                            "response.content_part.added",
                            json!({
                                "type": "response.content_part.added",
                                "output_index": self.output_index,
                                "content_index": self.content_part_index,
                                "part": {
                                    "type": "output_text",
                                    "text": "",
                                    "annotations": []
                                }
                            }),
                        );

                        // 合并 prelude + 两个事件
                        prelude.extend_from_slice(&item_added);
                        prelude.extend_from_slice(&part_added);
                        Some(Bytes::from(prelude))
                    }
                    IrContentBlockStart::Thinking => {
                        self.output_index = *index;
                        self.current_block = Some(ResponsesBlockKind::Thinking);
                        self.current_text = String::new();
                        let item_id = format!("item_{}", uuid::Uuid::new_v4().simple());

                        let added = mk(
                            "response.output_item.added",
                            json!({
                                "type": "response.output_item.added",
                                "output_index": self.output_index,
                                "item": {
                                    "id": item_id,
                                    "type": "reasoning",
                                    "status": "in_progress",
                                    "summary": []
                                }
                            }),
                        );
                        prelude.extend_from_slice(&added);
                        Some(Bytes::from(prelude))
                    }
                    IrContentBlockStart::ToolUse { id, name } => {
                        self.output_index = *index;
                        self.current_block = Some(ResponsesBlockKind::ToolUse);
                        self.current_text = String::new();
                        self.current_tool_id = id.clone();
                        self.current_tool_name = name.clone();

                        let added = mk(
                            "response.output_item.added",
                            json!({
                                "type": "response.output_item.added",
                                "output_index": self.output_index,
                                "item": {
                                    "type": "function_call",
                                    "status": "in_progress",
                                    "call_id": id,
                                    "name": name,
                                    "arguments": ""
                                }
                            }),
                        );
                        prelude.extend_from_slice(&added);
                        Some(Bytes::from(prelude))
                    }
                }
            }
            IrStreamEvent::ContentBlockDelta { index, delta } => {
                // 累积文本到当前 block
                match delta {
                    IrContentDelta::TextDelta(text) => {
                        self.current_text.push_str(text);
                        Some(mk(
                            "response.output_text.delta",
                            json!({
                                "type": "response.output_text.delta",
                                "output_index": *index,
                                "content_index": self.content_part_index,
                                "delta": text
                            }),
                        ))
                    }
                    IrContentDelta::ThinkingDelta(thinking) => {
                        self.current_text.push_str(thinking);
                        Some(mk(
                            "response.reasoning.delta",
                            json!({
                                "type": "response.reasoning.delta",
                                "output_index": *index,
                                "delta": thinking
                            }),
                        ))
                    }
                    IrContentDelta::InputJsonDelta(partial) => {
                        self.current_text.push_str(partial);
                        Some(mk(
                            "response.function_call_arguments.delta",
                            json!({
                                "type": "response.function_call_arguments.delta",
                                "output_index": *index,
                                "delta": partial
                            }),
                        ))
                    }
                }
            }
            IrStreamEvent::ContentBlockStop { index } => {
                // 发送完整的事件序列：xxx.done + output_item.done（携带完整 item）
                let output_index = *index;
                match self.current_block {
                    Some(ResponsesBlockKind::Text) => {
                        let text = self.current_text.clone();
                        let part_done = mk(
                            "response.content_part.done",
                            json!({
                                "type": "response.content_part.done",
                                "output_index": output_index,
                                "content_index": self.content_part_index,
                                "part": {
                                    "type": "output_text",
                                    "text": text,
                                    "annotations": []
                                }
                            }),
                        );
                        let item_done = mk(
                            "response.output_item.done",
                            json!({
                                "type": "response.output_item.done",
                                "output_index": output_index,
                                "item": {
                                    "type": "message",
                                    "status": "completed",
                                    "role": "assistant",
                                    "content": [{
                                        "type": "output_text",
                                        "text": text,
                                        "annotations": []
                                    }]
                                }
                            }),
                        );
                        self.current_block = None;
                        self.current_text.clear();
                        let joined = part_done.iter().chain(item_done.iter()).copied().collect::<Vec<u8>>();
                        Some(Bytes::from(joined))
                    }
                    Some(ResponsesBlockKind::Thinking) => {
                        let reasoning_done = mk(
                            "response.reasoning.done",
                            json!({
                                "type": "response.reasoning.done",
                                "output_index": output_index,
                                "reasoning": {
                                    "summary": []
                                }
                            }),
                        );
                        let item_done = mk(
                            "response.output_item.done",
                            json!({
                                "type": "response.output_item.done",
                                "output_index": output_index,
                                "item": {
                                    "type": "reasoning",
                                    "status": "completed",
                                    "summary": []
                                }
                            }),
                        );
                        self.current_block = None;
                        self.current_text.clear();
                        let joined = reasoning_done.iter().chain(item_done.iter()).copied().collect::<Vec<u8>>();
                        Some(Bytes::from(joined))
                    }
                    Some(ResponsesBlockKind::ToolUse) => {
                        let args = self.current_text.clone();
                        let args_done = mk(
                            "response.function_call_arguments.done",
                            json!({
                                "type": "response.function_call_arguments.done",
                                "output_index": output_index,
                                "arguments": args
                            }),
                        );
                        let item_done = mk(
                            "response.output_item.done",
                            json!({
                                "type": "response.output_item.done",
                                "output_index": output_index,
                                "item": {
                                    "type": "function_call",
                                    "status": "completed",
                                    "call_id": self.current_tool_id,
                                    "name": self.current_tool_name,
                                    "arguments": args
                                }
                            }),
                        );
                        self.current_block = None;
                        self.current_text.clear();
                        let joined = args_done.iter().chain(item_done.iter()).copied().collect::<Vec<u8>>();
                        Some(Bytes::from(joined))
                    }
                    None => {
                        // 没有打开的 block，直接发 output_item.done
                        let done = mk(
                            "response.output_item.done",
                            json!({
                                "type": "response.output_item.done",
                                "output_index": output_index,
                                "item": {}
                            }),
                        );
                        self.current_block = None;
                        Some(done)
                    }
                }
            }
            IrStreamEvent::MessageDelta {
                stop_reason,
                usage,
            } => {
                // 延迟到 finalize
                None
            }
            IrStreamEvent::MessageStop => {
                // 延迟到 finalize
                None
            }
        }
    }

    /// 关闭当前打开的 block（新 block 开始前的清理）。
    /// 发出 xxx.done + output_item.done（携带完整 item）。
    fn close_block(&mut self) -> Option<Bytes> {
        let mk = |event_type: &str, payload: Value| -> Bytes {
            let data = serde_json::to_string(&payload).unwrap_or_default();
            Bytes::from(format!("event: {}\ndata: {}\n\n", event_type, data))
        };

        let output_index = self.output_index;
        let mut combined = Vec::new();
        match self.current_block {
            Some(ResponsesBlockKind::Text) => {
                combined.push(mk(
                    "response.content_part.done",
                    json!({
                        "type": "response.content_part.done",
                        "output_index": output_index,
                        "content_index": self.content_part_index,
                        "part": {
                            "type": "output_text",
                            "text": self.current_text
                        }
                    }),
                ));
                combined.push(mk(
                    "response.output_item.done",
                    json!({
                        "type": "response.output_item.done",
                        "output_index": output_index,
                        "item": {
                            "type": "message",
                            "role": "assistant",
                            "content": [{
                                "type": "output_text",
                                "text": self.current_text,
                                "annotations": []
                            }]
                        }
                    }),
                ));
            }
            Some(ResponsesBlockKind::Thinking) => {
                combined.push(mk(
                    "response.reasoning.done",
                    json!({
                        "type": "response.reasoning.done",
                        "output_index": output_index,
                        "reasoning": {
                            "summary": []
                        }
                    }),
                ));
                combined.push(mk(
                    "response.output_item.done",
                    json!({
                        "type": "response.output_item.done",
                        "output_index": output_index,
                        "item": {
                            "type": "reasoning",
                            "summary": []
                        }
                    }),
                ));
            }
            Some(ResponsesBlockKind::ToolUse) => {
                combined.push(mk(
                    "response.function_call_arguments.done",
                    json!({
                        "type": "response.function_call_arguments.done",
                        "output_index": output_index,
                        "arguments": self.current_text
                    }),
                ));
                combined.push(mk(
                    "response.output_item.done",
                    json!({
                        "type": "response.output_item.done",
                        "output_index": output_index,
                        "item": {
                            "type": "function_call",
                            "call_id": self.current_tool_id,
                            "name": self.current_tool_name,
                            "arguments": self.current_text
                        }
                    }),
                ));
            }
            None => return None,
        }

        self.current_block = None;
        self.current_text.clear();
        // 拼接所有事件字节为一个 Bytes
        let joined = combined.iter().flat_map(|b| b.iter().copied()).collect::<Vec<u8>>();
        Some(Bytes::from(joined))
    }

    /// 流结束时渲染收尾事件（response.completed）。
    pub fn finalize(&mut self, usage: &IrUsage) -> Vec<Bytes> {
        let mut events = vec![];
        let mk = |event_type: &str, payload: Value| -> Bytes {
            let data = serde_json::to_string(&payload).unwrap_or_default();
            Bytes::from(format!("event: {}\ndata: {}\n\n", event_type, data))
        };

        // 关闭任何未完成的 block
        if self.current_block.is_some() {
            if let Some(b) = self.close_block() {
                events.push(b);
            }
        }

        let output_tokens = if usage.output_tokens > 0 {
            usage.output_tokens
        } else {
            usage.output_chars / 4
        };

        events.push(mk(
            "response.completed",
            json!({
                "type": "response.completed",
                "response": {
                    "id": self.response_id,
                    "object": "response",
                    "created_at": self.created,
                    "model": self.model,
                    "status": "completed",
                    "output": [],
                    "usage": {
                        "input_tokens": usage.input_tokens + usage.cache_read_input_tokens,
                        "output_tokens": output_tokens,
                        "total_tokens": usage.input_tokens + usage.cache_read_input_tokens + output_tokens,
                        "input_tokens_details": {
                            "cached_tokens": usage.cache_read_input_tokens
                        }
                    }
                }
            }),
        ));

        events
    }
}

impl Default for ResponsesRenderState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ir_req_to_responses_basic() {
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
        let v = ir_req_to_responses(&req);
        assert_eq!(v["model"], "gpt-4o");
        assert_eq!(v["stream"], true);
        assert_eq!(v["max_output_tokens"], 4096);
        // input 应该包含 system 和 user 消息
        let input = v["input"].as_array().unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "system");
        assert_eq!(input[1]["type"], "message");
        assert_eq!(input[1]["role"], "user");
    }

    #[test]
    fn test_ir_req_to_responses_with_tools() {
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
            max_tokens: None,
            temperature: Some(0.7),
            top_p: None,
            thinking: Some(IrThinkingConfig {
                enabled: true,
                budget_tokens: Some(16384),
            }),
            stream: true,
        };
        let v = ir_req_to_responses(&req);
        assert_eq!(v["tools"][0]["type"], "function");
        assert_eq!(v["tools"][0]["name"], "search");
        assert_eq!(v["tools"][0]["description"], "Search the web");
        assert_eq!(v["tool_choice"], "required");
        assert_eq!(v["reasoning"]["effort"], "high");
        assert_eq!(v["temperature"], 0.7);
    }

    #[test]
    fn test_ir_req_to_responses_tool_use_and_result() {
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
        let v = ir_req_to_responses(&req);
        let input = v["input"].as_array().unwrap();
        // function_call item
        assert_eq!(input[0]["type"], "function_call");
        assert_eq!(input[0]["call_id"], "call_1");
        assert_eq!(input[0]["name"], "search");
        // function_call_output item
        assert_eq!(input[1]["type"], "function_call_output");
        assert_eq!(input[1]["call_id"], "call_1");
        assert_eq!(input[1]["output"], "result text");
    }

    #[test]
    fn test_render_event_message_start() {
        let mut state = ResponsesRenderState::new();
        let ev = IrStreamEvent::MessageStart {
            id: "resp_123".to_string(),
            model: "gpt-4o".to_string(),
        };
        let bytes = state.render_event(&ev).unwrap();
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("event: response.created"));
        assert!(s.contains("\"id\":\"resp_123\""));
        assert!(s.contains("\"model\":\"gpt-4o\""));
    }

    #[test]
    fn test_render_event_text_start() {
        let mut state = ResponsesRenderState::new();
        state.response_id = "resp_1".to_string();
        state.model = "gpt-4o".to_string();

        let ev = IrStreamEvent::ContentBlockStart {
            index: 0,
            block: IrContentBlockStart::Text,
        };
        let bytes = state.render_event(&ev).unwrap();
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("response.output_item.added"));
        assert!(s.contains("response.content_part.added"));
        assert!(s.contains("\"type\":\"message\""));
    }

    #[test]
    fn test_render_event_text_delta() {
        let mut state = ResponsesRenderState::new();
        state.response_id = "resp_1".to_string();
        state.model = "gpt-4o".to_string();

        let ev = IrStreamEvent::ContentBlockDelta {
            index: 0,
            delta: IrContentDelta::TextDelta("Hello".to_string()),
        };
        let bytes = state.render_event(&ev).unwrap();
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("event: response.output_text.delta"));
        assert!(s.contains("\"delta\":\"Hello\""));
    }

    #[test]
    fn test_render_event_thinking_delta() {
        let mut state = ResponsesRenderState::new();
        state.response_id = "resp_1".to_string();
        state.model = "o1".to_string();

        let ev = IrStreamEvent::ContentBlockDelta {
            index: 0,
            delta: IrContentDelta::ThinkingDelta("thinking...".to_string()),
        };
        let bytes = state.render_event(&ev).unwrap();
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("event: response.reasoning.delta"));
        assert!(s.contains("\"delta\":\"thinking...\""));
    }

    #[test]
    fn test_render_event_tool_use_start() {
        let mut state = ResponsesRenderState::new();
        state.response_id = "resp_1".to_string();
        state.model = "gpt-4o".to_string();

        let ev = IrStreamEvent::ContentBlockStart {
            index: 0,
            block: IrContentBlockStart::ToolUse {
                id: "call_1".to_string(),
                name: "search".to_string(),
            },
        };
        let bytes = state.render_event(&ev).unwrap();
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("event: response.output_item.added"));
        assert!(s.contains("\"type\":\"function_call\""));
        assert!(s.contains("\"call_id\":\"call_1\""));
        assert!(s.contains("\"name\":\"search\""));
    }

    #[test]
    fn test_render_event_input_json_delta() {
        let mut state = ResponsesRenderState::new();
        state.response_id = "resp_1".to_string();
        state.model = "gpt-4o".to_string();

        let ev = IrStreamEvent::ContentBlockDelta {
            index: 0,
            delta: IrContentDelta::InputJsonDelta("{\"q\":".to_string()),
        };
        let bytes = state.render_event(&ev).unwrap();
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("event: response.function_call_arguments.delta"));
        assert!(s.contains("\"delta\":\"{\\\"q\\\":\""));
    }

    #[test]
    fn test_finalize_completed() {
        let mut state = ResponsesRenderState::new();
        state.response_id = "resp_1".to_string();
        state.model = "gpt-4o".to_string();

        let usage = IrUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_input_tokens: 80,
            ..Default::default()
        };
        let events = state.finalize(&usage);
        assert_eq!(events.len(), 1);
        let s = String::from_utf8_lossy(&events[0]);
        assert!(s.contains("event: response.completed"));
        assert!(s.contains("\"status\":\"completed\""));
        assert!(s.contains("\"input_tokens\":180")); // 100 + 80
        assert!(s.contains("\"output_tokens\":50"));
        assert!(s.contains("\"cached_tokens\":80"));
    }

    #[test]
    fn test_finalize_fallback_chars_to_tokens() {
        let mut state = ResponsesRenderState::new();
        state.response_id = "resp_1".to_string();
        state.model = "gpt-4o".to_string();

        let usage = IrUsage {
            output_tokens: 0,
            output_chars: 120,
            ..Default::default()
        };
        let events = state.finalize(&usage);
        let s = String::from_utf8_lossy(&events[0]);
        assert!(s.contains("\"output_tokens\":30")); // 120 / 4
    }
}
