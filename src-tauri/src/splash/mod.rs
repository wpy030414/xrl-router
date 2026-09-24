//! 冷启动环形加载动画 — 纯 Rust 原生渲染，不依赖 WebView。
//!
//! 在 Tauri build() 阻塞 WebView2/WKWebView 初始化期间保持流畅动画，
//! 前端 React 首次渲染后通过 `app-ready` 事件触发关闭。

pub(crate) mod render;

#[cfg(target_os = "windows")]
mod win;
#[cfg(target_os = "windows")]
use win::WinSplash as PlatformSplash;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos::MacSplash as PlatformSplash;

use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) struct SplashScreen {
    inner: PlatformSplash,
    closed: AtomicBool,
}

impl SplashScreen {
    /// 创建并显示环形动画窗口。失败返回 None（不影响正常启动）。
    pub(crate) fn new() -> Option<Self> {
        PlatformSplash::new().map(|inner| Self { inner, closed: AtomicBool::new(false) })
    }

    /// 关闭动画窗口。幂等。
    pub(crate) fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.inner.close();
        }
    }

    /// 将环形动画吸附到主窗口**客户区**的严格中心。
    ///
    /// `"center": true` 只在创建期对装饰外框做工作区居中；Windows 在
    /// setup 里 `set_decorations(false)` 去掉标题栏后，用户实际看到的
    /// 窗口中心是客户区中心——因此按实测 `inner_position`/`inner_size`
    /// 对齐，不复刻 Tauri 的边框换算（DPI / 任务栏位置 / 多屏均免疫）。
    pub(crate) fn recenter_to_window(&self, window: &tauri::WebviewWindow) {
        if let (Ok(pos), Ok(size)) = (window.inner_position(), window.inner_size()) {
            self.inner.recenter(
                pos.x + size.width as i32 / 2,
                pos.y + size.height as i32 / 2,
            );
        }
    }
}

impl Drop for SplashScreen {
    fn drop(&mut self) { self.close(); }
}