import { create } from 'zustand';
import { settingsApi, type AppSettings, type SettingsUpdate } from '@/lib/api';

interface SettingsState {
  settings: AppSettings | null;
  loading: boolean;
  fetchSettings: () => Promise<void>;
  updateSettings: (data: SettingsUpdate) => Promise<void>;
}

export const useSettingsStore = create<SettingsState>((set) => ({
  settings: null,
  loading: false,

  async fetchSettings() {
    set({ loading: true });
    try {
      const settings = await settingsApi.get();
      set({ settings, loading: false });
    } catch (e) {
      set({ loading: false });
      throw e;
    }
  },

  async updateSettings(data: SettingsUpdate) {
    await settingsApi.update(data);
    // 服务端会归一化（密钥只回显「是否已配置」），重新拉取而不是本地合并
    const settings = await settingsApi.get();
    set({ settings });
  },
}));
