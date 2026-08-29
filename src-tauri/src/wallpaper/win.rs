//! Windows：壁纸窗口的桌面层挂载与样式（自实现，配方与社区插件一致）。
//!
//! 此前用 `tauri-plugin-desktop-underlay`：恢复路径能挂上、运行中现场设置
//! 却挂不上（窗口停留在顶层、盖住应用；其状态机 + 双重主线程 dispatch
//! 与自建窗口流程不匹配）。改为自实现同款配方并**挂载后验证**：
//!
//! 1. 向 Progman 发 `WM_SPAWN_WORKERW (0x052C, (0xD, 0x1))`（幂等唤醒）；
//! 2. 定位壁纸宿主 WorkerW（Progman 直接子窗优先；经典 DefView-宿主兜底）；
//! 3. `SetParent` 挂入 + **验证**（失败 sleep 250ms 重试一次）；
//! 4. **Z 序锚定（防盖桌面图标）**：Win8+ `SetParent` 不会把 popup 转成真正的
//!    子窗口（窗口仍留在顶层 Z 序、盖住图标），须手动补 `WS_CHILD`（清
//!    `WS_POPUP`）并 `SetWindowPos(HWND_BOTTOM)` 沉到宿主最底层——壁纸层
//!    必须位于 `SHELLDLL_DefView`（桌面图标层）之下；
//! 5. 精确铺满主屏（补偿 Win11 无边框窗口隐形内边框，防左/顶空白条）——
//!    该 `SetWindowPos` 带 `SWP_NOZORDER`，绝不扰动第 4 步的 Z 序锚定；
//! 6. 点击穿透 `WS_EX_TRANSPARENT`（顶层 + WebView2 子窗递归 1s 补轮）。
//!    **严禁 `WS_EX_LAYERED`**：分层窗口不设属性不显示内容。
//! 7. show 之后再次复查 Z 序（`mod.rs` 延迟主线程复查）：show/WebView2
//!    初始化可能重新排序，把壁纸窗顶回宿主上层。

use std::time::Duration;

use tauri::WebviewWindow;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, FindWindowExW, FindWindowW, GetSystemMetrics, GetWindow,
    GetWindowLongPtrW, GetWindowRect, GWL_EXSTYLE, GWL_STYLE, GW_HWNDNEXT, HWND_BOTTOM,
    SendMessageTimeoutW, SetParent, SetWindowLongPtrW, SetWindowPos, IsWindow, SM_CXSCREEN,
    SM_CYSCREEN, SMTO_NORMAL, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WS_CHILD,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_POPUP,
};

/// 点击穿透扩展样式（不包含 WS_EX_LAYERED，见模块注释）。
const CLICK_THROUGH_EX: u32 = WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0;

/// 私有消息：让 Progman 生成/唤醒壁纸 WorkerW（未归档常量）。
const WM_SPAWN_WORKERW: u32 = 0x052C;

/// 把壁纸窗口挂入桌面壁纸层（须在主线程调用；见 mod.rs `run_on_main`）。
pub fn mount(win: &WebviewWindow) -> Result<(), String> {
    let hwnd = win.hwnd().map_err(|e| format!("get wallpaper hwnd: {e}"))?;
    let host = find_wallpaper_host()
        .ok_or_else(|| "wallpaper host (WorkerW) not found".to_string())?;
    tracing::info!(?host, ?hwnd, "wallpaper: attaching to WorkerW");

    // SetParent：不成功则 250ms 后重试一次（首次失败多为 explorer 侧瞬时状态）。
    let mut attached = attach(hwnd, host);
    if !attached {
        std::thread::sleep(Duration::from_millis(250));
        attached = attach(hwnd, host);
    }
    if !attached {
        return Err("SetParent into WorkerW failed after retry".into());
    }
    // 验证：GetParent 对 popup 型窗口返回的是 owner 而非 parent，
    // 用「宿主子窗枚举命中」判定（跨线程枚举稳定可靠）。
    if !is_child_of(hwnd, host) {
        return Err("SetParent did not take effect (not in host children)".into());
    }

    // Z 序锚定（防盖桌面图标）：首次失败多为系统仍在调整窗口树，
    // 250ms 后重试一次；仍失败不算致命——mod.rs show 后的延迟复查兜底。
    if let Err(e) = ensure_bottom_zorder(hwnd, host) {
        tracing::warn!("wallpaper: bottom z-order attempt 1 failed: {e}");
        std::thread::sleep(Duration::from_millis(250));
        if let Err(e) = ensure_bottom_zorder(hwnd, host) {
            tracing::warn!("wallpaper: bottom z-order failed: {e}");
        }
    }

    fit_monitor(win)?;
    apply_click_through(win);
    tracing::info!(?host, "wallpaper: mounted into WorkerW");
    Ok(())
}

/// show / WebView2 初始化后的延迟复查：再次钉到宿主最底层（幂等）。
/// 窗口若已脱离宿主（Explorer 重启等外部销毁）只报错，交给自愈重建。
pub fn recheck_zorder(win: &WebviewWindow) -> Result<(), String> {
    let hwnd = win.hwnd().map_err(|e| format!("get wallpaper hwnd: {e}"))?;
    let host = find_wallpaper_host()
        .ok_or_else(|| "wallpaper host (WorkerW) not found".to_string())?;
    if !is_child_of(hwnd, host) {
        return Err("wallpaper window no longer child of host".into());
    }
    ensure_bottom_zorder(hwnd, host)
}

/// SetParent 挂载（返回是否成功）。
fn attach(hwnd: HWND, host: HWND) -> bool {
    unsafe { SetParent(hwnd, Some(host)).is_ok() }
}

/// 把壁纸窗钉到宿主（WorkerW）子窗 Z 序的最底层（防盖桌面图标）。
///
/// 为什么需要：
/// - Win8+ `SetParent` 对 popup 风格窗口只做"重设父链"，窗口仍留在顶层
///   Z 序（旧行为见 ADR-042 注记），视觉上盖住 `SHELLDLL_DefView`（桌面
///   图标层）；手动转 `WS_CHILD`（清 `WS_POPUP`）使其成为真正子窗、参与
///   宿主 Z 序；
/// - `SetWindowPos(HWND_BOTTOM, SWP_NOMOVE|SWP_NOSIZE)` 沉底——幂等，
///   show / WebView2 初始化等引起重排后可安全重复调用；
/// - 任何后续 `SetWindowPos` 都必须带 `SWP_NOZORDER`（见 `fit_monitor`），
///   否则 `HWND_TOP` 默认值会把壁纸窗顶回宿主上层、盖住图标。
pub fn ensure_bottom_zorder(hwnd: HWND, host: HWND) -> Result<(), String> {
    // 宿主已销毁（Explorer 重启等瞬态）：交由重建流程处理，此处只报错。
    if !unsafe { IsWindow(Some(host)).as_bool() } {
        return Err("wallpaper host is gone".into());
    }
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        if style & WS_CHILD.0 as isize == 0 {
            // popup → child：保留其余样式位（无标题栏/边框，无可见副作用）。
            let _ = SetWindowLongPtrW(
                hwnd,
                GWL_STYLE,
                (style & !(WS_POPUP.0 as isize)) | WS_CHILD.0 as isize,
            );
        }
    }
    unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_BOTTOM),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
    }
    .map_err(|e| format!("SetWindowPos(HWND_BOTTOM): {e}"))?;

    // 验证：壁纸窗之下（更靠底层）不应再有兄弟窗——宿主内只有我们与
    // 桌面图标层（DefView 链）。有残留则说明系统仍在调整窗口树，
    // 交给调用方（挂载时的 250ms 重试 / mod.rs 延迟复查）再钉一次。
    let next = unsafe { GetWindow(hwnd, GW_HWNDNEXT) };
    if let Ok(below) = next {
        if !below.is_invalid() {
            return Err(format!(
                "still not at bottom: sibling below = {:?}",
                below
            ));
        }
    }
    Ok(())
}

/// 判定窗口是否为宿主（WorkerW）的子窗口：枚举宿主子窗命中。
fn is_child_of(hwnd: HWND, host: HWND) -> bool {
    let mut m = Match { target: hwnd, found: false };
    unsafe {
        let _ = EnumChildWindows(
            Some(host),
            Some(is_target_proc),
            LPARAM(&mut m as *mut Match as isize),
        );
    }
    m.found
}

/// 匹配上下文（lparam 传递）。
#[repr(C)]
struct Match {
    target: HWND,
    found: bool,
}

/// EnumChildWindows 回调：匹配目标 hwnd。
unsafe extern "system" fn is_target_proc(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    let m = lparam.0 as *mut Match;
    if hwnd == unsafe { (*m).target } {
        unsafe {
            (*m).found = true;
        }
        return windows::core::BOOL::from(false); // 停止枚举
    }
    windows::core::BOOL::from(true)
}

/// 精确铺满主屏：补偿 Win11 无边框窗口的隐形内边框（实测约左/右 7px、
/// 上 1px——WebView 内容区不从窗口原点起算，否则左侧/顶部露空白条）。
/// 以 WebView 子窗（类名 `WRY_WEBVIEW`）在窗口内的位置为准外扩定位。
fn fit_monitor(win: &WebviewWindow) -> Result<(), String> {
    let hwnd = win.hwnd().map_err(|e| format!("get wallpaper hwnd: {e}"))?;
    unsafe {
        let mut outer = RECT::default();
        let _ = GetWindowRect(hwnd, &mut outer);
        let child = FindWindowExW(Some(hwnd), None, w!("WRY_WEBVIEW"), None).unwrap_or_default();
        if child.is_invalid() {
            // 子窗尚未创建（WebView2 异步初始化）：保持现有大小即可
            return Ok(());
        }
        let mut crect = RECT::default();
        let _ = GetWindowRect(child, &mut crect);
        // 子窗在窗口内的偏移（cl, ct）与右侧/底部余量
        let cl = crect.left - outer.left;
        let ct = crect.top - outer.top;
        let m_r = (outer.right - outer.left) - cl - (crect.right - crect.left);
        let m_b = (outer.bottom - outer.top) - ct - (crect.bottom - crect.top);
        let mon_w = GetSystemMetrics(SM_CXSCREEN);
        let mon_h = GetSystemMetrics(SM_CYSCREEN);
        let _ = SetWindowPos(
            hwnd,
            None,
            -cl,
            -ct,
            mon_w + cl + m_r,
            mon_h + ct + m_b,
            SWP_NOACTIVATE | SWP_NOZORDER, // 绝不扰动 Z 序（否则 HWND_TOP 盖图标）
        );
    }
    Ok(())
}

/// 点击穿透（须在主线程调用）：顶层 + WebView2 子窗口递归补
/// `WS_EX_TRANSPARENT`——子窗口不穿透则桌面图标无法点击，1s 后补一轮
/// （WebView2 子窗口异步创建）。
fn apply_click_through(win: &WebviewWindow) {
    let Ok(hwnd) = win.hwnd() else {
        return;
    };
    make_click_through(hwnd);
    let win = win.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(1));
        if let Ok(hwnd) = win.hwnd() {
            make_click_through(hwnd);
            unsafe {
                let _ = EnumChildWindows(Some(hwnd), Some(click_through_proc), LPARAM(0));
            }
        }
    });
}

/// 给单个窗口加点击穿透扩展样式（保留既有样式位，**不动** LAYERED——
/// 透明 WebView 的合成路径需要它）。
fn make_click_through(hwnd: HWND) {
    let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    unsafe {
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | CLICK_THROUGH_EX as isize);
    }
}

/// EnumChildWindows 回调：递归补穿透样式。
unsafe extern "system" fn click_through_proc(hwnd: HWND, _lparam: LPARAM) -> windows::core::BOOL {
    make_click_through(hwnd);
    windows::core::BOOL::from(true)
}

// ── WorkerW 宿主定位 ────────────────────────────────────────────────────────

/// 找壁纸宿主 WorkerW（按两种已知布局依次尝试）：
///
/// 1. **Win10/11 实测布局**（2024+ Explorer）：`SHELLDLL_DefView` 是 Progman
///    直接子窗，壁纸 WorkerW 是 Progman 的直接子窗（全屏）；
/// 2. 未找到时向 Progman 发 `WM_SPAWN_WORKERW` 唤醒后重找；
/// 3. **经典布局**兜底：含 `SHELLDLL_DefView` 子窗的顶层 WorkerW 之后的
///    下一个顶层 WorkerW 兄弟。
///
/// 注意：顶层还有大量其它应用的 136x38 隐藏 WorkerW，绝不能直接取顶层第一个。
fn find_wallpaper_host() -> Option<HWND> {
    // 方案一：Progman 的直接子窗 WorkerW。
    let progman = unsafe { FindWindowW(w!("Progman"), PCWSTR::null()) }.ok()?;
    if let Ok(host) = unsafe { FindWindowExW(Some(progman), None, w!("WorkerW"), None) } {
        return Some(host);
    }
    // 唤醒壁纸 WorkerW 后重找（幂等；参数与社区插件一致）。
    unsafe {
        let _ = SendMessageTimeoutW(
            progman,
            WM_SPAWN_WORKERW,
            WPARAM(0xD),
            LPARAM(0x1),
            SMTO_NORMAL,
            1000,
            None,
        );
    }
    if let Ok(host) = unsafe { FindWindowExW(Some(progman), None, w!("WorkerW"), None) } {
        return Some(host);
    }
    // 方案二：经典顶层布局（兜底）。
    let mut owner: Option<HWND> = None;
    unsafe {
        let _ = EnumWindows(
            Some(find_workerw_proc),
            LPARAM(&mut owner as *mut Option<HWND> as isize),
        );
    }
    let owner = owner?;
    match unsafe { FindWindowExW(None, Some(owner), w!("WorkerW"), None) } {
        Ok(host) => Some(host),
        Err(_) => None,
    }
}

/// EnumWindows 回调：匹配含 `SHELLDLL_DefView` 子窗的 WorkerW。
unsafe extern "system" fn find_workerw_proc(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    if class_name(hwnd) == "WorkerW" {
        if let Ok(_defview) = unsafe { FindWindowExW(Some(hwnd), None, w!("SHELLDLL_DefView"), None) }
        {
            let slot = lparam.0 as *mut Option<HWND>;
            unsafe {
                *slot = Some(hwnd);
            }
            return windows::core::BOOL::from(false); // 停止枚举
        }
    }
    windows::core::BOOL::from(true)
}

/// 窗口类名（截断容错，失败返回空串）。
fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { windows::Win32::UI::WindowsAndMessaging::GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}
