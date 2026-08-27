import { RouterProvider } from 'react-router-dom';
import { router } from './router';
import { useEffect } from 'react';
import { wsClient } from './lib/ws';
import { isTauri, listen } from './lib/tauri';

export function App() {
  useEffect(() => {
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
      listen('plugin-register', (payload: any) => {
        console.log('[Plugin] Register event:', payload);
        // TODO: 触发 PluginRegisterDialog
      });

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
