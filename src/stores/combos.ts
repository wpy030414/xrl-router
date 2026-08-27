import { create } from 'zustand';
import { combosApi, type Combo } from '@/lib/api';

interface CombosState {
  combos: Combo[];
  loading: boolean;
  fetchCombos: () => Promise<void>;
  createCombo: (data: { name: string; members: string[]; enabled?: boolean }) => Promise<Combo>;
  updateCombo: (id: string, data: { name?: string; members?: string[]; enabled?: boolean }) => Promise<Combo>;
  deleteCombo: (id: string) => Promise<void>;
}

export const useCombosStore = create<CombosState>((set, get) => ({
  combos: [],
  loading: false,

  async fetchCombos() {
    set({ loading: true });
    try {
      const combos = await combosApi.list();
      set({ combos, loading: false });
    } catch (e) {
      set({ loading: false });
      throw e;
    }
  },

  async createCombo(data: { name: string; members: string[]; enabled?: boolean }) {
    const combo = await combosApi.create(data);
    set((state) => ({ combos: [...state.combos, combo] }));
    return combo;
  },

  async updateCombo(id: string, data: { name?: string; members?: string[]; enabled?: boolean }) {
    const combo = await combosApi.update(id, data);
    set((state) => ({
      combos: state.combos.map((c) => (c.id === id ? combo : c)),
    }));
    return combo;
  },

  async deleteCombo(id: string) {
    await combosApi.delete(id);
    set((state) => ({
      combos: state.combos.filter((c) => c.id !== id),
    }));
  },
}));
