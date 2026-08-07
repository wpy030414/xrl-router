<template>
  <div class="page">
    <h2 class="md-typescale-headline-medium page__title">
      {{ isEdit ? t('providerNew.title.edit') : isPluginMode ? t('providerNew.title.plugin') : t('providerNew.title.create') }}
    </h2>

    <md-outlined-text-field
      :value="name"
      :label="t('providerNew.name_label')"
      class="field"
      @input="name = ($event.target as HTMLInputElement).value"
    ></md-outlined-text-field>

    <md-outlined-select
      v-show="!isPluginMode"
      :value="kind"
      :label="t('providerNew.kind_label')"
      class="field"
      menu-positioning="fixed"
      @input="kind = ($event.target as HTMLInputElement).value as 'openai' | 'anthropic' | 'responses'"
    >
      <md-select-option value="anthropic"><span slot="headline">Anthropic Messages</span></md-select-option>
      <md-select-option value="openai"><span slot="headline">OpenAI Chat Completions</span></md-select-option>
      <md-select-option value="responses"><span slot="headline">OpenAI Responses</span></md-select-option>
    </md-outlined-select>

    <md-outlined-text-field
      v-show="!isPluginMode"
      :value="baseUrl"
      :label="t('providerNew.base_url_label')"
      placeholder="https://api.example.com"
      class="field"
      @input="baseUrl = ($event.target as HTMLInputElement).value"
    ></md-outlined-text-field>

    <md-outlined-text-field
      v-show="!isPluginMode"
      :value="apiKeysText"
      type="textarea"
      rows="5"
      :label="t('providerNew.api_key_label')"
      placeholder="sk-xxxxxxxx&#10;sk-yyyyyyyy"
      class="field"
      @input="apiKeysText = ($event.target as HTMLInputElement).value"
    ></md-outlined-text-field>

    <md-outlined-text-field
      :value="modelsText"
      type="textarea"
      rows="5"
      :label="t('providerNew.models_label')"
      :placeholder="t('providerNew.models_placeholder')"
      class="field"
      @input="modelsText = ($event.target as HTMLInputElement).value"
    ></md-outlined-text-field>

    <div v-if="isPluginMode && pluginInfo && pluginInfo.key_count > 0" class="keys-info">
      <span class="mdi mdi-key"></span>
      {{ t('providerNew.keys_synced', { count: pluginInfo.key_count }) }}
    </div>

    <div class="actions">
      <md-text-button @click="$router.push('/providers')">{{ t('common.cancel') }}</md-text-button>
      <md-filled-button @click="save" :disabled="saving || !canSave">
        {{ saving ? t('providerNew.saving') : isEdit ? t('providerNew.save_edit') : t('providerNew.save_create') }}
      </md-filled-button>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted } from 'vue';
import { useRouter, useRoute } from 'vue-router';
import { providersApi, keysApi, modelsApi } from '../api';
import { t } from '../i18n';

const router = useRouter();
const route = useRoute();

const editId = ref<string | null>(null);
const isEdit = computed(() => !!editId.value);

// —— 插件模式状态 ——
const pluginId = ref('');
const pluginInfo = ref<any>(null);
const isPluginMode = computed(() => !!pluginId.value);

const name = ref('');
const baseUrl = ref('');
const kind = ref<'anthropic' | 'openai' | 'responses'>('anthropic');
const apiKeysText = ref('');
const modelsText = ref('');
const saving = ref(false);

// API Path 由 API 格式派生（不在界面显示）
const apiPathDerived = computed(() => {
  switch (kind.value) {
    case 'anthropic': return '/v1/messages';
    case 'responses': return '/v1/responses';
    default: return '/v1/chat/completions';
  }
});

const canSave = computed(() => name.value.trim() && baseUrl.value.trim());

function parseLines(text: string): string[] {
  return text
    .split('\n')
    .map((s) => s.trim())
    .filter(Boolean);
}

function parseModelLine(line: string): { model_id: string; display_name: string } {
  const idx = line.indexOf('<-');
  if (idx >= 0) {
    const mid = line.slice(0, idx).trim();
    const alias = line.slice(idx + 2).trim();
    return { model_id: mid, display_name: alias || mid };
  }
  const mid = line.trim();
  return { model_id: mid, display_name: mid };
}

async function save() {
  const payload = {
    name: name.value.trim(),
    kind: kind.value,
    base_url: baseUrl.value.trim(),
    api_path: apiPathDerived.value,
    config: isPluginMode.value
      ? { plugin_id: pluginId.value, delegated: true }
      : {},
  };
  saving.value = true;
  try {
    let providerId: string;

    if (isEdit.value) {
      await providersApi.update(editId.value!, payload);
      providerId = editId.value!;
    } else if (isPluginMode.value) {
      // 插件模式：provider 已在注册时创建，不重复创建，只更新
      providerId = pluginInfo.value.provider.id;
      await providersApi.update(providerId, payload);
    } else {
      const p = await providersApi.create(payload);
      providerId = p.id;
    }

    // API Keys（一行一个）：编辑时全量同步（删旧建新），创建时直接建
    // 插件模式下跳过——密钥由插件从 .env 自动同步到 Router 密钥池
    if (!isPluginMode.value) {
      if (isEdit.value) {
        const oldKeys = await keysApi.list(providerId);
        for (const k of oldKeys) {
          await keysApi.delete(providerId, k.id);
        }
      }
      for (const key of parseLines(apiKeysText.value)) {
        await keysApi.create(providerId, { name: payload.name + t('providerNew.key_suffix'), key });
      }
    }

    // 模型：全量同步（删旧建新）。
    // 插件模式首次添加也要删旧——register 时已预建 models，直接 create 会撞 UNIQUE 约束
    const models = parseLines(modelsText.value).map(parseModelLine);
    if (isEdit.value || isPluginMode.value) {
      const oldModels = await modelsApi.list(providerId);
      for (const m of oldModels) {
        await modelsApi.delete(m.id);
      }
    }
    for (const m of models) {
      if (!m.model_id) continue;
      await modelsApi.create({
        provider_id: providerId,
        model_id: m.model_id,
        display_name: m.display_name,
        tier: 'custom',
      });
    }

    // 插件模式首次添加：确认激活插件供应商（编辑模式不重复确认）
    if (isPluginMode.value && !isEdit.value) {
      await fetch(`http://localhost:19068/api/plugins/${pluginId.value}/confirm`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
      });
    }

    router.push('/providers');
  } catch (e: any) {
    alert(t('providerNew.save_failed', { msg: e?.message || e }));
  } finally {
    saving.value = false;
  }
}

onMounted(async () => {
  // 插件模式：从 query 参数读取 plugin_id + provider_id，预填所有字段
  const qPluginId = route.query.plugin_id as string | undefined;
  if (qPluginId) {
    pluginId.value = qPluginId;
    try {
      const resp = await fetch(`http://localhost:19068/api/plugins/${qPluginId}`);
      if (resp.ok) {
        const data = await resp.json();
        pluginInfo.value = data;
        name.value = data.provider?.name || qPluginId;
        kind.value = (data.provider?.kind as 'anthropic' | 'openai' | 'responses') || 'anthropic';
        baseUrl.value = data.provider?.base_url || '';
        modelsText.value = (data.models || [])
          .map((m: any) =>
            m.display_name && m.display_name !== m.model_id
              ? `${m.model_id}<-${m.display_name}`
              : m.model_id,
          )
          .join('\n');
      }
    } catch (e: any) {
      alert(t('providerNew.plugin_load_failed', { msg: e?.message || e }));
    }
    return;
  }

  // 普通编辑模式：回填已有 provider
  const id = route.params.id as string;
  if (!id) return;
  editId.value = id;
  try {
    const p = await providersApi.get(id);
    name.value = p.name;
    kind.value = (p.kind as 'anthropic' | 'openai' | 'responses') || 'anthropic';
    baseUrl.value = p.base_url || '';

    // 插件供应商（config_json 含 plugin_id）：同样隐藏连接字段
    const cfgPluginId = (p.config as any)?.plugin_id as string | undefined;
    if (cfgPluginId) {
      pluginId.value = cfgPluginId;
      // 加载插件详情获取 key_count 等信息
      try {
        const resp = await fetch(`http://localhost:19068/api/plugins/${cfgPluginId}`);
        if (resp.ok) {
          pluginInfo.value = await resp.json();
        }
      } catch { /* 插件信息加载失败不阻塞编辑 */ }
    }

    // 回填模型（display_name 与 model_id 不同时写成 model_id<-alias）
    const models = await modelsApi.list(id);
    modelsText.value = models
      .map((m: any) =>
        m.display_name && m.display_name !== m.model_id
          ? `${m.model_id}<-${m.display_name}`
          : m.model_id,
      )
      .join('\n');

    // 回填明文密钥（插件模式下密钥由插件托管，无需回填）
    if (!cfgPluginId) {
      const keys = await keysApi.list(id);
      if (keys.length) {
        apiKeysText.value = keys
          .map((k: any) => k.key_plain || '')
          .filter(Boolean)
          .join('\n');
      }
    }
  } catch (e: any) {
    alert(t('providerNew.load_failed', { msg: e?.message || e }));
  }
});
</script>

<style scoped>
.page { max-width: 640px; }
.back-btn { display: inline-flex; align-items: center; gap: 4px; border: none; background: transparent; color: var(--md-sys-color-on-surface-variant); cursor: pointer; font-family: inherit; font-size: 0.875rem; padding: 0; margin-bottom: 12px; }
.back-btn:hover { color: var(--md-sys-color-on-surface); }
.page__title { margin: 0 0 24px; }
.field { width: 100%; margin-bottom: 16px; display: block; }
.actions { display: flex; justify-content: flex-end; gap: 8px; margin-top: 24px; }
.keys-info { display: flex; align-items: center; gap: 6px; color: var(--md-sys-color-primary); font-size: 0.9em; margin-bottom: 16px; }
</style>
