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
//
// 色彩生成使用 Material 官方的 HCT 色彩空间（基于 CIE Lab），
// 确保所有色相都有正确的感知亮度（tone）和对比度层次。

import { settingsApi } from './api';
import { Hct, SchemeContent, MaterialDynamicColors } from '@material/material-color-utilities';

// Tauri 窗口主题同步：动态导入，非 Tauri 环境不加载。
async function applyWindowTheme(t: 'light' | 'dark' | null) {
  if (!('__TAURI_INTERNALS__' in window)) return;
  try {
    const { getCurrentWindow } = await import('@tauri-apps/api/window');
    await getCurrentWindow().setTheme(t);
  } catch {
    // 非 Tauri 环境无原生窗口，忽略
  }
}

export type Theme = 'light' | 'dark' | 'system';

const STORAGE_KEY = 'theme';
const HUE_KEY = 'theme-hue';
const DEFAULT_HUE = 200;

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
// 使用 Material 官方的 HCT 色彩空间（基于 CIE Lab），
// 确保所有色相都有正确的感知亮度（tone）和对比度层次。

function applyHue(hue: number) {
  const root = document.documentElement.style;

  const isDark = document.documentElement.getAttribute('data-theme') === 'dark';

  // 将 HSL 色相转换为 ARGB，让 HCT 提取正确的色相
  // 滑块使用 HSL 色相（264° = 紫色），HCT 色相空间不同
  const hslToArgb = (h: number, s: number, l: number): number => {
    s /= 100;
    l /= 100;
    const k = (n: number) => (n + h / 30) % 12;
    const a = s * Math.min(l, 1 - l);
    const f = (n: number) => l - a * Math.max(-1, Math.min(k(n) - 3, Math.min(9 - k(n), 1)));
    const r = Math.round(f(0) * 255);
    const g = Math.round(f(8) * 255);
    const b = Math.round(f(4) * 255);
    return (0xff << 24) | (r << 16) | (g << 8) | b;
  };

  // 用中等饱和度生成种子颜色，HCT 会提取正确的色相
  const seedArgb = hslToArgb(hue, 70, 50);
  const seedColor = Hct.fromInt(seedArgb);

  // SchemeContent 生成 Material Design 3 的动态配色方案
  const scheme = new SchemeContent(seedColor, isDark, 0);

  // MaterialDynamicColors 提供所有 MD3 标准色
  const colors = new MaterialDynamicColors();

  // 将 ARGB 整数转换为 CSS 格式
  const toCss = (argb: number) => {
    const r = (argb >> 16) & 0xff;
    const g = (argb >> 8) & 0xff;
    const b = argb & 0xff;
    return `rgb(${r}, ${g}, ${b})`;
  };

  // 设置 CSS 变量
  const set = (name: string, argb: number) => {
    root.setProperty(name, toCss(argb));
  };

  // Primary 角色
  set('--md-sys-color-primary', colors.primary().getArgb(scheme));
  set('--md-sys-color-on-primary', colors.onPrimary().getArgb(scheme));
  set('--md-sys-color-primary-container', colors.primaryContainer().getArgb(scheme));
  set('--md-sys-color-on-primary-container', colors.onPrimaryContainer().getArgb(scheme));
  set('--md-sys-color-inverse-primary', colors.inversePrimary().getArgb(scheme));

  // Secondary 角色（低饱和度的 primary 色相）
  set('--md-sys-color-secondary', colors.secondary().getArgb(scheme));
  set('--md-sys-color-on-secondary', colors.onSecondary().getArgb(scheme));
  set('--md-sys-color-secondary-container', colors.secondaryContainer().getArgb(scheme));
  set('--md-sys-color-on-secondary-container', colors.onSecondaryContainer().getArgb(scheme));

  // Tertiary 角色（色相偏移约 +60°）
  set('--md-sys-color-tertiary', colors.tertiary().getArgb(scheme));
  set('--md-sys-color-on-tertiary', colors.onTertiary().getArgb(scheme));
  set('--md-sys-color-tertiary-container', colors.tertiaryContainer().getArgb(scheme));
  set('--md-sys-color-on-tertiary-container', colors.onTertiaryContainer().getArgb(scheme));

  // Surface 角色（基于 primary 色相的极低饱和度变体）
  set('--md-sys-color-surface', colors.surface().getArgb(scheme));
  set('--md-sys-color-on-surface', colors.onSurface().getArgb(scheme));
  set('--md-sys-color-surface-variant', colors.surfaceVariant().getArgb(scheme));
  set('--md-sys-color-on-surface-variant', colors.onSurfaceVariant().getArgb(scheme));
  set('--md-sys-color-surface-dim', colors.surfaceDim().getArgb(scheme));
  set('--md-sys-color-surface-bright', colors.surfaceBright().getArgb(scheme));
  set('--md-sys-color-surface-container-lowest', colors.surfaceContainerLowest().getArgb(scheme));
  set('--md-sys-color-surface-container-low', colors.surfaceContainerLow().getArgb(scheme));
  set('--md-sys-color-surface-container', colors.surfaceContainer().getArgb(scheme));
  set('--md-sys-color-surface-container-high', colors.surfaceContainerHigh().getArgb(scheme));
  set('--md-sys-color-surface-container-highest', colors.surfaceContainerHighest().getArgb(scheme));

  // Outline 角色
  set('--md-sys-color-outline', colors.outline().getArgb(scheme));
  set('--md-sys-color-outline-variant', colors.outlineVariant().getArgb(scheme));

  // Inverse 角色
  set('--md-sys-color-inverse-surface', colors.inverseSurface().getArgb(scheme));
  set('--md-sys-color-inverse-on-surface', colors.inverseOnSurface().getArgb(scheme));

  // Background 角色（通常与 surface 相同）
  set('--md-sys-color-background', colors.background().getArgb(scheme));
  set('--md-sys-color-on-background', colors.onBackground().getArgb(scheme));
}

// 同步 UI 设置到后端（LAN install 页面可读取）
async function syncThemeToBackend(t: Theme) {
  try {
    await settingsApi.update({ theme: t });
  } catch {
    // API 不可用（纯前端调试），忽略
  }
}

async function syncHueToBackend(hue: number) {
  try {
    await settingsApi.update({ hue });
  } catch {
    // API 不可用，忽略
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
  void syncThemeToBackend(t);
}

export function setHue(hue: number) {
  localStorage.setItem(HUE_KEY, String(hue));
  applyHue(hue);
  void syncHueToBackend(hue);
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
