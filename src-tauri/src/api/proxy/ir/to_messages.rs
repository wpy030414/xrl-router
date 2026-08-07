//! IR → Anthropic Messages 方向的格式翻译：请求体与流式事件渲染。
//!
//! IR 以 Anthropic SSE 为骨架，所以渲染器相对简单——
//! 主要工作是把强类型 IR 序列化为 `Value` / SSE 字节。

use bytes::Bytes;
use serde_json::{json, Value};

use super::types::*;

/// 将 IR 请求体序列化为 Anthropic Messages 格式。
pub fn ir_req_to_messages(req: &IrRequest) -> Value {
    let mut out = json!({
        "model": req.model,
        "stream": req.stream,
    });

    // System prompt
    if let Some(ref system) = req.system {
        out["system"] = match system {
            IrSystemContent::Text(t) => json!(t),
            IrSystemContent::Blocks(blocks) => {
                let arr: Vec<Value> = blocks
                    .iter()
                    .map(|b| {
                        let mut obj = json!({"type": "text", "text": b.text});
                        if let Some(ref cc) = b.cache_control {
                            obj["cache_control"] = cc.clone();
                        }
                        obj
                    })
                    .collect();
                json!(arr)
            }
        };
    }

    // Messages
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|msg| {
            let role = match msg.role {
                IrRole::User => "user",
                IrRole::Assistant => "assistant",
            };
            let content = render_anthropic_content(&msg.content);
            json!({"role": role, "content": content})
        })
        .collect();
    out["messages"] = json!(messages);

    // Tools
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut obj = json!({
                    "name": t.name,
                    "input_schema": t.input_schema,
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
            IrToolChoice::Auto => json!({"type": "auto"}),
            IrToolChoice::Any => json!({"type": "any"}),
            IrToolChoice::None => json!({"type": "none"}),
            IrToolChoice::Tool { name } => json!({"type": "tool", "name": name}),
        };
    }

    // Thinking config
    if let Some(ref thinking) = req.thinking {
        if thinking.enabled {
            let mut t = json!({"type": "enabled"});
            if let Some(budget) = thinking.budget_tokens {
                t["budget_tokens"] = json!(budget);
            }
            out["thinking"] = t;
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

/// 将 IR content blocks 渲染为 Anthropic content 数组。
fn render_anthropic_content(blocks: &[IrContentBlock]) -> Value {
    let arr: Vec<Value> = blocks
        .iter()
        .filter_map(|block| match block {
            IrContentBlock::Text { text, cache_control } => {
                let mut obj = json!({"type": "text", "text": text});
                if let Some(cc) = cache_control {
                    obj["cache_control"] = cc.clone();
                }
                Some(obj)
            }
            IrContentBlock::Image { source } => {
                let src = match source {
                    IrImageSource::Base64 { media_type, data } => {
                        json!({"type": "base64", "media_type": media_type, "data": data})
                    }
                    IrImageSource::Url { url } => json!({"type": "url", "url": url}),
                };
                Some(json!({"type": "image", "source": src}))
            }
            IrContentBlock::Thinking {
                thinking,
                signature,
            } => {
                let mut obj = json!({"type": "thinking", "thinking": thinking});
                if let Some(sig) = signature {
                    obj["signature"] = json!(sig);
                }
                Some(obj)
            }
            IrContentBlock::ToolUse { id, name, input } => {
                Some(json!({"type": "tool_use", "id": id, "name": name, "input": input}))
            }
            IrContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let c = match content {
                    IrToolResultContent::Text(t) => json!(t),
                    IrToolResultContent::Blocks(blocks) => render_anthropic_content(blocks),
                };
                let mut obj = json!({"type": "tool_result", "tool_use_id": tool_use_id, "content": c});
                if *is_error {
                    obj["is_error"] = json!(true);
                }
                Some(obj)
            }
        })
        .collect();
    json!(arr)
}

// ═══════════════════════════════════════════════════════════════════
// 流式事件渲染
// ═══════════════════════════════════════════════════════════════════

/// Messages SSE 渲染状态机。
///
/// IR 事件与 Anthropic SSE 几乎同构，状态主要用于：
/// - 记录 msg_id / model（message_start 时捕获，后续 chunk 复用）
/// - 追踪是否已发过 message_start（避免重复）
pub struct MessagesRenderState {
    msg_id: String,
    model: String,
    started: bool,
    /// 最后一个 MessageDelta 的 stop_reason（finalize 时使用）。
    last_stop_reason: Option<IrStopReason>,
    /// 最后一个 MessageDelta 的 usage（finalize 时使用）。
    last_usage: Option<IrUsage>,
}

impl MessagesRenderState {
    pub fn new() -> Self {
        Self {
            msg_id: String::new(),
            model: String::new(),
            started: false,
            last_stop_reason: None,
            last_usage: None,
        }
    }

    /// 将 IR 流式事件渲染为 Anthropic SSE 字节段。
    ///
    /// 返回 `None` 表示该事件不产生输出（如 MessageDelta 延迟到 finalize）。
    pub fn render_event(&mut self, ev: &IrStreamEvent) -> Option<Bytes> {
        let mk = |event_type: &str, payload: Value| -> Bytes {
            let data = serde_json::to_string(&payload).unwrap_or_default();
            Bytes::from(format!("event: {}\ndata: {}\n\n", event_type, data))
        };

        match ev {
            IrStreamEvent::MessageStart { id, model } => {
                self.msg_id = id.clone();
                self.model = model.clone();
                self.started = true;
                Some(mk(
                    "message_start",
                    json!({
                        "type": "message_start",
                        "message": {
                            "id": id,
                            "type": "message",
                            "role": "assistant",
                            "model": model,
                            "content": [],
                            "stop_reason": null,
                            "stop_sequence": null,
                            "usage": {"input_tokens": 0, "output_tokens": 0}
                        }
                    }),
                ))
            }
            IrStreamEvent::ContentBlockStart { index, block } => {
                let content_block = match block {
                    IrContentBlockStart::Text => json!({"type": "text", "text": ""}),
                    IrContentBlockStart::Thinking => {
                        json!({"type": "thinking", "thinking": ""})
                    }
                    IrContentBlockStart::ToolUse { id, name } => {
                        json!({"type": "tool_use", "id": id, "name": name, "input": {}})
                    }
                };
                Some(mk(
                    "content_block_start",
                    json!({"type": "content_block_start", "index": index, "content_block": content_block}),
                ))
            }
            IrStreamEvent::ContentBlockDelta { index, delta } => {
                let d = match delta {
                    IrContentDelta::TextDelta(text) => {
                        json!({"type": "text_delta", "text": text})
                    }
                    IrContentDelta::ThinkingDelta(thinking) => {
                        json!({"type": "thinking_delta", "thinking": thinking})
                    }
                    IrContentDelta::InputJsonDelta(partial) => {
                        json!({"type": "input_json_delta", "partial_json": partial})
                    }
                };
                Some(mk(
                    "content_block_delta",
                    json!({"type": "content_block_delta", "index": index, "delta": d}),
                ))
            }
            IrStreamEvent::ContentBlockStop { index } => Some(mk(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": index}),
            )),
            IrStreamEvent::MessageDelta { stop_reason, usage } => {
                // 暂存，延迟到 finalize 发出（等 usage 到齐）
                if let Some(sr) = stop_reason {
                    self.last_stop_reason = Some(*sr);
                }
                if let Some(u) = usage {
                    self.last_usage = Some(u.clone());
                }
                None
            }
            IrStreamEvent::MessageStop => {
                // MessageStop 也延迟到 finalize
                None
            }
        }
    }

    /// 流结束时渲染收尾事件（message_delta + message_stop）。
    ///
    /// 使用累积的 usage 确保 token 计数准确。
    pub fn finalize(&mut self, usage: &IrUsage) -> Vec<Bytes> {
        let mut events = Vec::new();
        let mk = |event_type: &str, payload: Value| -> Bytes {
            let data = serde_json::to_string(&payload).unwrap_or_default();
            Bytes::from(format!("event: {}\ndata: {}\n\n", event_type, data))
        };

        let stop_reason = self
            .last_stop_reason
            .map(|sr| sr.as_anthropic_str())
            .unwrap_or("end_turn");

        let output_tokens = if usage.output_tokens > 0 {
            usage.output_tokens
        } else {
            usage.output_chars / 4
        };

        events.push(mk(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                "usage": {
                    "output_tokens": output_tokens,
                    "cache_read_input_tokens": usage.cache_read_input_tokens,
                }
            }),
        ));
        events.push(mk("message_stop", json!({"type": "message_stop"})));
        events
    }
}

impl Default for MessagesRenderState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ir_req_to_messages_basic() {
        let req = IrRequest {
            model: "claude-opus-4-8".to_string(),
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
        let v = ir_req_to_messages(&req);
        assert_eq!(v["model"], "claude-opus-4-8");
        assert_eq!(v["system"], "Be helpful.");
        assert_eq!(v["max_tokens"], 4096);
        assert_eq!(v["stream"], true);
        assert_eq!(v["messages"][0]["role"], "user");
    }

    #[test]
    fn test_ir_req_to_messages_with_tools() {
        let req = IrRequest {
            model: "claude".to_string(),
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
                budget_tokens: Some(5000),
            }),
            stream: true,
        };
        let v = ir_req_to_messages(&req);
        assert_eq!(v["tools"][0]["name"], "search");
        assert_eq!(v["tools"][0]["description"], "Search the web");
        assert_eq!(v["tool_choice"]["type"], "any");
        assert_eq!(v["thinking"]["type"], "enabled");
        assert_eq!(v["thinking"]["budget_tokens"], 5000);
        assert_eq!(v["temperature"], 0.7);
    }

    #[test]
    fn test_ir_req_to_messages_image() {
        let req = IrRequest {
            model: "claude".to_string(),
            system: None,
            messages: vec![IrMessage {
                role: IrRole::User,
                content: vec![
                    IrContentBlock::Text {
                        text: "What is this?".to_string(),
                        cache_control: None,
                    },
                    IrContentBlock::Image {
                        source: IrImageSource::Base64 {
                            media_type: "image/png".to_string(),
                            data: "abc123".to_string(),
                        },
                    },
                ],
            }],
            tools: vec![],
            tool_choice: None,
            max_tokens: Some(1024),
            temperature: None,
            top_p: None,
            thinking: None,
            stream: false,
        };
        let v = ir_req_to_messages(&req);
        let content = &v["messages"][0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["data"], "abc123");
    }

    #[test]
    fn test_render_event_message_start() {
        let mut state = MessagesRenderState::new();
        let ev = IrStreamEvent::MessageStart {
            id: "msg_123".to_string(),
            model: "claude-opus-4-8".to_string(),
        };
        let bytes = state.render_event(&ev).unwrap();
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("event: message_start"));
        assert!(s.contains("\"id\":\"msg_123\""));
        assert!(s.contains("\"model\":\"claude-opus-4-8\""));
    }

    #[test]
    fn test_render_event_content_block_text() {
        let mut state = MessagesRenderState::new();
        let ev = IrStreamEvent::ContentBlockStart {
            index: 0,
            block: IrContentBlockStart::Text,
        };
        let bytes = state.render_event(&ev).unwrap();
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("event: content_block_start"));
        assert!(s.contains("\"type\":\"text\""));
    }

    #[test]
    fn test_render_event_text_delta() {
        let mut state = MessagesRenderState::new();
        let ev = IrStreamEvent::ContentBlockDelta {
            index: 0,
            delta: IrContentDelta::TextDelta("Hello".to_string()),
        };
        let bytes = state.render_event(&ev).unwrap();
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("event: content_block_delta"));
        assert!(s.contains("\"type\":\"text_delta\""));
        assert!(s.contains("\"text\":\"Hello\""));
    }

    #[test]
    fn test_render_event_message_delta_deferred() {
        let mut state = MessagesRenderState::new();
        let ev = IrStreamEvent::MessageDelta {
            stop_reason: Some(IrStopReason::EndTurn),
            usage: Some(IrUsage {
                output_tokens: 100,
                ..Default::default()
            }),
        };
        // MessageDelta 延迟到 finalize，render_event 返回 None
        assert!(state.render_event(&ev).is_none());
        assert_eq!(state.last_stop_reason, Some(IrStopReason::EndTurn));
    }

    #[test]
    fn test_finalize_emits_message_delta_and_stop() {
        let mut state = MessagesRenderState::new();
        state.last_stop_reason = Some(IrStopReason::ToolUse);

        let usage = IrUsage {
            output_tokens: 300,
            cache_read_input_tokens: 8000,
            ..Default::default()
        };
        let events = state.finalize(&usage);
        assert_eq!(events.len(), 2);

        let s0 = String::from_utf8_lossy(&events[0]);
        assert!(s0.contains("event: message_delta"));
        assert!(s0.contains("\"stop_reason\":\"tool_use\""));
        assert!(s0.contains("\"output_tokens\":300"));

        let s1 = String::from_utf8_lossy(&events[1]);
        assert!(s1.contains("event: message_stop"));
    }

    #[test]
    fn test_finalize_fallback_chars_to_tokens() {
        let mut state = MessagesRenderState::new();
        let usage = IrUsage {
            output_tokens: 0,
            output_chars: 120, // 120 / 4 = 30
            ..Default::default()
        };
        let events = state.finalize(&usage);
        let s = String::from_utf8_lossy(&events[0]);
        assert!(s.contains("\"output_tokens\":30"));
    }
}
