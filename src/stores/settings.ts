import { create } from 'zustand';
import { settingsApi, type AppSettings } from '@/lib/api';

interface SettingsState {
  settings: AppSettings | null;
  loading: boolean;
  fetchSettings: () => Promise<void>;
  updateSettings: (data: Partial<AppSettings>) => Promise<void>;
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

  async updateSettings(data: Partial<AppSettings>) {
    await settingsApi.update(data);
    set((state) => ({
      settings: state.settings ? { ...state.settings, ...data } : null,
    }));
  },
}));
