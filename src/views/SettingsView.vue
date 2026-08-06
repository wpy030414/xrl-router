<template>
  <div class="page">
    <div class="page__header">
      <h2 class="md-typescale-headline-medium page__title">{{ t('settings.title') }}</h2>
    </div>

    <md-tabs>
      <md-secondary-tab @click="activeTab = 0" :aria-selected="activeTab === 0"
        ><span class="mdi mdi-cog-outline tab-icon"></span> {{ t('settings.tab.general') }}</md-secondary-tab>
      <md-secondary-tab @click="activeTab = 1" :aria-selected="activeTab === 1"
        ><span class="mdi mdi-directions tab-icon"></span> {{ t('settings.tab.routing') }}</md-secondary-tab>
      <md-secondary-tab @click="activeTab = 2" :aria-selected="activeTab === 2"
        ><span class="mdi mdi-database-outline tab-icon"></span> {{ t('settings.tab.data') }}</md-secondary-tab>
    </md-tabs>

    <!-- ========== 通用 TAB ========== -->
    <div v-show="activeTab === 0" class="tab-panel">
      <!-- 关于 -->
      <section class="card section">
        <div class="section__head">
          <span class="section__icon mdi mdi-information-outline"></span>
          <div>
            <h3 class="md-typescale-title-medium">{{ t('settings.about.title', { version: appVersion }) }}</h3>
            <p class="md-typescale-body-medium section__desc">{{ t('settings.about.desc') }}</p>
            <a class="section__link md-typescale-body-medium" :href="GITHUB_URL" @click.prevent="openExternal">
              <span class="mdi mdi-open-in-new"></span>
              {{ t('settings.about.github') }}
            </a>
          </div>
        </div>
      </section>

      <!-- 语言 -->
      <section class="card section">
        <div class="section__head">
          <span class="section__icon mdi mdi-translate"></span>
          <div>
            <h3 class="md-typescale-title-medium">{{ t('settings.language.title') }}</h3>
            <p class="md-typescale-body-medium section__desc">{{ t('settings.language.desc') }}</p>
          </div>
        </div>
        <div class="section__body">
          <!-- no-checkmark：set 内部 grid-auto-columns:1fr 等宽列取内容最宽列，
               选中项的 checkmark 会推挤所有列宽；去掉后列宽由最长标签决定，稳定不跳动 -->
          <md-outlined-segmented-button-set>
            <md-outlined-segmented-button
              no-checkmark
              :selected="locale === 'zh-CN'"
              :label="t('settings.language.zh-CN')"
              @click="switchLocale('zh-CN')"
            ></md-outlined-segmented-button>
            <md-outlined-segmented-button
              no-checkmark
              :selected="locale === 'en'"
              :label="t('settings.language.en')"
              @click="switchLocale('en')"
            ></md-outlined-segmented-button>
          </md-outlined-segmented-button-set>
        </div>
      </section>

      <!-- 外观主题 -->
      <section class="card section">
        <div class="section__head">
          <span class="section__icon mdi mdi-palette"></span>
          <div>
            <h3 class="md-typescale-title-medium">{{ t('settings.theme.title') }}</h3>
            <p class="md-typescale-body-medium section__desc">{{ t('settings.theme.desc') }}</p>
          </div>
        </div>
        <div class="section__body">
          <md-outlined-segmented-button-set>
            <md-outlined-segmented-button
              no-checkmark
              :selected="theme === 'system'"
              :label="t('settings.theme.system')"
              @click="chooseTheme('system')"
            ></md-outlined-segmented-button>
            <md-outlined-segmented-button
              no-checkmark
              :selected="theme === 'light'"
              :label="t('settings.theme.light')"
              @click="chooseTheme('light')"
            ></md-outlined-segmented-button>
            <md-outlined-segmented-button
              no-checkmark
              :selected="theme === 'dark'"
              :label="t('settings.theme.dark')"
              @click="chooseTheme('dark')"
            ></md-outlined-segmented-button>
          </md-outlined-segmented-button-set>
        </div>
        <div class="section__body hue-row">
          <span class="hue-label md-typescale-label-large">{{ t('settings.theme.hue') }}</span>
          <input
            type="range" min="0" max="360" step="1"
            :value="hue"
            class="hue-slider"
            @input="onHueInput"
          />
          <span class="hue-value mono">{{ hue }}°</span>
          <span class="hue-preview" :style="{ background: `hsl(${hue}, 50%, 42%)` }"></span>
          <md-text-button class="hue-reset" @click="resetHue">{{ t('settings.theme.hue_reset') }}</md-text-button>
        </div>
      </section>

      <!-- 开机静默启动 -->
      <section class="card section">
        <div class="section__head">
          <span class="section__icon mdi mdi-power"></span>
          <div>
            <h3 class="md-typescale-title-medium">{{ t('settings.autostart.title') }}</h3>
            <p class="md-typescale-body-medium section__desc">{{ t('settings.autostart.desc') }}</p>
          </div>
        </div>
        <div class="section__body switch-row">
          <md-switch :selected="autostart" @change="toggleAutostart"></md-switch>
          <span class="md-typescale-body-medium switch-label">{{ autostart ? t('settings.autostart.on') : t('settings.autostart.off') }}</span>
        </div>
      </section>
    </div>

    <!-- ========== 路由 TAB ========== -->
    <div v-show="activeTab === 1" class="tab-panel">
      <section class="card section">
        <div class="section__head">
          <span class="section__icon mdi mdi-magnify"></span>
          <div>
            <h3 class="md-typescale-title-medium">{{ t('settings.websearch.title') }}</h3>
            <p class="md-typescale-body-medium section__desc">{{ t('settings.websearch.desc') }}</p>
          </div>
        </div>
        <div class="section__body switch-row">
          <md-switch :selected="hijack" @change="toggleHijack"></md-switch>
          <span class="md-typescale-body-medium switch-label">{{ hijack ? t('settings.websearch.on') : t('settings.websearch.off') }}</span>
        </div>
      </section>

      <!-- 故障转移 -->
      <section class="card section">
        <div class="section__head">
          <span class="section__icon mdi mdi-swap-horizontal"></span>
          <div>
            <h3 class="md-typescale-title-medium">{{ t('settings.failover.title') }}</h3>
            <p class="md-typescale-body-medium section__desc">{{ t('settings.failover.desc') }}</p>
          </div>
        </div>
        <div class="section__body switch-row">
          <md-switch :selected="failover" @change="toggleFailover"></md-switch>
          <span class="md-typescale-body-medium switch-label">{{ failover ? t('settings.failover.on') : t('settings.failover.off') }}</span>
        </div>
      </section>
    </div>

    <!-- ========== 数据 TAB ========== -->
    <div v-show="activeTab === 2" class="tab-panel">
      <!-- 用户数据（导出 / 导入） -->
      <section class="card section">
        <div class="section__head">
          <span class="section__icon mdi mdi-database-sync-outline"></span>
          <div>
            <h3 class="md-typescale-title-medium">{{ t('settings.data.title') }}</h3>
            <p class="md-typescale-body-medium section__desc">{{ t('settings.data.desc') }}</p>
          </div>
        </div>
        <div class="section__body">
          <md-outlined-button @click="exportData" :disabled="dataBusy">
            <span slot="icon" class="mdi mdi-download"></span>
            {{ t('settings.data.export.button') }}
          </md-outlined-button>
          <md-outlined-button @click="importData" :disabled="dataBusy">
            <span slot="icon" class="mdi mdi-upload"></span>
            {{ t('settings.data.import.button') }}
          </md-outlined-button>
        </div>
      </section>

      <!-- 重置 -->
      <section class="card section section--danger">
        <div class="section__head">
          <span class="section__icon mdi mdi-delete-forever"></span>
          <div>
            <h3 class="md-typescale-title-medium">{{ t('settings.data.reset.title') }}</h3>
            <p class="md-typescale-body-medium section__desc">{{ t('settings.data.reset.desc') }}</p>
          </div>
        </div>
        <div class="section__body">
          <md-outlined-button class="danger-btn" @click="destroyOpen = true">
            <span slot="icon" class="mdi mdi-delete-forever"></span>
            {{ t('settings.data.reset.button') }}
          </md-outlined-button>
        </div>
      </section>
    </div>

    <!-- 重置确认对话框 -->
    <md-dialog :open="destroyOpen" @close="destroyOpen = false">
      <div slot="headline">{{ t('settings.data.reset.title') }}</div>
      <div slot="content" class="dialog-content">
        <p class="md-typescale-body-medium">{{ t('settings.data.reset.confirm') }}</p>
      </div>
      <div slot="actions">
        <md-text-button @click="destroyOpen = false">{{ t('settings.cancel') }}</md-text-button>
        <md-text-button class="confirm-destroy" @click="confirmDestroy">{{ t('settings.confirm') }}</md-text-button>
      </div>
    </md-dialog>

    <!-- 导入确认对话框 -->
    <md-dialog :open="importOpen" @close="importOpen = false">
      <div slot="headline">{{ t('settings.data.import.title') }}</div>
      <div slot="content" class="dialog-content">
        <p class="md-typescale-body-medium">{{ t('settings.data.import.confirm') }}</p>
      </div>
      <div slot="actions">
        <md-text-button @click="importOpen = false">{{ t('settings.cancel') }}</md-text-button>
        <md-text-button @click="confirmImport">{{ t('settings.confirm') }}</md-text-button>
      </div>
    </md-dialog>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted } from 'vue';
import { useRouter } from 'vue-router';
import { getVersion } from '@tauri-apps/api/app';
import { settingsApi, dataApi } from '../api';
import { getTheme, setTheme, getHue, setHue, type Theme } from '../theme';
import { open as openUrl } from '@tauri-apps/plugin-shell';
import { save, open } from '@tauri-apps/plugin-dialog';
import { readTextFile, writeTextFile } from '@tauri-apps/plugin-fs';
import { isEnabled, enable, disable } from '@tauri-apps/plugin-autostart';
import { t, setLocale, i18n } from '../i18n';
import type { Locale } from '../i18n';

const router = useRouter();
const GITHUB_URL = 'https://github.com/wpy030414/xrl-router';

const appVersion = ref('—');
const destroyOpen = ref(false);
const importOpen = ref(false);
const activeTab = ref(0);
const dataBusy = ref(false);
const pendingImportSql = ref('');

async function openExternal() {
  try {
    await openUrl(GITHUB_URL);
  } catch {
    window.open(GITHUB_URL, '_blank');
  }
}

// ── 语言 ──
const locale = ref(i18n.locale);

function switchLocale(loc: Locale) {
  locale.value = loc;
  setLocale(loc);
}

// ── 主题 ──
const theme = ref<Theme>(getTheme());
const hue = ref(getHue());

function chooseTheme(t: Theme) {
  theme.value = t;
  setTheme(t);
}

function onHueInput(e: Event) {
  const val = Number((e.target as HTMLInputElement).value);
  hue.value = val;
  setHue(val);
}

function resetHue() {
  hue.value = 264;
  setHue(264);
}

// ── 开机自启 ──
const autostart = ref(false);

async function toggleAutostart() {
  const next = !autostart.value;
  autostart.value = next;
  try {
    if (next) {
      await enable();
    } else {
      await disable();
    }
  } catch {
    autostart.value = !next;
  }
}

// ── WebSearch 劫持 ──
const hijack = ref(false);

async function toggleHijack() {
  const next = !hijack.value;
  hijack.value = next;
  try {
    await settingsApi.update({ websearch_hijack: next });
  } catch {
    hijack.value = !next;
  }
}

// ── 故障转移 ──
const failover = ref(false);

async function toggleFailover() {
  const next = !failover.value;
  failover.value = next;
  try {
    await settingsApi.update({ failover_enabled: next });
  } catch {
    failover.value = !next;
  }
}

// ── 数据导入/导出 ──
async function exportData() {
  dataBusy.value = true;
  try {
    const sql = await dataApi.export();
    const filePath = await save({
      defaultPath: `xrl-router-backup-${new Date().toISOString().slice(0, 10)}.sql`,
      filters: [{ name: 'SQL', extensions: ['sql'] }],
    });
    if (filePath) {
      await writeTextFile(filePath, sql);
    }
  } catch (e) {
    console.error('[Export] failed:', e);
  } finally {
    dataBusy.value = false;
  }
}

async function importData() {
  dataBusy.value = true;
  try {
    const filePath = await open({
      filters: [{ name: 'SQL', extensions: ['sql'] }],
      multiple: false,
    });
    if (filePath) {
      const sql = await readTextFile(filePath as string);
      pendingImportSql.value = sql;
      importOpen.value = true;
    }
  } catch (e) {
    console.error('[Import] file read failed:', e);
  } finally {
    dataBusy.value = false;
  }
}

async function confirmImport() {
  importOpen.value = false;
  try {
    await dataApi.import(pendingImportSql.value);
    pendingImportSql.value = '';
    // 导入成功后刷新页面以反映新数据
    window.location.reload();
  } catch (e) {
    console.error('[Import] failed:', e);
  }
}

// ── 重置 ──
async function confirmDestroy() {
  destroyOpen.value = false;
  try {
    await dataApi.reset();
  } catch {
    // ignore
  }
  localStorage.clear();
  router.push('/');
}

// ── 初始化 ──
onMounted(async () => {
  // 加载设置
  try {
    const s = await settingsApi.get();
    hijack.value = !!s.websearch_hijack;
    failover.value = !!s.failover_enabled;
  } catch {
    // ignore
  }

  // 版本号
  try {
    appVersion.value = await getVersion();
  } catch {
    // 非 Tauri 环境
  }

  // 开机自启状态
  try {
    autostart.value = await isEnabled();
  } catch {
    // 非 Tauri 环境
  }
});
</script>

<style scoped>
.page__header { margin-bottom: 24px; }
.page__title { margin: 0; color: var(--md-sys-color-on-surface); }

/* Tabs */
md-tabs {
  margin-bottom: 20px;
}
md-secondary-tab {
  --md-secondary-tab-active-indicator-color: var(--md-sys-color-primary);
  --md-secondary-tab-active-focus-label-text-color: var(--md-sys-color-primary);
  --md-secondary-tab-active-pressed-label-text-color: var(--md-sys-color-primary);
}
.tab-icon { font-size: 18px; margin-right: 4px; vertical-align: -3px; }

.tab-panel { display: flex; flex-direction: column; gap: 16px; }

.section {
  background: var(--md-sys-color-surface-container-low);
  border-radius: var(--md-sys-shape-corner-medium);
  padding: 20px;
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.section__head { display: flex; align-items: flex-start; gap: 12px; }
.section__icon {
  width: 40px; height: 40px;
  border-radius: var(--md-sys-shape-corner-full);
  background: var(--md-sys-color-surface-container-high);
  color: var(--md-sys-color-on-surface-variant);
  display: flex; align-items: center; justify-content: center;
  font-size: 24px; flex-shrink: 0;
}
.section__head h3 { margin: 0; color: var(--md-sys-color-on-surface); }
.section__desc { margin: 2px 0 0; color: var(--md-sys-color-on-surface-variant); }

.section__link {
  display: inline-flex; align-items: center; gap: 6px;
  color: var(--md-sys-color-primary); text-decoration: none;
  margin-top: 2px;
}
.section__link:hover { text-decoration: underline; }

.section__body { display: flex; gap: 12px; align-items: center; flex-wrap: wrap; }
.section--danger .section__icon { background: var(--md-sys-color-error-container); color: var(--md-sys-color-on-error-container); }
.danger-btn { color: var(--md-sys-color-error); }

.switch-row { display: flex; align-items: center; gap: 12px; }
.switch-label { color: var(--md-sys-color-on-surface-variant); }

.confirm-destroy { color: var(--md-sys-color-error); }
.dialog-content { min-width: 320px; }

/* 令牌色滑块 */
.hue-row {
  display: flex; align-items: center; gap: 12px;
  padding-top: 8px;
  border-top: 1px solid var(--md-sys-color-outline-variant);
}
.hue-label {
  flex-shrink: 0;
  color: var(--md-sys-color-on-surface-variant);
  white-space: nowrap;
}
.hue-slider {
  flex: 1; min-width: 120px;
  -webkit-appearance: none; appearance: none;
  height: 12px;
  border-radius: 6px;
  background: linear-gradient(to right,
    hsl(0,50%,42%), hsl(60,50%,42%), hsl(120,50%,42%),
    hsl(180,50%,42%), hsl(240,50%,42%), hsl(300,50%,42%), hsl(360,50%,42%));
  outline: none;
  cursor: pointer;
}
.hue-slider::-webkit-slider-thumb {
  -webkit-appearance: none; appearance: none;
  width: 20px; height: 20px;
  border-radius: 50%;
  background: #fff;
  border: 3px solid var(--md-sys-color-primary);
  box-shadow: 0 1px 3px rgba(0,0,0,0.3);
  cursor: pointer;
}
.hue-slider::-moz-range-thumb {
  width: 20px; height: 20px;
  border-radius: 50%;
  background: #fff;
  border: 3px solid var(--md-sys-color-primary);
  box-shadow: 0 1px 3px rgba(0,0,0,0.3);
  cursor: pointer;
}
.hue-value {
  flex-shrink: 0;
  min-width: 40px; text-align: right;
  color: var(--md-sys-color-on-surface-variant);
  font-variant-numeric: tabular-nums;
}
.hue-preview {
  flex-shrink: 0;
  width: 24px; height: 24px;
  border-radius: var(--md-sys-shape-corner-full);
  box-shadow: inset 0 0 0 1px rgba(0,0,0,0.1);
}
.hue-reset {
  flex-shrink: 0;
  --md-sys-color-primary: var(--md-sys-color-on-surface-variant);
}
</style>
