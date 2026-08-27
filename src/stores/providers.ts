import { create } from 'zustand';
import { providersApi, type Provider } from '@/lib/api';

interface ProvidersState {
  providers: Provider[];
  fetchProviders: () => Promise<void>;
  reorderProviders: (ids: string[]) => Promise<void>;
}

export const useProvidersStore = create<ProvidersState>((set, get) => ({
  providers: [],

  async fetchProviders() {
    const providers = await providersApi.list();
    set({ providers });
  },

  async reorderProviders(ids: string[]) {
    const { providers, fetchProviders } = get();
    const map = new Map(providers.map((p) => [p.id, p]));
    const reordered = ids.map((id) => map.get(id)).filter((p): p is Provider => !!p);
    set({ providers: reordered });
    try {
      await providersApi.reorder(ids);
    } catch (e) {
      // 保存失败时回滚到服务端顺序
      await fetchProviders();
      throw e;
    }
  },
}));
