import { create } from 'zustand';
import zhCN from './zh-CN';
import en from './en';
import { settingsApi } from '@/lib/api';
import { invoke } from '@/lib/tauri';

export type Locale = 'zh-CN' | 'en';

const dictionaries: Record<Locale, Record<string, string>> = {
  'zh-CN': zhCN,
  en,
};

interface I18nState {
  locale: Locale;
  setLocale: (locale: Locale) => void;
}

export const useI18nStore = create<I18nState>((set) => ({
  locale: (localStorage.getItem('locale') as Locale) || 'zh-CN',
  setLocale: (locale) => {
    localStorage.setItem('locale', locale);
    set({ locale });
    // 同步 Tauri 原生菜单 + 后端
    invoke('set_locale', { locale }).catch(() => {});
    settingsApi.update({ locale }).catch(() => {});
  },
}));

export function useT() {
  const locale = useI18nStore((s) => s.locale);
  const dict = dictionaries[locale];

  return (key: string, params?: Record<string, string | number>): string => {
    let text = dict[key] ?? key;
    if (params) {
      for (const [k, v] of Object.entries(params)) {
        text = text.replace(new RegExp(`\\{${k}\\}`, 'g'), String(v));
      }
    }
    return text;
  };
}

export function initI18n() {
  // 从 localStorage 读取偏好（store 创建时已完成）
}
