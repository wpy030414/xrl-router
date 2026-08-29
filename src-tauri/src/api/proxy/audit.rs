//! 对话审查工具：消息清洗、指纹计算、首条用户消息提取。
//!
//! 代理层在 `audit_enabled` 开启时调用，对请求消息做：
//! 1. 清洗（剥离 Image base64、截断超长文本）
//! 2. 指纹计算（SHA256 前 3 条消息的 role + 截断文本）
//! 3. 提取首条用户消息预览

use sha2::{Digest, Sha256};

use super::ir::types::{IrContentBlock, IrMessage, IrRole, IrToolResultContent};

/// 单条 Text 块的最大字符数（防止单条超长消息撑爆 DB）。
const MAX_TEXT_LEN: usize = 10_000;
/// 指纹计算时每条消息的截断字符数。
const FINGERPRINT_TRUNCATE: usize = 200;
/// 首条用户消息预览的最大字符数。
const PREVIEW_MAX_LEN: usize = 100;

/// 清洗消息：剥离 Image base64 → "[image]" 占位，Text 截断 10KB，去掉 cache_control。
pub fn sanitize_messages(messages: &[IrMessage]) -> Vec<IrMessage> {
    messages
        .iter()
        .map(|msg| IrMessage {
            role: msg.role,
            content: msg
                .content
                .iter()
                .map(|block| match block {
                    IrContentBlock::Image { .. } => IrContentBlock::Text {
                        text: "[image]".to_string(),
                        cache_control: None,
                    },
                    IrContentBlock::Text { text, .. } => IrContentBlock::Text {
                        text: truncate_chars(text, MAX_TEXT_LEN),
                        cache_control: None,
                    },
                    IrContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => IrContentBlock::ToolResult {
                        tool_use_id: tool_use_id.clone(),
                        content: sanitize_tool_result(content),
                        is_error: *is_error,
                    },
                    other => other.clone(),
                })
                .collect(),
        })
        .collect()
}

/// 清洗 tool_result 内容（递归剥离图片）。
fn sanitize_tool_result(content: &IrToolResultContent) -> IrToolResultContent {
    match content {
        IrToolResultContent::Text(t) => IrToolResultContent::Text(truncate_chars(t, MAX_TEXT_LEN)),
        IrToolResultContent::Blocks(blocks) => IrToolResultContent::Blocks(
            blocks
                .iter()
                .map(|b| match b {
                    IrContentBlock::Image { .. } => IrContentBlock::Text {
                        text: "[image]".to_string(),
                        cache_control: None,
                    },
                    IrContentBlock::Text { text, .. } => IrContentBlock::Text {
                        text: truncate_chars(text, MAX_TEXT_LEN),
                        cache_control: None,
                    },
                    other => other.clone(),
                })
                .collect(),
        ),
    }
}

/// 计算对话指纹：SHA256(service_key_id + ":" + msg[0].role + ":" + trunc(text,200) + ...)。
/// 最多取前 3 条消息。
pub fn compute_fingerprint(service_key_id: &str, messages: &[IrMessage]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(service_key_id.as_bytes());

    let take = messages.len().min(3);
    for i in 0..take {
        hasher.update(b":");
        hasher.update(role_str(&messages[i].role).as_bytes());
        hasher.update(b":");
        let text = extract_text(&messages[i]);
        let truncated = truncate_chars(&text, FINGERPRINT_TRUNCATE);
        hasher.update(truncated.as_bytes());
    }

    format!("{:x}", hasher.finalize())
}

/// 提取第一条 user 消息的纯文本预览（≤100 字符）。
pub fn extract_first_user_message(messages: &[IrMessage]) -> String {
    for msg in messages {
        if msg.role == IrRole::User {
            let text = extract_text(msg);
            return truncate_chars(&text, PREVIEW_MAX_LEN);
        }
    }
    String::new()
}

/// 提取最后一条消息的纯文本预览（≤100 字符）— 简约版，剥离系统标签。
pub fn extract_last_message(messages: &[IrMessage]) -> String {
    if let Some(msg) = messages.last() {
        let text = extract_text(msg);
        let stripped = strip_system_tags(&text);
        return truncate_chars(&stripped.trim(), PREVIEW_MAX_LEN);
    }
    String::new()
}

/// 提取最后一条消息的原始文本预览（≤100 字符）— 原始版，保留系统标签。
pub fn extract_last_message_raw(messages: &[IrMessage]) -> String {
    if let Some(msg) = messages.last() {
        let text = extract_text(msg);
        return truncate_chars(&text.trim(), PREVIEW_MAX_LEN);
    }
    String::new()
}

/// 剥离系统注入的标签。
fn strip_system_tags(text: &str) -> String {
    text
        .replace("<system-reminder>", "")
        .replace("</system-reminder>", "")
        .replace("<total_tokens>", "")
        .replace("</total_tokens>", "")
        .replace("<local-command-caveat>", "")
        .replace("</local-command-caveat>", "")
        .replace("<local-command-stdout>", "")
        .replace("</local-command-stdout>", "")
        .replace("<task-notification>", "")
        .replace("</task-notification>", "")
}

/// 从消息中提取所有 Text 块的文本拼接。
fn extract_text(msg: &IrMessage) -> String {
    let mut parts = Vec::new();
    for block in &msg.content {
        if let IrContentBlock::Text { text, .. } = block {
            parts.push(text.as_str());
        }
    }
    parts.join("")
}

/// IrRole → 字符串。
fn role_str(role: &IrRole) -> &'static str {
    match role {
        IrRole::User => "user",
        IrRole::Assistant => "assistant",
    }
}

/// 按 Unicode 字符截断（避免切到多字节字符中间）。
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::proxy::ir::types::*;

    fn user_msg(text: &str) -> IrMessage {
        IrMessage {
            role: IrRole::User,
            content: vec![IrContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    fn assistant_msg(text: &str) -> IrMessage {
        IrMessage {
            role: IrRole::Assistant,
            content: vec![IrContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    #[test]
    fn test_fingerprint_stable() {
        let msgs = vec![
            user_msg("你好"),
            assistant_msg("你好！有什么可以帮您？"),
            user_msg("写个诗"),
        ];
        let fp1 = compute_fingerprint("sk-1", &msgs);
        let fp2 = compute_fingerprint("sk-1", &msgs);
        assert_eq!(fp1, fp2, "same input → same fingerprint");

        // Different service_key → different fingerprint
        let fp3 = compute_fingerprint("sk-2", &msgs);
        assert_ne!(fp1, fp3);
    }

    #[test]
    fn test_fingerprint_different_conversations() {
        let msgs_a = vec![user_msg("写首诗")];
        let msgs_b = vec![user_msg("帮我写代码")];
        let fp_a = compute_fingerprint("sk-1", &msgs_a);
        let fp_b = compute_fingerprint("sk-1", &msgs_b);
        assert_ne!(fp_a, fp_b);
    }

    #[test]
    fn test_fingerprint_uses_first_3_messages() {
        let msgs_short = vec![user_msg("hello"), assistant_msg("hi")];
        let msgs_long = vec![
            user_msg("hello"),
            assistant_msg("hi"),
            user_msg("how are you"),
            assistant_msg("I'm fine"),
        ];
        // Both should use first 3 (or fewer) messages, so the short one uses 2
        // and the long one uses 3 → different fingerprints
        let fp_short = compute_fingerprint("sk", &msgs_short);
        let fp_long = compute_fingerprint("sk", &msgs_long);
        assert_ne!(fp_short, fp_long);
    }

    #[test]
    fn test_sanitize_strips_images() {
        let msgs = vec![IrMessage {
            role: IrRole::User,
            content: vec![
                IrContentBlock::Text {
                    text: "看看这张图".to_string(),
                    cache_control: Some(serde_json::json!({"type": "ephemeral"})),
                },
                IrContentBlock::Image {
                    source: IrImageSource::Base64 {
                        media_type: "image/png".to_string(),
                        data: "iVBORw0KGgo...".to_string(),
                    },
                },
            ],
        }];
        let sanitized = sanitize_messages(&msgs);
        assert_eq!(sanitized[0].content.len(), 2);
        // Image → "[image]" text
        match &sanitized[0].content[1] {
            IrContentBlock::Text { text, cache_control } => {
                assert_eq!(text, "[image]");
                assert!(cache_control.is_none());
            }
            _ => panic!("expected text block"),
        }
        // cache_control stripped from first block
        match &sanitized[0].content[0] {
            IrContentBlock::Text { cache_control, .. } => {
                assert!(cache_control.is_none());
            }
            _ => panic!("expected text block"),
        }
    }

    #[test]
    fn test_extract_first_user() {
        let msgs = vec![assistant_msg("系统欢迎"), user_msg("你好世界"), user_msg("再见")];
        let first = extract_first_user_message(&msgs);
        assert_eq!(first, "你好世界");
    }

    #[test]
    fn test_extract_last_message() {
        let msgs = vec![user_msg("你好"), assistant_msg("你好！"), user_msg("写个诗")];
        let last = extract_last_message(&msgs);
        assert_eq!(last, "写个诗");
    }

    #[test]
    fn test_extract_last_message_empty() {
        let msgs: Vec<IrMessage> = vec![];
        let last = extract_last_message(&msgs);
        assert_eq!(last, "");
    }
}
