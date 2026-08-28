import { useCallback, useEffect, useMemo, useState } from 'react';
import { Play, Pause } from 'lucide-react';
import { useT } from '@/i18n';
import { useFm } from '@/hooks/useFm';
import { invoke, isTauri } from '@/lib/tauri';
import { cn } from '@/lib/utils';
import { PixelScene } from '@/components/PixelScene';
import {
  ContextMenu,
  ContextMenuCheckboxItem,
  ContextMenuContent,
  ContextMenuTrigger,
} from '@/components/ui/context-menu';

interface WallpaperInfo {
  enabled: boolean;
  supported: boolean;
}

/**
 * Claude FM — 极简播放视图。
 *
 * <main> 背景常驻一幅像素风景（seed = 曲目索引，每首歌专属画面，切歌即换画）：
 * - 未播放 / 暂停：黑白静止（grayscale + 冻结帧），像待机画面；
 * - 播放中：彩色 + 持续动画。
 * 窗口底部以等宽字体展示当前「标题 - 歌手」。暂停只是静音（radio 模式），
 * 曲目信息照常跟随轮播。
 *
 * 右击像素艺术画面可勾选「设置为桌面背景」：Rust 侧动态创建 wallppaper
 * 窗口挂到桌面壁纸层，两处画面共用引擎权威时钟（`fm_scene_t`），严格同步；
 * 壁纸画面不显示播放按钮与歌曲信息。
 *
 * 本视图不做 relative 定位：场景与底栏 absolute 锚定到最近的定位祖先
 * （SidebarInset 的 <main>，自带 relative），铺满侧边栏右侧整个可视区；
 * 按钮在文档流里居中，保证页面高度 = 视口（无滚动）。
 */
export function ClaudeFmView() {
  const t = useT();
  const fm = useFm();
  const [wallpaper, setWallpaper] = useState<WallpaperInfo>({
    enabled: false,
    supported: false,
  });

  // 桌面壁纸劫持状态（勾选态 + 平台支持情况，来自后端）
  useEffect(() => {
    if (!isTauri()) return;
    let mounted = true;
    void invoke<WallpaperInfo>('wallpaper_get_state').then((s) => {
      if (s && mounted) setWallpaper(s);
    });
    return () => {
      mounted = false;
    };
  }, []);

  // 动画时钟采样（两窗口统一用引擎权威值）；非 Tauri（浏览器调试）为 undefined
  const sampleT = useMemo(
    () => (isTauri() ? () => invoke<number>('fm_scene_t') : undefined),
    []
  );

  const handleToggle = useCallback(async () => {
    if (!isTauri()) return;
    await invoke('fm_toggle');
  }, []);

  const handleWallpaperToggle = useCallback(async (checked: boolean) => {
    if (!isTauri()) return;
    setWallpaper((prev) => ({ ...prev, enabled: checked })); // 乐观更新
    // 直连 @tauri-apps/api/core：失败时拿到真实错误（lib/tauri.ts 的 invoke 会吞掉错误信息）
    try {
      const { invoke: invokeCore } = await import('@tauri-apps/api/core');
      const ok = await invokeCore<boolean>('wallpaper_set', { enabled: checked });
      setWallpaper((prev) => ({ ...prev, enabled: !!ok }));
    } catch (e) {
      console.error('[wallpaper] wallpaper_set failed:', e);
      window.alert(`wallpaper_set failed:\n${e}`);
      setWallpaper((prev) => ({ ...prev, enabled: !checked })); // 回滚
    }
  }, []);

  const caption = [fm.track.title, fm.track.artist].filter(Boolean).join(' - ');

  return (
    <div className="flex min-h-[calc(100vh_-_4rem)] items-center justify-center">
      {/* 像素艺术背景：未播放/暂停时全黑白 + 静止，播放时彩色 + 动画。
          右击（ContextMenu）可勾选「设置为桌面背景」——桌面被劫持为与
          应用内严格同步的像素艺术动画（无按钮、无歌曲信息）。 */}
      <ContextMenu>
        <ContextMenuTrigger asChild>
          <div
            className={cn(
              // duration-700 同时作用于入场 fade-in 与 grayscale 过渡
              'absolute inset-0 animate-in fade-in duration-700 transition-[filter]',
              !fm.playing && 'grayscale'
            )}
            aria-hidden
          >
            <PixelScene seed={fm.track.index} playing={fm.playing} sampleT={sampleT} />
          </div>
        </ContextMenuTrigger>
        <ContextMenuContent>
          {wallpaper.supported && (
            <ContextMenuCheckboxItem
              checked={wallpaper.enabled}
              onCheckedChange={handleWallpaperToggle}
            >
              {wallpaper.enabled ? t('fm.unsetWallpaper') : t('fm.setWallpaper')}
            </ContextMenuCheckboxItem>
          )}
        </ContextMenuContent>
      </ContextMenu>

      {/* 播放按钮（居中） */}
      <button
        type="button"
        aria-label={fm.playing ? t('fm.pause') : t('fm.play')}
        disabled={!fm.ready}
        onClick={handleToggle}
        className={cn(
          'relative z-10 flex h-16 w-16 items-center justify-center rounded-full bg-white text-zinc-900 ring-1 ring-black/10',
          'shadow-[0_10px_32px_rgba(0,0,0,0.35)] transition-all duration-300',
          'hover:scale-105 hover:bg-zinc-100',
          'disabled:cursor-default disabled:opacity-40 disabled:hover:scale-100',
          fm.playing && 'shadow-[0_0_44px_rgba(255,255,255,0.4)]'
        )}
      >
        {fm.playing ? <Pause className="h-6 w-6" /> : <Play className="ml-0.5 h-6 w-6" />}
      </button>

      {/* 窗口底部：当前「标题 - 歌手」 */}
      {caption && (
        <div className="absolute inset-x-0 bottom-7 z-10 flex justify-center px-6">
          <span className="max-w-full truncate rounded-full bg-black/35 px-4 py-1.5 font-mono text-xs text-white/90 backdrop-blur-sm sm:text-sm">
            {caption}
          </span>
        </div>
      )}
    </div>
  );
}

export default ClaudeFmView;
