<template>
  <div class="fm-view">
    <button
      class="fm-toggle"
      :disabled="!fmState.ready"
      :aria-label="fmState.playing ? t('fm.pause') : t('fm.play')"
      @click="fmPlayer.toggle"
    >
      <span
        class="mdi fm-toggle__icon"
        :class="fmState.playing ? 'mdi-pause' : 'mdi-play'"
      ></span>
    </button>
    <div class="fm-caption">{{ caption }}</div>
  </div>
</template>

<script setup lang="ts">
// Claude FM — 播放器视图（极简版）。
// 播放器本体是模块级单例（src/fm/player.ts），与视图生命周期解耦：
// 本组件只读共享状态、转发交互；路由切换（组件卸载）不影响播放。
// 底部等宽字体展示「歌手 - 歌曲名」（由后端 fm-meta 事件驱动更新）。
import { computed } from 'vue';
import { t } from '../i18n';
import { fmState, fmPlayer } from '../fm/player';

const caption = computed(() => {
  // 始终显示当前曲目（暂停不清空，避免暂停/播放闪烁）。
  // 元数据未到（title/artist 为空）时返回空串占位。
  if (!fmState.track.title && !fmState.track.artist) return '';
  return `${fmState.track.title} - ${fmState.track.artist}`;
});
</script>

<style scoped>
.fm-view {
  position: relative;
  display: flex;
  align-items: center;
  justify-content: center;
  min-height: calc(100vh - 64px);
}

.fm-toggle {
  width: 96px;
  height: 96px;
  border: none;
  border-radius: var(--md-sys-shape-corner-full);
  background: var(--md-sys-color-primary-container);
  color: var(--md-sys-color-on-primary-container);
  cursor: pointer;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  box-shadow: var(--md-sys-elevation-2);
  transition: background 200ms cubic-bezier(0.2, 0, 0, 1), box-shadow 200ms;
}
.fm-toggle:hover:not(:disabled) {
  background: var(--md-sys-color-primary);
  color: var(--md-sys-color-on-primary);
  box-shadow: var(--md-sys-elevation-3);
}
.fm-toggle:disabled { cursor: default; opacity: 0.85; }
.fm-toggle__icon { font-size: 44px; }

/* 窗口底部等宽字体展示「歌手 - 歌曲名」 */
.fm-caption {
  position: absolute;
  left: 0;
  right: 0;
  bottom: 0;
  text-align: center;
  font-family: 'Roboto Mono', ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 0.8rem;
  color: var(--md-sys-color-on-surface-variant);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  padding: 0 16px;
}
</style>
