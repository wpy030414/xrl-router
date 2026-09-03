import { create } from 'zustand';
import { localModelsApi, type LocalModel, type BackendDetect } from '@/lib/api';

interface LocalModelsState {
  models: LocalModel[];
  loading: boolean;
  backends: BackendDetect | null;
  fetchModels: () => Promise<void>;
  fetchBackends: () => Promise<void>;
  updateStatus: (id: string, status: string, port?: number | null) => void;
}

export const useLocalModelsStore = create<LocalModelsState>((set) => ({
  models: [],
  loading: false,
  backends: null,

  async fetchModels() {
    set({ loading: true });
    try {
      const models = await localModelsApi.list();
      set({ models, loading: false });
    } catch {
      set({ loading: false });
    }
  },

  async fetchBackends() {
    try {
      const backends = await localModelsApi.backends();
      set({ backends });
    } catch {
      // ignore
    }
  },

  updateStatus(id, status, port) {
    set((state) => ({
      models: state.models.map((m) =>
        m.id === id ? { ...m, status: status as LocalModel['status'], port: port ?? m.port } : m
      ),
    }));
  },
}));
