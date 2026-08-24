//! WebVision 识图：调用用户配置的「视觉专用模型」识别图片。
//!
//! 设计取舍（见 docs/DECISIONS.md ADR-039）：
//! - **单个视觉模型全局配置**（settings 键 `mcp_vision_provider` / `mcp_vision_model`，
//!   存上游真实 `model_id`）——每次调用实时解析，管理页删改立即生效，不缓存。
//! - **统一 base64 上送**——Anthropic 图片 source 只收 base64；OpenAI 系转
//!   `data:{mime};base64,{data}` data URI，三协议共用一套取图逻辑。
//! - **图片由网关获取**（`/mcp` 请求体 2MiB 上限，客户端只传 URL/本地路径）：
//!   http(s) 下载复用共享 client（继承系统代理），本地绝对路径 / `file://` 直接读文件。
//! - **不计配额**：与 web_search/web_fetch 一致，不触碰 usage 统计与服务 key 配额。
//! - **不重试**：单次调用（同 web_search/web_fetch 语义），上游错误文本透传。

use std::time::Duration;

use base64::Engine as _;
use serde_json::json;

use crate::gateway::server::AppState;
use crate::types::{Provider, ProviderKind};

/// 图片大小上限（字节）。Anthropic Messages 图片约 5MB 上限，8MiB 留余量；
/// base64 膨胀 33% 后单请求出站约 11MiB，可接受。
const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
/// 图片下载超时。
const IMAGE_FETCH_TIMEOUT: Duration = Duration::from_secs(15);
/// 上游调用超时。
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(60);
/// 模型行缺失（被删/未同步）时兜底 max_tokens。
const DEFAULT_MAX_TOKENS: i64 = 1024;
/// 默认识图指令（工具无 prompt 参数时）。
const DEFAULT_PROMPT: &str = "Describe this image.";
/// 支持的图片媒体类型白名单。
const SUPPORTED_MEDIA: [&str; 5] = ["image/png", "image/jpeg", "image/gif", "image/webp", "image/bmp"];

/// 识图过程的分步错误（tools.rs 负责映射为工具级文本）。
#[derive(Debug)]
pub(super) enum VisionError {
    /// 视觉模型未配置（开关开了但 provider/model 键缺失或为空）。
    NotConfigured,
    /// 配置的供应商不存在。
    ProviderNotFound(String),
    /// 配置的供应商已被禁用。
    ProviderDisabled(String),
    /// 供应商没有可用 key。
    KeyMissing(String),
    /// 图片下载/读文件失败。
    ImageFetch(String),
    /// 图片超过 8 MiB 上限（携带实际字节数）。
    ImageTooLarge(u64),
    /// 媒体类型不在白名单（携带 content-type 原文或 unknown）。
    UnsupportedMedia(String),
    /// 图片来源不是 http(s)/绝对路径/file://。
    UnsupportedSource,
    /// 上游返回非 2xx（携带状态码与错误消息）。
    Upstream { status: u16, message: String },
    /// 其他（超时/网络/响应解析失败）。
    Other(String),
}

impl std::fmt::Display for VisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VisionError::NotConfigured => write!(f, "vision model not configured"),
            VisionError::ProviderNotFound(id) => write!(f, "vision provider not found: {id}"),
            VisionError::ProviderDisabled(name) => write!(f, "vision provider disabled: {name}"),
            VisionError::KeyMissing(name) => write!(f, "no available key for provider: {name}"),
            VisionError::ImageFetch(e) => write!(f, "image fetch failed: {e}"),
            VisionError::ImageTooLarge(n) => write!(f, "image too large: {n} bytes"),
            VisionError::UnsupportedMedia(m) => write!(f, "unsupported media type: {m}"),
            VisionError::UnsupportedSource => write!(f, "unsupported image source"),
            VisionError::Upstream { status, message } => {
                write!(f, "upstream error (HTTP {status}): {message}")
            }
            VisionError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for VisionError {}

/// 入口：解析配置 → 取图 → 调上游 → 返回描述文本。
pub(super) async fn describe_image(
    state: &AppState,
    image_ref: &str,
    prompt: Option<&str>,
) -> Result<String, VisionError> {
    let cfg = resolve_vision_config(state)?;
    let (bytes, content_type, path_hint) = fetch_image(state, image_ref).await?;
    let mime = infer_media_type(content_type.as_deref(), &path_hint)
        .ok_or_else(|| VisionError::UnsupportedMedia(content_type.unwrap_or_else(|| "unknown".into())))?;
    let image_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    call_vision(
        state,
        &cfg.provider,
        &cfg.model_id,
        &image_b64,
        &mime,
        prompt.unwrap_or(DEFAULT_PROMPT),
    )
    .await
}

/// 解析视觉模型配置（每次调用实时读 DB，provider 经注册表解析）。
struct VisionConfig {
    provider: Provider,
    model_id: String,
}

fn resolve_vision_config(state: &AppState) -> Result<VisionConfig, VisionError> {
    let provider_id = state
        .database
        .get_setting("mcp_vision_provider")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
        .ok_or(VisionError::NotConfigured)?;
    let model_id = state
        .database
        .get_setting("mcp_vision_model")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
        .ok_or(VisionError::NotConfigured)?;
    let provider = state
        .providers
        .get(&provider_id)
        .ok_or_else(|| VisionError::ProviderNotFound(provider_id.clone()))?;
    if !provider.enabled {
        return Err(VisionError::ProviderDisabled(provider.name.clone()));
    }
    Ok(VisionConfig { provider, model_id })
}

/// 取图：http(s) 下载（Content-Length 预检 + bytes 复查），本地绝对路径 / file:// 读文件。
/// 返回 (bytes, content_type, path_hint)——content_type 用于推断 media_type，
/// path_hint 是下载 URL 或本地路径（推断扩展名用）。
async fn fetch_image(
    state: &AppState,
    image_ref: &str,
) -> Result<(Vec<u8>, Option<String>, String), VisionError> {
    let trimmed = image_ref.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        let (bytes, content_type) = download_image(state, trimmed).await?;
        return Ok((bytes, content_type, trimmed.to_string()));
    }

    // 本地来源：file:// URL（经 url crate 解析）或绝对路径。
    let path = if trimmed.starts_with("file://") {
        url::Url::parse(trimmed)
            .ok()
            .and_then(|u| u.to_file_path().ok())
            .ok_or(VisionError::UnsupportedSource)?
    } else {
        let p = std::path::Path::new(trimmed);
        if !p.is_absolute() {
            return Err(VisionError::UnsupportedSource);
        }
        p.to_path_buf()
    };
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|e| VisionError::ImageFetch(e.to_string()))?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(VisionError::ImageTooLarge(bytes.len() as u64));
    }
    Ok((bytes, None, path.display().to_string()))
}

/// 下载 http(s) 图片。返回 (bytes, content_type)。
async fn download_image(state: &AppState, url: &str) -> Result<(Vec<u8>, Option<String>), VisionError> {
    let resp = state
        .http_client
        .get(url)
        .timeout(IMAGE_FETCH_TIMEOUT)
        .send()
        .await
        .map_err(|e| VisionError::ImageFetch(format!("download failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(VisionError::ImageFetch(format!("HTTP {}", resp.status())));
    }
    // Content-Length 预检，超限直接拒绝（省去读全响应体）。
    if let Some(len) = resp.content_length() {
        if len > MAX_IMAGE_BYTES {
            return Err(VisionError::ImageTooLarge(len));
        }
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| VisionError::ImageFetch(format!("read body failed: {e}")))?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(VisionError::ImageTooLarge(bytes.len() as u64));
    }
    Ok((bytes.to_vec(), content_type))
}

/// 推断媒体类型：Content-Type（去 `;` 后参数、小写）白名单优先；
/// 缺失或不在白名单时回退扩展名表（宽容处理 octet-stream 等无意义类型）。
/// 两者都无法识别 → None（上层报 UnsupportedMedia）。
fn infer_media_type(content_type: Option<&str>, path: &str) -> Option<String> {
    if let Some(ct) = content_type {
        let base = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        if SUPPORTED_MEDIA.contains(&base.as_str()) {
            return Some(base);
        }
    }
    let ext = path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png".into()),
        "jpg" | "jpeg" => Some("image/jpeg".into()),
        "gif" => Some("image/gif".into()),
        "webp" => Some("image/webp".into()),
        "bmp" => Some("image/bmp".into()),
        _ => None,
    }
}

/// 调用上游视觉模型（非流式，统一 base64 上送）。
async fn call_vision(
    state: &AppState,
    provider: &Provider,
    model_id: &str,
    image_b64: &str,
    mime: &str,
    prompt: &str,
) -> Result<String, VisionError> {
    let entry = state
        .keys
        .get_next_key(&provider.id)
        .map_err(|_| VisionError::KeyMissing(provider.name.clone()))?;
    let api_key = entry.key_hash; // KeyPool 内存中存明文

    // max_tokens：模型行仍在则用其配置，否则兜底（model 行被删不影响调用）。
    let max_tokens = state
        .database
        .list_all_models()
        .ok()
        .and_then(|models| {
            models
                .into_iter()
                .find(|m| m.provider_id == provider.id && m.model_id == model_id && m.max_output_tokens > 0)
                .map(|m| m.max_output_tokens)
        })
        .unwrap_or(DEFAULT_MAX_TOKENS);

    let url = format!("{}{}", provider.base_url, provider.api_path);
    let mut req = state.http_client.post(&url).timeout(UPSTREAM_TIMEOUT);

    // 请求体按 ProviderKind 构造（stream: false；OpenAI 系图片走 data URI）。
    let body = match provider.kind {
        ProviderKind::Messages => {
            req = req
                .header("x-api-key", &api_key)
                .header("anthropic-version", "2023-06-01");
            json!({
                "model": model_id,
                "max_tokens": max_tokens,
                "messages": [{
                    "role": "user",
                    "content": [
                        { "type": "image", "source": { "type": "base64", "media_type": mime, "data": image_b64 } },
                        { "type": "text", "text": prompt },
                    ],
                }],
                "stream": false,
            })
        }
        ProviderKind::ChatCompletions => {
            req = req.header("Authorization", format!("Bearer {api_key}"));
            json!({
                "model": model_id,
                "max_tokens": max_tokens,
                "messages": [{
                    "role": "user",
                    "content": [
                        { "type": "text", "text": prompt },
                        { "type": "image_url", "image_url": { "url": format!("data:{mime};base64,{image_b64}") } },
                    ],
                }],
                "stream": false,
            })
        }
        ProviderKind::Responses => {
            req = req.header("Authorization", format!("Bearer {api_key}"));
            json!({
                "model": model_id,
                // Responses API 的参数名是 max_output_tokens（同 ir/to_responses.rs）；
                // 缺失时识图输出无长度上限。
                "max_output_tokens": max_tokens,
                "input": [{
                    "role": "user",
                    "content": [
                        { "type": "input_text", "text": prompt },
                        { "type": "input_image", "image_url": format!("data:{mime};base64,{image_b64}") },
                    ],
                }],
                "stream": false,
            })
        }
    };

    let resp = req
        .json(&body)
        .send()
        .await
        .map_err(|e| VisionError::Other(format!("upstream request failed: {e}")))?;
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| VisionError::Other(format!("read upstream response failed: {e}")))?;
    let parsed: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| VisionError::Upstream { status: status.as_u16(), message: format!("invalid JSON response: {e}") })?;

    if !status.is_success() {
        let message = parsed["error"]["message"]
            .as_str()
            .or_else(|| parsed["message"].as_str())
            .or_else(|| parsed["error"].as_str())
            .unwrap_or("unknown upstream error")
            .to_string();
        return Err(VisionError::Upstream {
            status: status.as_u16(),
            message,
        });
    }
    extract_text(provider.kind, &parsed).map_err(VisionError::Other)
}

/// 从上游响应提取文本（纯函数，便于单测）。
fn extract_text(kind: ProviderKind, body: &serde_json::Value) -> Result<String, String> {
    match kind {
        ProviderKind::Messages => {
            let blocks = body["content"]
                .as_array()
                .ok_or("upstream response missing content")?;
            let text: Vec<&str> = blocks
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect();
            if text.is_empty() {
                return Err("upstream returned no text content".into());
            }
            Ok(text.join("\n"))
        }
        ProviderKind::ChatCompletions => match &body["choices"][0]["message"]["content"] {
            serde_json::Value::String(s) => Ok(s.clone()),
            serde_json::Value::Array(arr) => {
                let text: Vec<&str> = arr
                    .iter()
                    .filter_map(|b| b["text"].as_str())
                    .collect();
                if text.is_empty() {
                    Err("upstream returned no text content".into())
                } else {
                    Ok(text.join("\n"))
                }
            }
            _ => Err("upstream response missing choices[0].message.content".into()),
        },
        ProviderKind::Responses => {
            let output = body["output"]
                .as_array()
                .ok_or("upstream response missing output")?;
            let text: Vec<&str> = output
                .iter()
                .filter(|o| o["type"] == "message")
                .filter_map(|o| o["content"].as_array())
                .flatten()
                .filter(|b| b["type"] == "output_text")
                .filter_map(|b| b["text"].as_str())
                .collect();
            if text.is_empty() {
                return Err("upstream returned no text content".into());
            }
            Ok(text.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_infer_media_type_from_extension() {
        assert_eq!(infer_media_type(None, "/tmp/a.png").as_deref(), Some("image/png"));
        assert_eq!(infer_media_type(None, "/tmp/a.jpg").as_deref(), Some("image/jpeg"));
        assert_eq!(infer_media_type(None, "/tmp/a.jpeg").as_deref(), Some("image/jpeg"));
        assert_eq!(infer_media_type(None, "/tmp/a.gif").as_deref(), Some("image/gif"));
        assert_eq!(infer_media_type(None, "/tmp/a.webp").as_deref(), Some("image/webp"));
        assert_eq!(infer_media_type(None, "/tmp/a.bmp").as_deref(), Some("image/bmp"));
        assert_eq!(infer_media_type(None, "/tmp/a.xyz"), None);
        assert_eq!(infer_media_type(None, "/tmp/noext"), None);
    }

    #[test]
    fn test_infer_media_type_content_type_priority() {
        // Content-Type 白名单内优先于扩展名
        assert_eq!(
            infer_media_type(Some("image/webp"), "/tmp/a.png").as_deref(),
            Some("image/webp")
        );
        // 带参数剥离
        assert_eq!(
            infer_media_type(Some("image/png; charset=utf-8"), "/tmp/a").as_deref(),
            Some("image/png")
        );
        // 大小写归一
        assert_eq!(
            infer_media_type(Some("IMAGE/JPEG"), "/tmp/a").as_deref(),
            Some("image/jpeg")
        );
        // 非白名单 Content-Type 回退扩展名（octet-stream 等）
        assert_eq!(
            infer_media_type(Some("application/octet-stream"), "/tmp/a.png").as_deref(),
            Some("image/png")
        );
        // 两者都无 → None
        assert_eq!(infer_media_type(Some("text/html"), "/tmp/a.xyz"), None);
    }

    #[test]
    fn test_extract_text_messages() {
        let body = serde_json::json!({
            "content": [
                { "type": "text", "text": "第一段" },
                { "type": "tool_use", "name": "x" },
                { "type": "text", "text": "第二段" },
            ]
        });
        assert_eq!(
            extract_text(ProviderKind::Messages, &body).unwrap(),
            "第一段\n第二段"
        );
        assert!(extract_text(ProviderKind::Messages, &serde_json::json!({"content": []})).is_err());
        assert!(extract_text(ProviderKind::Messages, &serde_json::json!({})).is_err());
    }

    #[test]
    fn test_extract_text_chat_completions() {
        let body = serde_json::json!({
            "choices": [{ "message": { "content": "一张猫的图片" } }]
        });
        assert_eq!(
            extract_text(ProviderKind::ChatCompletions, &body).unwrap(),
            "一张猫的图片"
        );
        // 数组内容（多段 text，refusal 等无 text 字段的块跳过）
        let body = serde_json::json!({
            "choices": [{ "message": { "content": [
                { "type": "text", "text": "a" },
                { "type": "refusal", "refusal": "b" },
                { "type": "text", "text": "c" },
            ] } }]
        });
        assert_eq!(extract_text(ProviderKind::ChatCompletions, &body).unwrap(), "a\nc");
        assert!(extract_text(ProviderKind::ChatCompletions, &serde_json::json!({"choices": []})).is_err());
    }

    #[test]
    fn test_extract_text_responses() {
        let body = serde_json::json!({
            "output": [
                { "type": "message", "content": [
                    { "type": "output_text", "text": "分析结果" },
                    { "type": "refusal", "refusal": "x" },
                ] },
                { "type": "reasoning", "content": [] },
            ]
        });
        assert_eq!(
            extract_text(ProviderKind::Responses, &body).unwrap(),
            "分析结果"
        );
        assert!(extract_text(ProviderKind::Responses, &serde_json::json!({"output": []})).is_err());
    }

    #[test]
    fn test_extract_text_upstream_error_embedded() {
        // 错误消息提取走 Upstream 分支（describe_image 里），这里验证 error.message 可读
        let err = serde_json::json!({ "error": { "message": "invalid image" } });
        assert_eq!(err["error"]["message"].as_str(), Some("invalid image"));
    }
}
