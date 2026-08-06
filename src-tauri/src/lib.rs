// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(deprecated)]

use crate::config::Config;
use crate::db::Database;
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tracing::{info, error};

mod config;
mod crypto;
mod db;
mod error;
mod gateway;
mod http;

mod api;
mod keys;
mod middleware;
mod models;
mod plugin;
mod providers;
mod search;
mod types;

use gateway::server::AppState;

/// 开机自启（autostart 登录项）携带的参数：命中时启动不显示窗口，静默驻留托盘。
/// 必须与 `tauri_plugin_autostart::init` 的 args 保持一致（见下方 Builder 配置）。
const ARG_MINIMIZED: &str = "--minimized";

// ── Autostart Tauri commands ──
#[tauri::command]
fn get_autostart_status(app: tauri::AppHandle) -> bool {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().unwrap_or(false)
}

/// 返回本机 gateway 的客户端可连接地址（前端 Claude FM 音频 + API 调用用）。
/// `Config.host` 可能是 `0.0.0.0`（bind 通配地址，客户端不可直连），
/// 故对 `0.0.0.0` / `::` 统一返回 `127.0.0.1`。
#[tauri::command]
fn get_gateway_base(app: tauri::AppHandle) -> String {
    let state = app.state::<Arc<AppState>>();
    let cfg = &state.config;
    let host = if cfg.host == "0.0.0.0" || cfg.host == "::" {
        "127.0.0.1"
    } else {
        &cfg.host
    };
    format!("http://{}:{}", host, cfg.port)
}

#[tauri::command]
fn set_autostart(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    if enabled {
        app.autolaunch().enable().map_err(|e| e.to_string())
    } else {
        app.autolaunch().disable().map_err(|e| e.to_string())
    }
}

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
        ("Show Window", "Quit", "Claude FM")
    } else {
        ("显示窗口", "退出", "Claude FM")
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

/// 前端 Claude FM 预热完成时调用：把 FM 勾选项加入托盘菜单。
/// 预热完成前 FM 菜单项隐藏，避免在音源未就绪时误操作。
#[tauri::command]
fn fm_ready(app: tauri::AppHandle) -> Result<(), String> {
    let state_guard = app.state::<std::sync::Mutex<TrayMenuState>>();
    let mut state = state_guard.lock().map_err(|e| e.to_string())?;
    if state.fm_item.is_some() {
        return Ok(()); // 已加入过，幂等
    }
    // 重建托盘菜单：显示窗口 / Claude FM / 退出
    // 用缓存 locale（不依赖 AppState——启动早期 AppState 尚未 manage）
    let (_, _, fm_txt) = tray_texts(&state.locale);
    let fm_item = tauri::menu::CheckMenuItem::with_id(
        &app,
        "fm",
        fm_txt,
        true,
        false,
        None::<&str>,
    )
    .map_err(|e| e.to_string())?;
    let menu = tauri::menu::Menu::with_items(&app, &[&state.show_item, &fm_item, &state.quit_item])
        .map_err(|e| e.to_string())?;
    state.fm_item = Some(fm_item.clone());
    app.tray_by_id("main-tray")
        .ok_or_else(|| "tray not found".to_string())?
        .set_menu(Some(menu))
        .map_err(|e| e.to_string())?;
    info!("fm_ready: Claude FM menu item added to tray");
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
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
        .invoke_handler(tauri::generate_handler![
            get_autostart_status,
            set_autostart,
            set_locale,
            fm_set_playing,
            fm_ready,
            get_gateway_base
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
            let app_state = Arc::new(AppState::new(config.clone(), database.clone(), master_key));
            app.manage(app_state.clone());

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
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    // Claude FM：勾选 = 播放，取消勾选 = 暂停。
                    // 勾选状态由点击事件自行翻转（CheckMenuItem 原生行为），
                    // 播放器随后 emit fm-toggle 事件驱动前端 toggle。
                    "fm" => {
                        info!("tray: fm clicked");
                        let _ = app.emit("fm-toggle", ());
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;

            // Start gateway server in Tauri's async runtime
            let state = app_state.clone();
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Spawn FM radio engine before starting gateway (engine needs app_handle for fm-meta events)
                state.fm.clone().spawn(app_handle);
                if let Err(e) = gateway::server::start_gateway(state).await {
                    error!("Gateway server failed: {}", e);
                }
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            // Hide to tray on close instead of quitting, so the gateway keeps running.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
