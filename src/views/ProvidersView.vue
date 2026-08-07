<template>
  <div class="page">
    <div class="page__header">
      <h2 class="md-typescale-headline-medium page__title">{{ t('providers.title') }}</h2>
      <md-filled-button @click="$router.push('/providers/new')">
        <span slot="icon" class="mdi mdi-plus"></span>
        {{ t('providers.add') }}
      </md-filled-button>
    </div>

    <div v-if="loading" class="empty-state">
      <md-circular-progress indeterminate></md-circular-progress>
    </div>

    <div v-else-if="!providers.length" class="empty-state">
      <span class="mdi mdi-inbox-outline empty-state__icon"></span>
      <p class="md-typescale-body-large">{{ t('common.empty') }}</p>
    </div>

    <div v-else>
      <div class="card-grid" ref="gridEl">
        <article
        v-for="p in providers"
        :key="p.id"
        class="card"
        :data-id="p.id"
      >
        <span class="card__drag mdi mdi-drag-horizontal-variant" :title="t('providers.drag_tip')"></span>
        <span class="card__avatar mdi" :class="avatarClass(p)">
            <!-- Official Anthropic logo from simple-icons -->
            <svg v-if="p.kind === 'messages'" viewBox="0 0 24 24" width="22" height="22" fill="currentColor">
              <path d="M11.376 24L10.776 23.544L10.44 22.8L10.776 21.312L11.16 19.392L11.472 17.856L11.76 15.96L11.928 15.336L11.904 15.288L11.784 15.312L10.344 17.28L8.16 20.232L6.432 22.056L6.024 22.224L5.304 21.864L5.376 21.192L5.784 20.616L8.16 17.568L9.6 15.672L10.536 14.592L10.512 14.448H10.464L4.128 18.576L3 18.72L2.496 18.264L2.568 17.52L2.808 17.28L4.704 15.96L9.432 13.32L9.504 13.08L9.432 12.96H9.192L8.4 12.912L5.712 12.84L3.384 12.744L1.104 12.624L0.528 12.504L0 11.784L0.048 11.424L0.528 11.112L1.224 11.16L2.736 11.28L5.016 11.424L6.672 11.52L9.12 11.784H9.504L9.552 11.616L9.432 11.52L9.336 11.424L6.96 9.84L4.416 8.16L3.072 7.176L2.352 6.672L1.992 6.216L1.848 5.208L2.496 4.488L3.384 4.56L3.6 4.608L4.488 5.304L6.384 6.768L8.88 8.616L9.24 8.904L9.408 8.808V8.736L9.24 8.472L7.896 6.024L6.456 3.528L5.808 2.496L5.64 1.872C5.576 1.656 5.544 1.416 5.544 1.152L6.288 0.144001L6.696 0L7.704 0.144001L8.112 0.504001L8.736 1.92L9.72 4.152L11.28 7.176L11.736 8.088L11.976 8.904L12.072 9.168H12.24V9.024L12.36 7.296L12.6 5.208L12.84 2.52L12.912 1.752L13.296 0.840001L14.04 0.360001L14.616 0.624001L15.096 1.32L15.024 1.752L14.76 3.6L14.184 6.504L13.824 8.472H14.04L14.28 8.208L15.264 6.912L16.92 4.848L17.64 4.032L18.504 3.12L19.056 2.688H20.088L20.832 3.816L20.496 4.992L19.44 6.336L18.552 7.464L17.28 9.168L16.512 10.536L16.584 10.632H16.752L19.608 10.008L21.168 9.744L22.992 9.432L23.832 9.816L23.928 10.2L23.592 11.016L21.624 11.496L19.32 11.952L15.888 12.768L15.84 12.792L15.888 12.864L17.424 13.008L18.096 13.056H19.728L22.752 13.272L23.544 13.8L24 14.424L23.928 14.928L22.704 15.528L21.072 15.144L17.232 14.232L15.936 13.92H15.744V14.016L16.848 15.096L18.84 16.896L21.36 19.224L21.48 19.8L21.168 20.28L20.832 20.232L18.624 18.552L17.76 17.808L15.84 16.2H15.72V16.368L16.152 17.016L18.504 20.544L18.624 21.624L18.456 21.96L17.832 22.176L17.184 22.056L15.792 20.136L14.376 17.952L13.224 16.008L13.104 16.104L12.408 23.352L12.096 23.712L11.376 24Z"/>
            </svg>
            <!-- Official OpenAI logo from simple-icons -->
            <svg v-else viewBox="0 0 24 24" width="22" height="22" fill="currentColor">
              <path d="M22.2819 9.8211a5.9847 5.9847 0 0 0-.5157-4.9108 6.0462 6.0462 0 0 0-6.5098-2.9A6.0651 6.0651 0 0 0 4.9807 4.1818a5.9847 5.9847 0 0 0-3.9977 2.9 6.0462 6.0462 0 0 0 .7427 7.0966 5.98 5.98 0 0 0 .511 4.9107 6.051 6.051 0 0 0 6.5146 2.9001A5.9847 5.9847 0 0 0 13.2599 24a6.0557 6.0557 0 0 0 5.7718-4.2058 5.9894 5.9894 0 0 0 3.9977-2.9001 6.0557 6.0557 0 0 0-.7475-7.0729zm-9.022 12.6081a4.4755 4.4755 0 0 1-2.8764-1.0408l.1419-.0804 4.7783-2.7582a.7948.7948 0 0 0 .3927-.6813v-6.7369l2.02 1.1686a.071.071 0 0 1 .038.052v5.5826a4.504 4.504 0 0 1-4.4945 4.4944zm-9.6607-4.1254a4.4708 4.4708 0 0 1-.5346-3.0137l.142.0852 4.783 2.7582a.7712.7712 0 0 0 .7806 0l5.8428-3.3685v2.3324a.0804.0804 0 0 1-.0332.0615L9.74 19.9502a4.4992 4.4992 0 0 1-6.1408-1.6464zM2.3408 7.8956a4.485 4.485 0 0 1 2.3655-1.9728V11.6a.7664.7664 0 0 0 .3879.6765l5.8144 3.3543-2.0201 1.1685a.0757.0757 0 0 1-.071 0l-4.8303-2.7865A4.504 4.504 0 0 1 2.3408 7.872zm16.5963 3.8558L13.1038 8.364 15.1192 7.2a.0757.0757 0 0 1 .071 0l4.8303 2.7913a4.4944 4.4944 0 0 1-.6765 8.1042v-5.6772a.79.79 0 0 0-.407-.667zm2.0107-3.0231l-.142-.0852-4.7735-2.7818a.7759.7759 0 0 0-.7854 0L9.409 9.2297V6.8974a.0662.0662 0 0 1 .0284-.0615l4.8303-2.7866a4.4992 4.4992 0 0 1 6.6802 4.66zM8.3065 12.863l-2.02-1.1638a.0804.0804 0 0 1-.038-.0567V6.0742a4.4992 4.4992 0 0 1 7.3757-3.4537l-.142.0805L8.704 5.459a.7948.7948 0 0 0-.3927.6813zm1.0976-2.3654l2.602-1.4998 2.6069 1.4998v2.9994l-2.5974 1.4997-2.6067-1.4997Z"/>
            </svg>
          </span>
        <div class="card__body">
          <h3 class="md-typescale-title-medium card__name">
            <span class="card__name-text" :title="p.name">{{ p.name }}</span>
            <span
              v-if="keyStatsMap[p.id]"
              class="card__key-stats"
              :class="{ 'key-stats--bad': keyStatsMap[p.id].green === 0 }"
              :title="t('providers.keys_available', { green: keyStatsMap[p.id].green, total: keyStatsMap[p.id].total })"
            >
              {{ keyStatsMap[p.id].green }}/{{ keyStatsMap[p.id].total }}
            </span>
          </h3>
          <span v-if="isPluginProvider(p)" class="card__endpoint md-typescale-body-medium" :class="{ 'endpoint--offline': !pluginOnlineMap[p.id] }" :title="t('providers.plugin_delegated')">{{ pluginOnlineMap[p.id] ? t('providers.plugin_online') : t('providers.plugin_offline') }}</span>
          <span v-else-if="p.base_url" class="card__endpoint md-typescale-body-medium mono" :title="p.base_url">{{ p.base_url }}</span>
        </div>
        <div class="card__actions">
          <md-icon-button
            :id="'prov-btn-' + p.id"
            class="card__more-btn"
            @click="toggleMenu(p)"
          >
            <span class="mdi mdi-dots-vertical"></span>
          </md-icon-button>
        </div>
      </article>
      </div>
    </div>

    <!-- Shared action menu (single instance, re-anchors per card) -->
    <md-menu
      :open="menuOpen != null"
      :anchor="menuAnchor"
      positioning="fixed"
      @closed="menuOpen = null"
    >
      <md-menu-item @click="editFromMenu">
        <span class="mdi mdi-pencil-outline"></span> {{ t('common.edit') }}
      </md-menu-item>
      <md-menu-item class="menu-item--danger" @click="deleteFromMenu">
        <span class="mdi mdi-delete-outline"></span> {{ t('common.delete') }}
      </md-menu-item>
    </md-menu>

    <md-dialog :open="deleteOpen" @close="deleteOpen = false">
      <div slot="headline">{{ t('providers.delete_title') }}</div>
      <div slot="content" class="form">
        <p class="md-typescale-body-medium">{{ t('providers.delete_confirm', { name: deleteTarget?.name || '' }) }}</p>
      </div>
      <div slot="actions">
        <md-text-button @click="deleteOpen = false">{{ t('common.cancel') }}</md-text-button>
        <md-text-button class="confirm-del" @click="confirmDelete">{{ t('providers.delete_confirm_btn') }}</md-text-button>
      </div>
    </md-dialog>
  </div>
</template>

<script setup lang="ts">
import { ref, reactive, computed, onMounted, onUnmounted, nextTick } from 'vue';
import { useRouter } from 'vue-router';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import Sortable from 'sortablejs';
import { providersApi, keysApi, type Provider } from '../api';
import { wsClient } from '../ws';
import { useProviderStore } from '../stores/providers';
import { t } from '../i18n';

const router = useRouter();
const providerStore = useProviderStore();

const providers = ref<Provider[]>([]);
const loading = ref(true);
const gridEl = ref<HTMLElement | null>(null);
let sortable: Sortable | null = null;
const deleteOpen = ref(false);
const deleteTarget = ref<Provider | null>(null);
const menuOpen = ref<string | null>(null);
const menuAnchor = ref('');
const menuTarget = ref<Provider | null>(null);

// 插件在线状态：provider_id -> online。
// 由 /api/plugins 列表初始化，由 plugin-online/offline/activated Tauri 事件实时刷新。
const pluginOnlineMap = reactive<Record<string, boolean>>({});
let unlistenFns: UnlistenFn[] = [];

// 判断是否为插件供应商（config 含 plugin_id，即委托 Provider）
function isPluginProvider(p: Provider): boolean {
  return !!(p.config as any)?.plugin_id;
}

// 头像着色：普通供应商与已连接插件→品牌彩色；未连接插件→黑白灰。
function avatarClass(p: Provider): string {
  if (isPluginProvider(p) && !pluginOnlineMap[p.id]) {
    return 'avatar--offline';
  }
  return p.kind === 'messages' ? 'avatar--anthropic' : 'avatar--openai';
}

function toggleMenu(p: Provider) {
  if (menuOpen.value === p.id) {
    menuOpen.value = null;
  } else {
    menuTarget.value = p;
    menuAnchor.value = 'prov-btn-' + p.id;
    menuOpen.value = p.id;
  }
}

function editFromMenu() {
  if (menuTarget.value) {
    router.push(`/providers/${menuTarget.value.id}/edit`);
  }
}

function deleteFromMenu() {
  if (menuTarget.value) {
    openDeleteConfirm(menuTarget.value);
  }
}

// Key stats map: provider_id -> { green, total }
const keyStatsMap = reactive<Record<string, { green: number; total: number }>>({});

// Function to load key stats from API
async function loadKeyStats() {
  try {
    const allKeys = await keysApi.list();
    const map: Record<string, { green: number; total: number }> = {};
    for (const k of allKeys) {
      if (!map[k.provider_id]) {
        map[k.provider_id] = { green: 0, total: 0 };
      }
      map[k.provider_id].total++;
      if (k.status === 'green' || k.status === 'unknown') {
        map[k.provider_id].green++;
      }
    }
    // Update reactive map
    for (const [pid, stats] of Object.entries(map)) {
      keyStatsMap[pid] = stats;
    }
  } catch {
    // ignore
  }
}

// WS handler for real-time key stats updates
function onKeyStats(event: any) {
  if (event.type === 'key_stats') {
    keyStatsMap[event.provider_id] = {
      green: event.green,
      total: event.total,
    };
  }
}

// 拉取插件在线状态：/api/plugins 列表带 connected 字段，按 provider_id 建表。
// 初始加载用；之后由 plugin-online/offline/activated 事件实时刷新。
async function loadPluginStatuses() {
  try {
    const resp = await fetch('http://localhost:19068/api/plugins');
    if (!resp.ok) return;
    const plugins = await resp.json();
    for (const p of plugins) {
      if (p.provider_id) pluginOnlineMap[p.provider_id] = !!p.connected;
    }
  } catch {
    // 忽略——后端未起或请求失败时，插件图标默认按灰显
  }
}

function openDeleteConfirm(p: Provider) {
  deleteTarget.value = p;
  deleteOpen.value = true;
}

async function confirmDelete() {
  if (!deleteTarget.value) return;
  await providersApi.delete(deleteTarget.value.id);
  deleteOpen.value = false;
  deleteTarget.value = null;
  await fetchProviders();
}

async function fetchProviders() {
  loading.value = true;
  try {
    providers.value = await providersApi.list();
  } finally {
    loading.value = false;
  }
  // 必须等 loading=false 触发 v-else 分支渲染后，gridEl 才有值，才能初始化 Sortable。
  await initSortable();
}

// 拖拽排序：Sortable 仅操作 DOM，onEnd 时把 DOM 顺序同步进 store 并持久化。
// 必须先 nextTick 等 v-else 分支渲染完成，gridEl 才有值；再检查 sortable 防重复初始化。
async function initSortable() {
  await nextTick();
  if (!gridEl.value) return;
  if (sortable) return;
  const opts: any = {
    animation: 150,
    handle: '.card__drag',
    // Tauri WebView(WebKit) 坑位三连：
    // 1. 原生 HTML5 dnd 不可靠 → forceFallback 用鼠标事件模拟；
    // 2. WebKit 的 PointerEvent 合成有缺陷（mousedown 生效但 move 被吞）→ 强制 mouse 事件；
    // 3. ghost 挂容器内可能被 overflow 裁剪 → fallbackOnBody 挂 body。
    forceFallback: true,
    supportPointer: false,
    fallbackOnBody: true,
    ghostClass: 'card--ghost',
    chosenClass: 'card--chosen',
    dragClass: 'card--dragging',
    onEnd: async (evt: { oldIndex: number | null; newIndex: number | null }) => {
      const { oldIndex, newIndex } = evt;
      if (oldIndex == null || newIndex == null || oldIndex === newIndex) return;
      // 按 Sortable 索引原地移动数组，保持 DOM 与 store 顺序一致
      const arr = [...providers.value];
      const [moved] = arr.splice(oldIndex, 1);
      arr.splice(newIndex, 0, moved);
      providers.value = arr;
      const ids = arr.map((p) => p.id);
      try {
        await providerStore.reorderProviders(ids);
      } catch {
        // 保存失败：store 已回滚到服务端顺序，重拉一遍保证视图一致
        await providerStore.fetchProviders();
      }
    },
  };
  sortable = new Sortable(gridEl.value, opts);
}

onMounted(async () => {
  fetchProviders();
  loadKeyStats();
  loadPluginStatuses();
  wsClient.connect();
  wsClient.on('key_stats', onKeyStats);

  // 监听插件生命周期 Tauri 事件，实时刷新图标在线状态。
  // 后端在 register/reconnect/confirm/disconnect 时 emit，payload 含 provider_id。
  unlistenFns = await Promise.all([
    listen<{ provider_id: string }>('plugin-online', (e) => {
      if (e.payload?.provider_id) pluginOnlineMap[e.payload.provider_id] = true;
    }),
    listen<{ provider_id: string }>('plugin-activated', (e) => {
      if (e.payload?.provider_id) pluginOnlineMap[e.payload.provider_id] = true;
    }),
    listen<{ provider_id: string }>('plugin-offline', (e) => {
      if (e.payload?.provider_id) pluginOnlineMap[e.payload.provider_id] = false;
    }),
  ]);
});

onUnmounted(() => {
  wsClient.off('key_stats', onKeyStats);
  sortable?.destroy();
  unlistenFns.forEach((fn) => fn());
  unlistenFns = [];
});
</script>

<style scoped>
.page__header { display: flex; justify-content: space-between; align-items: flex-start; margin-bottom: 24px; gap: 16px; flex-wrap: wrap; }
.page__title { margin: 0; }
.empty-state { display: flex; flex-direction: column; align-items: center; gap: 8px; padding: 64px 24px; text-align: center; }
.empty-state__icon { font-size: 48px; color: var(--md-sys-color-on-surface-variant); }
.card-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(320px, 1fr)); gap: 16px; }
.card {
  background: var(--md-sys-color-surface-container-low); border-radius: var(--md-sys-shape-corner-medium);
  padding: 20px; display: grid; grid-template-columns: 24px 44px 1fr auto; gap: 12px; align-items: start;
  cursor: default;
}
.card__drag {
  display: flex; align-items: center; justify-content: center;
  color: var(--md-sys-color-on-surface-variant);
  cursor: grab; font-size: 20px; line-height: 1;
  padding-top: 12px;
  touch-action: none;
}
.card__drag:active { cursor: grabbing; }
.card--ghost { opacity: 0.35; outline: 2px dashed var(--md-sys-color-primary); outline-offset: -2px; }
.card--chosen { background: var(--md-sys-color-surface-container-high); }
.card--dragging { opacity: 0.8; transform: scale(1.02); }
.card__avatar { width: 44px; height: 44px; border-radius: var(--md-sys-shape-corner-full); display: flex; align-items: center; justify-content: center; }
.card__avatar svg { width: 22px; height: 22px; }
.avatar--openai { background: var(--md-sys-color-openai-brand); color: #fff; }
.avatar--anthropic { background: var(--md-sys-color-anthropic-brand); color: #fff; }
.avatar--offline { background: var(--md-sys-color-surface-container-high); color: var(--md-sys-color-on-surface-variant); }
.endpoint--offline { color: var(--md-sys-color-outline); font-style: italic; }
.card__body { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
.card__name { margin: 0; display: flex; align-items: center; gap: 6px; min-width: 0; }
.card__name-text { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
.card__enabled-dot { width: 8px; height: 8px; border-radius: 50%; flex-shrink: 0; }
.dot--on { background: var(--md-sys-color-primary); }
.dot--off { background: var(--md-sys-color-outline-variant); }
.card__key-stats {
  display: inline-flex;
  align-items: center;
  padding: 1px 8px;
  border-radius: var(--md-sys-shape-corner-full);
  background: var(--md-sys-color-surface-container-high);
  color: var(--md-sys-color-on-surface-variant);
  font-size: 0.75rem;
  font-weight: 500;
  font-variant-numeric: tabular-nums;
}
.key-stats--bad {
  background: var(--md-sys-color-error-container);
  color: var(--md-sys-color-on-error-container);
}
.card__chip { display: inline-flex; align-items: center; padding: 2px 8px; border-radius: var(--md-sys-shape-corner-full); font-size: 0.75rem; font-weight: 500; }
.chip { display: inline-flex; align-items: center; padding: 2px 10px; border-radius: var(--md-sys-shape-corner-full); background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); font-size: 0.75rem; width: fit-content; margin-top: 2px; }
.card__endpoint { color: var(--md-sys-color-on-surface-variant); font-size: 0.75rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; display: block; margin-top: 4px; }
.card__actions { display: flex; justify-content: flex-end; position: relative; }
.card__more-btn { --md-icon-button-icon-size: 20px; width: 36px; height: 36px; }
.form { min-width: 300px; }
.confirm-del { color: var(--md-sys-color-error); }
.mono { font-family: 'Roboto Mono', monospace; }
</style>

<!-- md-menu teleports to document root, so its styles must not be scoped -->
<style>
.menu-item--danger { --md-menu-item-label-text-color: var(--md-sys-color-error); color: var(--md-sys-color-error); }
</style>
