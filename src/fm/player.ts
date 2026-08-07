// Claude FM — 前端播放器（极简版）。
//
// 所有播放逻辑（歌单、墙钟锚点、音源解析、切歌）在后端 FmEngine 完成。后端输出
// 一条永不关闭的 HTTP chunked 直播流 GET /fm/live，前端只需一个 <audio> 标签收听。
//
// **caption 同步**：caption 基于 <audio>.currentTime（播放位置）计算，而非字节流
// 信号。后端 /fm/meta 给当前曲墙钟锚点 epoch，/fm/schedule 给曲目表；前端用
// timeupdate 事件，以 `Date.now()/1000 - epoch + currentTime` 在曲目表上二分当前曲，
// 使 caption 严格跟随播放进度，与耳朵对齐（无论客户端缓冲多少秒）。
//
// 生命周期与应用进程绑定，而非任何视图组件：
// - 路由切换不销毁 <audio>，音乐持续；
// - 窗口关闭只隐藏到托盘，进程与 webview 常驻，音乐同样持续；
// - 模块 import 时创建单例，进程退出即结束。

import { reactive, readonly, watch } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

interface FmTrack { artist: string; title: string; dur: number }
interface FmMeta { artist: string; title: string; index: number; epoch: number }

/** 播放器共享状态（readonly 暴露给视图） */
const state = reactive({
  /** <audio> canplay 后置 true：解锁播放按钮与托盘 FM 项 */
  ready: false,
  /** 播放/暂停（由 <audio> 本地管理） */
  playing: false,
  /** 当前曲目元数据（由 timeupdate + 锚点计算驱动更新） */
  track: { artist: '', title: '', index: 0 } as { artist: string; title: string; index: number },
});

export const fmState = readonly(state);

/** 模块级单例 <audio>：与路由/组件解耦，进程级生命周期 */
const radio = new Audio();
radio.preload = 'auto';

/** 本机 gateway 地址（Tauri 环境由后端查询，纯浏览器用 vite dev proxy） */
let gatewayBase = '';

// ── caption 同步状态 ──
// 锚点：当前曲目在墙钟上的起点 epoch 秒。从 /fm/meta 拉取。
// 曲目表：从 /fm/schedule 拉取，用于按墙钟偏移二分当前曲。
let anchorEpoch = 0;
let anchorIndex = 0;
let schedule: FmTrack[] = [];
let totalDur = 0;

async function initGatewayBase() {
  if ('__TAURI_INTERNALS__' in window) {
    try {
      gatewayBase = await invoke<string>('get_gateway_base');
      return;
    } catch { /* fallthrough */ }
  }
  gatewayBase = '';
}

/** 用 currentTime + 锚点在曲目表上二分当前曲，更新 caption。 */
function recomputeCaption() {
  if (!schedule.length || !anchorEpoch) return;
  // 墙钟偏移（秒）= 此刻墙钟距锚点曲起点的秒数 + 客户端播放位置。
  // 加 currentTime 让 caption 跟随播放进度而非读取进度，与耳朵对齐。
  const nowSec = Math.floor(Date.now() / 1000);
  const offset = (nowSec - anchorEpoch) + radio.currentTime;
  const pos = ((offset % totalDur) + totalDur) % totalDur;
  // 二分：找最后一首 cum_start <= pos。
  let acc = 0;
  let idx = 0;
  for (let i = 0; i < schedule.length; i++) {
    if (acc <= pos) idx = i;
    else break;
    acc += schedule[i].dur;
  }
  const t = schedule[idx];
  if (state.track.index !== idx || state.track.title !== t.title) {
    state.track = { artist: t.artist, title: t.title, index: idx };
  }
}

/** 挂载直播流源：启动时调用（main.ts initPrewarm） */
async function prewarm() {
  await initGatewayBase();

  // 拉取锚点 + 曲目表（caption 同步的依据）。
  try {
    const [meta, sched] = await Promise.all([
      fetch(`${gatewayBase}/fm/meta`).then(r => r.json()) as Promise<FmMeta>,
      fetch(`${gatewayBase}/fm/schedule`).then(r => r.json()) as Promise<FmTrack[]>,
    ]);
    anchorEpoch = meta.epoch;
    anchorIndex = meta.index;
    schedule = sched;
    totalDur = sched.reduce((s, t) => s + t.dur, 0);
    state.track = { artist: meta.artist, title: meta.title, index: meta.index };
  } catch { /* 后端未就绪时静默 */ }

  // 挂上永不关闭的直播流
  radio.src = `${gatewayBase}/fm/live`;

  radio.addEventListener('canplay', () => {
    if (!state.ready) {
      state.ready = true;
      invoke('fm_ready').catch(() => {});
    }
  }, { once: true });

  // caption 同步：每次播放进度推进 → 重算当前曲。
  // timeupdate ≈ 4Hz，足够流畅；不依赖全局 fm-meta 事件（避免与解码进度错位）。
  radio.addEventListener('timeupdate', recomputeCaption);

  // 流中断恢复：error/stalled/waiting → 指数退避重连 /fm/live。
  // 后端广播为永不关闭的 chunked 流，上游掉线/网络抖动会让 <audio> 静默挂死，
  // 这里兜底重挂源续播。重连后锚点不变（墙钟继续走），caption 自动续上。
  let retry = 0;
  let retryTimer: ReturnType<typeof setTimeout> | null = null;
  const reattach = () => {
    if (retryTimer) clearTimeout(retryTimer);
    const delay = Math.min(1000 * 2 ** retry, 8000); // 1s→2s→4s→8s 封顶
    retry += 1;
    retryTimer = setTimeout(() => {
      try {
        radio.src = `${gatewayBase}/fm/live`;
        radio.load();
        if (state.playing) radio.play().catch(() => {});
      } catch { /* 静默，等待下次重试 */ }
    }, delay);
  };
  for (const ev of ['error', 'stalled', 'waiting'] as const) {
    radio.addEventListener(ev, () => {
      if (!state.playing) return; // 暂停时不主动重连
      reattach();
    });
  }
  // 成功恢复播放 → 清零退避计数。
  radio.addEventListener('playing', () => { retry = 0; });
}

// ── 播放控制 ──

function play() {
  state.playing = true;
  radio.play().catch(() => { state.playing = false; });
}

function pause() {
  state.playing = false;
  radio.pause();
}

function toggle() {
  if (!state.ready) return;
  if (state.playing) pause(); else play();
}

export const fmPlayer = { toggle };

// ── 托盘联动（Tauri） ──

let trayBound = false;

async function bindTray() {
  if (trayBound || !('__TAURI_INTERNALS__' in window)) return;
  trayBound = true;
  try {
    // 托盘菜单点击 → toggle
    await listen('fm-toggle', () => toggle());
    // 播放状态 → 同步托盘勾选
    watch(
      () => state.playing,
      (playing) => { invoke('fm_set_playing', { playing }).catch(() => {}); },
    );
  } catch { /* 非 Tauri 环境 */ }
}

export function initPrewarm() { void prewarm(); }
export function initTraySync() { void bindTray(); }
