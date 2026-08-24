<template>
  <div class="page">
    <div class="page__header">
      <h2 class="md-typescale-headline-medium page__title">{{ t('keys.title') }}</h2>
      <md-filled-button @click="openCreate">
        <MdiIcon :path="mdiPlus" slot="icon" />
        {{ t('keys.create') }}
      </md-filled-button>
    </div>

    <div v-if="loading" class="empty-state"><md-circular-progress indeterminate></md-circular-progress></div>

    <div v-else-if="!keys.length" class="empty-state">
      <MdiIcon :path="mdiInboxOutline" class="empty-state__icon" />
      <p class="md-typescale-body-large">{{ t('common.empty') }}</p>
    </div>

    <div v-else class="table-card">
      <table class="table">
        <thead>
          <tr class="md-typescale-label-large">
            <th>{{ t('keys.col_key') }}</th><th>{{ t('keys.col_models') }}</th><th>{{ t('keys.col_quota') }}</th><th>{{ t('keys.col_created') }}</th><th>{{ t('keys.col_updated') }}</th><th></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="k in keys" :key="k.id" class="md-typescale-body-medium">
            <td class="key-cell mono" :title="k.name">{{ k.name || t('common.unnamed') }} ({{ k.key_masked }})</td>
            <td class="models-cell">
              <div class="models-inner">
                <span v-if="!k.allowed_models || !k.allowed_models.length" class="chip chip--all md-typescale-label-medium">{{ t('common.all') }}</span>
                <span v-for="m in (k.allowed_models || [])" :key="m" class="chip md-typescale-label-medium">{{ m }}</span>
              </div>
            </td>
            <td class="quota-cell">
              <div v-if="quotaLines(k).length" class="quota-lines">
                <span v-for="line in quotaLines(k)" :key="line.key" class="quota-line mono" :class="{ 'quota-line--over': line.over }">
                  {{ line.key }}: {{ line.percent.toFixed(0) }}% {{ line.resets_in }}
                </span>
              </div>
              <span v-else class="muted">-</span>
            </td>
            <td class="time-cell">{{ formatTime(k.created_at) }}</td>
            <td class="time-cell">{{ formatTime(k.updated_at) }}</td>
            <td class="actions-cell">
              <md-icon-button :id="'key-btn-' + k.id" @click="toggleMenu(k)">
                <MdiIcon :path="mdiDotsVertical" />
              </md-icon-button>
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <!-- Shared action menu (single instance, re-anchors per row) -->
    <md-menu
      :open="menuOpen != null"
      :anchor="menuAnchor"
      positioning="fixed"
      @closed="menuOpen = null"
    >
      <md-menu-item @click="renameFromMenu">
        <MdiIcon :path="mdiPencilOutline" /> {{ t('keys.rename') }}
      </md-menu-item>
      <md-menu-item @click="permFromMenu">
        <MdiIcon :path="mdiShieldOutline" /> {{ t('keys.edit_perm') }}
      </md-menu-item>
      <md-menu-item @click="quotaFromMenu">
        <MdiIcon :path="mdiTuneVariant" /> {{ t('keys.config_quota') }}
      </md-menu-item>
      <md-menu-item class="menu-item--danger" @click="deleteFromMenu">
        <MdiIcon :path="mdiDeleteOutline" /> {{ t('common.delete') }}
      </md-menu-item>
    </md-menu>

    <md-dialog :open="createOpen" @close="createOpen = false">
      <div slot="headline">{{ t('keys.create') }}</div>
      <div slot="content" class="form">
        <md-outlined-text-field :value="newName" :label="t('keys.rename_label')" class="field" @input="newName = ($event.target as HTMLInputElement).value"></md-outlined-text-field>
      </div>
      <div slot="actions">
        <md-text-button @click="createOpen = false">{{ t('common.cancel') }}</md-text-button>
        <md-filled-button @click="createKey">{{ t('common.create') }}</md-filled-button>
      </div>
    </md-dialog>

    <md-dialog :open="renameOpen" @close="renameOpen = false">
      <div slot="headline">{{ t('keys.rename_title') }}</div>
      <div slot="content" class="form">
        <md-outlined-text-field :value="renameName" :label="t('keys.rename_label')" class="field" @input="renameName = ($event.target as HTMLInputElement).value"></md-outlined-text-field>
      </div>
      <div slot="actions">
        <md-text-button @click="renameOpen = false">{{ t('common.cancel') }}</md-text-button>
        <md-filled-button @click="renameKey">{{ t('common.save') }}</md-filled-button>
      </div>
    </md-dialog>

    <md-dialog :open="!!newKeyPlain" @close="newKeyPlain = ''">
      <div slot="headline">{{ t('keys.created_once') }}</div>
      <div slot="content" class="form">
        <p class="warn md-typescale-body-medium"><MdiIcon :path="mdiAlert" />{{ t('keys.save_warning') }}</p>
        <p class="md-typescale-body-medium deploy-label">{{ t('keys.plain_key') }}</p>
        <div class="key-box mono md-typescale-body-large">{{ newKeyPlain }}</div>
        <template v-if="deployLink">
          <p class="md-typescale-body-medium deploy-label">{{ t('keys.deploy_link') }}</p>
          <div class="key-box mono md-typescale-body-medium deploy-box">{{ deployLink }}</div>
        </template>
      </div>
      <div slot="actions">
        <md-text-button @click="copyKey"><MdiIcon :path="mdiContentCopy" slot="icon" />{{ t('keys.copy_key') }}</md-text-button>
        <md-text-button :disabled="!deployLink" @click="copyDeployLink"><MdiIcon :path="mdiLinkVariant" slot="icon" />{{ t('keys.copy_deploy') }}</md-text-button>
        <md-filled-button @click="newKeyPlain = ''">{{ t('common.done') }}</md-filled-button>
      </div>
    </md-dialog>

    <md-dialog :open="permOpen" @close="permOpen = false">
      <div slot="headline">{{ t('keys.perm_title', { name: editingKey?.name || t('common.unnamed') }) }}</div>
      <div slot="content" class="form">
        <p class="md-typescale-body-medium perm-desc">{{ t('keys.perm_desc') }}</p>
        <md-circular-progress v-if="modelsLoading" indeterminate></md-circular-progress>
        <div v-else class="perm-list">
          <template v-for="p in providerModels" :key="p.name">
            <div class="perm-provider-label md-typescale-label-large">{{ p.name }}</div>
            <label v-for="m in p.models" :key="m" class="perm-item md-typescale-body-medium">
              <md-checkbox :checked="permSet.has(m)" @click="togglePerm(m)"></md-checkbox>
              {{ m }}
            </label>
          </template>
        </div>
        <p v-if="!allModels.length && !modelsLoading" class="md-typescale-body-medium">{{ t('keys.perm_no_models') }}</p>
      </div>
      <div slot="actions">
        <md-text-button @click="permOpen = false">{{ t('common.cancel') }}</md-text-button>
        <md-filled-button @click="savePerms">{{ t('common.save') }}</md-filled-button>
      </div>
    </md-dialog>

    <md-dialog :open="quotaOpen" @close="quotaOpen = false">
      <div slot="headline">{{ t('keys.quota_title', { name: quotaKey?.name || t('common.unnamed') }) }}</div>
      <div slot="content" class="form">
        <p class="md-typescale-body-medium quota-desc">{{ t('keys.quota_desc') }}</p>
        <md-outlined-text-field type="number" min="0" :label="t('keys.quota_5h_label')" class="field"
          :value="String(quota5h)" @input="quota5h = parseInt(($event.target as HTMLInputElement).value || '0', 10)">
        </md-outlined-text-field>
        <div class="quota-preview mono md-typescale-body-medium">{{ t('keys.quota_preview', { value: formatAbbrev(quota5h) }) }}</div>
        <md-outlined-text-field type="number" min="0" :label="t('keys.quota_7d_label')" class="field"
          :value="String(quota7d)" @input="quota7d = parseInt(($event.target as HTMLInputElement).value || '0', 10)">
        </md-outlined-text-field>
        <div class="quota-preview mono md-typescale-body-medium">{{ t('keys.quota_preview', { value: formatAbbrev(quota7d) }) }}</div>
      </div>
      <div slot="actions">
        <md-text-button @click="quotaOpen = false">{{ t('common.cancel') }}</md-text-button>
        <md-filled-button @click="saveQuota">{{ t('common.save') }}</md-filled-button>
      </div>
    </md-dialog>

    <md-dialog :open="deleteOpen" @close="deleteOpen = false">
      <div slot="headline">{{ t('keys.delete_title') }}</div>
      <div slot="content" class="form">
        <p class="md-typescale-body-medium">{{ t('keys.delete_confirm', { name: deleteTarget?.name || t('common.unnamed') }) }}</p>
      </div>
      <div slot="actions">
        <md-text-button @click="deleteOpen = false">{{ t('common.cancel') }}</md-text-button>
        <md-text-button class="confirm-del" @click="confirmDelete">{{ t('keys.delete_confirm_btn') }}</md-text-button>
      </div>
    </md-dialog>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from 'vue';
import '@material/web/iconbutton/icon-button.js';
import '@material/web/menu/menu.js';
import '@material/web/menu/menu-item.js';
import '@material/web/textfield/outlined-text-field.js';
import '@material/web/checkbox/checkbox.js';
import '@material/web/progress/circular-progress.js';
import {
  mdiPlus, mdiInboxOutline, mdiDotsVertical, mdiPencilOutline,
  mdiShieldOutline, mdiTuneVariant, mdiDeleteOutline, mdiAlert,
  mdiContentCopy, mdiLinkVariant,
} from '@mdi/js';
import { serviceKeysApi, providersApi, modelsApi, combosApi, installApi, type ServiceKey } from '../api';
import { wsClient } from '../ws';
import { t } from '../i18n';
import MdiIcon from '../components/MdiIcon.vue';

type KeyRow = ServiceKey & { used_5h?: number; used_7d?: number };

const keys = ref<KeyRow[]>([]);
const loading = ref(true);
const createOpen = ref(false);
const newName = ref('');
const newKeyPlain = ref('');
// 分发链接：本机 IP + 公共端口 + 明文 key。newKeyPlain 有值时拉取本机 IP。
const localIp = ref<string | null>(null);
const localPort = ref<number>(19068);
const deployLink = computed(() => {
  if (!newKeyPlain.value || !localIp.value) return '';
  return `http://${localIp.value}:${localPort.value}/install?t=${newKeyPlain.value}`;
});
watch(newKeyPlain, async (v) => {
  if (v && !localIp.value) {
    try {
      const r = await installApi.localIp();
      if (r.ip) localIp.value = r.ip;
      if (r.port) localPort.value = r.port;
    } catch {
      // 取不到 IP 时 deployLink 留空，仅显示明文 key
    }
  }
});

const permOpen = ref(false);
const editingKey = ref<ServiceKey | null>(null);
const permSet = ref<Set<string>>(new Set());
const allModels = ref<string[]>([]);
const providerModels = ref<{ name: string; models: string[] }[]>([]);
const modelsLoading = ref(false);

// 配置额度对话框
const quotaOpen = ref(false);
const quotaKey = ref<ServiceKey | null>(null);
const quota5h = ref(0);
const quota7d = ref(0);

// 共享操作菜单（单实例，按行重定向 anchor）
const menuOpen = ref<string | null>(null);
const menuAnchor = ref('');
const menuTarget = ref<ServiceKey | null>(null);
function toggleMenu(k: ServiceKey) {
  if (menuOpen.value === k.id) {
    menuOpen.value = null;
  } else {
    menuTarget.value = k;
    menuAnchor.value = 'key-btn-' + k.id;
    menuOpen.value = k.id;
  }
}
function renameFromMenu() {
  if (!menuTarget.value) return;
  editingKey.value = menuTarget.value;
  renameName.value = menuTarget.value.name || '';
  renameOpen.value = true;
}
function permFromMenu() {
  if (menuTarget.value) openPerms(menuTarget.value);
}
function quotaFromMenu() {
  if (!menuTarget.value) return;
  quotaKey.value = menuTarget.value;
  quota5h.value = menuTarget.value.quota_5h ?? 0;
  quota7d.value = menuTarget.value.quota_7d ?? 0;
  quotaOpen.value = true;
}
function deleteFromMenu() {
  if (menuTarget.value) openDeleteMenu(menuTarget.value);
}

/** 滚动窗口剩余时间（秒 → XdYh / XhYm），用于限额列与额度预览。 */
function resetsIn(remainingSecs: number): string {
  const r = Math.max(0, Math.floor(remainingSecs));
  const d = Math.floor(r / 86400);
  const h = Math.floor((r % 86400) / 3600);
  const m = Math.max(1, Math.floor((r % 3600) / 60));
  if (d > 0) return `${d}d${h}h`;
  if (h > 0) return `${h}h${m}m`;
  return `${m}m`;
}

/** 省略读数：≥1e8 → x.xx亿/B，≥1e4 → x.xx万/K，否则原样；0 → 不设限。 */
function formatAbbrev(n: number): string {
  if (!n || n <= 0) return t('keys.unlimited');
  if (n >= 1e8) return (n / 1e8).toFixed(2) + t('keys.unit_yi');
  if (n >= 1e4) return (n / 1e4).toFixed(2) + t('keys.unit_wan');
  return String(n);
}

/** 限额列的窗口行：未设限（quota<=0）的窗口不展示。 */
function quotaLines(k: KeyRow): { key: string; resets_in: string; percent: number; over: boolean }[] {
  const lines: { key: string; resets_in: string; percent: number; over: boolean }[] = [];
  const now = Math.floor(Date.now() / 1000);
  const push = (used: number, limit: number, label: string, windowSecs: number) => {
    if (!limit || limit <= 0) return;
    lines.push({
      key: label,
      resets_in: resetsIn(windowSecs - (now % windowSecs)),
      percent: (used / limit) * 100,
      over: used >= limit,
    });
  };
  push(k.used_5h ?? 0, k.quota_5h ?? 0, '5h', 5 * 3600);
  push(k.used_7d ?? 0, k.quota_7d ?? 0, '7d', 7 * 86400);
  return lines;
}

async function saveQuota() {
  if (!quotaKey.value) return;
  try {
    await serviceKeysApi.update(quotaKey.value.id, {
      quota_5h: quota5h.value,
      quota_7d: quota7d.value,
    });
    quotaOpen.value = false;
    await fetchKeys();
  } catch (e: any) {
    alert(t('keys.save_quota_failed', { msg: e?.message || e }));
  }
}

const renameOpen = ref(false);
const renameName = ref('');
async function renameKey() {
  if (!editingKey.value) return;
  try {
    await serviceKeysApi.update(editingKey.value.id, { name: renameName.value.trim() || t('common.unnamed') });
    renameOpen.value = false;
    await fetchKeys();
  } catch (e: any) {
    alert(t('keys.rename_failed', { msg: e?.message || e }));
  }
}

// 首次加载显示 spinner；后续刷新（WS 事件 / 轮询 / 增删改后）静默更新数据，
// 由 Vue diff 只改变化的单元格，避免整表闪 spinner 造成「强制刷新」观感。
const loaded = ref(false);
async function fetchKeys() {
  if (!loaded.value) loading.value = true; // 仅首次进入 loading
  try {
    keys.value = await serviceKeysApi.list();
    loaded.value = true;
  } finally {
    loading.value = false;
  }
}
function openCreate() { newName.value = ''; createOpen.value = true; }
async function createKey() {
  try {
    const r = await serviceKeysApi.create({ name: newName.value || t('common.unnamed') });
    createOpen.value = false;
    newKeyPlain.value = r.key;
    await fetchKeys();
  } catch (e: any) {
    alert(t('keys.create_failed', { msg: e?.message || e }));
  }
}
async function copyKey() { try { await navigator.clipboard.writeText(newKeyPlain.value); } catch {} }
async function copyDeployLink() {
  if (!deployLink.value) return;
  try { await navigator.clipboard.writeText(deployLink.value); } catch {}
}
const deleteOpen = ref(false);
const deleteTarget = ref<ServiceKey | null>(null);
function openDeleteMenu(k: ServiceKey) {
  deleteTarget.value = k;
  deleteOpen.value = true;
}
async function confirmDelete() {
  if (!deleteTarget.value) return;
  try {
    await serviceKeysApi.delete(deleteTarget.value.id);
    deleteOpen.value = false;
    deleteTarget.value = null;
    await fetchKeys();
  } catch (e: any) {
    alert(t('keys.delete_failed', { msg: e?.message || e }));
  }
}

function openPerms(k: ServiceKey) {
  editingKey.value = k;
  permSet.value = new Set(k.allowed_models || []);
  permOpen.value = true;
  fetchAvailableModels();
}

async function fetchAvailableModels() {
  modelsLoading.value = true;
  try {
    const [providers, models, combos] = await Promise.all([providersApi.list(), modelsApi.list(), combosApi.list()]);
    const providerName = new Map(providers.map((p) => [p.id, p.name]));
    const groupsMap = new Map<string, string[]>();
    for (const m of models) {
      const pname = providerName.get(m.provider_id) || t('common.unknown');
      const name = m.display_name || m.model_id;
      if (!groupsMap.has(pname)) groupsMap.set(pname, []);
      if (!groupsMap.get(pname)!.includes(name)) groupsMap.get(pname)!.push(name);
    }
    const groups = Array.from(groupsMap.entries()).map(([name, ms]) => ({ name, models: ms.sort() }));
    // 组合别名独立分组：授予组合名 = 授予其全部成员；只授予成员名则调用组合会被 403
    const comboGroup = combos.filter((c) => c.enabled).map((c) => c.name).sort();
    if (comboGroup.length) {
      groups.push({ name: t('keys.perm_group_combos'), models: comboGroup });
    }
    providerModels.value = groups;
    allModels.value = groups.flatMap((g) => g.models);
  } catch {} finally { modelsLoading.value = false; }
}

function togglePerm(model: string) {
  const s = new Set(permSet.value);
  s.has(model) ? s.delete(model) : s.add(model);
  permSet.value = s;
}

async function savePerms() {
  if (!editingKey.value) return;
  const models = [...permSet.value];
  try {
    await serviceKeysApi.update(editingKey.value.id, { name: editingKey.value.name || undefined, allowed_models: models });
    permOpen.value = false;
    await fetchKeys();
  } catch (e: any) {
    alert(t('keys.save_perm_failed', { msg: e?.message || e }));
  }
}

function formatTime(t: number): string { const d = new Date(t*1000); return `${d.getFullYear()}-${String(d.getMonth()+1).padStart(2,'0')}-${String(d.getDate()).padStart(2,'0')} ${String(d.getHours()).padStart(2,'0')}:${String(d.getMinutes()).padStart(2,'0')}`; }

// 用量刷新策略：
// 1. 后端每 5s 通过 WS 广播 usage_stats_changed（每次代理请求写库后都会触发），
//    收到即重新拉取——用量百分比、剩余时间实时保持新鲜；
// 2. WS 断连/空闲时降级为 60s 兜底轮询，保证窗口重置等边界仍能收敛。
let refreshTimer: ReturnType<typeof setInterval> | null = null;
function onUsageChanged() {
  fetchKeys();
}
onMounted(() => {
  fetchKeys();
  wsClient.connect();
  wsClient.on('usage_stats_changed', onUsageChanged);
  refreshTimer = setInterval(fetchKeys, 60000);
});
onUnmounted(() => {
  wsClient.off('usage_stats_changed', onUsageChanged);
  if (refreshTimer) clearInterval(refreshTimer);
});
</script>

<style scoped>
.page__header { display: flex; justify-content: space-between; align-items: flex-start; margin-bottom: 24px; gap: 16px; flex-wrap: wrap; }
.page__title { margin: 0; }
.empty-state { display: flex; flex-direction: column; align-items: center; gap: 8px; padding: 64px 24px; text-align: center; }
.empty-state__icon { font-size: 48px; color: var(--md-sys-color-on-surface-variant); }
.table-card { background: var(--md-sys-color-surface-container-low); border-radius: var(--md-sys-shape-corner-medium); padding: 16px; overflow-x: auto; }
/* 内容完整展示：不截断、不换行，由内容自然撑开列宽；超宽时表格横向滚动。 */
.table { border-collapse: collapse; table-layout: auto; width: max-content; min-width: 100%; }
.table th { text-align: left; padding: 12px 16px; color: var(--md-sys-color-on-surface-variant); vertical-align: middle; white-space: nowrap; }
.table td { padding: 12px 16px; vertical-align: middle; white-space: nowrap; }
.table tr { border-bottom: 1px solid var(--md-sys-color-outline-variant); }
.table tr:last-child { border-bottom: none; }
.models-inner { display: inline-flex; flex-wrap: nowrap; gap: 4px 6px; align-items: center; vertical-align: middle; }
.quota-lines { display: flex; flex-direction: column; gap: 2px; }
.quota-line { font-size: 0.78rem; line-height: 1.5; color: var(--md-sys-color-on-surface-variant); }
.quota-line--over { color: var(--md-sys-color-error); font-weight: 500; }
.muted { color: var(--md-sys-color-on-surface-variant); }
.quota-desc { color: var(--md-sys-color-on-surface-variant); margin: 0; }
.quota-preview { color: var(--md-sys-color-on-surface-variant); font-size: 0.8rem; font-style: italic ; margin-top: -8px; }
.time-cell { color: var(--md-sys-color-on-surface-variant); }
/* 右侧按钮列固定：横向滚动时按钮始终可见（无阴影，保持简洁）。 */
.actions-cell { position: sticky; right: 0; background: var(--md-sys-color-surface-container-low); text-align: right; }
.mono { font-family: 'Roboto Mono', monospace; }
.chip { display: inline-flex; align-items: center; padding: 2px 8px; border-radius: var(--md-sys-shape-corner-small); background: var(--md-sys-color-primary-container); color: var(--md-sys-color-on-primary-container); font-size: 0.75rem; line-height: 1.4; }
.chip--all { background: var(--md-sys-color-surface-container-highest); color: var(--md-sys-color-on-surface-variant); }
.form { display: flex; flex-direction: column; gap: 16px; min-width: 360px; }
.field { width: 100%; }
.warn { display: flex; align-items: center; gap: 8px; color: var(--md-sys-color-on-error-container); background: var(--md-sys-color-error-container); padding: 12px; border-radius: var(--md-sys-shape-corner-small); margin: 0; }
.key-box { background: var(--md-sys-color-surface-container-high); padding: 16px; border-radius: var(--md-sys-shape-corner-medium); word-break: break-all; border: 1px solid var(--md-sys-color-outline-variant); }
.deploy-label { margin: 4px 0 0; color: var(--md-sys-color-on-surface-variant); }
.deploy-box { font-size: 0.75rem; }
.perm-desc { color: var(--md-sys-color-on-surface-variant); margin: 0; }
.perm-list { max-height: 300px; overflow-y: auto; display: flex; flex-direction: column; gap: 4px; }
.perm-provider-label { color: var(--md-sys-color-on-surface-variant); padding: 8px 0 4px; border-top: 1px solid var(--md-sys-color-outline-variant); margin-top: 4px; }
.perm-provider-label:first-child { border-top: none; margin-top: 0; }
.perm-item { display: flex; align-items: center; gap: 8px; cursor: pointer; padding: 6px 0 6px 8px; }
.perm-item md-checkbox { flex-shrink: 0; margin-top: -2px; }
</style>

<!-- md-menu teleports to document root, so its styles must not be scoped -->
<style>
.menu-item--danger { --md-menu-item-label-text-color: var(--md-sys-color-error); color: var(--md-sys-color-error); }
</style>