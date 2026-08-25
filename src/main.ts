import { createApp } from 'vue';
import { createPinia } from 'pinia';
import { createRouter, createWebHistory } from 'vue-router';
import App from './App.vue';
import { routes } from './router.js';
import { initTheme, initSystemThemeListener } from './theme';
import { initI18n } from './i18n';

// Material Web Components — 入口必需组件（ConnectionStatus / PluginRegisterDialog 使用）
// 必须在 mount 前同步加载：若延迟到挂载后，未定义的 custom element 会被浏览器
// 当普通元素渲染、直接显示 light DOM 内容（如 PluginRegisterDialog 的标题/按钮），
// 造成「发现空插件」的假弹窗。
import '@material/web/button/filled-button.js';
import '@material/web/button/text-button.js';
import '@material/web/icon/icon.js';
import '@material/web/dialog/dialog.js';

// ── 第一步：同步初始化（只读 localStorage + 设置 CSS，微秒级）──
// 这两步必须在挂载前完成，确保首帧就有正确的主题色和语言。
initTheme();
initSystemThemeListener();
initI18n();

// ── 第二步：创建 Vue 应用并立即挂载（尽快结束白屏）──
const app = createApp(App);
const pinia = createPinia();
const router = createRouter({
  history: createWebHistory(),
  routes,
});

// Global error handler for Vue components
app.config.errorHandler = (err, instance, info) => {
  console.error('[Vue Error]', err);
  console.error('[Component]', instance!.$options?.name || 'Anonymous');
  console.error('[Info]', info);
};

// Global error handler for unhandled promise rejections
window.addEventListener('unhandledrejection', (event) => {
  console.error('[Unhandled Rejection]', event.reason);
});

app.use(pinia);
app.use(router);
app.mount('#app');

// ── 第三步：挂载完成后异步收尾（UI 已经可见，不影响交互）──

// 淡出启动画面
const splash = document.getElementById('splash');
if (splash) {
  splash.classList.add('fade-out');
  splash.addEventListener('transitionend', () => splash.remove());
}

// Claude FM 初始化：连接后端引擎，获取初始状态并监听事件。
// 移到挂载后：FM 状态不影响首屏渲染，不需要阻塞 mount。
import('./fm/player').then(({ initFm }) => initFm());