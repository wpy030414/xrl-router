//! IR（中间表示）类型定义 — 三种 LLM 协议的统一抽象。
//!
//! 所有内部工具（websearch 劫持、usage 追踪、错误构造）只操作 IR 类型，
//! 与具体协议格式（Anthropic Messages / OpenAI Chat Completions / OpenAI Responses）解耦。
//!
//! 设计原则：
//! - 以 Anthropic Messages 为骨架（内容块模型最丰富）
//! - 并集覆盖三种格式的所有字段
//! - 强类型 Rust struct/enum，serde 自动序列化

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ═══════════════════════════════════════════════════════════════════
// 请求类型
// ═══════════════════════════════════════════════════════════════════

/// 统一请求体 — 三种客户端格式的并集。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrRequest {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<IrSystemContent>,
    #[serde(default)]
    pub messages: Vec<IrMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<IrTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<IrToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<IrThinkingConfig>,
    #[serde(default)]
    pub stream: bool,
}

/// System prompt — 纯文本或多段（含 cache_control）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IrSystemContent {
    Text(String),
    Blocks(Vec<IrSystemBlock>),
}

/// System prompt 中的单个块。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrSystemBlock {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<Value>,
}

/// 一条消息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrMessage {
    pub role: IrRole,
    pub content: Vec<IrContentBlock>,
}

/// 消息角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IrRole {
    User,
    Assistant,
}

/// 统一内容块 — 覆盖 Anthropic content blocks + OpenAI content parts
/// + Responses input/output items。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum IrContentBlock {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<Value>,
    },
    #[serde(rename = "image")]
    Image { source: IrImageSource },
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: IrToolResultContent,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
}

/// 图像来源 — base64 或 URL。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum IrImageSource {
    #[serde(rename = "base64")]
    Base64 { media_type: String, data: String },
    #[serde(rename = "url")]
    Url { url: String },
}

/// tool_result 的内容 — 纯文本或多块（Anthropic 支持多块）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IrToolResultContent {
    Text(String),
    Blocks(Vec<IrContentBlock>),
}

/// 工具定义。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrTool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Value,
}

/// 工具选择模式。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode")]
pub enum IrToolChoice {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "any")]
    Any,
    #[serde(rename = "none")]
    None,
    #[serde(rename = "tool")]
    Tool { name: String },
}

/// Thinking/Reasoning 配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrThinkingConfig {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u64>,
}

// ═══════════════════════════════════════════════════════════════════
// 流式事件
// ═══════════════════════════════════════════════════════════════════

/// 统一流式事件 — 以 Anthropic SSE 为骨架（最丰富）。
/// 三种上游格式的 chunk 都先解析为 IrStreamEvent，再渲染为客户端格式。
#[derive(Debug, Clone)]
pub enum IrStreamEvent {
    MessageStart {
        id: String,
        model: String,
    },
    ContentBlockStart {
        index: usize,
        block: IrContentBlockStart,
    },
    ContentBlockDelta {
        index: usize,
        delta: IrContentDelta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        stop_reason: Option<IrStopReason>,
        usage: Option<IrUsage>,
    },
    MessageStop,
}

/// content_block_start 的块类型。
#[derive(Debug, Clone)]
pub enum IrContentBlockStart {
    Text,
    Thinking,
    ToolUse { id: String, name: String },
}

/// content_block_delta 的增量类型。
#[derive(Debug, Clone)]
pub enum IrContentDelta {
    TextDelta(String),
    ThinkingDelta(String),
    /// tool_use 参数的流式 JSON 片段。
    InputJsonDelta(String),
}

/// 停止原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrStopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
}

impl IrStopReason {
    /// Anthropic 格式字符串。
    pub fn as_anthropic_str(&self) -> &'static str {
        match self {
            Self::EndTurn => "end_turn",
            Self::ToolUse => "tool_use",
            Self::MaxTokens => "max_tokens",
        }
    }

    /// OpenAI Chat Completions finish_reason 字符串。
    pub fn as_chat_finish_reason(&self) -> &'static str {
        match self {
            Self::EndTurn => "stop",
            Self::ToolUse => "tool_calls",
            Self::MaxTokens => "length",
        }
    }

    /// 从 Anthropic Messages stop_reason 解析。
    pub fn from_messages(s: &str) -> Self {
        match s {
            "tool_use" => Self::ToolUse,
            "max_tokens" => Self::MaxTokens,
            _ => Self::EndTurn,
        }
    }

    /// 从 OpenAI Chat Completions finish_reason 解析。
    pub fn from_chat_completions(s: &str) -> Self {
        match s {
            "tool_calls" => Self::ToolUse,
            "length" => Self::MaxTokens,
            _ => Self::EndTurn,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// Usage
// ═══════════════════════════════════════════════════════════════════

/// 统一 token usage — 覆盖三种格式的所有 token 字段。
#[derive(Debug, Clone, Default)]
pub struct IrUsage {
    /// 未缓存输入 token + 首次写缓存 token（写缓存 = 首次处理输入）。
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// 缓存命中读取的 token（真正的「缓存」）。
    pub cache_read_input_tokens: u64,
    /// 写缓存的 token（首次处理输入，并入 input_tokens 计算）。
    pub cache_creation_input_tokens: u64,
    /// 输出字符数，用于上游未报 token 时的回退估算（chars / 4）。
    pub output_chars: u64,
}

// ═══════════════════════════════════════════════════════════════════
// 错误
// ═══════════════════════════════════════════════════════════════════

/// 统一错误。
#[derive(Debug, Clone)]
pub struct IrError {
    pub error_type: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ir_stop_reason_roundtrip_anthropic() {
        assert_eq!(IrStopReason::from_messages("end_turn"), IrStopReason::EndTurn);
        assert_eq!(IrStopReason::from_messages("tool_use"), IrStopReason::ToolUse);
        assert_eq!(IrStopReason::from_messages("max_tokens"), IrStopReason::MaxTokens);
        assert_eq!(IrStopReason::EndTurn.as_anthropic_str(), "end_turn");
    }

    #[test]
    fn test_ir_stop_reason_roundtrip_chat() {
        assert_eq!(IrStopReason::from_chat_completions("stop"), IrStopReason::EndTurn);
        assert_eq!(IrStopReason::from_chat_completions("tool_calls"), IrStopReason::ToolUse);
        assert_eq!(IrStopReason::from_chat_completions("length"), IrStopReason::MaxTokens);
        assert_eq!(IrStopReason::EndTurn.as_chat_finish_reason(), "stop");
    }

    #[test]
    fn test_ir_request_serde() {
        let req = IrRequest {
            model: "gpt-4o".to_string(),
            system: Some(IrSystemContent::Text("You are helpful.".to_string())),
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
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["model"], "gpt-4o");
        assert_eq!(json["max_tokens"], 4096);
        assert_eq!(json["stream"], true);
    }

    #[test]
    fn test_ir_content_block_variants() {
        // Text
        let text = IrContentBlock::Text {
            text: "hi".to_string(),
            cache_control: None,
        };
        let v = serde_json::to_value(&text).unwrap();
        assert_eq!(v["type"], "text");
        assert_eq!(v["text"], "hi");

        // Image (base64)
        let img = IrContentBlock::Image {
            source: IrImageSource::Base64 {
                media_type: "image/png".to_string(),
                data: "abc123".to_string(),
            },
        };
        let v = serde_json::to_value(&img).unwrap();
        assert_eq!(v["type"], "image");
        assert_eq!(v["source"]["type"], "base64");

        // ToolUse
        let tu = IrContentBlock::ToolUse {
            id: "call_1".to_string(),
            name: "get_weather".to_string(),
            input: serde_json::json!({"city": "Tokyo"}),
        };
        let v = serde_json::to_value(&tu).unwrap();
        assert_eq!(v["type"], "tool_use");
        assert_eq!(v["name"], "get_weather");
    }

    #[test]
    fn test_ir_tool_choice_serde() {
        let auto = IrToolChoice::Auto;
        let v = serde_json::to_value(&auto).unwrap();
        assert_eq!(v["mode"], "auto");

        let tool = IrToolChoice::Tool { name: "search".to_string() };
        let v = serde_json::to_value(&tool).unwrap();
        assert_eq!(v["mode"], "tool");
        assert_eq!(v["name"], "search");
    }

    #[test]
    fn test_ir_usage_default() {
        let u = IrUsage::default();
        assert_eq!(u.input_tokens, 0);
        assert_eq!(u.output_tokens, 0);
        assert_eq!(u.cache_read_input_tokens, 0);
        assert_eq!(u.output_chars, 0);
    }
}
