// Claude FM — 前端播放器（极简版）。
//
// 所有播放逻辑（歌单、时钟、预加载、切歌）已移入 Rust 后端 FmEngine。
// 后端输出一条永不关闭的 HTTP chunked 直播流 GET /fm/live，
// 前端只需一个 <audio> 标签像收音机一样收听。
//
// 生命周期与应用进程绑定，而非任何视图组件：
// - 路由切换不销毁 <audio>，音乐持续；
// - 窗口关闭只隐藏到托盘，进程与 webview 常驻，音乐同样持续；
// - 模块 import 时创建单例，进程退出即结束。

import { reactive, readonly, watch } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

interface FmMeta { artist: string; title: string; index: number }

/** 播放器共享状态（readonly 暴露给视图） */
const state = reactive({
  /** <audio> canplay 后置 true：解锁播放按钮与托盘 FM 项 */
  ready: false,
  /** 播放/暂停（由 <audio> 本地管理） */
  playing: false,
  /** 当前曲目元数据（由 fm-meta 事件驱动更新） */
  track: { artist: '', title: '', index: 0 } as FmMeta,
});

export const fmState = readonly(state);

/** 模块级单例 <audio>：与路由/组件解耦，进程级生命周期 */
const radio = new Audio();
radio.preload = 'auto';

/** 本机 gateway 地址（Tauri 环境由后端查询，纯浏览器用 vite dev proxy） */
let gatewayBase = '';

async function initGatewayBase() {
  if ('__TAURI_INTERNALS__' in window) {
    try {
      gatewayBase = await invoke<string>('get_gateway_base');
      return;
    } catch { /* fallthrough */ }
  }
  gatewayBase = '';
}

/** 挂载直播流源：启动时调用（main.ts initPrewarm） */
async function prewarm() {
  await initGatewayBase();

  // 获取初始曲目元数据（首次连接前）
  try {
    const meta = await fetch(`${gatewayBase}/fm/meta`).then(r => r.json());
    state.track = meta;
  } catch { /* 后端未就绪时静默 */ }

  // 挂上永不关闭的直播流
  radio.src = `${gatewayBase}/fm/live`;

  radio.addEventListener('canplay', () => {
    if (!state.ready) {
      state.ready = true;
      invoke('fm_ready').catch(() => {});
    }
  }, { once: true });

  // 流中断恢复：error/stalled/waiting → 指数退避重连 /fm/live。
  // 后端广播为永不关闭的 chunked 流，上游掉线/网络抖动会让 <audio> 静默挂死，
  // 这里兜底重挂源续播。
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
    // 后端推送切歌元数据 → 更新 caption
    await listen<FmMeta>('fm-meta', (event) => {
      state.track = event.payload;
    });
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
