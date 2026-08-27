import { create } from 'zustand';

type Theme = 'light' | 'dark' | 'system';
type Locale = 'zh-CN' | 'en';

interface UiState {
  theme: Theme;
  locale: Locale;
  sidebarCollapsed: boolean;
  setTheme: (theme: Theme) => void;
  setLocale: (locale: Locale) => void;
  toggleSidebar: () => void;
}

export const useUiStore = create<UiState>((set) => ({
  theme: (localStorage.getItem('theme') as Theme) || 'system',
  locale: (localStorage.getItem('locale') as Locale) || 'zh-CN',
  sidebarCollapsed: false,

  setTheme: (theme) => {
    localStorage.setItem('theme', theme);
    set({ theme });
  },

  setLocale: (locale) => {
    localStorage.setItem('locale', locale);
    set({ locale });
  },

  toggleSidebar: () => {
    set((state) => ({ sidebarCollapsed: !state.sidebarCollapsed }));
  },
}));
