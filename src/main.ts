import { createApp } from 'vue';
import { createPinia } from 'pinia';
import { createRouter, createWebHistory } from 'vue-router';
import App from './App.vue';
import { routes } from './router.js';
import { initTheme, initSystemThemeListener } from './theme';
import { initI18n } from './i18n';
import { initPrewarm } from './fm/player';

// 应用持久化的主题（在 mount 前设置，避免闪烁）
initTheme();
initSystemThemeListener();

// 初始化国际化
initI18n();

// Claude FM 首曲预热：启动即解析直链并预缓冲，首次点击播放几乎零延迟
initPrewarm();

// Material Web Components — individual imports (not all.js)
import '@material/web/button/filled-button.js';
import '@material/web/button/outlined-button.js';
import '@material/web/button/text-button.js';
import '@material/web/button/filled-tonal-button.js';
import '@material/web/iconbutton/icon-button.js';
import '@material/web/icon/icon.js';
import '@material/web/list/list.js';
import '@material/web/list/list-item.js';
import '@material/web/menu/menu.js';
import '@material/web/menu/menu-item.js';
import '@material/web/divider/divider.js';
import '@material/web/dialog/dialog.js';
import '@material/web/textfield/filled-text-field.js';
import '@material/web/textfield/outlined-text-field.js';
import '@material/web/select/outlined-select.js';
import '@material/web/select/filled-select.js';
import '@material/web/select/select-option.js';
import '@material/web/switch/switch.js';
import '@material/web/checkbox/checkbox.js';
import '@material/web/chips/chip-set.js';
import '@material/web/chips/input-chip.js';
import '@material/web/progress/circular-progress.js';
import '@material/web/tabs/tabs.js';
import '@material/web/tabs/secondary-tab.js';
import '@material/web/labs/segmentedbutton/outlined-segmented-button.js';
import '@material/web/labs/segmentedbuttonset/outlined-segmented-button-set.js';

// Material Design Icons (MDI)
import '@mdi/font/css/materialdesignicons.min.css';

const app = createApp(App);
const pinia = createPinia();
const router = createRouter({
  history: createWebHistory(),
  routes,
});

// Global error handler for Vue components
app.config.errorHandler = (err, instance, info) => {
  console.error('[Vue Error]', err);
  console.error('[Component]', instance?.$options?.name || 'Anonymous');
  console.error('[Info]', info);
};

// Global error handler for unhandled promise rejections
window.addEventListener('unhandledrejection', (event) => {
  console.error('[Unhandled Rejection]', event.reason);
});

app.use(pinia);
app.use(router);
app.mount('#app');