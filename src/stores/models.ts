import { create } from 'zustand';
import { modelsApi, type Model } from '@/lib/api';

interface ModelsState {
  models: Model[];
  loading: boolean;
  fetchModels: (providerId?: string) => Promise<void>;
  syncModels: (providerId: string) => Promise<void>;
  createModel: (data: any) => Promise<Model>;
  updateModel: (modelId: string, data: any) => Promise<Model>;
  deleteModel: (modelId: string) => Promise<void>;
}

export const useModelsStore = create<ModelsState>((set, get) => ({
  models: [],
  loading: false,

  async fetchModels(providerId?: string) {
    set({ loading: true });
    try {
      const models = await modelsApi.list(providerId);
      set({ models, loading: false });
    } catch (e) {
      set({ loading: false });
      throw e;
    }
  },

  async syncModels(providerId: string) {
    await modelsApi.sync(providerId);
    await get().fetchModels(providerId);
  },

  async createModel(data: any) {
    const model = await modelsApi.create(data);
    set((state) => ({ models: [...state.models, model] }));
    return model;
  },

  async updateModel(modelId: string, data: any) {
    const model = await modelsApi.update(modelId, data);
    set((state) => ({
      models: state.models.map((m) => (m.id === modelId ? model : m)),
    }));
    return model;
  },

  async deleteModel(modelId: string) {
    await modelsApi.delete(modelId);
    set((state) => ({
      models: state.models.filter((m) => m.id !== modelId),
    }));
  },
}));
