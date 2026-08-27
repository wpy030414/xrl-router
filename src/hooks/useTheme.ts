import { useEffect } from 'react';
import { create } from 'zustand';

type Theme = 'light' | 'dark' | 'system';

interface ThemeState {
  theme: Theme;
  hue: number;
  setTheme: (theme: Theme) => void;
  setHue: (hue: number) => void;
}

export const useThemeStore = create<ThemeState>((set) => ({
  theme: (localStorage.getItem('theme') as Theme) || 'system',
  hue: parseInt(localStorage.getItem('theme-hue') || '200'),
  setTheme: (theme) => {
    localStorage.setItem('theme', theme);
    applyTheme(theme);
    set({ theme });
  },
  setHue: (hue) => {
    localStorage.setItem('theme-hue', String(hue));
    applyHue(hue);
    set({ hue });
  },
}));

function applyTheme(theme: Theme) {
  const resolved = theme === 'system' ? getSystemTheme() : theme;
  document.documentElement.setAttribute('data-theme', resolved);
  applyHue(useThemeStore.getState().hue);
  syncTauriWindowTheme(resolved);
}

function getSystemTheme(): 'light' | 'dark' {
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
}

async function syncTauriWindowTheme(theme: 'light' | 'dark') {
  if (!('__TAURI_INTERNALS__' in window)) return;
  try {
    const { getCurrentWindow } = await import('@tauri-apps/api/window');
    await getCurrentWindow().setTheme(theme);
  } catch {
    // ignore
  }
}

// 基于 HSL 色相生成主题色 CSS 变量（替代 @material/material-color-utilities）
// 注意：shadcn 的 Tailwind 颜色是 hsl(var(--primary)) 包裹消费，--primary 必须写成
// 「空格分隔三元组」（如 "200 70% 65%"）；写成完整 hsl(...) 会构成嵌套 hsl(hsl(...))
// 非法 CSS，导致所有 primary 色元素透明失效。
function applyHue(hue: number) {
  const root = document.documentElement.style;
  const isDark = document.documentElement.getAttribute('data-theme') === 'dark';

  // 根据色相生成 primary 色
  const primaryLight = `${hue} 70% 45%`;
  const primaryDark = `${hue} 70% 65%`;
  const primaryForeground = `0 0% ${isDark ? '10%' : '98%'}`;

  root.setProperty('--primary', isDark ? primaryDark : primaryLight);
  root.setProperty('--primary-foreground', primaryForeground);

  // 品牌色（直接以完整色值被 var() 消费，无需三元组）
  root.setProperty('--color-openai-brand', '#10a37f');
  root.setProperty('--color-anthropic-brand', '#d97757');
}

export function initTheme() {
  applyTheme(useThemeStore.getState().theme);
}

export function useTheme() {
  const { theme, hue, setTheme, setHue } = useThemeStore();

  useEffect(() => {
    applyTheme(theme);
  }, [theme]);

  useEffect(() => {
    applyHue(hue);
  }, [hue]);

  useEffect(() => {
    const mq = window.matchMedia('(prefers-color-scheme: dark)');
    const handler = () => {
      if (useThemeStore.getState().theme === 'system') {
        applyTheme('system');
      }
    };
    mq.addEventListener('change', handler);
    return () => mq.removeEventListener('change', handler);
  }, []);

  return { theme, hue, setTheme, setHue };
}
