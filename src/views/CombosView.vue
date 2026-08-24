<template>
  <div class="page">
    <div class="page__header">
      <h2 class="md-typescale-headline-medium page__title">{{ t('combos.title') }}</h2>
      <md-filled-button @click="$router.push('/combos/new')">
        <MdiIcon :path="mdiPlus" slot="icon" />
        {{ t('combos.add') }}
      </md-filled-button>
    </div>

    <div v-if="loading" class="empty-state">
      <md-circular-progress indeterminate></md-circular-progress>
    </div>

    <div v-else-if="!combos.length" class="empty-state">
      <MdiIcon :path="mdiSetMerge" class="empty-state__icon" />
      <p class="md-typescale-body-large">{{ t('common.empty') }}</p>
    </div>

    <div v-else>
      <div class="card-grid">
        <article v-for="c in combos" :key="c.id" class="card">
          <span class="card__avatar"><MdiIcon :path="mdiSetMerge" /></span>
          <div class="card__body">
            <h3 class="md-typescale-title-medium card__name">
              <span class="card__name-text" :title="c.name">{{ c.name }}</span>
              <span
                v-if="!c.enabled"
                class="card__disabled"
              >{{ t('combos.disabled') }}</span>
            </h3>
            <div v-if="c.members.length" class="card__members">
              <span
                v-for="(m, i) in c.members"
                :key="m"
                class="member-chip"
                :title="t('combos.member_order', { pos: i + 1 })"
              >
                {{ i + 1 }}. {{ m }}
              </span>
            </div>
          </div>
          <div class="card__actions">
            <md-icon-button
              :id="'combo-btn-' + c.id"
              class="card__more-btn"
              @click="toggleMenu(c)"
            >
              <MdiIcon :path="mdiDotsVertical" />
            </md-icon-button>
          </div>
        </article>
      </div>
    </div>

    <!-- Shared action menu (single instance, re-anchors per card) -->
    <md-menu
      :open="menuOpen != null"
      :anchor="menuAnchor"
      positioning="fixed"
      @closed="menuOpen = null"
    >
      <md-menu-item @click="editFromMenu">
        <MdiIcon :path="mdiPencilOutline" /> {{ t('common.edit') }}
      </md-menu-item>
      <md-menu-item class="menu-item--danger" @click="deleteFromMenu">
        <MdiIcon :path="mdiDeleteOutline" /> {{ t('common.delete') }}
      </md-menu-item>
    </md-menu>

    <md-dialog :open="deleteOpen" @close="deleteOpen = false">
      <div slot="headline">{{ t('combos.delete_title') }}</div>
      <div slot="content" class="form">
        <p class="md-typescale-body-medium">{{ t('combos.delete_confirm', { name: deleteTarget?.name || '' }) }}</p>
      </div>
      <div slot="actions">
        <md-text-button @click="deleteOpen = false">{{ t('common.cancel') }}</md-text-button>
        <md-text-button class="confirm-del" @click="confirmDelete">{{ t('combos.delete_confirm_btn') }}</md-text-button>
      </div>
    </md-dialog>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted } from 'vue';
import '@material/web/iconbutton/icon-button.js';
import '@material/web/menu/menu.js';
import '@material/web/menu/menu-item.js';
import '@material/web/progress/circular-progress.js';
import { useRouter } from 'vue-router';
import { mdiPlus, mdiSetMerge, mdiDotsVertical, mdiPencilOutline, mdiDeleteOutline } from '@mdi/js';
import { combosApi, type Combo } from '../api';
import { t } from '../i18n';
import MdiIcon from '../components/MdiIcon.vue';

const router = useRouter();

const combos = ref<Combo[]>([]);
const loading = ref(true);
const deleteOpen = ref(false);
const deleteTarget = ref<Combo | null>(null);
const menuOpen = ref<string | null>(null);
const menuAnchor = ref('');
const menuTarget = ref<Combo | null>(null);

function toggleMenu(c: Combo) {
  if (menuOpen.value === c.id) {
    menuOpen.value = null;
  } else {
    menuTarget.value = c;
    menuAnchor.value = 'combo-btn-' + c.id;
    menuOpen.value = c.id;
  }
}

function editFromMenu() {
  if (menuTarget.value) {
    router.push(`/combos/${menuTarget.value.id}/edit`);
  }
}

function deleteFromMenu() {
  if (menuTarget.value) {
    deleteTarget.value = menuTarget.value;
    deleteOpen.value = true;
  }
}

async function confirmDelete() {
  if (!deleteTarget.value) return;
  try {
    await combosApi.delete(deleteTarget.value.id);
  } catch (e: any) {
    alert(t('combos.delete_failed', { msg: e?.message || e }));
  }
  deleteOpen.value = false;
  deleteTarget.value = null;
  await fetchCombos();
}

async function fetchCombos() {
  loading.value = true;
  try {
    combos.value = await combosApi.list();
  } catch {
    // 请求失败保持空列表
  } finally {
    loading.value = false;
  }
}

onMounted(fetchCombos);
</script>

<style scoped>
.page__header { display: flex; justify-content: space-between; align-items: flex-start; margin-bottom: 24px; gap: 16px; flex-wrap: wrap; }
.page__title { margin: 0; }
.empty-state { display: flex; flex-direction: column; align-items: center; gap: 8px; padding: 64px 24px; text-align: center; }
.empty-state__icon { font-size: 48px; color: var(--md-sys-color-on-surface-variant); }
.card-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(320px, 1fr)); gap: 16px; }
.card {
  background: var(--md-sys-color-surface-container-low); border-radius: var(--md-sys-shape-corner-medium);
  padding: 20px; display: grid; grid-template-columns: 44px 1fr auto; gap: 12px; align-items: start;
}
.card__avatar {
  width: 44px; height: 44px; border-radius: var(--md-sys-shape-corner-full);
  display: flex; align-items: center; justify-content: center;
  background: var(--md-sys-color-tertiary-container); color: var(--md-sys-color-on-tertiary-container);
}
.card__avatar svg { width: 22px; height: 22px; }
.card__body { display: flex; flex-direction: column; gap: 6px; min-width: 0; }
.card__name { margin: 0; display: flex; align-items: center; gap: 6px; min-width: 0; }
.card__name-text { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
.card__disabled {
  display: inline-flex; align-items: center; padding: 1px 8px; border-radius: var(--md-sys-shape-corner-full);
  background: var(--md-sys-color-surface-container-high);
  color: var(--md-sys-color-on-surface-variant);
  font-size: 0.75rem; font-weight: 500;
}
.card__members { display: flex; flex-wrap: wrap; gap: 4px; }
.member-chip {
  display: inline-flex; align-items: center; padding: 2px 10px;
  border-radius: var(--md-sys-shape-corner-full);
  background: var(--md-sys-color-secondary-container);
  color: var(--md-sys-color-on-secondary-container);
  font-size: 0.75rem; font-variant-numeric: tabular-nums;
}
.card__actions { display: flex; justify-content: flex-end; position: relative; }
.card__more-btn { --md-icon-button-icon-size: 20px; width: 36px; height: 36px; }
.form { min-width: 300px; }
.confirm-del { color: var(--md-sys-color-error); }
</style>

<!-- md-menu teleports to document root, so its styles must not be scoped -->
<style>
.menu-item--danger { --md-menu-item-label-text-color: var(--md-sys-color-error); color: var(--md-sys-color-error); }
</style>
