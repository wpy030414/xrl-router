//! Claude FM — 广播电台引擎（后端直接播放）。
//!
//! 音频解码与播放由 rodio 在后端完成，直接输出到系统音频设备，消除前端 `<audio>`
//! 缓冲层导致的同步误差。系统媒体控制（souvlaki）接入 macOS Now Playing / Windows
//! SMTC / Linux MPRIS。前端仅负责展示元信息和控制播放暂停。
//!
//! # Radio 模式
//!
//! 电台永远在广播——「暂停」不是停止时间轴，而是 `sink.set_volume(0.0)` 静音：
//! 音频继续按实时速率被消费，`sink.empty()` 照常触发切歌。恢复播放时可能已经
//! 切到下一首了，与真正的收音机一致。首次播放时用点击时刻的墙钟位置 re-anchor
//! （修正启动下载的 1~3s 漂移），之后时间轴与墙钟严格对齐。
//!
//! # 数据流
//!
//! ```text
//! FmEngine (std::thread + rodio)
//!   ├─ 种子: 首轮用墙钟 now % TOTAL_DURATION 定位起始曲目 + 曲内偏移 seek
//!   ├─ preload_track() → tauri::async_runtime::spawn 异步下载 MP3 字节
//!   ├─ rodio::Decoder → Sink.append → 系统音频设备
//!   ├─ 同时启动下一曲预加载（双缓冲，切歌零等待）
//!   ├─ 更新 souvlaki now-playing metadata（经 run_on_main_thread dispatch 到主线程）
//!   └─ sink.empty() → 顺序切下一首
//! ```
//!
//! # 线程模型
//!
//! - **播放线程**（std::thread）：rodio 音频输出 + 下载 + 控制消息处理。
//! - **souvlaki MediaControls**：由 lib.rs 主线程创建并存入 Tauri managed state
//!   （`MediaControlsState`）。macOS 的 MPRemoteCommandCenter / MPNowPlayingInfoCenter
//!   必须在主线程调用，故引擎的所有 souvlaki 调用经 `run_on_main_thread()` dispatch
//!   到主线程执行——否则系统媒体控制（▶/⏸ 状态、Now Playing 信息）静默失效。

use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{Emitter, Manager};
use tokio::sync::oneshot;
use tracing::{error, info, warn};

// ── 歌单 ────────────────────────────────────────────────────────────────────

/// 曲目信息。
struct Track {
    artist: &'static str,
    title: &'static str,
    netease_id: u64,
    dur: u64,
}

/// 歌单（21 首，循环播放）。
const TRACKS: &[Track] = &[
    Track { artist: "Aedh", title: "A Message For Cynthia", netease_id: 1951672257, dur: 107 },
    Track { artist: "Ben Seretan", title: "criss cross applesauce right in the stream of the amp", netease_id: 2118624028, dur: 321 },
    Track { artist: "Ben Seretan", title: "walls are humming", netease_id: 2118624029, dur: 428 },
    Track { artist: "Chad Crouch", title: "Shipping Lanes", netease_id: 1365588629, dur: 194 },
    Track { artist: "Damon Boucher", title: "Chill no. 1", netease_id: 1458837958, dur: 68 },
    Track { artist: "ERA C T NOD 1", title: "better days", netease_id: 1879479122, dur: 82 },
    Track { artist: "E*Rock", title: "Forest Clearing", netease_id: 566288317, dur: 153 },
    Track { artist: "Grabek", title: "three", netease_id: 1822177137, dur: 260 },
    Track { artist: "Joya", title: "Miss you", netease_id: 2726124148, dur: 134 },
    Track { artist: "Kyle Preston", title: "We Made It. We Finally Made It", netease_id: 1805007083, dur: 417 },
    Track { artist: "Memory Palace", title: "Tru Blue", netease_id: 1947169149, dur: 189 },
    Track { artist: "Owen Kelley", title: "Tonkotsu (Reloaded)", netease_id: 2033253579, dur: 168 },
    Track { artist: "PADELM", title: "Cloudscape Suspended", netease_id: 1917893384, dur: 207 },
    Track { artist: "Parker Tichko", title: "Fiddleheads Unfurling", netease_id: 2628859872, dur: 190 },
    Track { artist: "Parker Tichko", title: "Wilting in the wind", netease_id: 2628860760, dur: 139 },
    Track { artist: "Passport", title: "Reunion", netease_id: 1489250872, dur: 113 },
    Track { artist: "Pothoa", title: "driftwood", netease_id: 2751540024, dur: 160 },
    Track { artist: "Siren and the Sea", title: "Instinct", netease_id: 1481058563, dur: 234 },
    Track { artist: "TERNS", title: "Flux", netease_id: 1866114631, dur: 160 },
    Track { artist: "Yuuki Matthews", title: "Cherry Blossom Petals", netease_id: 2743042411, dur: 190 },
    Track { artist: "Yuuki Matthews", title: "Transient Glowing", netease_id: 2743042413, dur: 183 },
];

/// 歌单总时长（秒）= 墙钟取模周期。
const TOTAL_DURATION: u64 = 4306;

// ── 引擎控制消息 ────────────────────────────────────────────────────────────

/// 播放控制消息：前端 / 托盘 / 系统媒体键 → 播放线程。
pub enum FmControl {
    Toggle,
    Play,
    Pause,
}

/// 播放状态快照（供 Tauri command `fm_get_state` 读取）。
#[derive(Clone, Serialize)]
pub struct FmPlaybackState {
    pub playing: bool,
    pub ready: bool,
    pub artist: String,
    pub title: String,
    pub index: usize,
    /// 像素场景动画时钟（秒）：仅播放（未静音）时按真实流逝累计，暂停冻结。
    /// 主窗口与壁纸窗口统一经 `fm_scene_t` 采样，保证两处画面严格同步。
    pub scene_t: f64,
}

// ── 引擎 ────────────────────────────────────────────────────────────────────

/// 广播电台引擎（进程级单例）。
///
/// 播放线程以 `std::thread::spawn` 运行（rodio 需要稳定线程），通过 `mpsc` channel
/// 接收播放控制消息，通过共享 `Mutex<FmPlaybackState>` 暴露当前状态。
///
/// `control_rx` 仅在 `spawn()` 时取一次（`Option::take`），保证接收端只被消费一次。
/// 所有 clone 的 FmEngine 共享同一个 `control_tx`，向同一个播放线程发送控制消息。
#[derive(Clone)]
pub struct FmEngine {
    /// 播放控制消息发送端（clone 安全，所有副本共享）。
    control_tx: Arc<std::sync::mpsc::Sender<FmControl>>,
    /// 播放控制消息接收端（spawn 时 take，保证只消费一次）。
    control_rx: Arc<Mutex<Option<std::sync::mpsc::Receiver<FmControl>>>>,
    /// 播放状态快照（供 Tauri command 读取）。
    state: Arc<Mutex<FmPlaybackState>>,
    /// HTTP 客户端（用于 CDN 下载）。
    http_client: reqwest::Client,
}

impl FmEngine {
    pub fn new(http_client: reqwest::Client) -> Self {
        let (control_tx, control_rx) = std::sync::mpsc::channel();
        Self {
            control_tx: Arc::new(control_tx),
            control_rx: Arc::new(Mutex::new(Some(control_rx))),
            state: Arc::new(Mutex::new(FmPlaybackState {
                playing: false,
                ready: false,
                artist: String::new(),
                title: String::new(),
                index: 0,
                scene_t: 0.0,
            })),
            http_client,
        }
    }

    /// 切换播放/暂停。
    pub fn toggle(&self) {
        let _ = self.control_tx.send(FmControl::Toggle);
    }

    /// 返回播放控制消息发送端的 clone（供 souvlaki 回调使用）。
    pub fn control_tx_clone(&self) -> std::sync::mpsc::Sender<FmControl> {
        (*self.control_tx).clone()
    }

    /// 返回当前像素场景动画时钟（秒；引擎未就绪时 0.0）。
    pub fn scene_t(&self) -> f64 {
        self.state.lock().map(|s| s.scene_t).unwrap_or(0.0)
    }

    /// 返回当前播放状态快照。
    pub fn get_state(&self) -> FmPlaybackState {
        self.state
            .lock()
            .map(|s| s.clone())
            .unwrap_or(FmPlaybackState {
                playing: false,
                ready: false,
                artist: String::new(),
                title: String::new(),
                index: 0,
                scene_t: 0.0,
            })
    }

    /// 启动播放线程。在 lib.rs setup 中调用。
    ///
    /// souvlaki MediaControls 由 lib.rs 主线程创建并 manage 为 `MediaControlsState`，
    /// 引擎通过 `app_handle` 在需要时 dispatch 到主线程访问（macOS 系统媒体 API
    /// 必须在主线程调用）。
    pub fn spawn(self, app_handle: tauri::AppHandle) {
        // 取走接收端（只取一次，保证不会被重复消费）。
        let control_rx = self
            .control_rx
            .lock()
            .ok()
            .and_then(|mut rx| rx.take())
            .expect("FmEngine::spawn called twice or control_rx missing");

        let state = self.state.clone();
        let http_client = self.http_client.clone();

        std::thread::Builder::new()
            .name("fm-playback".into())
            .spawn(move || {
                engine_loop(app_handle, control_rx, state, http_client);
            })
            .expect("failed to spawn FM playback thread");
    }
}

// ── 播放线程 ────────────────────────────────────────────────────────────────

/// 引擎主循环：在专用 std::thread 中运行。
///
/// 1. 创建 rodio OutputStream + Sink（线程生命周期内持续）
/// 2. 墙钟种子定位起始曲目
/// 3. emit fm-ready → 前端/托盘添加 FM 菜单项
/// 4. 循环播放：异步预加载（双缓冲）+ rodio 解码播放 + 控制消息处理
fn engine_loop(
    app_handle: tauri::AppHandle,
    control_rx: std::sync::mpsc::Receiver<FmControl>,
    state: Arc<Mutex<FmPlaybackState>>,
    http_client: reqwest::Client,
) {
    // rodio OutputStream：必须在当前线程持续存活，drop 则音频停止。
    let (_stream, stream_handle) = match rodio::OutputStream::try_default() {
        Ok(s) => s,
        Err(e) => {
            error!("FM: failed to create audio output stream: {}", e);
            return;
        }
    };
    let sink = rodio::Sink::try_new(&stream_handle).expect("failed to create audio sink");
    // 初始静音（radio 模式：「暂停」= 静音，时间轴继续走）。
    // 用户点击播放时才恢复音量 + seek 到墙钟对齐位置。
    sink.set_volume(0.0);

    // 引擎就绪 → 通知前端 + Rust 侧直接添加托盘 FM 菜单项（不依赖前端中转）。
    {
        if let Ok(mut s) = state.lock() {
            s.ready = true;
        }
        // Rust 侧直接更新托盘菜单：避免前端未加载时丢失菜单项。
        let _ = crate::add_fm_menu_item(&app_handle);
        let _ = app_handle.emit("fm-ready", ());
    }

    // 预计算累计起始偏移（秒），用于墙钟种子二分。
    let cum_start: Vec<u64> = {
        let mut v = Vec::with_capacity(TRACKS.len());
        let mut acc: u64 = 0;
        for t in TRACKS {
            v.push(acc);
            acc += t.dur;
        }
        v
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut idx = seed_from_wallclock(now, &cum_start);
    // 曲内偏移（秒）：墙钟在歌单周期上的位置 − 当前曲的累计起始偏移。
    // 首次播放 seek 到这里，从"此刻该在的位置"续播而非曲目开头。
    let seed_offset = now % TOTAL_DURATION - cum_start[idx];

    info!(
        "FM engine started ({} tracks, {}s total); seed track {} @ {}+{}s",
        TRACKS.len(),
        TOTAL_DURATION,
        idx,
        cum_start[idx],
        seed_offset
    );

    // 预加载缓存：绑定目标曲目索引，re-anchor 跳曲时按索引匹配。
    struct Preload {
        index: usize,
        rx: oneshot::Receiver<Vec<u8>>,
    }

    // Radio 模式状态：
    // - muted=true：静音广播（暂停），时间轴照常走；
    // - pending_seek：首曲/重对齐时 seek 到曲内偏移。
    let mut muted = true;
    let mut pending_seek: Option<u64> = Some(seed_offset);
    let mut next_preload: Option<Preload> = None;

    // 像素场景时钟的上一采样时刻（真实流逝累计，暂停/静音冻结）。
    let mut last_tick = Instant::now();

    // 初始曲目状态（引擎就绪但未播放）。
    {
        let track = &TRACKS[idx];
        if let Ok(mut s) = state.lock() {
            s.artist = track.artist.to_string();
            s.title = track.title.to_string();
            s.index = idx;
        }
        // 通知前端当前曲目（即使未播放）。
        let _ = app_handle.emit(
            "fm-meta",
            serde_json::json!({
                "artist": track.artist,
                "title": track.title,
                "index": idx,
            }),
        );
    }

    'outer: loop {
        let track = &TRACKS[idx];

        // 场景时钟按真实流逝推进（切曲下载/解码的静默间隙照走，与音频一致）。
        tick_scene_t(&state, muted, &mut last_tick);

        // ── 获取音频字节：优先使用预加载结果，否则实时下载 ──

        let bytes = if let Some(pre) = next_preload.take() {
            if pre.index == idx {
                // 预加载的就是当前曲，等待结果（通常已完成）。
                match pre.rx.blocking_recv() {
                    Ok(b) if !b.is_empty() => Some(b),
                    _ => {
                        warn!(
                            "FM preload failed, re-fetching track {}",
                            track.netease_id
                        );
                        fetch_track_sync(&http_client, track.netease_id)
                    }
                }
            } else {
                // re-anchor 跳曲导致预加载失配，丢弃重新下载。
                fetch_track_sync(&http_client, track.netease_id)
            }
        } else {
            // 首次或预加载缺失，同步下载。
            fetch_track_sync(&http_client, track.netease_id)
        };

        let bytes = match bytes {
            Some(b) => b,
            None => {
                warn!(
                    "FM track {} download failed, skipping",
                    track.netease_id
                );
                idx = (idx + 1) % TRACKS.len();
                continue;
            }
        };

        // ── 解码并送入 Sink ──

        let source = match rodio::Decoder::new(Cursor::new(bytes)) {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    "FM track {} decode failed: {}, skipping",
                    track.netease_id, e
                );
                idx = (idx + 1) % TRACKS.len();
                continue;
            }
        };
        sink.append(source);

        // 音量恢复：muted=false（播放中）时确保有声。
        // - 正常切歌：音量已是 1.0，幂等；
        // - re-anchor 切歌（continue 'outer）：clear() 后重新 append，
        //   恢复播放状态（clear 内部 pause）+ 音量。
        // - 静音（暂停）期间：保持 0。
        if !muted {
            sink.play();
            sink.set_volume(1.0);
        } else {
            sink.set_volume(0.0);
        }

        // 首轮 seek 到墙钟对齐的曲内偏移（"此刻该在的位置"续播而非曲目开头）。
        // rodio 的 Sink 在 append 后通过内部控制队列在音频线程上执行 seek，
        // 对已 append 的 source 生效（symphonia 解码器支持 seek）。
        if let Some(offset) = pending_seek.take() {
            if let Err(e) = sink.try_seek(Duration::from_secs(offset)) {
                warn!(
                    "FM track {} seek to {offset}s failed: {}",
                    track.netease_id, e
                );
            } else {
                info!("FM seeded track {} at +{}s", track.netease_id, offset);
            }
        }
        {
            if let Ok(mut s) = state.lock() {
                s.artist = track.artist.to_string();
                s.title = track.title.to_string();
                s.index = idx;
                // playing 状态：由控制消息驱动，不在此处覆写。
                // 首次播放由用户点击触发，之后保持用户上次的意图。
            }
            let _ = app_handle.emit(
                "fm-meta",
                serde_json::json!({
                    "artist": track.artist,
                    "title": track.title,
                    "index": idx,
                }),
            );
        }

        // 更新系统媒体控制 Now Playing（dispatch 到主线程，macOS 系统 API 要求）。
        update_media_controls(&app_handle, track.artist, track.title);

        // 启动下一曲预加载（双缓冲：当前曲播放时后台下载下一曲）。
        let next_idx = (idx + 1) % TRACKS.len();
        let next_id = TRACKS[next_idx].netease_id;
        let client = http_client.clone();
        let (preload_tx, preload_rx) = oneshot::channel();
        tauri::async_runtime::spawn(async move {
            if let Some(bytes) = preload_track(&client, next_id).await {
                let _ = preload_tx.send(bytes);
            }
        });
        next_preload = Some(Preload { index: next_idx, rx: preload_rx });

        // ── 等待曲目结束 + 处理控制消息 ──

        // Radio 模式：暂停 = 静音（sink.set_volume(0.0)），时间轴照常走。
        // 恢复播放时 re-anchor 到墙钟此刻的位置——静音期间可能已切到下一首。
        'wait: loop {
            // 100ms 轮询：检查控制消息 + sink 是否播完。
            match control_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(FmControl::Toggle) => {
                    if muted {
                        // 恢复播放（re-anchor 逻辑见 helper）。
                        if resume_playback_if_muted(
                            &sink,
                            &mut muted,
                            &mut idx,
                            &cum_start,
                            &mut pending_seek,
                            &state,
                            &app_handle,
                        ) {
                            continue 'outer; // 已切歌：外层重新下载/播放新曲
                        }
                        // 同一曲：seek + 音量已恢复，继续等待本曲结束。
                    } else {
                        // 暂停 = 静音（radio 模式）：时间轴继续走，声音消失。
                        sink.set_volume(0.0);
                        muted = true;
                        set_playing(&state, false);
                        update_playback(&app_handle, false, None);
                        let _ = app_handle.emit("fm-state-changed", false);
                    }
                }
                Ok(FmControl::Play) => {
                    if muted {
                        if resume_playback_if_muted(
                            &sink,
                            &mut muted,
                            &mut idx,
                            &cum_start,
                            &mut pending_seek,
                            &state,
                            &app_handle,
                        ) {
                            continue 'outer;
                        }
                        // 同一曲：seek + 音量已恢复，继续等待。
                    }
                }
                Ok(FmControl::Pause) => {
                    if !muted {
                        // 暂停 = 静音（radio 模式）。
                        sink.set_volume(0.0);
                        muted = true;
                        set_playing(&state, false);
                        update_playback(&app_handle, false, None);
                        let _ = app_handle.emit("fm-state-changed", false);
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }

            // 场景时钟：每 100ms 轮询迭代表推进一次（取控制消息之后的 muted 值）。
            tick_scene_t(&state, muted, &mut last_tick);

            if sink.empty() {
                break 'wait;
            }
        }

        // 曲目结束 → 下一首（预加载已就绪，零等待）。
        idx = (idx + 1) % TRACKS.len();
        // 静音期间切歌：音量保持 0，继续静音广播；有声时保持 1.0。
    }
}

/// 若当前处于静音（暂停）状态，尝试恢复播放：用墙钟此刻位置 re-anchor。
///
/// - 同一曲内：seek 修正漂移 + 恢复音量。
/// - 已切到下一曲：清空 sink、设置 pending_seek、返回 `true` 触发外层重开循环。
fn resume_playback_if_muted(
    sink: &rodio::Sink,
    muted: &mut bool,
    idx: &mut usize,
    cum_start: &[u64],
    pending_seek: &mut Option<u64>,
    state: &Arc<Mutex<FmPlaybackState>>,
    app_handle: &tauri::AppHandle,
) -> bool {
    if !*muted {
        return false;
    }
    let now_sec = wallclock_now();
    let pos = now_sec % TOTAL_DURATION;
    let new_idx = seed_from_wallclock(now_sec, cum_start);
    let new_offset = pos - cum_start[new_idx];
    if new_idx != *idx {
        // 静音期间切歌了：清掉当前播放，外层循环重新下载/播放新曲。
        sink.clear(); // 注意：clear() 内部会 pause，下一轮恢复播放时需 play()
        *idx = new_idx;
        *pending_seek = Some(new_offset);
        *muted = false;
        return true;
    }
    // 同一曲：seek 修正漂移后恢复音量 + 解除 clear() 可能留下的 pause。
    sink.play();
    let _ = sink.try_seek(Duration::from_secs(new_offset));
    sink.set_volume(1.0);
    *muted = false;
    set_playing(state, true);
    update_playback(app_handle, true, Some(new_offset));
    let _ = app_handle.emit("fm-state-changed", true);
    false
}

/// 推进像素场景动画时钟：仅未静音（播放中）时按真实流逝累计，暂停/静音冻结。
fn tick_scene_t(state: &Arc<Mutex<FmPlaybackState>>, muted: bool, last: &mut Instant) {
    let now = Instant::now();
    if !muted {
        if let Ok(mut s) = state.lock() {
            s.scene_t += now.duration_since(*last).as_secs_f64();
        }
    }
    *last = now;
}

/// 当前墙钟秒（epoch）。
fn wallclock_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// 同步播放状态到系统媒体控制（souvlaki）。
///
/// macOS 的 MPNowPlayingInfoCenter 必须在主线程调用——souvlaki 由主线程创建并
/// manage 在 `MediaControlsState`，此处经 `run_on_main_thread` dispatch。
fn update_playback(
    app_handle: &tauri::AppHandle,
    playing: bool,
    progress_secs: Option<u64>,
) {
    let handle = app_handle.clone();
    let _ = app_handle.run_on_main_thread(move || {
        if let Some(guard) = handle.try_state::<MediaControlsState>() {
            let mut guard = guard.0.lock().unwrap();
            if let Some(ctrl) = guard.as_mut() {
                let playback = if playing {
                    souvlaki::MediaPlayback::Playing {
                        progress: progress_secs.map(|s| souvlaki::MediaPosition(Duration::from_secs(s))),
                    }
                } else {
                    souvlaki::MediaPlayback::Paused { progress: None }
                };
                let _ = ctrl.set_playback(playback);
            }
        }
    });
}

/// 更新系统媒体控制 Now Playing 信息（dispatch 到主线程）。
fn update_media_controls(app_handle: &tauri::AppHandle, artist: &'static str, title: &'static str) {
    let handle = app_handle.clone();
    let _ = app_handle.run_on_main_thread(move || {
        if let Some(guard) = handle.try_state::<MediaControlsState>() {
            let mut guard = guard.0.lock().unwrap();
            if let Some(ctrl) = guard.as_mut() {
                let _ = ctrl.set_metadata(souvlaki::MediaMetadata {
                    title: Some(title),
                    artist: Some(artist),
                    ..Default::default()
                });
            }
        }
    });
}

/// 更新共享播放状态。
fn set_playing(state: &Arc<Mutex<FmPlaybackState>>, playing: bool) {
    if let Ok(mut s) = state.lock() {
        s.playing = playing;
    }
}

// ── 辅助函数 ────────────────────────────────────────────────────────────────

/// 系统媒体控制状态（souvlaki）。
///
/// 由 lib.rs 主线程创建并 `app.manage()`。macOS 的 MPRemoteCommandCenter /
/// MPNowPlayingInfoCenter 必须在主线程调用，故引擎线程经 `run_on_main_thread`
/// dispatch 到这里访问（见 `update_playback` / `update_media_controls`）。
pub struct MediaControlsState(pub Mutex<Option<souvlaki::MediaControls>>);

/// 用墙钟定位种子曲目索引。
fn seed_from_wallclock(now: u64, cum_start: &[u64]) -> usize {
    let pos = now % TOTAL_DURATION;
    let mut lo = 0usize;
    let mut hi = TRACKS.len() - 1;
    while lo < hi {
        let mid = (lo + hi + 1) >> 1;
        if cum_start[mid] <= pos {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// 异步预加载：解析音源直链 + 下载完整 MP3 字节。
async fn preload_track(client: &reqwest::Client, netease_id: u64) -> Option<Vec<u8>> {
    let src = resolve_src(client, netease_id).await?;
    let bytes = client.get(&src).send().await.ok()?.bytes().await.ok()?;
    if bytes.is_empty() {
        return None;
    }
    Some(bytes.to_vec())
}

/// 同步下载曲目：在 std::thread 中通过 `tauri::async_runtime::block_on` 桥接异步。
fn fetch_track_sync(client: &reqwest::Client, netease_id: u64) -> Option<Vec<u8>> {
    let client = client.clone();
    tauri::async_runtime::block_on(preload_track(&client, netease_id))
}

/// 解析音源直链：优先 paugram 解析接口，失败回退网易云官方外链。
async fn resolve_src(client: &reqwest::Client, id: u64) -> Option<String> {
    let paugram_url = format!("https://api.paugram.com/netease/?id={id}");
    if let Ok(resp) = client.get(&paugram_url).send().await {
        if resp.status().is_success() {
            if let Ok(data) = resp.json::<serde_json::Value>().await {
                if let Some(link) = data.get("link").and_then(|l| l.as_str()) {
                    if !link.is_empty() {
                        return Some(link.to_string());
                    }
                }
            }
        }
    }
    // 回退网易云官方外链（302 → CDN HTTP）。
    Some(format!("https://music.163.com/song/media/outer/url?id={id}"))
}
