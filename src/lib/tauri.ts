// Tauri API 封装
// 非 Tauri 环境（浏览器）下经 isTauri() 守卫安全降级；
// 各模块顶层不触碰 window，浏览器中静态导入无副作用。

import { invoke as invokeCore } from '@tauri-apps/api/core';
import { listen as listenEvent } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { getVersion } from '@tauri-apps/api/app';

export function isTauri(): boolean {
  return '__TAURI_INTERNALS__' in window;
}

/** 检测是否为 Windows 平台（同步版本，用 UA 检测） */
export function isWindows(): boolean {
  if (!isTauri()) return false;
  const ua = navigator.userAgent.toLowerCase();
  return ua.includes('win');
}

/** 重新导出 getCurrentWindow 供前端窗口控制使用 */
export { getCurrentWindow };

export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T | null> {
  if (!isTauri()) return null;
  try {
    return await invokeCore<T>(cmd, args);
  } catch {
    return null;
  }
}

export async function listen<T>(
  event: string,
  handler: (payload: T) => void
): Promise<() => void> {
  if (!isTauri()) return () => {};
  try {
    const unlisten = await listenEvent<T>(event, (e) => handler(e.payload));
    return unlisten;
  } catch {
    return () => {};
  }
}

// 插件按需使用的快捷函数（插件包各自入独立 chunk，保持动态加载）
export const tauriAutostart = {
  isEnabled: async () => {
    if (!isTauri()) return false;
    try {
      const { isEnabled } = await import('@tauri-apps/plugin-autostart');
      return await isEnabled();
    } catch {
      return false;
    }
  },
  enable: async () => {
    if (!isTauri()) return;
    try {
      const { enable } = await import('@tauri-apps/plugin-autostart');
      await enable();
    } catch {
      // ignore
    }
  },
  disable: async () => {
    if (!isTauri()) return;
    try {
      const { disable } = await import('@tauri-apps/plugin-autostart');
      await disable();
    } catch {
      // ignore
    }
  },
};

export const tauriDialog = {
  save: async (opts: any) => {
    if (!isTauri()) return null;
    try {
      const { save } = await import('@tauri-apps/plugin-dialog');
      return await save(opts);
    } catch {
      return null;
    }
  },
  open: async (opts: any) => {
    if (!isTauri()) return null;
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      return await open(opts);
    } catch {
      return null;
    }
  },
};

export const tauriFs = {
  readTextFile: async (path: string) => {
    if (!isTauri()) return null;
    try {
      const { readTextFile } = await import('@tauri-apps/plugin-fs');
      return await readTextFile(path);
    } catch {
      return null;
    }
  },
  writeTextFile: async (path: string, content: string) => {
    if (!isTauri()) return;
    try {
      const { writeTextFile } = await import('@tauri-apps/plugin-fs');
      await writeTextFile(path, content);
    } catch {
      // ignore
    }
  },
};

export const tauriShell = {
  open: async (url: string) => {
    if (!isTauri()) return;
    try {
      const { open } = await import('@tauri-apps/plugin-shell');
      await open(url);
    } catch {
      // ignore
    }
  },
};

export const tauriWindow = {
  setTheme: async (theme: 'light' | 'dark' | null) => {
    if (!isTauri()) return;
    try {
      await getCurrentWindow().setTheme(theme);
    } catch {
      // ignore
    }
  },
};

export const tauriApp = {
  getVersion: async () => {
    if (!isTauri()) return null;
    try {
      return await getVersion();
    } catch {
      return null;
    }
  },
};
