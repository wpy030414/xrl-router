// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use crate::config::Config;
use crate::db::Database;
use std::sync::Arc;
use tauri::Manager;
use tracing::{info, error};

mod config;
mod crypto;
mod db;
mod error;
mod gateway;
mod http;

mod api;
mod keys;
mod local;
mod mcp;
mod middleware;
mod models;
mod plugin;
mod providers;
// pub：MCP 工具模块复用 SearchHttp / search()
pub mod search;
mod types;
// 桌面壁纸劫持（FM 像素艺术 → 桌面层；透明 WebView + 社区插件挂载）
mod wallpaper;
// 系统资源监控（CPU/内存/显存占用）
mod system;

// SDK 合规验证（fixtures 导出 + Python 官方 SDK 校验脚本）。
// 本目录仅 test 构建；正式构建不编译任何测试代码。
#[cfg(test)]
#[path = "sdk_test/fixtures.rs"]
mod sdk_fixtures;

use gateway::server::AppState;

/// 开机自启（autostart 登录项）携带的参数：命中时启动不显示窗口，静默驻留托盘。
/// 必须与 `tauri_plugin_autostart::init` 的 args 保持一致（见下方 Builder 配置）。
const ARG_MINIMIZED: &str = "--minimized";

// ── i18n（托盘菜单文本随前端语言切换） ──

/// 托盘菜单项句柄（前端切换语言时 set_locale 更新文本）。
struct TrayMenuState {
    show_item: tauri::menu::MenuItem<tauri::Wry>,
    quit_item: tauri::menu::MenuItem<tauri::Wry>,
    /// 当前 locale（持久化于 DB；此处缓存避免 fm_ready 依赖 AppState，
    /// 启动早期 AppState 尚未 manage 时会 panic）。
    locale: String,
    /// Claude FM 勾选项：勾选 = 播放，取消 = 暂停。
    /// 预热完成（fm_ready）后才加入菜单，初始为 None（隐藏）。
    fm_item: Option<tauri::menu::CheckMenuItem<tauri::Wry>>,
}

/// 按 locale 返回托盘菜单文本（zh-CN 默认）。
fn tray_texts(locale: &str) -> (&'static str, &'static str, &'static str) {
    if locale == "en" {
        ("Show Window", "Quit", "FM")
    } else {
        ("显示窗口", "退出", "FM")
    }
}

/// 前端切换语言时调用：持久化 locale 并更新托盘菜单文本。
#[tauri::command]
fn set_locale(app: tauri::AppHandle, locale: String) -> Result<(), String> {
    use tauri::Manager;
    let state = app.state::<Arc<AppState>>();
    state
        .database
        .set_setting("locale", &locale)
        .map_err(|e| e.to_string())?;
    let (show_txt, quit_txt, fm_txt) = tray_texts(&locale);
    let menu_state = app.state::<std::sync::Mutex<TrayMenuState>>();
    let mut menu_state = menu_state.lock().map_err(|e| e.to_string())?;
    let _ = menu_state.show_item.set_text(show_txt);
    let _ = menu_state.quit_item.set_text(quit_txt);
    if let Some(fm) = &menu_state.fm_item {
        let _ = fm.set_text(fm_txt);
    }
    // 更新缓存 locale（供 fm_ready 使用，避免依赖 AppState）
    menu_state.locale = locale;
    Ok(())
}

/// 前端 Claude FM 播放状态变化时调用：同步托盘菜单勾选。
/// 勾选 = 播放中；取消勾选 = 已暂停。
#[tauri::command]
fn fm_set_playing(app: tauri::AppHandle, playing: bool) -> Result<(), String> {
    let state = app.state::<std::sync::Mutex<TrayMenuState>>();
    let state = state.lock().map_err(|e| e.to_string())?;
    if let Some(fm) = &state.fm_item {
        fm.set_checked(playing).map_err(|e| e.to_string())
    } else {
        Ok(())
    }
}

/// 切换 FM 播放/暂停（前端 / 托盘 / 系统媒体键均可调用）。
#[tauri::command]
fn fm_toggle(app: tauri::AppHandle) -> Result<(), String> {
    let state = app.state::<Arc<AppState>>();
    state.fm.toggle();
    Ok(())
}

/// 获取 FM 播放状态快照（前端初始化时调用）。
#[tauri::command]
fn fm_get_state(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    let state = app.state::<Arc<AppState>>();
    let ps = state.fm.get_state();
    Ok(serde_json::json!({
        "playing": ps.playing,
        "ready": ps.ready,
        "artist": ps.artist,
        "title": ps.title,
        "index": ps.index,
    }))
}

/// 把 FM 勾选项加入托盘菜单（Rust 侧直接调用，不依赖前端中转）。
/// 幂等：重复调用不会重复添加。
pub(crate) fn add_fm_menu_item(app: &tauri::AppHandle) -> Result<(), String> {
    let state_guard = app.state::<std::sync::Mutex<TrayMenuState>>();
    let mut state = state_guard.lock().map_err(|e| e.to_string())?;
    if state.fm_item.is_some() {
        return Ok(()); // 已加入过，幂等
    }
    // 重建托盘菜单：显示窗口 / Claude FM / 退出
    let (_, _, fm_txt) = tray_texts(&state.locale);
    let fm_item = tauri::menu::CheckMenuItem::with_id(
        app,
        "fm",
        fm_txt,
        true,
        false,
        None::<&str>,
    )
    .map_err(|e| e.to_string())?;
    let menu = tauri::menu::Menu::with_items(app, &[&state.show_item, &fm_item, &state.quit_item])
        .map_err(|e| e.to_string())?;
    state.fm_item = Some(fm_item.clone());
    app.tray_by_id("main-tray")
        .ok_or_else(|| "tray not found".to_string())?
        .set_menu(Some(menu))
        .map_err(|e| e.to_string())?;
    info!("fm_ready: Claude FM menu item added to tray");
    Ok(())
}

/// 前端 Claude FM 预热完成时调用：把 FM 勾选项加入托盘菜单。
/// 预热完成前 FM 菜单项隐藏，避免在音源未就绪时误操作。
#[tauri::command]
fn fm_ready(app: tauri::AppHandle) -> Result<(), String> {
    add_fm_menu_item(&app)
}

// ── 像素场景动画时钟（主窗口与壁纸窗口共用采样源） ──

/// 当前像素场景动画时钟（秒）。`PixelScene` 的 `sampleT` 每帧采样，
/// 保证主窗口与桌面壁纸窗口的动画相位完全一致。
#[tauri::command]
fn fm_scene_t(app: tauri::AppHandle) -> Result<f64, String> {
    let state = app.state::<Arc<AppState>>();
    Ok(state.fm.scene_t())
}

// ── 桌面壁纸劫持（FM 像素艺术 → 桌面层） ──

/// 启用/禁用桌面壁纸劫持：动态创建/销毁 "wallpaper" 窗口并写入 DB 供重启恢复。
/// 返回实际状态（前端据此设置勾选态）。
#[tauri::command]
async fn wallpaper_set(app: tauri::AppHandle, enabled: bool) -> Result<bool, String> {
    {
        let state = app.state::<wallpaper::WallpaperState>();
        if enabled {
            match state.enable(&app) {
                Ok(()) => info!("wallpaper: enabled"),
                Err(e) => {
                    tracing::warn!("wallpaper: enable failed: {e}");
                    return Err(e);
                }
            }
        } else {
            info!("wallpaper: disabled");
            state.disable();
        }
    }
    // 持久化：窗口已生效，DB 写失败仅告警（避免把实际状态回滚成冲突）。
    let state = app.state::<Arc<AppState>>();
    if let Err(e) = state.database.set_setting(
        wallpaper::SETTING_KEY,
        if enabled { "true" } else { "false" },
    ) {
        tracing::warn!("wallpaper_set: failed to persist setting: {e}");
    }
    Ok(app.state::<wallpaper::WallpaperState>().is_enabled())
}

/// 桌面壁纸劫持状态（勾选态 + 平台支持情况，前端据此决定菜单可见性）。
#[tauri::command]
fn wallpaper_get_state(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    let state = app.state::<wallpaper::WallpaperState>();
    Ok(serde_json::json!({
        "enabled": state.is_enabled(),
        "supported": wallpaper::supported(),
    }))
}

/// 获取系统资源使用情况（CPU/内存/显存占用）
#[tauri::command]
fn get_system_resources(app: tauri::AppHandle) -> Result<system::SystemResources, String> {
    let state = app.state::<std::sync::Mutex<system::SystemMonitor>>();
    let monitor = state.lock().map_err(|e| e.to_string())?;
    Ok(monitor.get_resources())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 全进程统一 WebView2 浏览器参数：关闭 Chromium 原生窗口遮挡检测。
    // Windows 壁纸已改 GDI 直绘（无 WebView），此参数仅为存在 WebView 的
    // 场景兜底（macOS 壁纸 WebView 方案在 WorkerW/桌面层同样会被误判遮挡）。
    // 必须经环境变量在任何 WebView 创建前注入——同一 user data folder 的
    // 环境共享浏览器进程，不同参数会冲突（0x8007139F）。
    #[cfg(target_os = "windows")]
    std::env::set_var(
        "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
        "--disable-features=CalculateNativeWinOcclusion",
    );

    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .json()
        .init();

    // Load configuration (env vars only — actual paths resolved in setup())
    let config = Config::from_env();
    info!("xrl-router starting...");
    info!("Server port: {}", config.port);

    // 开机自启时系统以 `xrl-router --minimized` 拉起进程；setup 里据此隐藏窗口。
    // 手动启动（无该参数）不受影响，正常弹出窗口。
    let silent_start = std::env::args().any(|a| a == ARG_MINIMIZED);
    if silent_start {
        info!("Silent start (--minimized): window will be hidden to tray");
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![ARG_MINIMIZED]),
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            set_locale,
            fm_set_playing,
            fm_toggle,
            fm_get_state,
            fm_ready,
            fm_scene_t,
            wallpaper_set,
            wallpaper_get_state,
            get_system_resources,
        ])
        .setup(move |app| {
            // Resolve data directory using Tauri's path API:
            //   macOS: ~/Library/Application Support/im.xrl.router/
            //   Linux: ~/.config/im.xrl.router/
            //   Windows: C:\Users\<user>\AppData\Roaming\im.xrl.router\
            let data_dir = app.path().app_data_dir()
                .expect("无法获取应用数据目录");
            std::fs::create_dir_all(&data_dir).ok();
            info!("Data directory: {}", data_dir.display());

            // 静默启动（开机自启 --minimized）：窗口隐藏到托盘，网关照常运行。
            // setup 在窗口首次绘制前执行，hide 无闪烁。
            if silent_start {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                    info!("Window hidden (silent start)");
                }
            }

            // ── 移除窗口装饰（Windows 去标题栏） ──
            // macOS: titleBarStyle: "Overlay" 已保留红绿灯，不能调 set_decorations(false)
            //        ——否则连圆角 + 红绿灯一起干掉（主人说窗口圆角消失了就是这个原因）
            // Windows: 必须 set_decorations(false) 才能移除原生标题栏
            #[cfg(target_os = "windows")]
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_decorations(false);
                info!("Window decorations removed (Windows)");
            }

            let db_path = data_dir.join("xrl-router.db");
            let master_key_path = data_dir.join("master.key");

            info!("Database path: {}", db_path.display());

            // Load or create master key (encrypts Provider API keys at rest)
            let master_key = crypto::load_or_create_master_key(&master_key_path)
                .map_err(|e| {
                    error!("Failed to initialize master key: {}", e);
                    Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
                })?;

            // Initialize database
            let database = Database::new(&db_path)
                .map_err(|e| {
                    error!("Failed to open database: {}", e);
                    Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
                })?;

            // Run migrations
            database.migrate().map_err(|e| {
                error!("Failed to run database migrations: {}", e);
                Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
            })?;

            // Create shared application state with all registries
            let app_state = Arc::new(AppState::new(config.clone(), database.clone(), master_key, &data_dir));
            app.manage(app_state.clone());

            // ── 桌面壁纸劫持（FM 像素艺术 → 桌面层）──
            // 状态管理 + 启动恢复：上次勾选过则重建壁纸窗口。
            app.manage(wallpaper::WallpaperState::default());

            // ── 系统资源监控（CPU/内存/显存占用）──
            // 为前端提供实时系统资源使用情况
            app.manage(std::sync::Mutex::new(system::SystemMonitor::new()));
            if database
                .get_setting(wallpaper::SETTING_KEY)
                .ok()
                .flatten()
                .as_deref()
                == Some("true")
            {
                info!("Restoring desktop wallpaper (wallpaper_enabled=true)");
                // 惰性恢复：setup 期间事件循环尚未泵送，建窗必须在循环就绪后。
                // 主窗口 WebView2 初始化未完成时第二 webview 会报 0x8007139F
                // （组/资源状态错误）——延迟 2s + 重试 3 次兜底。
                let app_handle = app.handle().clone();
                std::thread::spawn(move || {
                    for attempt in 1..=3 {
                        std::thread::sleep(std::time::Duration::from_millis(2000));
                        let Some(state) = app_handle.try_state::<wallpaper::WallpaperState>() else {
                            return;
                        };
                        match state.enable(&app_handle) {
                            Ok(()) => {
                                info!("Restored desktop wallpaper");
                                return;
                            }
                            Err(e) => {
                                error!("Failed to restore desktop wallpaper (attempt {attempt}): {e}");
                            }
                        }
                    }
                });
            }

            // MCP 工具模块需要全局 AppState 引用（SearchHttp / 开关 / 渲染层），
            // ServerHandler 深处拿不到 axum State，启动时注入一次。
            // AppHandle 供 web_fetch 的 WebView 渲染层创建隐藏窗口。
            crate::mcp::init(app_state.clone(), app.handle().clone());

            // Pass Tauri AppHandle to PluginManager so it can emit events to frontend
            app_state.plugins.set_app_handle(app.handle().clone());

            // System tray: keep the gateway alive when the window is closed.
            // 菜单文本按持久化 locale 初始化（前端切换语言时经 set_locale 更新）。
            let locale = database
                .get_setting("locale")
                .ok()
                .flatten()
                .unwrap_or_else(|| "zh-CN".to_string());
            let (show_txt, quit_txt, _fm_txt) = tray_texts(&locale);
            let show_item =
                tauri::menu::MenuItem::with_id(app, "show", show_txt, true, None::<&str>)?;
            let quit_item =
                tauri::menu::MenuItem::with_id(app, "quit", quit_txt, true, None::<&str>)?;
            // Claude FM 勾选项：预热完成（fm_ready）后才加入菜单，初始隐藏。
            app.manage(std::sync::Mutex::new(TrayMenuState {
                show_item: show_item.clone(),
                quit_item: quit_item.clone(),
                locale: locale.clone(),
                fm_item: None,
            }));
            let menu = tauri::menu::Menu::with_items(app, &[&show_item, &quit_item])?;
            let _tray = tauri::tray::TrayIconBuilder::with_id("main-tray")
                .icon(app.default_window_icon().cloned().unwrap())
                .tooltip("xrl-router")
                .menu(&menu)
                .on_tray_icon_event(|tray, event| {
                    // 左键单击唤起主窗口；右键保持默认菜单行为（系统弹出上下文菜单）。
                    if let tauri::tray::TrayIconEvent::Click {
                        button: tauri::tray::MouseButton::Left,
                        button_state: tauri::tray::MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                })
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    // Claude FM：直接调用引擎 toggle（不再绕前端中转）。
                    // 勾选状态由 fm-state-changed 事件同步。
                    "fm" => {
                        info!("tray: fm clicked");
                        let state = app.state::<Arc<AppState>>();
                        state.fm.toggle();
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;

            // ── 系统媒体控制（souvlaki）初始化 ──
            // souvlaki 在主线程创建 MediaControls。macOS 的 MPRemoteCommandCenter /
            // MPNowPlayingInfoCenter 必须在主线程调用，故 MediaControls 存入 managed
            // state（`MediaControlsState`），引擎线程经 `run_on_main_thread` dispatch
            // 到这里访问。
            // Windows SMTC：souvlaki 的 `MediaControls::new` 对 hwnd=None 会 expect
            // panic（无 stub 降级），必须传真实窗口 HWND（tauri::WebviewWindow::hwnd）。
            #[cfg(target_os = "windows")]
            let hwnd: Option<*mut std::ffi::c_void> = app
                .get_webview_window("main")
                .and_then(|w| w.hwnd().ok())
                .map(|h| h.0 as *mut std::ffi::c_void);
            #[cfg(not(target_os = "windows"))]
            let hwnd: Option<*mut std::ffi::c_void> = None;
            let media_controls = souvlaki::MediaControls::new(souvlaki::PlatformConfig {
                display_name: "Claude FM",
                dbus_name: "im.xrl.router",
                hwnd,
            });
            let mut media_controls = match media_controls {
                Ok(mut ctrl) => {
                    let control_tx = app_state.fm.control_tx_clone();
                    let _ = ctrl.attach(move |event: souvlaki::MediaControlEvent| {
                        match event {
                            souvlaki::MediaControlEvent::Toggle => {
                                let _ = control_tx.send(crate::api::handlers::fm::FmControl::Toggle);
                            }
                            souvlaki::MediaControlEvent::Play => {
                                let _ = control_tx.send(crate::api::handlers::fm::FmControl::Play);
                            }
                            souvlaki::MediaControlEvent::Pause => {
                                let _ = control_tx.send(crate::api::handlers::fm::FmControl::Pause);
                            }
                            _ => {}
                        }
                    });
                    Some(ctrl)
                }
                Err(e) => {
                    tracing::warn!("Failed to initialize system media controls: {}", e);
                    None
                }
            };
            app.manage(crate::api::handlers::fm::MediaControlsState(
                std::sync::Mutex::new(media_controls.take()),
            ));

            // Start gateway server in Tauri's async runtime
            let state = app_state.clone();
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Spawn FM radio engine before starting gateway (engine needs app_handle for fm-meta events)
                state.fm.clone().spawn(app_handle.clone());
                if let Err(e) = gateway::server::start_gateway(state.clone()).await {
                    error!("Gateway server failed: {}", e);
                }
                // 本地模型 autostart：网关就绪后启动标记了自动启动的本地引擎
                state.local.auto_start_all().await;
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            // Hide to tray on close instead of quitting, so the gateway keeps running.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    let _ = window.hide();
                    api.prevent_close();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app_handle, event| match event {
            // ── macOS Dock 图标点击：从托盘唤起主窗口 ──
            // macOS 上窗口被 hide() 后点击 Dock 图标会触发 Reopen 事件，
            // 但 Tauri 默认不处理；必须手动 show + set_focus。
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => {
                if let Some(w) = app_handle.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            _ => {}
        });
}
