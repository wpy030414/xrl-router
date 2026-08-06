// 主题切换：light / dark / system，持久化到 localStorage，默认跟随系统。
// 通过 <html data-theme="light|dark"> 触发 index.html 里的 token 切换，
// 同时同步 Tauri 原生窗口主题，使标题栏等系统 UI 跟随。
//
// 重要：Tauri 中 window.setTheme('light'/'dark') 会强制窗口主题，而 WebView
// 的 prefers-color-scheme media query 跟随窗口主题。因此「跟随系统」模式下
// 必须 setTheme(null) 取消强制，media query 才能反映真实系统主题——
// 否则曾选过深色后，media query 永远返回深色，跟随系统失效。
//
// 令牌色（accent hue）：0-360 色相滑块，持久化到 localStorage。
// 通过覆盖 --md-sys-color-primary 系列 CSS 变量驱动全局主题色。
// 默认色相 264°（MD3 标准紫）。

import { getCurrentWindow } from '@tauri-apps/api/window';

export type Theme = 'light' | 'dark' | 'system';

const STORAGE_KEY = 'theme';
const HUE_KEY = 'theme-hue';
const DEFAULT_HUE = 264;

function systemTheme(): 'light' | 'dark' {
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
}

export function getTheme(): Theme {
  const saved = localStorage.getItem(STORAGE_KEY);
  if (saved === 'light' || saved === 'dark') return saved;
  return 'system';
}

// ── 令牌色 ──

export function getHue(): number {
  const saved = localStorage.getItem(HUE_KEY);
  if (saved !== null) {
    const n = parseInt(saved, 10);
    if (!isNaN(n) && n >= 0 && n <= 360) return n;
  }
  return DEFAULT_HUE;
}

// 从色相值生成完整 MD3 色彩令牌并覆盖到 :root。
// 派生规则（HCT→HSL 近似）：
//   primary   = 主色相，全饱和
//   secondary = 同色相，低饱和（柔和）
//   tertiary  = 色相偏移 +60°
//   surface   = 极低饱和度微染主色调
//   outline   = 低饱和中性
//   error     = 固定红，不随主色调变化
// 当 hue === DEFAULT_HUE 时移除所有覆盖，回退到 global.css 内置令牌。

const HUE_TOKENS = [
  '--md-sys-color-primary', '--md-sys-color-on-primary',
  '--md-sys-color-primary-container', '--md-sys-color-on-primary-container',
  '--md-sys-color-secondary', '--md-sys-color-on-secondary',
  '--md-sys-color-secondary-container', '--md-sys-color-on-secondary-container',
  '--md-sys-color-tertiary', '--md-sys-color-on-tertiary',
  '--md-sys-color-tertiary-container', '--md-sys-color-on-tertiary-container',
  '--md-sys-color-inverse-primary',
  '--md-sys-color-surface', '--md-sys-color-on-surface',
  '--md-sys-color-surface-variant', '--md-sys-color-on-surface-variant',
  '--md-sys-color-surface-dim', '--md-sys-color-surface-bright',
  '--md-sys-color-surface-container-lowest', '--md-sys-color-surface-container-low',
  '--md-sys-color-surface-container', '--md-sys-color-surface-container-high',
  '--md-sys-color-surface-container-highest',
  '--md-sys-color-outline', '--md-sys-color-outline-variant',
  '--md-sys-color-inverse-surface', '--md-sys-color-inverse-on-surface',
  '--md-sys-color-background', '--md-sys-color-on-background',
];

function applyHue(hue: number) {
  const root = document.documentElement.style;
  if (hue === DEFAULT_HUE) {
    for (const t of HUE_TOKENS) root.removeProperty(t);
    return;
  }

  const h = hue;
  const h2 = (hue + 60) % 360; // tertiary 色相偏移
  const isDark = document.documentElement.getAttribute('data-theme') === 'dark';

  function set(name: string, s: number, l: number, hVal: number = h) {
    root.setProperty(name, `hsl(${hVal}, ${s}%, ${l}%)`);
  }

  if (isDark) {
    // ── Dark ──
    // Primary（亮色 on dark bg）
    set('--md-sys-color-primary', 60, 78);
    set('--md-sys-color-on-primary', 48, 22);
    set('--md-sys-color-primary-container', 38, 36);
    set('--md-sys-color-on-primary-container', 100, 93);
    set('--md-sys-color-inverse-primary', 50, 42);

    // Secondary（柔和）
    set('--md-sys-color-secondary', 16, 82);
    set('--md-sys-color-on-secondary', 18, 22);
    set('--md-sys-color-secondary-container', 16, 30);
    set('--md-sys-color-on-secondary-container', 20, 92);

    // Tertiary（色相偏移 +60°）
    set('--md-sys-color-tertiary', 55, 82, h2);
    set('--md-sys-color-on-tertiary', 45, 22, h2);
    set('--md-sys-color-tertiary-container', 35, 34, h2);
    set('--md-sys-color-on-tertiary-container', 100, 93, h2);

    // Surface（微染主色调）
    set('--md-sys-color-surface', 8, 7);
    set('--md-sys-color-on-surface', 6, 92);
    set('--md-sys-color-surface-variant', 8, 28);
    set('--md-sys-color-on-surface-variant', 8, 80);
    set('--md-sys-color-surface-dim', 8, 7);
    set('--md-sys-color-surface-bright', 8, 22);
    set('--md-sys-color-surface-container-lowest', 6, 5);
    set('--md-sys-color-surface-container-low', 8, 10);
    set('--md-sys-color-surface-container', 8, 12);
    set('--md-sys-color-surface-container-high', 8, 16);
    set('--md-sys-color-surface-container-highest', 8, 20);
    set('--md-sys-color-outline', 8, 60);
    set('--md-sys-color-outline-variant', 8, 28);
    set('--md-sys-color-inverse-surface', 6, 92);
    set('--md-sys-color-inverse-on-surface', 8, 20);
    set('--md-sys-color-background', 8, 7);
    set('--md-sys-color-on-background', 6, 92);
  } else {
    // ── Light ──
    // Primary（深色 on light bg）
    set('--md-sys-color-primary', 50, 40);
    set('--md-sys-color-on-primary', 100, 98);
    set('--md-sys-color-primary-container', 100, 93);
    set('--md-sys-color-on-primary-container', 80, 14);
    set('--md-sys-color-inverse-primary', 60, 80);

    // Secondary（柔和）
    set('--md-sys-color-secondary', 16, 42);
    set('--md-sys-color-on-secondary', 100, 98);
    set('--md-sys-color-secondary-container', 20, 92);
    set('--md-sys-color-on-secondary-container', 18, 10);

    // Tertiary（色相偏移 +60°）
    set('--md-sys-color-tertiary', 45, 40, h2);
    set('--md-sys-color-on-tertiary', 100, 98, h2);
    set('--md-sys-color-tertiary-container', 100, 93, h2);
    set('--md-sys-color-on-tertiary-container', 75, 14, h2);

    // Surface（微染主色调）
    set('--md-sys-color-surface', 12, 97);
    set('--md-sys-color-on-surface', 8, 12);
    set('--md-sys-color-surface-variant', 10, 90);
    set('--md-sys-color-on-surface-variant', 8, 28);
    set('--md-sys-color-surface-dim', 10, 88);
    set('--md-sys-color-surface-bright', 12, 97);
    set('--md-sys-color-surface-container-lowest', 0, 100);
    set('--md-sys-color-surface-container-low', 12, 96);
    set('--md-sys-color-surface-container', 12, 94);
    set('--md-sys-color-surface-container-high', 10, 92);
    set('--md-sys-color-surface-container-highest', 10, 90);
    set('--md-sys-color-outline', 8, 48);
    set('--md-sys-color-outline-variant', 10, 80);
    set('--md-sys-color-inverse-surface', 8, 20);
    set('--md-sys-color-inverse-on-surface', 10, 95);
    set('--md-sys-color-background', 12, 97);
    set('--md-sys-color-on-background', 8, 12);
  }
}

// 同步 Tauri 原生窗口主题（标题栏等系统级 UI）。
// t 为 null = 取消强制，窗口恢复跟随系统（WebView media query 同步恢复真实值）。
// 非 Tauri 环境（纯浏览器调试）调用会抛错，静默忽略。
async function applyWindowTheme(t: 'light' | 'dark' | null) {
  try {
    await getCurrentWindow().setTheme(t);
  } catch {
    // 非 Tauri 环境无原生窗口，忽略
  }
}

export function setTheme(t: Theme) {
  localStorage.setItem(STORAGE_KEY, t);
  if (t === 'system') {
    document.documentElement.setAttribute('data-theme', systemTheme());
    applyHue(getHue());
    // 取消窗口强制 → WebView prefers-color-scheme 恢复跟随系统
    void applyWindowTheme(null);
  } else {
    document.documentElement.setAttribute('data-theme', t);
    applyHue(getHue());
    void applyWindowTheme(t);
  }
}

export function setHue(hue: number) {
  localStorage.setItem(HUE_KEY, String(hue));
  applyHue(hue);
}

export function initTheme() {
  const t = getTheme();
  if (t === 'system') {
    document.documentElement.setAttribute('data-theme', systemTheme());
    applyHue(getHue());
    void applyWindowTheme(null);
  } else {
    document.documentElement.setAttribute('data-theme', t);
    applyHue(getHue());
    void applyWindowTheme(t);
  }
}

// 监听系统主题变化，当用户选择「跟随系统」时自动响应。
// 窗口已取消强制（跟随系统），media query 会随系统变化自动触发 change。
export function initSystemThemeListener() {
  const mq = window.matchMedia('(prefers-color-scheme: dark)');
  mq.addEventListener('change', (e) => {
    if (getTheme() === 'system') {
      document.documentElement.setAttribute('data-theme', e.matches ? 'dark' : 'light');
      applyHue(getHue());
    }
  });
}
