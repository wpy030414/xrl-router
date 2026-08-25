<template>
  <div class="page">
    <h2 class="md-typescale-headline-medium page__title">
      {{ isEdit ? t('comboNew.title.edit') : t('comboNew.title.create') }}
    </h2>

    <md-outlined-text-field
      :value="name"
      :label="t('comboNew.name_label')"
      class="field"
      @input="name = ($event.target as HTMLInputElement).value"
    ></md-outlined-text-field>

    <p class="md-typescale-label-large available-label">{{ t('comboNew.selected_label') }}</p>
    <div class="selected-list">
      <div v-for="(m, i) in selected" :key="m" class="selected-item">
        <span class="selected-item__order mono">{{ i + 1 }}</span>
        <span class="selected-item__name">{{ m }}</span>
        <md-icon-button class="selected-item__btn" :disabled="i === 0" @click="moveUp(i)" :title="t('comboNew.move_up')">
          <MdiIcon :path="mdiArrowUp" />
        </md-icon-button>
        <md-icon-button class="selected-item__btn" :disabled="i === selected.length - 1" @click="moveDown(i)" :title="t('comboNew.move_down')">
          <MdiIcon :path="mdiArrowDown" />
        </md-icon-button>
        <md-icon-button class="selected-item__btn" @click="removeMember(i)" :title="t('comboNew.remove')">
          <MdiIcon :path="mdiClose" />
        </md-icon-button>
      </div>
      <p v-if="!selected.length" class="md-typescale-body-medium selected-empty">{{ t('comboNew.selected_empty') }}</p>
    </div>

    <p class="md-typescale-label-large available-label">{{ t('comboNew.available_label') }}</p>
    <md-circular-progress v-if="modelsLoading" indeterminate></md-circular-progress>
    <div v-else class="available-list">
      <template v-for="g in providerModels" :key="g.name">
        <div class="available-provider md-typescale-label-large">{{ g.name }}</div>
        <label v-for="m in g.models" :key="m" class="available-item md-typescale-body-medium">
          <md-checkbox :checked="selected.includes(m)" @click="toggleMember(m)"></md-checkbox>
          {{ m }}
        </label>
      </template>
      <p v-if="!providerModels.length" class="md-typescale-body-medium">{{ t('comboNew.no_models') }}</p>
    </div>

    <div class="enabled-row">
      <span class="md-typescale-body-medium">{{ t('comboNew.enabled_label') }}</span>
      <md-switch :selected="enabled" @change="enabled = ($event.target as any).selected"></md-switch>
    </div>

    <div class="actions">
      <md-text-button @click="$router.push('/combos')">{{ t('common.cancel') }}</md-text-button>
      <md-filled-button @click="save" :disabled="saving || !canSave">
        {{ saving ? t('comboNew.saving') : isEdit ? t('comboNew.save_edit') : t('comboNew.save_create') }}
      </md-filled-button>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted } from 'vue';
import '@material/web/textfield/outlined-text-field.js';
import '@material/web/checkbox/checkbox.js';
import '@material/web/switch/switch.js';
import '@material/web/iconbutton/icon-button.js';
import '@material/web/progress/circular-progress.js';
import { useRouter, useRoute } from 'vue-router';
import { mdiArrowUp, mdiArrowDown, mdiClose } from '@mdi/js';
import { combosApi, providersApi, modelsApi } from '../api';
import { t } from '../i18n';
import MdiIcon from '../components/MdiIcon.vue';

const router = useRouter();
const route = useRoute();

const editId = ref<string | null>(null);
const isEdit = computed(() => !!editId.value);

const name = ref('');
const enabled = ref(true);
const selected = ref<string[]>([]);
const providerModels = ref<{ name: string; models: string[] }[]>([]);
const modelsLoading = ref(true);
const saving = ref(false);

const canSave = computed(() => name.value.trim() && selected.value.length > 0);

/** 勾选/取消成员：选中追加到队尾（尝试顺序 = 勾选顺序），取消移除。 */
function toggleMember(m: string) {
  if (selected.value.includes(m)) {
    selected.value = selected.value.filter((x) => x !== m);
  } else {
    selected.value = [...selected.value, m];
  }
}

function moveUp(i: number) {
  if (i <= 0) return;
  const arr = [...selected.value];
  [arr[i - 1], arr[i]] = [arr[i], arr[i - 1]];
  selected.value = arr;
}

function moveDown(i: number) {
  if (i >= selected.value.length - 1) return;
  const arr = [...selected.value];
  [arr[i + 1], arr[i]] = [arr[i], arr[i + 1]];
  selected.value = arr;
}

function removeMember(i: number) {
  selected.value = selected.value.filter((_, idx) => idx !== i);
}

/** 可选模型：按 provider 分组的全部模型别名（enabled 与否都列出，禁用成员运行时会被跳过）。 */
async function fetchAvailableModels() {
  modelsLoading.value = true;
  try {
    const [providers, models] = await Promise.all([providersApi.list(), modelsApi.list()]);
    const providerName = new Map(providers.map((p) => [p.id, p.name]));
    const groupsMap = new Map<string, string[]>();
    for (const m of models) {
      const pname = providerName.get(m.provider_id) || t('common.unknown');
      const alias = m.display_name || m.model_id;
      if (!groupsMap.has(pname)) groupsMap.set(pname, []);
      if (!groupsMap.get(pname)!.includes(alias)) groupsMap.get(pname)!.push(alias);
    }
    providerModels.value = Array.from(groupsMap.entries()).map(([n, ms]) => ({ name: n, models: ms.sort() }));
  } catch (e: any) {
    alert(t('comboNew.load_failed', { msg: e?.message || e }));
  } finally {
    modelsLoading.value = false;
  }
}

async function save() {
  saving.value = true;
  const payload = {
    name: name.value.trim(),
    members: selected.value,
    enabled: enabled.value,
  };
  try {
    if (isEdit.value) {
      await combosApi.update(editId.value!, payload);
    } else {
      await combosApi.create(payload);
    }
    router.push('/combos');
  } catch (e: any) {
    alert(t('comboNew.save_failed', { msg: e?.message || e }));
  } finally {
    saving.value = false;
  }
}

onMounted(async () => {
  const id = route.params.id as string;
  if (id) {
    editId.value = id;
    try {
      const c = await combosApi.get(id);
      name.value = c.name;
      enabled.value = c.enabled;
      selected.value = [...c.members];
    } catch (e: any) {
      alert(t('comboNew.load_failed', { msg: e?.message || e }));
    }
  }
  fetchAvailableModels();
});
</script>

<style scoped>
.page { max-width: 640px; }
.page__title { margin: 0 0 24px; }
.field { width: 100%; margin-bottom: 16px; display: block; }
.selected-list {
  display: flex; flex-direction: column; gap: 4px; margin-bottom: 20px;
  background: var(--md-sys-color-surface-container-low);
  border-radius: var(--md-sys-shape-corner-medium); padding: 8px;
}
.selected-item {
  display: flex; align-items: center; gap: 8px; padding: 4px 8px;
  border-radius: var(--md-sys-shape-corner-small);
}
.selected-item:hover { background: var(--md-sys-color-surface-container-high); }
.selected-item__order {
  width: 20px; text-align: center; color: var(--md-sys-color-on-surface-variant);
  font-size: 0.8rem; flex-shrink: 0;
}
.selected-item__name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.selected-item__btn { --md-icon-button-icon-size: 18px; width: 32px; height: 32px; }
.selected-empty { color: var(--md-sys-color-on-surface-variant); margin: 8px; }
.available-label { margin: 0 0 8px; color: var(--md-sys-color-on-surface-variant); }
.available-list { max-height: 320px; overflow-y: auto; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-corner-medium); padding: 12px; margin-bottom: 20px; }
.available-provider { margin: 8px 0 4px; color: var(--md-sys-color-primary); }
.available-provider:first-child { margin-top: 0; }
.available-item { display: flex; align-items: center; gap: 8px; padding: 2px 0; cursor: pointer; }
.enabled-row { display: flex; align-items: center; gap: 12px; margin-bottom: 24px; }
.actions { display: flex; justify-content: flex-end; gap: 8px; }
.mono { font-family: 'Roboto Mono', monospace; }
</style>
