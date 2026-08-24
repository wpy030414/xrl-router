<template>
  <div class="app-shell">
    <nav class="nav-rail">
      <button
        v-for="item in navItems"
        :key="item.path"
        class="nav-item"
        :class="{ 'nav-item--active': isActive(item.path) }"
        @click="navigateTo(item.path)"
      >
        <span class="nav-item__icon"><MdiIcon :path="item.icon" /></span>
        <span class="nav-label">{{ t(item.labelKey) }}</span>
      </button>
    </nav>

    <main class="app-main">
      <slot />
    </main>
  </div>
</template>

<script setup lang="ts">
import { useRouter, useRoute } from 'vue-router';
import { mdiRadio, mdiCloud, mdiSetMerge, mdiKey, mdiChartBar, mdiCog } from '@mdi/js';
import { t } from '../i18n';
import MdiIcon from './MdiIcon.vue';

const router = useRouter();
const route = useRoute();

const navItems: { path: string; labelKey: string; icon: string }[] = [
  { path: '/fm', labelKey: 'nav.fm', icon: mdiRadio },
  { path: '/providers', labelKey: 'nav.providers', icon: mdiCloud },
  { path: '/combos', labelKey: 'nav.combos', icon: mdiSetMerge },
  { path: '/keys', labelKey: 'nav.keys', icon: mdiKey },
  { path: '/stats', labelKey: 'nav.stats', icon: mdiChartBar },
  { path: '/settings', labelKey: 'nav.settings', icon: mdiCog },
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
  display: flex;
  min-height: 100vh;
  background: var(--md-sys-color-surface);
  color: var(--md-sys-color-on-surface);
}

.nav-rail {
  display: flex;
  flex-direction: column;
  align-items: center;
  flex-shrink: 0;
  width: 84px;
  height: 100vh;
  position: sticky;
  top: 0;
  padding-top: 24px;
  background: var(--md-sys-color-surface);
  border-right: 1px solid var(--md-sys-color-outline-variant);
  box-sizing: border-box;
  overflow-y: auto;
}

.nav-item {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 4px;
  width: 100%;
  padding: 4px 0;
  border: none;
  background: transparent;
  font-family: inherit;
  cursor: pointer;
}

.nav-item__icon {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 40px;
  height: 40px;
  border-radius: var(--md-sys-shape-corner-full);
  color: var(--md-sys-color-on-surface-variant);
  font-size: 24px;
  transition:
    background-color 0.15s ease,
    color 0.15s ease;
}

.nav-item--active .nav-item__icon {
  background: var(--md-sys-color-secondary-container);
  color: var(--md-sys-color-on-secondary-container);
}

.nav-item:hover:not(.nav-item--active) .nav-item__icon {
  background: color-mix(in srgb, var(--md-sys-color-on-surface) 8%, transparent);
}

.nav-label {
  font-size: 0.75rem;
  font-weight: 500;
  line-height: 1rem;
  letter-spacing: 0.03125rem;
  text-align: center;
  color: var(--md-sys-color-on-surface-variant);
}

.nav-item--active .nav-label {
  color: var(--md-sys-color-on-secondary-container);
}

.app-main {
  flex: 1;
  min-width: 0;
  padding: 32px;
  box-sizing: border-box;
  display: grid;
  grid-template-columns: 1fr minmax(0, 880px) 1fr;
}

.app-main > :first-child {
  grid-column: 2;
}

@media (max-width: 480px) {
  .app-shell {
    flex-direction: column;
  }
  .nav-rail {
    position: fixed;
    bottom: 0;
    left: 0;
    right: 0;
    top: auto;
    width: 100%;
    height: auto;
    flex-direction: row;
    justify-content: space-around;
    padding: 4px 4px 8px;
    border-right: none;
    border-top: 1px solid var(--md-sys-color-outline-variant);
    z-index: 100;
  }
  .app-main {
    padding: 16px;
    padding-bottom: 84px;
    display: block;
  }
}
</style>
