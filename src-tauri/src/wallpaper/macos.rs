//! macOS：NSWindow 降级到桌面图标层。
//!
//! `kCGDesktopIconWindowLevel`（-2147483622）介于桌面壁纸图与桌面图标之间：
//! 窗口视觉上就是"活的壁纸"，图标仍在其上可点。配合：
//! - `setCollectionBehavior`（CanJoinAllSpaces | Stationary | FullScreenAuxiliary）
//!   随所有 Space / 全屏应用层级跟随；
//! - `setIgnoresMouseEvents` 点击穿透（图标继续可点）；
//! - `orderFront:`（nil sender）呈现——**禁止用 `window.show()`**：tao 的
//!   show 走 `makeKeyAndOrderFront` 会抢占键盘焦点。
//!
//! 已知限制：不做 `setCanBecomeKeyWindow` 覆写（需 NSWindow 子类），
//! 焦点由 ignoresMouseEvents + 创建时 focused(false) 兜底。

use objc2::msg_send;
use objc2::runtime::AnyObject;
use tauri::WebviewWindow;

/// kCGDesktopIconWindowLevel：壁纸图之上、桌面图标之下。
const KCG_DESKTOP_ICON_WINDOW_LEVEL: isize = -2_147_483_622;
/// NSWindowCollectionBehaviorCanJoinAllSpaces(1) | Stationary(16) | FullScreenAuxiliary(256)。
const COLLECTION_BEHAVIOR: u64 = 1 | 16 | 256;

/// 挂载壁纸窗口到桌面图标层（须在主线程调用；见 mod.rs `run_on_main`）。
pub fn mount(win: &WebviewWindow) -> Result<(), String> {
    let ns = win
        .ns_window()
        .map_err(|e| format!("get ns_window: {e}"))?
        as *mut AnyObject;
    unsafe {
        let _: () = msg_send![ns, setLevel: KCG_DESKTOP_ICON_WINDOW_LEVEL];
        let _: () = msg_send![ns, setCollectionBehavior: COLLECTION_BEHAVIOR];
        let _: () = msg_send![ns, setHasShadow: false];
        let _: () = msg_send![ns, setIgnoresMouseEvents: true];
        let _: () = msg_send![ns, orderFront: Option::<&AnyObject>::None];
    }
    Ok(())
}
