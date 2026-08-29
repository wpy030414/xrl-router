import { useEffect, useState } from 'react';
import { invoke, isTauri, listen } from '@/lib/tauri';

export interface FmMeta {
  artist: string;
  title: string;
  index: number;
}

export interface FmState {
  ready: boolean;
  playing: boolean;
  track: FmMeta;
}

/**
 * Claude FM 播放状态：初始化拉取 + 订阅后端广播事件。
 *
 * 主窗口（FmView）与壁纸窗口（WallpaperScene）共用同一套接线——
 * 后端事件是进程级广播，两窗口收到同样的种子/播放态，配合 `fm_scene_t`
 * 引擎权威时钟，渲染完全一致。
 */
export function useFm(): FmState {
  const [fmState, setFmState] = useState<FmState>({
    ready: false,
    playing: false,
    track: { artist: '', title: '', index: 0 },
  });

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

  return fmState;
}
