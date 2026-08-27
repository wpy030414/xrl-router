import React from 'react';
import ReactDOM from 'react-dom/client';
import { App } from './App';
import './index.css';
import { initI18n } from './i18n';
import { initTheme } from './hooks/useTheme';

// 初始化 i18n 与主题（主题如不在此处应用，懒加载设置页之前 data-theme 将缺失）
initI18n();
initTheme();

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
