import { useCallback, useEffect, useState } from 'react';
import { Play, Pause } from 'lucide-react';
import { useT } from '@/i18n';
import { isTauri, invoke, listen } from '@/lib/tauri';
import { cn } from '@/lib/utils';
import { PixelScene } from '@/components/PixelScene';

interface FmMeta {
  artist: string;
  title: string;
  index: number;
}

interface FmState {
  ready: boolean;
  playing: boolean;
  track: FmMeta;
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
 * 本视图不做 relative 定位：场景与底栏 absolute 锚定到最近的定位祖先
 * （SidebarInset 的 <main>，自带 relative），铺满侧边栏右侧整个可视区；
 * 按钮在文档流里居中，保证页面高度 = 视口（无滚动）。
 */
export function ClaudeFmView() {
  const t = useT();
  const [fmState, setFmState] = useState<FmState>({
    ready: false,
    playing: false,
    track: { artist: '', title: '', index: 0 },
  });

  // Initialize FM: get initial state + listen for Tauri events
  useEffect(() => {
    if (!isTauri()) return;

    let unlisteners: (() => void)[] = [];
    let mounted = true;

    const init = async () => {
      // Get initial state
      try {
        const ps = await invoke<FmMeta & { playing: boolean; ready: boolean }>('fm_get_state');
        if (ps && mounted) {
          setFmState({
            ready: ps.ready,
            playing: ps.playing,
            track: { artist: ps.artist, title: ps.title, index: ps.index },
          });
        }
      } catch {
        // Backend not ready yet
      }

      // Listen for track changes
      const unlistenMeta = await listen<FmMeta>('fm-meta', (payload) => {
        if (!mounted) return;
        setFmState((prev) => ({
          ...prev,
          track: {
            artist: payload.artist,
            title: payload.title,
            index: payload.index,
          },
        }));
      });

      const unlistenReady = await listen<void>('fm-ready', () => {
        if (!mounted) return;
        setFmState((prev) => ({ ...prev, ready: true }));
        invoke('fm_ready').catch(() => {});
      });

      const unlistenStateChanged = await listen<boolean>('fm-state-changed', (payload) => {
        if (!mounted) return;
        setFmState((prev) => ({ ...prev, playing: payload }));
        invoke('fm_set_playing', { playing: payload }).catch(() => {});
      });

      unlisteners = [unlistenMeta, unlistenReady, unlistenStateChanged];
    };

    init();

    return () => {
      mounted = false;
      unlisteners.forEach((fn) => fn());
    };
  }, []);

  const handleToggle = useCallback(async () => {
    if (!isTauri()) return;
    await invoke('fm_toggle');
  }, []);

  const caption = [fmState.track.title, fmState.track.artist].filter(Boolean).join(' - ');

  return (
    <div className="flex min-h-[calc(100vh_-_4rem)] items-center justify-center">
      {/* 像素艺术背景：未播放/暂停时全黑白 + 静止，播放时彩色 + 动画 */}
      <div
        className={cn(
          // duration-700 同时作用于入场 fade-in 与 grayscale 过渡
          'absolute inset-0 animate-in fade-in duration-700 transition-[filter]',
          !fmState.playing && 'grayscale'
        )}
        aria-hidden
      >
        <PixelScene seed={fmState.track.index} playing={fmState.playing} />
      </div>

      {/* 播放按钮（居中） */}
      <button
        type="button"
        aria-label={fmState.playing ? t('fm.pause') : t('fm.play')}
        disabled={!fmState.ready}
        onClick={handleToggle}
        className={cn(
          'relative z-10 flex h-16 w-16 items-center justify-center rounded-full bg-white text-zinc-900 ring-1 ring-black/10',
          'shadow-[0_10px_32px_rgba(0,0,0,0.35)] transition-all duration-300',
          'hover:scale-105 hover:bg-zinc-100',
          'disabled:cursor-default disabled:opacity-40 disabled:hover:scale-100',
          fmState.playing && 'shadow-[0_0_44px_rgba(255,255,255,0.4)]'
        )}
      >
        {fmState.playing ? <Pause className="h-6 w-6" /> : <Play className="ml-0.5 h-6 w-6" />}
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
