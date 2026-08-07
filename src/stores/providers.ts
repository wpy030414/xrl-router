import { defineStore } from 'pinia';
import { ref, computed } from 'vue';
import { providersApi } from '../api';

export interface Provider {
  id: string;
  name: string;
  kind: 'messages' | 'chat_completions' | 'responses';
  base_url: string;
  enabled: boolean;
  created_at: number;
  updated_at: number;
}

export const useProviderStore = defineStore('providers', () => {
  const providers = ref<Provider[]>([]);
  const loading = ref(false);
  const error = ref<string | null>(null);

  const enabledProviders = computed(() => providers.value.filter((p) => p.enabled));
  const providerCount = computed(() => providers.value.length);

  async function fetchProviders() {
    loading.value = true;
    error.value = null;
    try {
      providers.value = await providersApi.list();
    } catch (e: any) {
      error.value = e.message;
    } finally {
      loading.value = false;
    }
  }

  async function createProvider(data: any) {
    const provider = await providersApi.create(data);
    providers.value.push(provider);
    return provider;
  }

  async function updateProvider(id: string, data: any) {
    const provider = await providersApi.update(id, data);
    const idx = providers.value.findIndex((p) => p.id === id);
    if (idx >= 0) providers.value[idx] = provider;
    return provider;
  }

  async function deleteProvider(id: string) {
    await providersApi.delete(id);
    providers.value = providers.value.filter((p) => p.id !== id);
  }

  // 拖拽排序：重新赋值数组（保持响应式），并持久化新顺序
  async function reorderProviders(ids: string[]) {
    const map = new Map(providers.value.map((p) => [p.id, p]));
    providers.value = ids.map((id) => map.get(id)).filter((p): p is Provider => !!p);
    try {
      await providersApi.reorder(ids);
    } catch (e: any) {
      // 保存失败时回滚到服务端顺序
      await fetchProviders();
      throw e;
    }
  }

  return {
    providers,
    loading,
    error,
    enabledProviders,
    providerCount,
    fetchProviders,
    createProvider,
    updateProvider,
    deleteProvider,
    reorderProviders,
  };
});
