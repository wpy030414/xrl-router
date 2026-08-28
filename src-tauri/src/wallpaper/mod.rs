//! 桌面壁纸劫持：把 FM 像素艺术渲染到系统桌面层（切换/暂停与主窗口严格同步）。
//!
//! # 方案（见 docs/DECISIONS.md ADR-041/043）
//!
//! 动态创建第二个 WebviewWindow（label=`wallpaper`，`transparent(true)` +
//! `initialization_script` 注入 `__WALLPAPER_MODE__`，前端分支渲染
//! `WallpaperScene`——黑底全屏像素、无按钮/歌曲信息），随后挂入桌面壁纸层：
//!
//! - **Windows**：`tauri-plugin-desktop-underlay`（社区插件）`set_desktop_underlay`
//!   ——窗口 SetParent 进壁纸 WorkerW。**透明窗口是关键**：WebView2 内容经
//!   DWM 视觉合成上屏，是桌面 WorkerW 层唯一可靠渲染路径（GDI/重定向
//!   表面在桌面层不被合成；`WS_EX_LAYERED` 不设属性则不显示内容）。
//!   点击穿透为 `WS_EX_TRANSPARENT`（禁 LAYERED），见 `win.rs`。
//! - **macOS**：`macos.rs` objc2 `setLevel(kCGDesktopIconWindowLevel)` +
//!   `orderFront:`（不经 tao show 以免抢焦点）。
//!
//! # 时钟同步
//!
//! 像素场景动画时钟为**引擎权威**（`fm.rs` 的 `scene_t`，仅播放时按真实流逝
//! 累计、暂停冻结）：主窗口与壁纸窗口的 `PixelScene` 都以
//! `invoke('fm_scene_t')` 采样（采样失败回退本地 dt），天然同步。
//!
//! # 生命周期
//!
//! - 启用：建窗 → 尺寸铺主屏 → 主线程屏障挂载 → show；幂等。
//! - 取消：`enabled=false` 先行，再 `close()` 销毁；`on_window_event` 只拦截
//!   label == "main" 的关闭，壁纸窗口可正常销毁。
//! - 自愈：`WindowEvent::Destroyed`（Explorer 重启等）→ 清槽位 → 1s 复查重建；
//!   建窗失败发现残留同 label 窗口 → `destroy()` 强杀重试。
//! - 持久化：DB settings `wallpaper_enabled`（lib.rs 写入），启动时惰性恢复
//!   （延迟 2s + 重试，等主窗口 WebView2 初始化完成，避免 0x8007139F）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent};

#[cfg(target_os = "windows")]
mod win;
#[cfg(target_os = "macos")]
mod macos;

/// 壁纸窗口 label（capabilities/default.json 的 `windows` 列表需包含它）。
pub const WALLPAPER_LABEL: &str = "wallpaper";

/// DB settings 键：壁纸劫持勾选态（"true"/"false"）。
pub const SETTING_KEY: &str = "wallpaper_enabled";

/// 壁纸引擎运行时状态（`app.manage`）。
#[derive(Default)]
pub struct WallpaperState {
    /// 用户意图（是否启用）；重建逻辑以此复查，不随窗口销毁而翻转。
    enabled: Arc<AtomicBool>,
    /// 壁纸窗口槽位（销毁后清空，供重建复查"窗口是否已存在"）。
    window: Mutex<Option<WebviewWindow>>,
}

impl WallpaperState {
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// 启用壁纸劫持（任意线程可调；内部经主线程屏障建窗挂载）。
    pub fn enable(&self, app: &AppHandle) -> Result<(), String> {
        if !supported() {
            return Err("desktop wallpaper is only supported on Windows and macOS".into());
        }
        // 幂等：窗口已存在则仅置位。
        {
            let guard = self
                .window
                .lock()
                .map_err(|e| format!("wallpaper state poisoned: {e}"))?;
            if guard.is_some() {
                self.enabled.store(true, Ordering::SeqCst);
                return Ok(());
            }
        }

        let win = match create_window(app) {
            Ok(w) => w,
            Err(e) => {
                // 上一轮失败可能残留半死窗口：destroy 强杀（不走 CloseRequested）
                // 后轮询注册表（至多 5s），就绪后重试一次。
                if let Some(stale) = app.get_webview_window(WALLPAPER_LABEL) {
                    let _ = stale.destroy();
                    for _ in 0..50 {
                        std::thread::sleep(Duration::from_millis(100));
                        if app.get_webview_window(WALLPAPER_LABEL).is_none() {
                            break;
                        }
                    }
                    if app.get_webview_window(WALLPAPER_LABEL).is_some() {
                        return Err(format!("{e} (stale wallpaper window did not close in time)"));
                    }
                    create_window(app).map_err(|e2| format!("{e}; retry after close failed: {e2}"))?
                } else {
                    return Err(e);
                }
            }
        };
        // 尺寸铺满主屏（物理像素）。
        if let Err(e) = size_to_primary(&win) {
            let _ = win.close();
            return Err(format!("size_to_primary: {e}"));
        }
        // 平台挂载（AppKit / Win32 窗口操作必须在主线程）。
        let mount_win = win.clone();
        if let Err(e) = run_on_main(app, move || crate::wallpaper::mount(&mount_win)) {
            let _ = win.close();
            return Err(format!("platform attach: {e}"));
        }
        // 展示：Windows 经 tao show（focused(false) 保证不激活）；
        // macOS 已在 mount 内 orderFront（tao show 走 makeKeyAndOrderFront 抢焦点）。
        show_wallpaper(&win).map_err(|e| format!("show wallpaper window: {e}"))?;

        self.enabled.store(true, Ordering::SeqCst);
        self.register_rebuild(app, &win);
        {
            let mut guard = self
                .window
                .lock()
                .map_err(|e| format!("wallpaper state poisoned: {e}"))?;
            if guard.is_some() {
                let _ = win.close();
                return Ok(());
            }
            *guard = Some(win);
        }
        Ok(())
    }

    /// 取消壁纸劫持：先置位再销毁；Destroyed 回调见 `register_rebuild`，
    /// 因 `enabled=false` 不会触发重建。
    pub fn disable(&self) {
        self.enabled.store(false, Ordering::SeqCst);
        let win = self.window.lock().ok().and_then(|mut guard| guard.take());
        if let Some(win) = win {
            let _ = win.close();
        }
    }

    /// 注册窗口 Destroyed 监听：外部销毁（Explorer 重启等）后延迟重建。
    fn register_rebuild(&self, app: &AppHandle, win: &WebviewWindow) {
        let app = app.clone();
        win.on_window_event(move |event| {
            if let WindowEvent::Destroyed = event {
                if let Some(state) = app.try_state::<WallpaperState>() {
                    // 清槽位：窗口已销毁，句柄失效。
                    if let Ok(mut guard) = state.window.lock() {
                        guard.take();
                    }
                    if state.is_enabled() {
                        let app = app.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(Duration::from_secs(1));
                            if let Some(state) = app.try_state::<WallpaperState>() {
                                if !state.is_enabled() {
                                    return;
                                }
                                if let Err(e) = state.enable(&app) {
                                    tracing::warn!("wallpaper rebuild failed: {e}");
                                }
                            }
                        });
                    }
                }
            }
        });
    }
}

/// 平台是否支持桌面壁纸劫持（目前 Windows / macOS）。
pub fn supported() -> bool {
    cfg!(windows) || cfg!(target_os = "macos")
}

// ── 平台分派 ────────────────────────────────────────────────────────────────

/// 把壁纸窗口挂入桌面壁纸层（运行在主线程）。
#[cfg(target_os = "windows")]
fn mount(win: &WebviewWindow) -> Result<(), String> {
    crate::wallpaper::win::mount(win)
}

/// 把壁纸窗口挂入桌面壁纸层（运行在主线程）。
#[cfg(target_os = "macos")]
fn mount(win: &WebviewWindow) -> Result<(), String> {
    crate::wallpaper::macos::mount(win)
}

/// 展示壁纸窗口：Windows 走 tao（focused(false) 已加 WS_EX_NOACTIVATE）。
#[cfg(target_os = "windows")]
fn show_wallpaper(win: &WebviewWindow) -> Result<(), String> {
    win.show().map_err(|e| format!("show wallpaper window: {e}"))
}

/// 展示壁纸窗口：macOS 禁止用 tao show（抢焦点），mount 内已 orderFront。
#[cfg(target_os = "macos")]
fn show_wallpaper(_win: &WebviewWindow) -> Result<(), String> {
    Ok(())
}

// ── 窗口创建 / 主线程屏障 ────────────────────────────────────────────────────

/// 创建隐藏的壁纸 WebviewWindow（透明 + 壁纸模式标志，加载同一前端入口）。
fn create_window(app: &AppHandle) -> Result<WebviewWindow, String> {
    let mut builder = WebviewWindowBuilder::new(
        app,
        WALLPAPER_LABEL,
        WebviewUrl::App("index.html".into()),
    )
    .title("")
    .decorations(false)
    .resizable(false)
    .maximizable(false)
    .skip_taskbar(true)
    // focused(false) → Windows 加 WS_EX_NOACTIVATE / macOS 不走 makeKeyAndOrderFront
    .focused(false)
    .visible(false)
    .inner_size(8.0, 8.0)
    .background_color(tauri::window::Color(0, 0, 0, 255))
    // 透明窗口是桌面层渲染的关键：WebView 内容经 DWM 视觉合成上屏
    .transparent(true)
    .initialization_script("window.__WALLPAPER_MODE__ = true;");
    #[cfg(target_os = "windows")]
    {
        // WebView2 专用配置：显式关闭 Chromium 原生遮挡检测（WorkerW 子窗会被
        // 误判为"完全遮挡"而暂停合成）+ 独立 user data directory（同目录环境
        // 共享浏览器进程、参数必须一致，否则 0x8007139F）。
        let data_dir = app
            .path()
            .app_data_dir()
            .map(|d| d.join("wallpaper-webview2"))
            .unwrap_or_else(|_| std::path::PathBuf::from("wallpaper-webview2"));
        builder = builder
            .data_directory(data_dir)
            .additional_browser_args("--disable-features=CalculateNativeWinOcclusion");
    }
    builder.build().map_err(|e| format!("create wallpaper window: {e}"))
}

/// 窗口尺寸/位置 = 主显示器（物理像素，由 tauri 处理 DPI）。
fn size_to_primary(win: &WebviewWindow) -> Result<(), String> {
    let monitor = win
        .primary_monitor()
        .map_err(|e| format!("query primary monitor: {e}"))?
        .ok_or_else(|| "no primary monitor".to_string())?;
    win.set_size(*monitor.size())
        .map_err(|e| format!("set window size: {e}"))?;
    win.set_position(*monitor.position())
        .map_err(|e| format!("set window position: {e}"))?;
    Ok(())
}

/// 在主线程同步执行闭包（AppKit / Win32 窗口 API 必须）。
/// 调用方不得位于主线程（lib.rs setup 的恢复路径请走延迟线程）。
fn run_on_main<T: Send + 'static>(
    app: &AppHandle,
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<Result<T, String>>(1);
    app.run_on_main_thread(move || {
        let _ = tx.send(f());
    })
    .map_err(|e| format!("run_on_main_thread: {e}"))?;
    rx.recv_timeout(Duration::from_secs(10))
        .map_err(|_| "main thread barrier timeout".to_string())?
}
