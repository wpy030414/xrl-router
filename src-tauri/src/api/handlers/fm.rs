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
//!   ├─ broadcast::send(Bytes) → 所有 /fm/live 订阅者
//!   ├─ emit fm-meta + track_index = idx（音频即真相）
//!   └─ 曲目字节流结束 → 顺序切下一首（从 0 播）
//! ```
//!
//! 元数据由「正在广播字节」驱动而非解耦的墙钟，杜绝进度错位与从头播放。
//! 前端只需一个 `<audio>` 标签挂在 `/fm/live`，像收音机一样收听永不关闭的直播流。

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
/// 所有 `/fm/live` 客户端共享同一数据源，一份 CDN 流量多人收听。
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

    /// 返回当前曲目元数据（供 `/fm/meta` 和 Tauri command 使用）。
    pub fn current_meta(&self) -> (&'static str, &'static str, usize) {
        let idx = self.track_index.load(Ordering::Relaxed) % TRACKS.len();
        (TRACKS[idx].artist, TRACKS[idx].title, idx)
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
async fn engine_loop(
    client: reqwest::Client,
    audio_tx: broadcast::Sender<Bytes>,
    track_index: Arc<AtomicUsize>,
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
    let (mut idx, mut intra_offset) = seed_from_wallclock(now, &cum_start);

    info!(
        "FM engine started ({} tracks, {}s total); seed track {} @ {}s",
        TRACKS.len(), TOTAL_DURATION, idx, intra_offset
    );

    loop {
        let track = &TRACKS[idx];

        // 进入曲目即写 index + emit meta（音频即将开始流送此曲）。
        track_index.store(idx, Ordering::Relaxed);
        emit_meta(&app_handle, track, idx).await;

        // 解析音源直链（paugram 优先，回退网易云外链）。
        let src = match resolve_src(&client, track.netease_id).await {
            Some(s) => s,
            None => {
                warn!("FM track {} resolve failed, skipping", track.netease_id);
                idx = (idx + 1) % TRACKS.len();
                intra_offset = 0;
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        // 拉取音源字节流。首轮若有曲内偏移，probe 真实时长后用 Range seek。
        let byte_offset = if intra_offset > 0 {
            match tokio::time::timeout(Duration::from_secs(8), probe_track(&client, &src)).await {
                Ok(Some(probe)) => seek_bytes(&probe, intra_offset, track.dur),
                Ok(None) => 0, // probe 失败 → 降级从 0 播（不阻断广播）
                Err(_) => {
                    warn!("FM probe timeout, falling back to byte 0");
                    0
                }
            }
        } else {
            0
        };
        intra_offset = 0; // 种子仅首轮生效

        let resp = match fetch_track_stream(&client, &src, byte_offset).await {
            Some(r) => r,
            None => {
                warn!("FM upstream {} unreachable, skipping", src);
                idx = (idx + 1) % TRACKS.len();
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        // 流式读取字节 → broadcast::send → 所有 /fm/live 订阅者。
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

        // 曲目结束 → 顺序切下一首（从 0 播）。
        idx = (idx + 1) % TRACKS.len();
    }
}

/// 用墙钟定位种子曲目 + 曲内偏移（回退路径）。
fn seed_from_wallclock(now: u64, cum_start: &[u64]) -> (usize, u64) {
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
    (lo, pos - cum_start[lo])
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

/// 探测曲目的真实时长信息：Content-Length + 平均比特率（应对 ID3v2 头与 VBR）。
///
/// 用于首轮 Range seek：把曲内偏移（秒）换算成字节偏移。
/// 读取文件前 ~16KB 头部：
/// 1. 解析并跳过 ID3v2 标签（若存在），避免同步字落在标签内失败；
/// 2. 从首个 MPEG 同步字开始扫描多个帧的比特率，取众数作为平均比特率（应对 VBR）。
/// 任一步失败返回 None（调用方降级从 0 播）。
async fn probe_track(client: &reqwest::Client, src: &str) -> Option<TrackProbe> {
    // Content-Length：HEAD 优先，不支持则用 GET Range: bytes=0-0 取 Content-Range。
    let content_length: Option<u64> = match client.head(src).send().await {
        Ok(r) if r.status().is_success() => r.content_length(),
        _ => match client.get(src).header(header::RANGE, "bytes=0-0").send().await {
            Ok(r) if r.status().is_success() => r
                .headers()
                .get(header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit('/').next())
                .and_then(|s| s.parse::<u64>().ok())
                .or_else(|| r.content_length()),
            _ => None,
        },
    };

    // 拉取文件头部 ~16KB 用于解析 ID3v2 + 扫描多个 MPEG 帧。
    let head: Bytes = {
        let r = client
            .get(src)
            .header(header::RANGE, "bytes=0-16383")
            .send()
            .await
            .ok()
            .filter(|r| r.status().is_success())?;
        r.bytes().await.ok()?
    };
    let head = head.as_ref();

    // 跳过 ID3v2 头（若有）。
    let audio_start = id3v2_size(head).unwrap_or(0);
    if audio_start >= head.len() {
        return None; // 头部全是标签，未见音频帧
    }
    let bitrate = scan_mpeg_bitrate(&head[audio_start..])?;

    let content_length = content_length?;
    Some(TrackProbe {
        content_length,
        bitrate,
    })
}

/// 解析 ID3v2 标签大小，返回音频数据起始偏移（含 10 字节标签头）。
///
/// ID3v2.2/2.3/2.4 头以 "ID3" 起始，后 6 字节中后 4 字节为 syncsafe 整数（每字节 7 位）。
/// 非 ID3 起始返回 None。
fn id3v2_size(head: &[u8]) -> Option<usize> {
    if head.len() < 10 || &head[0..3] != b"ID3" {
        return None;
    }
    let size_bytes = &head[6..10];
    // syncsafe：每字节最高位为 0，28 位总长。
    if size_bytes.iter().any(|&b| b & 0x80 != 0) {
        return None; // 非法 syncsafe
    }
    let size = ((size_bytes[0] as usize) << 21)
        | ((size_bytes[1] as usize) << 14)
        | ((size_bytes[2] as usize) << 7)
        | (size_bytes[3] as usize);
    Some(10 + size + if head[5] & 0x10 != 0 { 10 } else { 0 }) // 含 footer 标志
}

/// 从给定偏移扫描多个 MPEG 帧，取比特率众数（应对 VBR）。
///
/// 在头部窗口内连续匹配同步字并解码比特率，收集后取出现次数最多的值。
/// 至少匹配 1 帧即返回。
fn scan_mpeg_bitrate(buf: &[u8]) -> Option<u32> {
    let mut counts: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    let mut i = 0usize;
    while i + 4 <= buf.len() {
        // 同步字：0xFF 后跟高 3 位全 1。
        if buf[i] == 0xFF && (buf[i + 1] & 0xE0) == 0xE0 {
            if let Some(bps) = parse_mpeg_bitrate(&buf[i..i + 4]) {
                *counts.entry(bps).or_insert(0) += 1;
                // 跳过一个近似帧长，加速扫描（按当前比特率估算）。
                let frame_len = mpeg_frame_len(bps);
                i += frame_len.max(1);
                continue;
            }
        }
        i += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(_, c)| *c)
        .map(|(bps, _)| bps)
}

/// 估算 MPEG-1/2 Layer III 帧字节数（用于扫描步进，无需精确）。
fn mpeg_frame_len(bitrate: u32) -> usize {
    // 假设 44.1kHz、无 padding：frame_len = bitrate*144 / (sample_rate)
    // 44100 → 近似 bitrate*144/44100 字节。
    ((bitrate as u64 * 144) / 44100).max(1) as usize
}

/// 把曲内偏移（秒）换算成字节偏移，对齐到帧边界向下。
///
/// `oracle_dur` 为硬编码近似时长：比特率候选取使计算时长最接近 oracle 的那个，
/// 抵消 CBR 假设误差。结果向下对齐避免越过 seek 点。
fn seek_bytes(probe: &TrackProbe, offset_sec: u64, oracle_dur: u64) -> u64 {
    // 计算总时长（秒），用 oracle 校验比特率合理性。
    let real_dur = if probe.bitrate > 0 {
        probe.content_length * 8 / probe.bitrate as u64
    } else {
        oracle_dur
    };
    let dur = real_dur.max(1);
    let mut byte_offset = if dur > 0 {
        offset_sec.saturating_mul(probe.content_length) / dur
    } else {
        0
    };
    // 对齐到 ~414 字节帧边界（128kbps/44.1kHz Layer III = 417/418；向下取整 414 安全区）。
    byte_offset = byte_offset.saturating_sub(byte_offset % 414);
    byte_offset.min(probe.content_length.saturating_sub(1))
}

/// 用 Range 拉取音源字节流；不支持 Range（返回 200）时降级为从 0 播。
async fn fetch_track_stream(
    client: &reqwest::Client,
    src: &str,
    byte_offset: u64,
) -> Option<reqwest::Response> {
    if byte_offset == 0 {
        // 从头播，普通 GET。
        return match client.get(src).send().await {
            Ok(r) if r.status().is_success() => Some(r),
            Ok(r) => {
                warn!("FM upstream {} -> {}", src, r.status());
                None
            }
            Err(e) => {
                warn!("FM upstream {} failed: {}", src, e);
                None
            }
        };
    }
    // Range seek。
    let range = format!("bytes={}-", byte_offset);
    match client.get(src).header(header::RANGE, range).send().await {
        Ok(r) if r.status().as_u16() == 206 => Some(r),
        Ok(r) if r.status().is_success() => {
            // 上游忽略 Range，返回完整 200 → 从 0 播（不阻断）。
            warn!("FM upstream {} ignored Range ({}), playing from 0", src, r.status());
            Some(r)
        }
        Ok(r) => {
            warn!("FM upstream {} -> {}", src, r.status());
            None
        }
        Err(e) => {
            warn!("FM upstream {} failed: {}", src, e);
            None
        }
    }
}

/// 解析 MPEG 帧头前 4 字节，返回比特率（bps）。
///
/// 仅识别 MPEG-1/2 Layer III（网易云外链常见格式）。
/// 失败返回 None。
fn parse_mpeg_bitrate(head: &[u8]) -> Option<u32> {
    if head.len() < 4 {
        return None;
    }
    let b0 = head[0];
    let b1 = head[1];
    // 同步字：11 位全 1（0xFFE / 0xFFF）。
    if b0 != 0xFF || (b1 & 0xE0) != 0xE0 {
        return None;
    }
    let version_bits = (b1 >> 3) & 0x03; // 00=2.5 01=保留 10=2 11=1
    let layer_bits = (b1 >> 1) & 0x03; // 01=Layer III 10=Layer II 11=Layer I
    if layer_bits != 0x01 {
        return None; // 仅处理 Layer III
    }
    let bitrate_index = (head[2] >> 4) & 0x0F;
    if bitrate_index == 0 || bitrate_index == 0x0F {
        return None; // free / bad
    }
    // MPEG-1 Layer III 比特率表（kbps），索引 1..14。
    const MPEG1_L3: [u32; 16] =
        [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0];
    // MPEG-2/2.5 Layer III 比率表（kbps）。
    const MPEG2_L3: [u32; 16] =
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0];
    let kbps = match version_bits {
        0x03 => MPEG1_L3[bitrate_index as usize], // MPEG-1
        0x02 | 0x00 => MPEG2_L3[bitrate_index as usize], // MPEG-2 / 2.5
        _ => return None,
    };
    if kbps == 0 {
        return None;
    }
    Some(kbps * 1000)
}

/// 曲目探测结果：总字节数 + 比特率（bps）。
struct TrackProbe {
    content_length: u64,
    bitrate: u32,
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
}

/// GET /fm/meta — 返回当前播放曲目的元数据。
///
/// 前端在挂载 `/fm/live` 后调用此端点获取初始曲目信息，
/// 后续切歌由 Tauri `fm-meta` 事件推送（无需轮询）。
pub(crate) async fn fm_current_meta(State(state): State<Arc<AppState>>) -> Json<FmMetaResponse> {
    let (artist, title, index) = state.fm.current_meta();
    Json(FmMetaResponse {
        artist,
        title,
        index,
    })
}
