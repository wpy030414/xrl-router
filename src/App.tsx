import { RouterProvider } from 'react-router';
import { router } from './router';
import { useEffect } from 'react';
import { wsClient } from './lib/ws';
import { useSettingsStore } from '@/stores/settings';
import { isTauri, listen } from './lib/tauri';

export function App() {
  useEffect(() => {
    // 应用启动时全局拉取设置（侧边栏审查入口等依赖 audit_enabled）
    useSettingsStore.getState().fetchSettings().catch(() => {});

    // 连接 WebSocket
    wsClient.connect();

    // 淡出启动画面
    const splash = document.getElementById('splash');
    if (splash) {
      splash.classList.add('fade-out');
      splash.addEventListener('transitionend', () => splash.remove());
    }

    // Tauri 事件监听
    if (isTauri()) {
      // plugin-register 由 PluginRegisterDialog 自监听处理

      listen('plugin-offline', (payload: any) => {
        console.log('[Plugin] Offline:', payload);
      });

      listen('plugin-online', (payload: any) => {
        console.log('[Plugin] Online:', payload);
      });
    }

    return () => {
      wsClient.disconnect();
    };
  }, []);

  return <RouterProvider router={router} />;
}
