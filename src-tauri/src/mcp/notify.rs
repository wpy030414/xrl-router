//! MCP Notify 工具：让 LLM 通过网关发送系统桌面通知。
//!
//! 复用 Tauri 内置的 `tauri-plugin-notification`（底层 `notify-rust`），无额外依赖。
//! 工具接收 `title`（必填）、`body`（可选）、`sound`（可选）参数，弹出操作系统通知中心的通知。
//!
//! ## 跨平台声音支持
//!
//! - `sound` 省略 / `null`：使用系统默认通知声音（等同于 `sound = "default"`）。
//! - `sound = "default"`：使用系统默认通知声音（推荐，跨平台最安全）。
//! - `sound = "<name>"`：使用平台特定声音名称：
//!   - **macOS**：系统声音名（`Glass`、`Basso`、`Frog`、`Hero`、`Submarine`、`Pop` 等，
//!     位于 `/System/Library/Sounds/`）。
//!   - **Linux**：freedesktop 声音名（`message-new-email`、`bell` 等，通过 D-Bus `sound-name` hint）。
//!   - **Windows**：toast 通知声音名。
//!
//! 名称无效时各平台行为不同：macOS 静默降级为无声，Linux/Windows 由底层决定。

use std::sync::OnceLock;

use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

/// 全局 AppHandle（`mcp::init` 注入，`lib.rs` setup 调用）。
static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();

/// 通知参数（宽松可选字段）。
pub(crate) struct NotifyParams<'a> {
    pub title: &'a str,
    pub body: Option<&'a str>,
    /// 声音：`None` 或 `"default"` 使用系统默认；`Some("<name>")` 平台特定声音名。
    pub sound: Option<&'a str>,
}

/// 注入 AppHandle（`lib.rs` setup 创建 AppState 后调用）。
pub(crate) fn init(app: AppHandle) {
    let _ = APP_HANDLE.set(app);
}

/// 发送系统桌面通知（宽松参数：仅 `title` 必填，`body` / `sound` 均可省略）。
pub(super) fn send_notification(params: &NotifyParams<'_>) -> Result<String, String> {
    let app = APP_HANDLE
        .get()
        .ok_or_else(|| "notification not initialized".to_string())?;

    let mut builder = app.notification().builder().title(params.title);
    if let Some(body_text) = params.body {
        builder = builder.body(body_text);
    }
    let sound = params.sound.unwrap_or("default");
    builder = builder.sound(sound);

    builder
        .show()
        .map_err(|e| format!("failed to send notification: {e}"))?;

    Ok(format!("Notification sent: {}", params.title))
}
