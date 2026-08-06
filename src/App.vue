<template>
  <ConnectionStatus />
  <AppShell>
    <router-view />
  </AppShell>
  <PluginRegisterDialog ref="pluginDialogRef" />
</template>

<script setup lang="ts">
import { ref, onMounted } from 'vue';
import { listen } from '@tauri-apps/api/event';
import AppShell from './components/AppShell.vue';
import ConnectionStatus from './components/ConnectionStatus.vue';
import PluginRegisterDialog from './components/PluginRegisterDialog.vue';
import { initTraySync } from './fm/player';

const pluginDialogRef = ref<InstanceType<typeof PluginRegisterDialog> | null>(null);

onMounted(async () => {
  // Claude FM 托盘菜单联动：启动即绑定，播放状态与托盘勾选双向同步
  initTraySync();

  // 监听插件注册事件
  await listen('plugin-register', (event: any) => {
    console.log('[Plugin] Register event:', event.payload);
    pluginDialogRef.value?.show(event.payload);
  });

  // 监听插件离线事件
  await listen('plugin-offline', (event: any) => {
    console.log('[Plugin] Offline:', event.payload);
    // TODO: 可选显示通知
  });

  // 监听插件上线事件
  await listen('plugin-online', (event: any) => {
    console.log('[Plugin] Online:', event.payload);
    // TODO: 可选显示通知
  });
});
</script>