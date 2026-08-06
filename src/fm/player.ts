// Claude FM — 全局播放器单例。
//
// 播放模型：真正的「直播流」——整张歌单是一条永不暂停的墙钟时间轴。
//   pos = (Date.now() / 1000) % TOTAL_DURATION
// 锚点固定为 Unix epoch（Date.now() == 0 的时刻），时间轴完全由墙钟定义：
// 任何设备、任何时刻算出的是同一个直播位置。暂停只是静音，恢复时跳到
// 当前的直播位置（就像 YouTube Live 一样，暂停期间广播照播，回来时
// 你已错过那一小段）。
//
// 生命周期与应用进程绑定，而非任何视图组件：
// - 视图切换（路由卸载）不销毁 <audio>，音乐持续播放；
// - 窗口关闭只隐藏到托盘（见 src-tauri/src/lib.rs on_window_event），
//   进程与 webview 常驻，音乐同样持续；
// - 模块首次 import 时创建单例，进程退出即结束。

import { reactive, readonly, watch } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

interface Track { artist: string; title: string; id: number; dur: number }

// 歌单（网易云曲目 ID，按展示顺序循环）。dur 为网易云真实时长（秒）。
// 部分曲目未收录于网易云，暂从歌单中省略。
export const TRACKS: readonly Track[] = [
  { artist: 'Aedh', title: 'A Message For Cynthia', id: 1951672257, dur: 107 },
  { artist: 'Ben Seretan', title: 'criss cross applesauce right in the stream of the amp', id: 2118624028, dur: 321 },
  { artist: 'Ben Seretan', title: 'walls are humming', id: 2118624029, dur: 428 },
  { artist: 'Chad Crouch', title: 'Shipping Lanes', id: 1365588629, dur: 194 },
  { artist: 'Damon Boucher', title: 'Chill no. 1', id: 1458837958, dur: 68 },
  { artist: 'ERA C T NOD 1', title: 'better days', id: 1879479122, dur: 82 },
  { artist: 'E*Rock', title: 'Forest Clearing', id: 566288317, dur: 153 },
  { artist: 'Grabek', title: 'three', id: 1822177137, dur: 260 },
  { artist: 'Joya', title: 'Miss you', id: 2726124148, dur: 134 },
  { artist: 'Kyle Preston', title: 'We Made It. We Finally Made It', id: 1805007083, dur: 417 },
  { artist: 'Memory Palace', title: 'Tru Blue', id: 1947169149, dur: 189 },
  { artist: 'Owen Kelley', title: 'Tonkotsu (Reloaded)', id: 2033253579, dur: 168 },
  { artist: 'PADELM', title: 'Cloudscape Suspended', id: 1917893384, dur: 207 },
  { artist: 'Parker Tichko', title: 'Fiddleheads Unfurling', id: 2628859872, dur: 190 },
  { artist: 'Parker Tichko', title: 'Wilting in the wind', id: 2628860760, dur: 139 },
  { artist: 'Passport', title: 'Reunion', id: 1489250872, dur: 113 },
  { artist: 'Pothoa', title: 'driftwood', id: 2751540024, dur: 160 },
  { artist: 'Siren and the Sea', title: 'Instinct', id: 1481058563, dur: 234 },
  { artist: 'TERNS', title: 'Flux', id: 1866114631, dur: 160 },
  { artist: 'Yuuki Matthews', title: 'Cherry Blossom Petals', id: 2743042411, dur: 190 },
  { artist: 'Yuuki Matthews', title: 'Transient Glowing', id: 2743042413, dur: 183 },
];

/** 歌单总时长（秒）= 时间轴取模的周期 */
export const TOTAL_DURATION = TRACKS.reduce((a, t) => a + t.dur, 0);

/** 曲目起始位置的累计偏移（秒），curStart[i] = TRACKS[0..i) 总时长 */
const CUM_START: number[] = [];
{
  let acc = 0;
  for (const t of TRACKS) { CUM_START.push(acc); acc += t.dur; }
}

/** 播放器共享状态（readonly 暴露给视图，状态变更仅由本模块内部驱动） */
const state = reactive({
  /** 当前曲目下标（时间轴当前位置所属曲目） */
  index: 0,
  playing: false,
  loading: false,
  /** 预热是否完成（音源就绪）：解锁播放按钮与托盘 FM 项 */
  ready: false,
  /** 时间轴当前位置（秒，墙钟推导，暂停时也持续推进） */
  pos: 0,
});

export const fmState = readonly(state);

/** 当前曲目 */
export function currentTrack(): Track {
  return TRACKS[state.index];
}

/** 将流时钟位置换算为曲目下标 + 曲内秒数 */
export function locate(pos: number): { index: number; offset: number } {
  const p = ((pos % TOTAL_DURATION) + TOTAL_DURATION) % TOTAL_DURATION;
  // CUM_START 单调递增，二分查找当前曲目
  let lo = 0, hi = TRACKS.length - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (CUM_START[mid] <= p) lo = mid;
    else hi = mid - 1;
  }
  return { index: lo, offset: p - CUM_START[lo] };
}

// ── 直播锚点（固定为 Unix epoch：Date.now() == 0 的时刻） ──
// 时间轴完全由墙钟定义，无需持久化任何锚点：
// pos = (Date.now() / 1000) % TOTAL_DURATION
// 任何设备、任何时刻算出的是同一个直播位置（真正的电台语义）。
const ANCHOR = 0;

/** 当前直播位置（秒）——无论是否播放，时间轴都在推进 */
function wallPos(): number {
  return (Date.now() - ANCHOR) / 1000;
}

// ── 音频实例（双缓冲流水线） ──
// 两个 <audio> 实例：cur 播放当前曲目，next 预加载下一首。
// 当前曲目 canplay 后立即加载第 n+1 首；切歌时 swap 实例，零空窗。
// preload='auto'：设置 src 后立即开始缓冲（预热/预加载依赖此行为）；
// 'none' 会让浏览器完全不抓取数据，预加载失效。
const cur = new Audio();
const next = new Audio();
cur.preload = 'auto';
next.preload = 'auto';

/** 当前实例已就绪的曲目下标（-1 未就绪）；next 预加载的目标下标（-1 空闲） */
let curIndex = -1;
let nextIndex = -1;
/** 当前实例的音源 URL（避免重复解析） */
let curSrc = '';
/** 异步解析世代号，防止竞态 */
let gen = 0;
/** 曲目不可播放（VIP/下架）时设置的跳过期限：时间轴越过该曲后才重试 */
let skipUntil = 0;
let ticker: number | undefined;

/** 实时解析播放直链：优先 paugram 解析接口，失败回退网易云官方外链 */
async function resolveSrc(track: Track): Promise<string> {
  try {
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(), 8000);
    const res = await fetch('https://api.paugram.com/netease/?id=' + track.id, { signal: ctrl.signal });
    clearTimeout(timer);
    const data = await res.json();
    if (data && data.link) return data.link;
  } catch {
    // 解析服务不可用时回退官方外链（同样会 302 到音频 CDN）
  }
  return 'https://music.163.com/song/media/outer/url?id=' + track.id;
}

/** 加载第 n 首到指定实例（异步解析 + 挂源，预加载用） */
async function loadTo(el: HTMLAudioElement, n: number) {
  const src = await resolveSrc(TRACKS[n]);
  if (el === cur && state.index !== n) return; // 加载期间已切走
  el.src = src;
}

/**
 * 预加载管道：当前实例 canplay 后，立即开始加载第 n+1 首。
 * 切歌时 n+1 已就绪（或正在加载），swap 实例实现零空窗。
 */
function preloadNext() {
  if (curIndex < 0 || nextIndex >= 0) return; // 无当前曲 / 已在预加载
  const n = (curIndex + 1) % TRACKS.length;
  nextIndex = n;
  void loadTo(next, n)
    .catch(() => {})
    .finally(() => {
      nextIndex = -1;
    });
}

/** 用直播位置对齐：切歌时 swap 到已预加载的下一首实例 */
async function seekToTimeline() {
  // 重入守卫：直载分支 await resolveSrc 期间（最长 8s）ticker 每 250ms
  // 会再次调用本函数，若不拦截会叠加直载请求，且旧请求因 gen 变化早退
  // 时 state.loading 永不复位 → UI 卡死在 loading 旋转（忙等）。
  if (state.loading) return;
  const g = ++gen;
  const { index, offset } = locate(state.pos);
  if (index !== state.index || curIndex !== index) {
    state.index = index;
    // 目标曲已预加载在 next 实例 → 与 cur 互换，实现零空窗切换
    if (nextIndex === index) {
      cur.src = next.src;
      cur.currentTime = offset;
      curIndex = index;
      if (state.playing) {
        cur.play().catch((e) => {
          if ((e as Error).name !== 'AbortError') curIndex = -1;
        });
      }
      next.removeAttribute('src');
      nextIndex = -1;
      preloadNext(); // 继续预加载下下首
      return;
    }
    // 目标曲未预加载（如跳过失效曲目）：直接加载到 cur
    curIndex = index;
    state.loading = true;
    const src = await resolveSrc(TRACKS[index]);
    if (g !== gen) {
      state.loading = false; // 直载已被更新操作取代，复位 loading 避免卡死
      return;
    }
    state.loading = false;
    cur.src = src;
    cur.currentTime = offset;
    if (state.playing) {
      cur.play().catch((e) => {
        if ((e as Error).name !== 'AbortError') curIndex = -1;
      });
    }
    preloadNext();
    return;
  }
  // 同曲校准位置
  if (curIndex === index) {
    try {
      if (Math.abs(cur.currentTime - offset) > 2) cur.currentTime = offset;
    } catch {
      // 非播放状态 seek 可能抛错，忽略
    }
  }
}

/** 每 250ms 一次心跳：无论是否播放，都推进时间轴（直播语义） */
function tick() {
  state.pos = wallPos();
  if (state.playing) {
    // 加载中跳过对齐：直载 await resolveSrc 期间（最长 8s）不再发起
    // 新的 seekToTimeline，避免叠加请求（seekToTimeline 内已有守卫，
    // 此处短路省一次函数调用）
    if (state.loading) return;
    // 播放中：跳过期限已过则重新加载该曲（可能恢复了版权/上架）
    if (skipUntil !== 0 && state.pos >= skipUntil) skipUntil = 0;
    if (skipUntil === 0) void seekToTimeline();
  }
}

function startTicker() {
  if (ticker !== undefined) return;
  ticker = window.setInterval(tick, 250);
}

function stopTicker() {
  if (ticker !== undefined) {
    window.clearInterval(ticker);
    ticker = undefined;
  }
}

function play() {
  // 直播语义：从「当前直播位置」开始，而不是从上次暂停位置
  state.playing = true;
  startTicker();
  tick();
  cur.play().catch((e) => {
    if ((e as Error).name !== 'AbortError') {
      // 播放被拒（曲目源失效/解码失败）：标记跳过，防止下一 tick 立刻重试
      // 同一曲目（无限循环忙等）。时间轴越过后（skipUntil）自动重试。
      const { index } = locate(state.pos);
      skipUntil = CUM_START[index] + TRACKS[index].dur + TRACKS[index].dur;
      // 退回暂停态（时间轴照走）
      state.playing = false;
      stopTicker();
    }
  });
}

function pause() {
  // 暂停只静音；时间轴继续推进，恢复时跳到当前直播位置
  state.playing = false;
  stopTicker();
  state.pos = wallPos();
  cur.pause();
}

function toggle() {
  if (state.loading) return;
  if (!state.ready) return; // 预热未完成，播放按钮/tray 均禁用
  if (state.playing) pause();
  else play();
}

/** 曲目外链失效（VIP/下架等）：跳过该曲，等时间轴越过后再尝试 */
function onError() {
  if (!state.playing) return;
  // 在途加载（直载 await resolveSrc 期间）src 尚未挂载，error 可能来自
  // 上一首残留实例，忽略避免重复跳过
  if (state.loading) return;
  const { index } = locate(state.pos);
  // 跳过本曲剩余时长 + 下一曲（本轮循环内不再重试该曲）
  skipUntil = CUM_START[index] + TRACKS[index].dur + TRACKS[index].dur;
  void seekToTimeline();
}

/** 单曲自然播完（ended）：时间轴继续，跳到下一曲边界（时间轴推导） */
function onEnded() {
  if (!state.playing) return;
  state.pos = wallPos();
  void seekToTimeline();
}

cur.addEventListener('ended', onEnded);
cur.addEventListener('error', onError);
// 当前曲目就绪后，立即预加载下一首（第 n+1 首）
cur.addEventListener('canplay', preloadNext);

// ── 启动预热 ──
// 应用启动即解析「当前直播位置对应曲目」的直链并预缓冲，
// 用户点击播放时几乎零延迟。
// （加载链路：解析 URL → 302 → 解码，需数秒；预热把这段挪到启动空闲期）
// 预热完成前：播放按钮禁用、托盘 FM 菜单项隐藏（fm_ready 通知后端）。
let prewarmed = false;

async function prewarm() {
  if (prewarmed) return;
  prewarmed = true;
  if (state.playing) return;
  const pos = wallPos();
  const { index } = locate(pos);
  // 预热「当前直播曲目」（而非固定第一首），保证首播即时且位置准确
  const src = await resolveSrc(TRACKS[index]);
  if (state.playing || state.index !== index) return;
  curSrc = src;
  curIndex = index;
  cur.src = src;
  // 首曲就绪（可缓冲）即解锁；同时触发 canplay → 预加载下一首
  state.ready = true;
  cur.addEventListener(
    'canplay',
    () => {
      if (!prewarmed) return;
      // 通知后端解锁托盘 FM 项（预热完成）
      invoke('fm_ready').catch(() => {});
      prewarmed = false; // 仅通知一次
    },
    { once: true },
  );
}

// 组件挂载时调用（main.ts 启动即调用）
export function initPrewarm() {
  void prewarm();
}

// ── 托盘联动（Tauri） ──
// 托盘菜单勾选项与播放状态双向同步：
// - 菜单点击 → Rust emit 'fm-toggle' → 这里 toggle()；
// - 播放状态变化 → watch → invoke('fm_set_playing') 回写勾选。
// 非 Tauri 环境（纯浏览器调试）invoke/listen 抛错或缺失，静默降级。
let trayBound = false;

async function bindTray() {
  if (trayBound || !('__TAURI_INTERNALS__' in window)) return;
  trayBound = true;
  try {
    await listen('fm-toggle', () => toggle());
    watch(
      () => state.playing,
      (playing) => {
        invoke('fm_set_playing', { playing }).catch(() => {});
      },
    );
  } catch {
    // 非 Tauri 环境无托盘，忽略
  }
}

// 组件挂载时调用（App.vue 或首次进入 /fm 时触发皆可）
export function initTraySync() {
  void bindTray();
}

export const fmPlayer = { toggle };

// 模块初始化：同步一次直播位置（立即让 index 指向当前直播曲目，
// 预热与 UI 都以此为准；此后由 ticker 持续推进）
state.pos = wallPos();
state.index = locate(state.pos).index;
