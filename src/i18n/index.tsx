import { create } from 'zustand';
import zhCN from './zh-CN';
import en from './en';
import { settingsApi } from '@/lib/api';
import { invoke, isTauri } from '@/lib/tauri';

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
    // Tauri：set_locale 已内含 DB 持久化 + 托盘文本更新，无需重复写 settings API
    // LAN 浏览器：invoke 不可用，回退到 HTTP settings API 持久化
    if (isTauri()) {
      invoke('set_locale', { locale }).catch(() => {});
    } else {
      settingsApi.update({ locale }).catch(() => {});
    }
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
