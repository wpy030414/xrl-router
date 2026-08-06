<template>
  <div class="app-shell">
    <aside class="nav-drawer">
      <nav class="nav-drawer__items">
        <button
          v-for="item in navItems"
          :key="item.path"
          class="nav-item"
          :class="{ 'nav-item--active': isActive(item.path) }"
          @click="navigateTo(item.path)"
        >
          <span class="nav-item__icon mdi" :class="item.icon"></span>
          <span class="nav-item__label md-typescale-label-large">{{ t(item.labelKey) }}</span>
        </button>
      </nav>
    </aside>

    <main class="app-main">
      <slot />
    </main>
  </div>
</template>

<script setup lang="ts">
import { useRouter, useRoute } from 'vue-router';
import { t } from '../i18n';

const router = useRouter();
const route = useRoute();

const navItems: { path: string; labelKey: string; icon: string }[] = [
  { path: '/fm', labelKey: 'nav.fm', icon: 'mdi-radio' },
  { path: '/providers', labelKey: 'nav.providers', icon: 'mdi-cloud' },
  { path: '/keys', labelKey: 'nav.keys', icon: 'mdi-key' },
  { path: '/stats', labelKey: 'nav.stats', icon: 'mdi-chart-bar' },
  { path: '/settings', labelKey: 'nav.settings', icon: 'mdi-cog' },
];

function isActive(path: string) {
  return route.path.startsWith(path);
}

function navigateTo(path: string) {
  router.push(path);
}
</script>

<style scoped>
.app-shell {
  display: grid;
  grid-template-columns: 280px 1fr;
  grid-template-rows: 1fr;
  min-height: 100vh;
  background: var(--md-sys-color-surface);
  color: var(--md-sys-color-on-surface);
}

.nav-drawer {
  background: var(--md-sys-color-surface-container-low);
  border-right: 1px solid var(--md-sys-color-outline-variant);
  padding: 32px 12px 16px;
  display: flex;
  flex-direction: column;
  gap: 4px;
  position: sticky;
  top: 0;
  height: 100vh;
  overflow-y: auto;
  box-sizing: border-box;
}

.nav-drawer__items {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.nav-item {
  display: flex;
  align-items: center;
  gap: 12px;
  width: 100%;
  padding: 0 16px;
  height: 56px;
  border: none;
  background: transparent;
  color: var(--md-sys-color-on-surface-variant);
  border-radius: var(--md-sys-shape-corner-full);
  cursor: pointer;
  font-family: inherit;
  font-size: inherit;
  transition: background 200ms cubic-bezier(0.2, 0, 0, 1), color 200ms;
}

.nav-item:hover {
  background: var(--md-sys-color-surface-container-high);
}

.nav-item--active {
  background: var(--md-sys-color-secondary-container);
  color: var(--md-sys-color-on-secondary-container);
  font-weight: 500;
}

.nav-item__icon { font-size: 24px; }

.app-main {
  padding: 32px;
  box-sizing: border-box;
  display: grid;
  grid-template-columns: 1fr minmax(0, 880px) 1fr;
}

.app-main > :first-child {
  grid-column: 2;
}

@media (max-width: 840px) {
  .app-shell {
    grid-template-columns: 76px 1fr;
  }
  .app-main {
    display: block;
  }
  .nav-drawer {
    padding: 20px 6px 12px;
  }
  .nav-item__label { display: none; }
  .nav-item { justify-content: center; padding: 0; }
  .app-main {
    padding: 24px 16px;
  }
}

@media (max-width: 480px) {
  .app-shell {
    grid-template-columns: 1fr;
    grid-template-rows: 1fr auto;
  }
  .nav-drawer {
    position: fixed;
    bottom: 0;
    left: 0;
    right: 0;
    top: auto;
    width: 100%;
    height: auto;
    flex-direction: row;
    justify-content: space-around;
    padding: 8px 4px;
    border-right: none;
    border-top: 1px solid var(--md-sys-color-outline-variant);
    z-index: 100;
  }
  .nav-drawer__items {
    flex-direction: row;
    width: 100%;
    justify-content: space-around;
  }
  .nav-item {
    flex-direction: column;
    width: auto;
    height: 56px;
    padding: 4px 12px;
    gap: 2px;
    font-size: 10px;
  }
  .nav-item__label {
    display: block;
    font-size: 11px;
  }
  .app-main {
    padding: 16px;
    padding-bottom: 80px;
  }
}
</style>
