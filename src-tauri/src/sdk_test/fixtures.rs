//! SDK 合规验证 fixture 导出（本目录仅 test 构建；由 lib.rs 挂载）。
//!
//! 把真实 IR 转换代码的输入/输出导出为 JSON（target/ir_fixtures/），
//! 供 `sdk-test/ir_sdk_verify.py` 用官方 SDK（openai / anthropic）做 3×3 校验：
//! - req.json：IR → 三种客户端格式的请求体
//! - stream.json：IR 事件 → 三种客户端格式的 SSE 帧
//! - parse.json：三种上游格式的 chunk 序列 → IR 事件 + usage
//! - subagent.json：subagent 场景（thinking → text → tool → 后续 text）
//!
//! 本模块不复制任何转换逻辑，只搬运真实 ir:: 函数的输入输出。

use crate::api::proxy::ir::types::*;
use serde_json::{json, Value};

/// 覆盖最广的典型 IR 请求：多段 system + 图像 + thinking + 工具调用 + 工具结果 + 思考配置。
fn rich_ir_request() -> IrRequest {
    IrRequest {
        model: "claude-opus-4-8".to_string(),
        system: Some(IrSystemContent::Blocks(vec![
            IrSystemBlock { text: "You are a helpful assistant.".to_string(), cache_control: None },
            IrSystemBlock { text: "Second system part.".to_string(), cache_control: None },
        ])),
        messages: vec![
            IrMessage {
                role: IrRole::User,
                content: vec![
                    IrContentBlock::Text { text: "What's the weather in Tokyo?".to_string(), cache_control: None },
                    IrContentBlock::Image { source: IrImageSource::Url { url: "https://example.com/img.png".to_string() } },
                ],
            },
            IrMessage {
                role: IrRole::Assistant,
                content: vec![
                    IrContentBlock::Thinking { thinking: "Let me check the weather API.".to_string(), signature: Some("sig_abc".to_string()) },
                    IrContentBlock::ToolUse { id: "call_1".to_string(), name: "get_weather".to_string(), input: json!({"city": "Tokyo"}) },
                ],
            },
            IrMessage {
                role: IrRole::User,
                content: vec![
                    IrContentBlock::ToolResult {
                        tool_use_id: "call_1".to_string(),
                        content: IrToolResultContent::Text("24°C, sunny".to_string()),
                        is_error: false,
                    },
                ],
            },
        ],
        tools: vec![IrTool {
            name: "get_weather".to_string(),
            description: Some("Get weather for a city".to_string()),
            input_schema: json!({"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}),
        }],
        tool_choice: Some(IrToolChoice::Auto),
        max_tokens: Some(4096),
        temperature: Some(0.7),
        top_p: None,
        thinking: Some(IrThinkingConfig { enabled: true, budget_tokens: Some(5000) }),
        stream: true,
    }
}

/// 流式 IR 事件序列（thinking → text → tool 全生命周期），三条渲染路径共用。
fn rich_stream_events() -> Vec<IrStreamEvent> {
    vec![
        IrStreamEvent::MessageStart {
            id: "msg_richer".to_string(),
            model: "claude-opus-4-8".to_string(),
            usage: Some(IrUsage { input_tokens: 120, output_tokens: 0, cache_read_input_tokens: 8000, cache_creation_input_tokens: 50, output_chars: 0 }),
        },
        IrStreamEvent::ContentBlockStart { index: 0, block: IrContentBlockStart::Thinking { signature: Some("sig_123".to_string()) } },
        IrStreamEvent::ContentBlockDelta { index: 0, delta: IrContentDelta::ThinkingDelta("Let me reason about the weather.".to_string()) },
        IrStreamEvent::ContentBlockStop { index: 0 },
        IrStreamEvent::ContentBlockStart { index: 1, block: IrContentBlockStart::Text },
        IrStreamEvent::ContentBlockDelta { index: 1, delta: IrContentDelta::TextDelta("The weather in Tokyo is ".to_string()) },
        IrStreamEvent::ContentBlockDelta { index: 1, delta: IrContentDelta::TextDelta("24°C and sunny.".to_string()) },
        IrStreamEvent::ContentBlockStop { index: 1 },
        IrStreamEvent::ContentBlockStart { index: 2, block: IrContentBlockStart::ToolUse { id: "call_9".to_string(), name: "get_weather".to_string() } },
        IrStreamEvent::ContentBlockDelta { index: 2, delta: IrContentDelta::InputJsonDelta("{\"city\":".to_string()) },
        IrStreamEvent::ContentBlockDelta { index: 2, delta: IrContentDelta::InputJsonDelta("\"Tokyo\"}".to_string()) },
        IrStreamEvent::ContentBlockStop { index: 2 },
        IrStreamEvent::MessageDelta { stop_reason: Some(IrStopReason::ToolUse), usage: Some(IrUsage { input_tokens: 120, output_tokens: 40, cache_read_input_tokens: 8000, cache_creation_input_tokens: 50, output_chars: 46 }) },
        IrStreamEvent::MessageStop,
    ]
}

/// 把 SSE 字节拆成独立帧数组（Bytes 可能合并多个事件，需按 `\n\n` 拆开）。
fn sse_to_frames(bytes: &bytes::Bytes) -> Vec<Value> {
    let s = String::from_utf8_lossy(bytes);
    s.split("\n\n")
        .filter(|chunk| !chunk.trim().is_empty())
        .map(|chunk| {
            let mut event = String::new();
            let mut data = String::new();
            for line in chunk.lines() {
                if let Some(v) = line.strip_prefix("event: ") {
                    event = v.to_string();
                } else if let Some(v) = line.strip_prefix("data: ") {
                    data = v.to_string();
                }
            }
            json!({"event": event, "data": data})
        })
        .collect()
}

/// subagent 场景：thinking → text → tool_use → 工具后继续文本（多块生命周期）。
fn subagent_stream_events() -> Vec<IrStreamEvent> {
    vec![
        IrStreamEvent::MessageStart {
            id: "msg_subagent".to_string(),
            model: "claude-opus-4-8".to_string(),
            usage: Some(IrUsage { input_tokens: 90, output_tokens: 0, cache_read_input_tokens: 0, cache_creation_input_tokens: 0, output_chars: 0 }),
        },
        IrStreamEvent::ContentBlockStart { index: 0, block: IrContentBlockStart::Thinking { signature: None } },
        IrStreamEvent::ContentBlockDelta { index: 0, delta: IrContentDelta::ThinkingDelta("I need to search the codebase.".to_string()) },
        IrStreamEvent::ContentBlockStop { index: 0 },
        IrStreamEvent::ContentBlockStart { index: 1, block: IrContentBlockStart::Text },
        IrStreamEvent::ContentBlockDelta { index: 1, delta: IrContentDelta::TextDelta("Let me look at the " .to_string()) },
        IrStreamEvent::ContentBlockDelta { index: 1, delta: IrContentDelta::TextDelta("handler.".to_string()) },
        IrStreamEvent::ContentBlockStop { index: 1 },
        IrStreamEvent::ContentBlockStart { index: 2, block: IrContentBlockStart::ToolUse { id: "toolu_sub".to_string(), name: "Bash".to_string() } },
        IrStreamEvent::ContentBlockDelta { index: 2, delta: IrContentDelta::InputJsonDelta("{\"command\":".to_string()) },
        IrStreamEvent::ContentBlockDelta { index: 2, delta: IrContentDelta::InputJsonDelta("\"ls\"}}".to_string()) },
        IrStreamEvent::ContentBlockStop { index: 2 },
        IrStreamEvent::ContentBlockStart { index: 3, block: IrContentBlockStart::Text },
        IrStreamEvent::ContentBlockDelta { index: 3, delta: IrContentDelta::TextDelta("Found 3 files.".to_string()) },
        IrStreamEvent::ContentBlockStop { index: 3 },
        IrStreamEvent::MessageDelta { stop_reason: Some(IrStopReason::EndTurn), usage: Some(IrUsage { input_tokens: 90, output_tokens: 18, cache_read_input_tokens: 0, cache_creation_input_tokens: 0, output_chars: 60 }) },
        IrStreamEvent::MessageStop,
    ]
}

/// 构造已注入搜索结果的 IR（模拟 enrich_ir_with_search 的输出）。
/// 用于验证 enriched IR（system blocks 含搜索结果、tools 已清除）序列化后能被
/// 官方 SDK（anthropic / openai）正确消费。
fn websearch_enriched_ir() -> IrRequest {
    let search_text = "[1] Rust Programming Language\nhttps://www.rust-lang.org/\nA language empowering everyone to build reliable and efficient software.\n\n[2] The Rust Book\nhttps://doc.rust-lang.org/book/\nAn introductory book about Rust.";

    let search_block = IrSystemBlock {
        text: format!(
            "[Web Search Results for: what is rust]\n{}\n\nUse the above search results to answer the user's question. Cite sources using [N] notation.",
            search_text
        ),
        cache_control: None,
    };

    IrRequest {
        model: "claude-sonnet-4-20250514".to_string(),
        system: Some(IrSystemContent::Blocks(vec![
            IrSystemBlock { text: "You are a helpful assistant.".to_string(), cache_control: None },
            search_block,
        ])),
        messages: vec![IrMessage {
            role: IrRole::User,
            content: vec![IrContentBlock::Text {
                text: "what is rust".to_string(),
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
    }
}

fn usage_to_json(u: &IrUsage) -> Value {
    json!({
        "input_tokens": u.input_tokens,
        "output_tokens": u.output_tokens,
        "cache_read_input_tokens": u.cache_read_input_tokens,
        "cache_creation_input_tokens": u.cache_creation_input_tokens,
        "output_chars": u.output_chars,
    })
}

fn ir_events_to_json(events: &[IrStreamEvent]) -> Vec<Value> {
    events.iter().map(|ev| match ev {
        IrStreamEvent::MessageStart { id, model, usage } => json!({"type": "MessageStart", "id": id, "model": model, "usage": usage_to_json(usage.as_ref().unwrap_or(&IrUsage::default()))}),
        IrStreamEvent::ContentBlockStart { index, block } => json!({"type": "ContentBlockStart", "index": index, "block": match block {
            IrContentBlockStart::Text => json!({"kind": "Text"}),
            IrContentBlockStart::Thinking { signature } => json!({"kind": "Thinking", "signature": signature}),
            IrContentBlockStart::ToolUse { id, name } => json!({"kind": "ToolUse", "id": id, "name": name}),
        }}),
        IrStreamEvent::ContentBlockDelta { index, delta } => json!({"type": "ContentBlockDelta", "index": index, "delta": match delta {
            IrContentDelta::TextDelta(t) => json!({"kind": "Text", "text": t}),
            IrContentDelta::ThinkingDelta(t) => json!({"kind": "Thinking", "text": t}),
            IrContentDelta::InputJsonDelta(p) => json!({"kind": "InputJson", "partial": p}),
        }}),
        IrStreamEvent::ContentBlockStop { index } => json!({"type": "ContentBlockStop", "index": index}),
        IrStreamEvent::MessageDelta { stop_reason, usage } => json!({"type": "MessageDelta", "stop_reason": stop_reason.map(|s| s.as_anthropic_str()), "usage": usage_to_json(usage.as_ref().unwrap_or(&IrUsage::default()))}),
        IrStreamEvent::MessageStop => json!({"type": "MessageStop"}),
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 导出 fixture 文件（cargo test 时自动运行）。
    #[test]
    fn export_sdk_verify_fixtures() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/ir_fixtures");
        std::fs::create_dir_all(&dir).unwrap();

        // ── req.json：IR → 三种客户端格式请求体 ──
        let ir = rich_ir_request();
        let req = json!({
            "messages": crate::api::proxy::ir::to_messages::ir_req_to_messages(&ir),
            "chat_completions": crate::api::proxy::ir::to_chat_completions::ir_req_to_chat_completions(&ir),
            "responses": crate::api::proxy::ir::to_responses::ir_req_to_responses(&ir),
        });
        std::fs::write(dir.join("req.json"), serde_json::to_string_pretty(&req).unwrap()).unwrap();

        // ── stream.json：IR 事件 → 三种客户端格式 SSE 帧 ──
        let events = rich_stream_events();
        let final_usage = IrUsage { input_tokens: 120, output_tokens: 40, cache_read_input_tokens: 8000, cache_creation_input_tokens: 50, output_chars: 46 };

        let mut msg_render = crate::api::proxy::ir::to_messages::MessagesRenderState::new();
        let mut msg_frames: Vec<Value> = vec![];
        for ev in &events {
            if let Some(b) = msg_render.render_event(ev) {
                msg_frames.extend(sse_to_frames(&b));
            }
        }
        for b in msg_render.finalize(&final_usage) {
            msg_frames.extend(sse_to_frames(&b));
        }

        let mut chat_render = crate::api::proxy::ir::to_chat_completions::ChatCompletionsRenderState::new();
        let mut chat_frames: Vec<Value> = vec![];
        for ev in &events {
            if let Some(b) = chat_render.render_event(ev) {
                chat_frames.extend(sse_to_frames(&b));
            }
        }
        for b in chat_render.finalize(&final_usage) {
            chat_frames.extend(sse_to_frames(&b));
        }

        let mut resp_render = crate::api::proxy::ir::to_responses::ResponsesRenderState::new();
        let mut resp_frames: Vec<Value> = vec![];
        for ev in &events {
            if let Some(b) = resp_render.render_event(ev) {
                resp_frames.extend(sse_to_frames(&b));
            }
        }
        for b in resp_render.finalize(&final_usage) {
            resp_frames.extend(sse_to_frames(&b));
        }

        let stream = json!({
            "messages": msg_frames,
            "chat_completions": chat_frames,
            "responses": resp_frames,
        });
        std::fs::write(dir.join("stream.json"), serde_json::to_string_pretty(&stream).unwrap()).unwrap();

        // ── subagent.json：subagent 场景（thinking → text → tool → 后续 text） ──
        let events2 = subagent_stream_events();
        let u2 = IrUsage { input_tokens: 90, output_tokens: 18, cache_read_input_tokens: 0, cache_creation_input_tokens: 0, output_chars: 60 };

        let mut mr2 = crate::api::proxy::ir::to_messages::MessagesRenderState::new();
        let mut mf2: Vec<Value> = vec![];
        for ev in &events2 { if let Some(b) = mr2.render_event(ev) { mf2.extend(sse_to_frames(&b)); } }
        for b in mr2.finalize(&u2) { mf2.extend(sse_to_frames(&b)); }

        let mut cr2 = crate::api::proxy::ir::to_chat_completions::ChatCompletionsRenderState::new();
        let mut cf2: Vec<Value> = vec![];
        for ev in &events2 { if let Some(b) = cr2.render_event(ev) { cf2.extend(sse_to_frames(&b)); } }
        for b in cr2.finalize(&u2) { cf2.extend(sse_to_frames(&b)); }

        let mut rr2 = crate::api::proxy::ir::to_responses::ResponsesRenderState::new();
        let mut rf2: Vec<Value> = vec![];
        for ev in &events2 { if let Some(b) = rr2.render_event(ev) { rf2.extend(sse_to_frames(&b)); } }
        for b in rr2.finalize(&u2) { rf2.extend(sse_to_frames(&b)); }

        let subagent = json!({
            "messages": mf2,
            "chat_completions": cf2,
            "responses": rf2,
        });
        std::fs::write(dir.join("subagent.json"), serde_json::to_string_pretty(&subagent).unwrap()).unwrap();

        // ── websearch_req.json: enriched IR（已注入搜索结果）→ 三种客户端格式请求体 ──
        let ir_ws = websearch_enriched_ir();
        let websearch_req = json!({
            "messages": crate::api::proxy::ir::to_messages::ir_req_to_messages(&ir_ws),
            "chat_completions": crate::api::proxy::ir::to_chat_completions::ir_req_to_chat_completions(&ir_ws),
            "responses": crate::api::proxy::ir::to_responses::ir_req_to_responses(&ir_ws),
        });
        std::fs::write(dir.join("websearch_req.json"), serde_json::to_string_pretty(&websearch_req).unwrap()).unwrap();

        // ── parse.json：三种上游 chunk 序列 → IR 事件 + usage ──
        let anthropic_chunks = json!([
            {"type": "message_start", "message": {"id": "msg_up_1", "type": "message", "role": "assistant", "model": "claude-sonnet-4-5", "content": [], "stop_reason": null, "stop_sequence": null, "usage": {"input_tokens": 210, "cache_creation_input_tokens": 50, "cache_read_input_tokens": 8000, "output_tokens": 0}}},
            {"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": "sig_up"}},
            {"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "Thinking..."}},
            {"type": "content_block_stop", "index": 0},
            {"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}},
            {"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "Hello from upstream"}},
            {"type": "content_block_stop", "index": 1},
            {"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": null}, "usage": {"output_tokens": 30, "cache_read_input_tokens": 8000}},
            {"type": "message_stop"}
        ]);
        let chat_chunks = json!([
            {"id": "chatcmpl_up", "object": "chat.completion.chunk", "created": 1700000000, "model": "gpt-4o", "choices": [{"index": 0, "delta": {"role": "assistant", "content": ""}, "finish_reason": null}]},
            {"id": "chatcmpl_up", "object": "chat.completion.chunk", "created": 1700000000, "model": "gpt-4o", "choices": [{"index": 0, "delta": {"content": "Hello from "}, "finish_reason": null}]},
            {"id": "chatcmpl_up", "object": "chat.completion.chunk", "created": 1700000000, "model": "gpt-4o", "choices": [{"index": 0, "delta": {"content": "chat upstream"}, "finish_reason": null}]},
            {"id": "chatcmpl_up", "object": "chat.completion.chunk", "created": 1700000000, "model": "gpt-4o", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 900, "completion_tokens": 45, "total_tokens": 945, "prompt_tokens_details": {"cached_tokens": 700}}}
        ]);
        let responses_chunks = json!([
            {"type": "response.created", "response": {"id": "resp_up", "object": "response", "created_at": 1700000000, "model": "gpt-4o", "status": "in_progress", "output": []}},
            {"type": "response.output_item.added", "output_index": 0, "item": {"id": "item_1", "type": "reasoning", "status": "in_progress", "content": [], "summary": []}},
            {"type": "response.reasoning.delta", "item_id": "item_1", "output_index": 0, "delta": "Deep thinking"},
            {"type": "response.reasoning.done", "item_id": "item_1", "output_index": 0, "content": [{"type": "reasoning_text", "text": "Deep thinking"}]},
            {"type": "response.output_item.done", "output_index": 0, "item": {"id": "item_1", "type": "reasoning", "status": "completed", "content": [{"type": "reasoning_text", "text": "Deep thinking"}], "summary": []}},
            {"type": "response.output_item.added", "output_index": 1, "item": {"id": "item_2", "type": "message", "status": "in_progress", "role": "assistant", "content": []}},
            {"type": "response.content_part.added", "item_id": "item_2", "output_index": 1, "content_index": 0, "part": {"type": "output_text", "text": "", "annotations": []}},
            {"type": "response.output_text.delta", "item_id": "item_2", "output_index": 1, "content_index": 0, "delta": "Hello from responses"},
            {"type": "response.output_text.done", "item_id": "item_2", "output_index": 1, "content_index": 0, "text": "Hello from responses"},
            {"type": "response.content_part.done", "item_id": "item_2", "output_index": 1, "content_index": 0, "part": {"type": "output_text", "text": "Hello from responses", "annotations": []}},
            {"type": "response.output_item.done", "output_index": 1, "item": {"id": "item_2", "type": "message", "status": "completed", "role": "assistant", "content": [{"type": "output_text", "text": "Hello from responses", "annotations": []}]}},
            {"type": "response.completed", "response": {"id": "resp_up", "object": "response", "created_at": 1700000000, "model": "gpt-4o", "status": "completed", "output": [], "usage": {"input_tokens": 500, "output_tokens": 25, "total_tokens": 525, "input_tokens_details": {"cached_tokens": 300}}}}
        ]);

        let mut anthropic_state = crate::api::proxy::ir::from_messages::MessagesParseState::new();
        let anthropic_events = anthropic_chunks
            .as_array().unwrap().iter()
            .flat_map(|c| crate::api::proxy::ir::from_messages::messages_chunk_to_ir(c, &mut anthropic_state))
            .collect::<Vec<_>>();

        let mut chat_state = crate::api::proxy::ir::from_chat_completions::ChatCompletionsParseState::new();
        let chat_events = chat_chunks
            .as_array().unwrap().iter()
            .flat_map(|c| crate::api::proxy::ir::from_chat_completions::chat_completions_chunk_to_ir(c, &mut chat_state))
            .collect::<Vec<_>>();

        let mut responses_state = crate::api::proxy::ir::from_responses::ResponsesParseState::new();
        let responses_events = responses_chunks
            .as_array().unwrap().iter()
            .flat_map(|c| crate::api::proxy::ir::from_responses::responses_chunk_to_ir(c, &mut responses_state))
            .collect::<Vec<_>>();

        let parse = json!({
            "anthropic_events": ir_events_to_json(&anthropic_events),
            "chat_events": ir_events_to_json(&chat_events),
            "responses_events": ir_events_to_json(&responses_events),
            "anthropic_usage": usage_to_json(&anthropic_state.usage),
            "chat_usage": usage_to_json(&chat_state.usage),
            "responses_usage": usage_to_json(&responses_state.usage),
        });
        std::fs::write(dir.join("parse.json"), serde_json::to_string_pretty(&parse).unwrap()).unwrap();

        eprintln!("fixtures exported to {}", dir.display());
    }
}
