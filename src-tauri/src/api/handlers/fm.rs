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
//!   ├─ 种子: 首轮用墙钟 now % TOTAL_DURATION 定位起始曲目 + 曲内偏移
//!   ├─ probe_track() → Content-Length + 首帧比特率 → 真实时长
//!   ├─ reqwest GET (首轮 Range: bytes=<offset>- seek) → CDN 字节流
//!   ├─ 按比特率实时节流（~1s 音频/1s 墙钟）→ broadcast::send
//!   ├─ emit fm-meta + track_index = idx（音频即真相）
//!   └─ 曲目字节流结束 → 顺序切下一首（从 0 播）
//! ```
//!
//! 元数据由「正在广播字节」驱动而非解耦的墙钟，且引擎按实时速率送字节——
//! 否则广播通道不背压，CDN 满速吞完一首歌会让元信息飞转快于耳朵听到的位置。
//! 前端只需一个 `<audio>` 标签挂在 `/fm/live`，像收音机一样收听永不关闭的直播流。

use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
/// 所有 `/fm/live` 客户端共享同一数据源，一份 CDN 流量多人收听。
#[derive(Clone)]
pub struct FmEngine {
    audio_tx: broadcast::Sender<Bytes>,
    track_index: Arc<AtomicUsize>,
    /// 当前曲目的「墙钟起点」epoch 秒。客户端用 `Date.now() - this + audio.currentTime`
    /// 算出当前在歌单时间轴上的位置，从而与播放进度对齐（caption 跟耳朵而非字节流）。
    track_epoch: Arc<AtomicU64>,
    http_client: reqwest::Client,
}

impl FmEngine {
    pub fn new(http_client: reqwest::Client) -> Self {
        let (audio_tx, _) = broadcast::channel(1024);
        Self {
            audio_tx,
            track_index: Arc::new(AtomicUsize::new(0)),
            track_epoch: Arc::new(AtomicU64::new(0)),
            http_client,
        }
    }

    /// 启动引擎后台任务。在 `start_gateway()` 中调用，传入 Tauri AppHandle
    /// 用于 emit `fm-meta` 事件通知前端切歌。
    pub fn spawn(self, app_handle: tauri::AppHandle) {
        let http_client = self.http_client.clone();
        let audio_tx = self.audio_tx.clone();
        let track_index = self.track_index.clone();
        let track_epoch = self.track_epoch.clone();

        tokio::spawn(async move {
            engine_loop(http_client, audio_tx, track_index, track_epoch, app_handle).await;
        });
    }

    /// 返回当前曲目元数据 + 其墙钟起点 epoch 秒（供 `/fm/meta`）。
    pub fn current_meta(&self) -> (&'static str, &'static str, usize, u64) {
        let idx = self.track_index.load(Ordering::Relaxed) % TRACKS.len();
        let epoch = self.track_epoch.load(Ordering::Relaxed);
        (TRACKS[idx].artist, TRACKS[idx].title, idx, epoch)
    }

    /// 订阅音频广播（供 `/fm/live` handler 使用）。
    pub fn subscribe(&self) -> broadcast::Receiver<Bytes> {
        self.audio_tx.subscribe()
    }
}

/// 引擎主循环：墙钟种子 + 串行收音机。
///
/// 元数据由「正在广播字节」驱动而非解耦的墙钟，杜绝进度错位与从头播放：
/// 1. 首轮用墙钟 `now % TOTAL_DURATION` 定位起始曲目 + 曲内偏移（种子）
/// 2. probe 真实时长 → Range seek 到曲内偏移（首轮才 seek，之后从 0 播）
/// 3. 进入曲目即写 `track_index` + emit `fm-meta`（音频即真相）
/// 4. 流式读字节 → broadcast::send → 所有订阅者
/// 5. 曲目字节流结束 → 顺序切下一首（从 0 播）
/// 6. 上游失败 → 切下一首并 emit（不再静默漂移）
/// 引擎主循环：墙钟门控切歌 + CDN 满速灌字节。
///
/// **广播模型的关键认知**：caption 同步靠墙钟，不靠字节对齐。
/// 引擎以 CDN 满速把字节灌进广播通道（不节流），客户端 `<audio>` 各自缓冲、
/// 按实时播放消费。元数据切歌点由墙钟独立计算——每曲墙钟起点写入 `track_epoch`，
/// 到 `track.dur` 即切下一首。前端用 `Date.now() - epoch + audio.currentTime` 算出
/// 当前在歌单时间轴上的位置，从而 caption 跟随**播放位置**而非读取位置，
/// 与耳朵严格对齐（无论客户端缓冲多少秒）。
async fn engine_loop(
    client: reqwest::Client,
    audio_tx: broadcast::Sender<Bytes>,
    track_index: Arc<AtomicUsize>,
    track_epoch: Arc<AtomicU64>,
    app_handle: tauri::AppHandle,
) {
    // 预计算累计起始偏移（秒），仅用于种子二分。
    let mut cum_start = Vec::with_capacity(TRACKS.len());
    let mut acc: u64 = 0;
    for t in TRACKS {
        cum_start.push(acc);
        acc += t.dur;
    }

    // 种子定位：墙钟 now % TOTAL_DURATION。广播模型——电台始终在真实时间线上
    // 往前走，重启后自动落到「此刻该在的位置」，确定性且对所有听众一致，无需持久化。
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut idx = seed_from_wallclock(now, &cum_start);

    // 首曲锚点：把当前墙钟视为该曲的「墙钟起点」，往前回拨曲内偏移秒——
    // 即假设该曲从 (now - intra_offset) 起播。这样后续时间轴自然延续。
    let intra_offset = now - cum_start[idx];
    let track0_epoch = now - intra_offset;
    track_index.store(idx, Ordering::Relaxed);
    track_epoch.store(track0_epoch, Ordering::Relaxed);
    info!(
        "FM engine started ({} tracks, {}s total); seed track {} @ {}s",
        TRACKS.len(), TOTAL_DURATION, idx, intra_offset
    );

    loop {
        let track = &TRACKS[idx];

        // 解析音源直链（paugram 优先，回退网易云外链）。
        let src = match resolve_src(&client, track.netease_id).await {
            Some(s) => s,
            None => {
                warn!("FM track {} resolve failed, skipping", track.netease_id);
                idx = (idx + 1) % TRACKS.len();
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        // 拉取音源字节流（reqwest 自动跟随 302 重定向到 CDN）。
        let resp = match client.get(&src).send().await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                warn!("FM upstream {} -> {}", src, r.status());
                idx = (idx + 1) % TRACKS.len();
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            Err(e) => {
                warn!("FM upstream {} failed: {}", src, e);
                idx = (idx + 1) % TRACKS.len();
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        // 流式读取字节 → broadcast::send → 所有 /fm/live 订阅者。
        // 满速灌入：客户端各自缓冲、按实时播放消费。节流交给墙钟门控（见下）。
        let track_start = Instant::now();
        let mut stream = resp.bytes_stream();
        loop {
            select! {
                biased;
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) if !bytes.is_empty() => {
                            // 无订阅者时 SendError 被忽略；Lagging 被忽略（慢客户端丢旧数据）。
                            let _ = audio_tx.send(bytes);
                        }
                        Some(Err(e)) => {
                            warn!("FM stream chunk error: {}", e);
                            break;
                        }
                        _ => break, // None = 曲目流结束
                    }
                }
            }
        }

        // 墙钟门控切歌：等待该曲墙钟时段结束再切下一首。
        // CDN 满速吞完通常远早于墙钟终点 → 补偿睡眠到 track.dur。
        // 若上游吞得比实时慢（弱网），elapsed 已超 dur → 补偿 0 直接切下一首；
        // 此时客户端播放也必然滞后，caption 跟随客户端 currentTime 也会滞后，保持一致。
        let elapsed = track_start.elapsed().as_secs();
        if elapsed < track.dur {
            tokio::time::sleep(Duration::from_secs(track.dur - elapsed)).await;
        }

        // 墙钟到达曲目终点 → 写下一曲锚点（idx + 新墙钟起点）+ emit fm-meta。
        // 墙钟门控保证切歌发生在「上一曲 dur 已过」时，故新曲起点 = 此刻墙钟，
        // 时间轴严格延续。
        idx = (idx + 1) % TRACKS.len();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        track_index.store(idx, Ordering::Relaxed);
        track_epoch.store(now, Ordering::Relaxed);
        emit_meta(&app_handle, &TRACKS[idx], idx).await;
    }
}

/// 用墙钟定位种子曲目索引（回退路径）。
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

/// 写入当前曲目元数据并发送 `fm-meta` 事件通知前端切歌。
///
/// 在引擎「即将开始流送某曲字节」时调用，保证 caption 与耳朵一致。
async fn emit_meta(app_handle: &tauri::AppHandle, track: &Track, idx: usize) {
    use tauri::Emitter;
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

/// 探测曲目真实时长信息等已移除：广播模型下 caption 同步靠墙钟锚点 + 客户端
/// `audio.currentTime`，不再需要 Range seek 或比特率节流（见 engine_loop 注释）。

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

/// GET /fm/live — 永不关闭的直播音频流（HTTP chunked）。
///
/// 客户端（前端 `<audio>` 标签）挂上此端点即开始收听。
/// 所有客户端共享同一 `broadcast` 通道，一份 CDN 数据多人收听。
/// 客户端断开时 broadcast receiver 被 drop，不影响引擎运行。
pub(crate) async fn fm_live(State(state): State<Arc<AppState>>) -> Response {
    let rx = state.fm.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| async move {
        match result {
            Ok(bytes) if !bytes.is_empty() => Some(Ok::<Bytes, Infallible>(bytes)),
            Ok(_) => None, // 过滤空字节
            // Lagging → 不往流里塞空字节（会破坏 MP3 解码），丢弃保持连接存活。
            // 真正的卡顿由客户端 error/stalled 监听重连兜底。
            Err(_) => None,
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
    /// 当前曲目墙钟起点（epoch 秒）。客户端用 `Date.now()/1000 - epoch + audio.currentTime`
    /// 算出当前在歌单时间轴上的位置（见 fm_current_meta 注释）。
    epoch: u64,
}

/// GET /fm/meta — 返回当前播放曲目的元数据 + 墙钟锚点。
///
/// 前端挂载 `/fm/live` 后调用此端点获取初始锚点 + 曲目表（`/fm/schedule`），
/// 之后用 `timeupdate` 事件 + `audio.currentTime` 自行计算当前曲目，
/// 不再依赖全局 `fm-meta` 事件推送（避免与客户端解码进度错位）。
pub(crate) async fn fm_current_meta(State(state): State<Arc<AppState>>) -> Json<FmMetaResponse> {
    let (artist, title, index, epoch) = state.fm.current_meta();
    Json(FmMetaResponse {
        artist,
        title,
        index,
        epoch,
    })
}

/// 歌单条目（客户端用于自行计算当前曲目）。
#[derive(Serialize)]
pub(crate) struct FmTrackItem {
    artist: &'static str,
    title: &'static str,
    dur: u64,
}

/// GET /fm/schedule — 返回完整歌单（曲目表）。
///
/// 客户端配合 `/fm/meta` 返回的锚点，用 `audio.currentTime` 在此表上二分
/// 当前曲目，使 caption 与播放进度严格同步。
pub(crate) async fn fm_schedule() -> Json<Vec<FmTrackItem>> {
    Json(
        TRACKS
            .iter()
            .map(|t| FmTrackItem {
                artist: t.artist,
                title: t.title,
                dur: t.dur,
            })
            .collect(),
    )
}
