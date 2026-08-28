import { create } from 'zustand';
import { localModelsApi, type LocalModel, type BackendDetect } from '@/lib/api';

interface LocalModelsState {
  models: LocalModel[];
  loading: boolean;
  backends: BackendDetect | null;
  /** 下载进度：id → { downloaded, total } */
  progress: Record<string, { downloaded: number; total: number }>;
  fetchModels: () => Promise<void>;
  fetchBackends: () => Promise<void>;
  updateProgress: (id: string, downloaded: number, total: number) => void;
  updateStatus: (id: string, status: string, port?: number | null) => void;
}

export const useLocalModelsStore = create<LocalModelsState>((set) => ({
  models: [],
  loading: false,
  backends: null,
  progress: {},

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

  updateProgress(id, downloaded, total) {
    set((state) => ({
      progress: { ...state.progress, [id]: { downloaded, total } },
    }));
  },

  updateStatus(id, status, port) {
    set((state) => ({
      models: state.models.map((m) =>
        m.id === id ? { ...m, status: status as LocalModel['status'], port: port ?? m.port } : m
      ),
    }));
  },
}));
