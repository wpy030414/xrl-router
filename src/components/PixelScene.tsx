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
 * 动画时钟只在播放时累加：暂停期间冻结，恢复播放从冻结点无缝续走，
 * 不会出现云朵/飞鸟跳位的现象。逻辑分辨率 160×90，放大铺满容器
 * （object-cover），由 browser 像素化渲染防平滑。
 */
export function PixelScene({ seed, playing }: { seed: number; playing: boolean }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  // 动画时钟：仅播放时按真实流逝累加；暂停冻结、恢复续走。
  const clockRef = useRef(0);
  const scene = useMemo(() => generateScene(seed), [seed]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    if (!playing) {
      // 未播放 / 暂停：静止单帧（时钟停在暂停时刻；切歌换 seed 也只重绘一帧）
      renderScene(ctx, scene, clockRef.current);
      return;
    }

    // 播放中：5fps 持续动画，时钟按真实流逝累加（后台节流也不会跳变）
    let last = performance.now();
    const draw = () => {
      const now = performance.now();
      clockRef.current += (now - last) / 1000;
      last = now;
      renderScene(ctx, scene, clockRef.current);
    };
    draw();
    const timer = window.setInterval(draw, 200);
    return () => window.clearInterval(timer);
  }, [scene, playing]);

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
