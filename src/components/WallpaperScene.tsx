import { useMemo } from 'react';
import { PixelScene } from './PixelScene';
import { useFm } from '@/hooks/useFm';
import { invoke, isTauri } from '@/lib/tauri';
import { cn } from '@/lib/utils';

/**
 * 壁纸模式入口（Rust 动态创建的 "wallpaper" 窗口加载的页面）。
 *
 * 与 ClaudeFmView 共享同一套 FM 事件接线（useFm）+ 引擎权威动画时钟
 * （`fm_scene_t` 采样）：两处画面严格同步。只渲染像素艺术——
 * 无播放/暂停按钮、无歌曲信息；未播放/暂停时同样黑白静止（grayscale）。
 */
export function WallpaperScene() {
  const fm = useFm();
  const sampleT = useMemo(
    () => (isTauri() ? () => invoke<number>('fm_scene_t') : undefined),
    []
  );

  return (
    <div
      className={cn(
        'fixed inset-0 bg-black transition-[filter] duration-700',
        !fm.playing && 'grayscale'
      )}
      aria-hidden
    >
      <PixelScene seed={fm.track.index} playing={fm.playing} sampleT={sampleT} />
    </div>
  );
}
