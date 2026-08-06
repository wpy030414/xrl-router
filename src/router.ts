import type { RouteRecordRaw } from 'vue-router';

export const routes: RouteRecordRaw[] = [
  { path: '/fm', name: 'ClaudeFm', component: () => import('./views/ClaudeFmView.vue') },
  { path: '/providers', name: 'Providers', component: () => import('./views/ProvidersView.vue') },
  { path: '/providers/new', name: 'ProviderNew', component: () => import('./views/ProviderNewView.vue') },
  { path: '/providers/:id/edit', name: 'ProviderEdit', component: () => import('./views/ProviderNewView.vue') },
  { path: '/keys', name: 'Keys', component: () => import('./views/KeysView.vue') },
  { path: '/stats', name: 'Stats', component: () => import('./views/StatsView.vue') },
  { path: '/settings', name: 'Settings', component: () => import('./views/SettingsView.vue') },
  { path: '/', redirect: '/providers' },
];