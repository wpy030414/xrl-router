import { useEffect, useMemo, useRef } from 'react';
import { generateScene, renderScene, SCENE_W, SCENE_H } from '@/lib/pixelart';

/**
 * 像素画布：按 seed 确定性绘制一幅风景。
 *
 * - playing=true：彩色 + 持续动画（星星闪烁 / 水面波光 / 流星 / 云层缓漂 /
 *   飞鸟掠空 / 树冠轻摆 / 日月光晕呼吸），5fps 低帧率重绘。
 * - playing=false（未播放或暂停）：静止单帧，时间冻结——画面内容与
 *   配色交给父级的 grayscale 滤镜负责（黑白 + 静止 = 待机画面）。
 *
 * 动画时钟：
 * - 提供 `sampleT`（Tauri 环境，主窗口/壁纸窗口都传 `invoke('fm_scene_t')`）
 *   时以**引擎权威时钟**为准——两个窗口同一时刻渲染同一帧，天然同步；
 *   采样失败时按真实流逝本地累加兜底（调度器节流也不跳变）。
 * - 未提供（浏览器调试）时维持本地时钟：暂停冻结、恢复续走。
 *
 * 逻辑分辨率 160×90，放大铺满容器（object-cover），由 browser 像素化
 * 渲染防平滑。
 */
export function PixelScene({
  seed,
  playing,
  sampleT,
}: {
  seed: number;
  playing: boolean;
  sampleT?: () => Promise<number | null>;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  // 动画时钟：播放中按采样/流逝累加；暂停冻结、恢复续走。跨 effect 保留。
  const clockRef = useRef(0);
  const scene = useMemo(() => generateScene(seed), [seed]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;
    let disposed = false;

    if (!playing) {
      // 未播放 / 暂停：静止单帧。挂载时先采样引擎时钟——暂停期间挂载
      // （如壁纸重建）也能与另一窗口同相位。采样失败则停留在本地时钟值。
      const renderFrame = () => {
        if (!disposed) renderScene(ctx, scene, clockRef.current);
      };
      if (sampleT) {
        void sampleT()
          .then((t) => {
            if (typeof t === 'number' && Number.isFinite(t)) clockRef.current = t;
          })
          .finally(renderFrame);
      } else {
        renderFrame();
      }
      return () => {
        disposed = true;
      };
    }

    // 播放中：5fps 持续动画。
    let last = performance.now();
    const draw = async () => {
      const now = performance.now();
      const dt = (now - last) / 1000;
      last = now;
      if (sampleT) {
        try {
          const t = await sampleT();
          if (typeof t === 'number' && Number.isFinite(t)) clockRef.current = t;
          else clockRef.current += dt; // invoke 失败 → 本地兜底
        } catch {
          clockRef.current += dt;
        }
      } else {
        clockRef.current += dt;
      }
      if (disposed) return;
      renderScene(ctx, scene, clockRef.current);
    };
    void draw();
    const timer = window.setInterval(() => void draw(), 200);
    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  }, [scene, playing, sampleT]);

  return (
    <canvas
      ref={canvasRef}
      width={SCENE_W}
      height={SCENE_H}
      aria-hidden
      className="h-full w-full object-cover"
      style={{ imageRendering: 'pixelated' }}
    />
  );
}
