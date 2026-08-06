//! Claude FM — 广播电台引擎。
//!
//! 所有播放逻辑（歌单管理、墙钟时间轴、音源解析、预加载、切歌、跳过失效曲目）
//! 全部在后端完成。引擎以 `tokio::spawn` 后台任务运行，通过 `broadcast::channel`
//! 将音频字节推送给所有订阅者。
//!
//! # 数据流
//!
//! ```text
//! FmEngine (tokio::spawn)
//!   ├─ 墙钟: SystemTime::now() % TOTAL_DURATION
//!   ├─ locate(pos) → 当前曲目 + 曲内偏移
//!   ├─ resolve_src() → paugram / 网易云外链
//!   ├─ reqwest GET → CDN 字节流
//!   └─ broadcast::send(Bytes) → 所有 /api/fm/live 订阅者
//! ```
//!
//! 前端只需一个 `<audio>` 标签挂在 `/api/fm/live`，像收音机一样收听永不关闭的直播流。

use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use futures::StreamExt;
use serde::Serialize;
use tokio::select;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tracing::{error, info, warn};

use crate::gateway::server::AppState;

// ── 歌单 ────────────────────────────────────────────────────────────────────

/// 曲目信息。
struct Track {
    artist: &'static str,
    title: &'static str,
    netease_id: u64,
    dur: u64,
}

/// 歌单（21 首，循环播放）。从前端 player.ts 移入后端统一管理。
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

// ── 引擎 ────────────────────────────────────────────────────────────────────

/// 广播电台引擎（进程级单例）。
///
/// 后台任务持续从 CDN 拉取音频字节并通过 `broadcast` 推送给所有订阅者。
/// 所有 `/api/fm/live` 客户端共享同一数据源，一份 CDN 流量多人收听。
#[derive(Clone)]
pub struct FmEngine {
    audio_tx: broadcast::Sender<Bytes>,
    track_index: Arc<AtomicUsize>,
    http_client: reqwest::Client,
}

impl FmEngine {
    pub fn new(http_client: reqwest::Client) -> Self {
        let (audio_tx, _) = broadcast::channel(1024);
        Self {
            audio_tx,
            track_index: Arc::new(AtomicUsize::new(0)),
            http_client,
        }
    }

    /// 启动引擎后台任务。在 `start_gateway()` 中调用，传入 Tauri AppHandle
    /// 用于 emit `fm-meta` 事件通知前端切歌。
    pub fn spawn(self, app_handle: tauri::AppHandle) {
        let http_client = self.http_client.clone();
        let audio_tx = self.audio_tx.clone();
        let track_index = self.track_index.clone();

        tokio::spawn(async move {
            engine_loop(http_client, audio_tx, track_index, app_handle).await;
        });
    }

    /// 返回当前曲目元数据（供 `/api/fm/meta` 和 Tauri command 使用）。
    pub fn current_meta(&self) -> (&'static str, &'static str, usize) {
        let idx = self.track_index.load(Ordering::Relaxed) % TRACKS.len();
        (TRACKS[idx].artist, TRACKS[idx].title, idx)
    }

    /// 订阅音频广播（供 `/api/fm/live` handler 使用）。
    pub fn subscribe(&self) -> broadcast::Receiver<Bytes> {
        self.audio_tx.subscribe()
    }
}

/// 引擎主循环：墙钟驱动，持续从 CDN 拉取音频字节并广播。
///
/// 循环逻辑：
/// 1. 墙钟定位当前曲目 + 跳过失效曲目
/// 2. 曲目变化时 emit `fm-meta` 通知前端
/// 3. reqwest GET 音源（自动跟随 302 重定向）
/// 4. 流式读取字节 → broadcast::send → 所有订阅者
/// 5. 曲目结束 → 切下一首
/// 6. 上游失败 → 标记跳过，切下一曲
async fn engine_loop(
    client: reqwest::Client,
    audio_tx: broadcast::Sender<Bytes>,
    track_index: Arc<AtomicUsize>,
    app_handle: tauri::AppHandle,
) {
    use tauri::Emitter;

    // 预计算累计起始偏移（秒），用于墙钟二分查找。
    let mut cum_start = Vec::with_capacity(TRACKS.len());
    let mut acc: u64 = 0;
    for t in TRACKS {
        cum_start.push(acc);
        acc += t.dur;
    }

    let mut cur_idx: usize = TRACKS.len(); // 哨兵值，确保首次迭代触发 meta emit
    let mut skip_until: u64 = 0;
    let mut _stream_rx: Option<broadcast::Receiver<Bytes>> = None;

    info!("FM engine started ({} tracks, {}s total)", TRACKS.len(), TOTAL_DURATION);

    loop {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let pos = now % TOTAL_DURATION;

        // 二分查找当前曲目。
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
        let idx = lo;

        // 跳过失效曲目：skip_until 记录时间轴上的跳过截止时间戳。
        if skip_until != 0 && now < skip_until {
            let next_idx = (idx + 1) % TRACKS.len();
            if next_idx != idx {
                track_index.store(next_idx, Ordering::Relaxed);
                cur_idx = next_idx;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        }
        if skip_until != 0 && now >= skip_until {
            skip_until = 0;
        }

        // 曲目变化 → emit fm-meta 通知前端更新 caption。
        if idx != cur_idx {
            cur_idx = idx;
            track_index.store(idx, Ordering::Relaxed);
            let track = &TRACKS[idx];
            info!("FM track: {} - {}", track.artist, track.title);
            let _ = app_handle.emit(
                "fm-meta",
                serde_json::json!({
                    "artist": track.artist,
                    "title": track.title,
                    "index": idx,
                }),
            );
        }

        // 解析音源直链（paugram 优先，回退网易云外链）。
        let track = &TRACKS[idx];
        let src = match resolve_src(&client, track.netease_id).await {
            Some(s) => s,
            None => {
                skip_until = now + track.dur + track.dur;
                continue;
            }
        };

        // 拉取音源字节流（reqwest 自动跟随 302 重定向到 CDN）。
        let resp = match client.get(&src).send().await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                warn!("FM upstream {} -> {}", src, r.status());
                skip_until = now + track.dur + track.dur;
                continue;
            }
            Err(e) => {
                warn!("FM upstream {} failed: {}", src, e);
                skip_until = now + track.dur + track.dur;
                continue;
            }
        };

        // 流式读取字节 → broadcast::send → 所有 /api/fm/live 订阅者。
        let mut stream = resp.bytes_stream();
        let mut chunk_err = false;

        loop {
            select! {
                biased;
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) if !bytes.is_empty() => {
                            // 无订阅者时 SendError 被忽略（引擎持续运行，字节丢弃）。
                            // Lagging 错误被忽略（慢客户端丢弃旧数据）。
                            let _ = audio_tx.send(bytes);
                        }
                        Some(Err(e)) => {
                            warn!("FM stream chunk error: {}", e);
                            chunk_err = true;
                            break;
                        }
                        _ => break, // None = 曲目流结束
                    }
                }
            }
        }

        if chunk_err {
            skip_until = now + 1;
        }

        // 曲目结束或出错 → 循环回到顶部，墙钟定位下一首。
    }
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
    Some(format!(
        "https://music.163.com/song/media/outer/url?id={id}"
    ))
}

// ── HTTP handlers ────────────────────────────────────────────────────────────

/// GET /api/fm/live — 永不关闭的直播音频流（HTTP chunked）。
///
/// 客户端（前端 `<audio>` 标签）挂上此端点即开始收听。
/// 所有客户端共享同一 `broadcast` 通道，一份 CDN 数据多人收听。
/// 客户端断开时 broadcast receiver 被 drop，不影响引擎运行。
pub(crate) async fn fm_live(State(state): State<Arc<AppState>>) -> Response {
    let rx = state.fm.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| async move {
        match result {
            Ok(bytes) if !bytes.is_empty() => Some(Ok::<Bytes, Infallible>(bytes)),
            Ok(_) => None, // 过滤空字节（keepalive 占位）
            Err(_) => Some(Ok(Bytes::new())), // Lagging → 空字节，保持流不断
        }
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "audio/mpeg")
        .header(header::CACHE_CONTROL, "no-cache, no-store")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|e| {
            error!("fm_live response build failed: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
        })
}

/// 曲目元数据响应。
#[derive(Serialize)]
pub(crate) struct FmMetaResponse {
    artist: &'static str,
    title: &'static str,
    index: usize,
}

/// GET /api/fm/meta — 返回当前播放曲目的元数据。
///
/// 前端在挂载 `/api/fm/live` 后调用此端点获取初始曲目信息，
/// 后续切歌由 Tauri `fm-meta` 事件推送（无需轮询）。
pub(crate) async fn fm_current_meta(State(state): State<Arc<AppState>>) -> Json<FmMetaResponse> {
    let (artist, title, index) = state.fm.current_meta();
    Json(FmMetaResponse {
        artist,
        title,
        index,
    })
}
