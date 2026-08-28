import React from 'react';
import ReactDOM from 'react-dom/client';
import { App } from './App';
import { WallpaperScene } from './components/WallpaperScene';
import './index.css';
import { initI18n } from './i18n';
import { initTheme } from './hooks/useTheme';

declare global {
  interface Window {
    /** Rust 壁纸窗口注入的标志（WebviewWindowBuilder::initialization_script）。 */
    __WALLPAPER_MODE__?: boolean;
  }
}

// 壁纸窗口（label = wallpaper）：只渲染像素艺术，不走 AppShell/router，
// 也无需 i18n/主题（壁纸无文案、黑底 + 像素画与主题无关）。
if (window.__WALLPAPER_MODE__ === true) {
  ReactDOM.createRoot(document.getElementById('root')!).render(
    <React.StrictMode>
      <WallpaperScene />
    </React.StrictMode>
  );
} else {
  // 初始化 i18n 与主题（主题如不在此处应用，懒加载设置页之前 data-theme 将缺失）
  initI18n();
  initTheme();

  ReactDOM.createRoot(document.getElementById('root')!).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>
  );
}
