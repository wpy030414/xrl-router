import { create } from 'zustand';
import { keysApi, type ApiKey } from '@/lib/api';

interface KeysState {
  keys: ApiKey[];
  loading: boolean;
  fetchKeys: (providerId?: string) => Promise<void>;
  createKey: (providerId: string, data: { name: string; key: string }) => Promise<ApiKey>;
  updateKey: (id: string, data: Partial<ApiKey>) => Promise<ApiKey>;
  deleteKey: (providerId: string, keyId: string) => Promise<void>;
  updateKeyHealth: (keyId: string, status: string) => void;
}

export const useKeysStore = create<KeysState>((set, get) => ({
  keys: [],
  loading: false,

  async fetchKeys(providerId?: string) {
    set({ loading: true });
    try {
      const keys = await keysApi.list(providerId);
      set({ keys, loading: false });
    } catch (e) {
      set({ loading: false });
      throw e;
    }
  },

  async createKey(providerId: string, data: { name: string; key: string }) {
    const key = await keysApi.create(providerId, data);
    set((state) => ({ keys: [...state.keys, key] }));
    return key;
  },

  async updateKey(id: string, data: Partial<ApiKey>) {
    const key = await keysApi.update(id, data);
    set((state) => ({
      keys: state.keys.map((k) => (k.id === id ? key : k)),
    }));
    return key;
  },

  async deleteKey(providerId: string, keyId: string) {
    await keysApi.delete(providerId, keyId);
    set((state) => ({
      keys: state.keys.filter((k) => k.id !== keyId),
    }));
  },

  updateKeyHealth(keyId: string, status: string) {
    set((state) => ({
      keys: state.keys.map((k) =>
        k.id === keyId ? { ...k, status: status as any } : k
      ),
    }));
  },
}));
