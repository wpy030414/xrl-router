// Claude FM 式像素风景生成器（确定性）。
//
// 同一 seed 恒得同一幅画（seed 取曲目索引 → 每首歌专属画面，切歌即换画）；
// 画面带轻量动画：星星闪烁、水面波光、流星周期性划过。
// 全部绘制在 160×90 的逻辑画布上，由 CSS image-rendering: pixelated 放大
// 铺满 <main>，得到低分辨率像素栅格的质感。

/** 逻辑画布尺寸（保持 16:9，放大渲染）。 */
export const SCENE_W = 160;
export const SCENE_H = 90;

/** 地平线 y：上方为天空，下方为水面/草地。 */
const HORIZON = 60;

/** 山脊的台阶宽度（像素阶梯感）。 */
const CHUNK = 8;

/** 确定性 PRNG（mulberry32）：seed 相同则序列相同。 */
function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** 四种氛围配色：夜 / 暮 / 晓 / 昼，由 seed 决定。 */
interface Mood {
  id: 'night' | 'dusk' | 'dawn' | 'day';
  sky: string[]; // 顶部 → 地平线，8 段
  starDensity: number; // 0~1
  moon: { r: number; color: string } | null;
  sun: { r: number; color: string } | null;
  clouds: string[];
  bird: boolean;
  birdColor: string;
  ridges: string[]; // 后 → 前 三层
  lifts: [number, number, number]; // 三层山脊相对地平线的抬高
  amps: [number, number, number]; // 三层山脊的高度噪声幅度
  water: { base: string; band: string; shine: string } | null;
  grass: { base: string; bands: string[] } | null;
  treeColor: string | null;
}

const MOODS: Mood[] = [
  {
    id: 'night',
    sky: ['#04061e', '#090b30', '#101345', '#181c58', '#222a6c', '#2e3a7e', '#3c4a8e', '#4c5c9c'],
    starDensity: 1,
    moon: { r: 6, color: '#e8e4cc' },
    sun: null,
    clouds: ['#323d72', '#2a345f'],
    bird: false,
    birdColor: '',
    ridges: ['#1e2558', '#131a44', '#0c1130'],
    lifts: [24, 15, 7],
    amps: [14, 10, 6],
    water: { base: '#0a0e2a', band: '#121843', shine: '#6c7cd0' },
    grass: null,
    treeColor: null,
  },
  {
    id: 'dusk',
    sky: ['#140c31', '#221146', '#35195c', '#4c2370', '#68307a', '#8a4476', '#aa5f66', '#c97f57'],
    starDensity: 0.28,
    moon: { r: 3, color: '#f3ecd8' },
    sun: { r: 5, color: '#ffc47d' },
    clouds: ['#95518b', '#7f437a', '#a75a80'],
    bird: true,
    birdColor: '#2e1746',
    ridges: ['#52296e', '#3a1c52', '#260f38'],
    lifts: [13, 8, 4],
    amps: [8, 6, 3.5],
    water: { base: '#2d1a50', band: '#41246a', shine: '#ffb877' },
    grass: null,
    treeColor: null,
  },
  {
    id: 'dawn',
    // 钢蓝 → 薄荷 → 淡玫瑰色地平线：清晨的粉紫朝霞，与暮色的橙紫区隔
    sky: ['#31527c', '#44688e', '#5a81a2', '#749cb4', '#92bac2', '#b3d2cc', '#e3d6c6', '#f6e4d4'],
    starDensity: 0.22,
    moon: null,
    sun: { r: 4, color: '#ffdca6' },
    clouds: ['#c4d6c8', '#a8c2b6'],
    bird: true,
    birdColor: '#2c4a5a',
    ridges: ['#48647e', '#355060', '#233848'],
    lifts: [13, 8, 4],
    amps: [8, 6, 3.5],
    water: { base: '#4b6580', band: '#5c7a97', shine: '#ffe8c4' },
    grass: null,
    treeColor: null,
  },
  {
    id: 'day',
    sky: ['#3170b8', '#4a86cc', '#639be0', '#7fb0e8', '#9cc2ee', '#b9d4f4', '#d1e2f8', '#e4edfa'],
    starDensity: 0,
    moon: null,
    sun: { r: 6, color: '#ffeaa9' },
    clouds: ['#ffffff', '#dde9f7'],
    bird: true,
    birdColor: '#1d3552',
    ridges: ['#3e7c5c', '#2f6349', '#214a37'],
    lifts: [22, 13, 6],
    amps: [12, 9, 5],
    water: null,
    grass: { base: '#2e5c40', bands: ['#3a6c4d', '#44775a'] },
    treeColor: '#1b402f',
  },
];

// ── 场景数据 ────────────────────────────────────────────────────────────────

interface Star { x: number; y: number; s: number; phase: number; speed: number }
/** 云：speed 为水平漂移速度（px/s，同风向东/西）。 */
interface Cloud { x: number; y: number; len: number; color: string; bumps: { x: number; len: number }[]; speed: number }
interface Ridge { base: number; heights: number[]; color: string }
/** 树：phase 用于树冠随微风摆动。 */
interface Tree { x: number; base: number; h: number; phase: number }
interface Shimmer { x: number; phase: number }
interface Reflect { x: number; color: string; phase: number; offs: number[] }
/** 鸟：speed 为横向速度，bob 为上下浮动的相位。 */
interface Bird { x: number; y: number; speed: number; bob: number }

export interface PixelScene {
  sky: string[];
  stars: Star[];
  moon: { x: number; y: number; r: number; color: string; phase: number } | null;
  sun: { x: number; y: number; r: number; color: string; phase: number } | null;
  clouds: Cloud[];
  ridges: Ridge[];
  water: {
    y: number; base: string; band: string; shine: string;
    shimmer: Shimmer[]; reflect: Reflect | null;
  } | null;
  grass: { y: number; base: string; bands: string[] } | null;
  trees: Tree[];
  treeColor: string;
  birds: Bird[];
  birdColor: string;
  shooting: { x: number; y: number } | null; // 夜：流星起点
}

/** 由 seed 生成确定性场景（不依赖时间）。 */
export function generateScene(seed: number): PixelScene {
  const mood = MOODS[((seed % MOODS.length) + MOODS.length) % MOODS.length];
  const rnd = mulberry32((seed >>> 0) * 2654435761 + 0x9e3779b9);

  // 星星
  const stars: Star[] = [];
  const starCount = Math.round(SCENE_W * 0.014 * mood.starDensity);
  for (let i = 0; i < starCount; i++) {
    stars.push({
      x: Math.floor(rnd() * SCENE_W),
      y: Math.floor(rnd() * (HORIZON - 20)),
      s: rnd() < 0.15 ? 2 : 1,
      phase: rnd() * Math.PI * 2,
      speed: 0.6 + rnd() * 1.4,
    });
  }

  // 月亮（夜 / 暮）：phase 驱动光晕呼吸
  const moon = mood.moon
    ? { x: 24 + rnd() * (SCENE_W - 76), y: 8 + rnd() * 12, r: mood.moon.r, color: mood.moon.color, phase: rnd() * Math.PI * 2 }
    : null;

  // 太阳：暮色低掛地平线，晓 / 昼掛天上
  const sun = mood.sun
    ? mood.id === 'dusk'
      ? { x: 20 + rnd() * (SCENE_W - 60), y: HORIZON - 9 - rnd() * 3, r: mood.sun.r, color: mood.sun.color, phase: rnd() * Math.PI * 2 }
      : { x: 20 + rnd() * (SCENE_W - 60), y: 6 + rnd() * 10, r: mood.sun.r, color: mood.sun.color, phase: rnd() * Math.PI * 2 }
    : null;

  // 云：主体两行 + 顶部预生成成簇凸起（连续小段，避免散点观感）。
  // 云端 speed：全场景同一风向，缓慢漂移（0.2~0.75 px/s，横穿约 4~13 分钟）。
  const clouds: Cloud[] = [];
  const cloudCount = 2 + Math.floor(rnd() * 3);
  const wind = rnd() < 0.5 ? -1 : 1;
  for (let i = 0; i < cloudCount; i++) {
    const len = 14 + Math.floor(rnd() * 14);
    const bumps: { x: number; len: number }[] = [];
    let bx = 1 + Math.floor(rnd() * 4);
    while (bx < len - 4) {
      const bl = 2 + Math.floor(rnd() * 5);
      bumps.push({ x: bx, len: bl });
      bx += bl + 2 + Math.floor(rnd() * 5);
    }
    clouds.push({
      x: Math.floor(rnd() * SCENE_W) - 10,
      y: 8 + Math.floor(rnd() * 26),
      len,
      color: mood.clouds[i % mood.clouds.length],
      bumps,
      speed: wind * (0.2 + rnd() * 0.55),
    });
  }

  // 三层山脊（后 → 前）：每个 CHUNK 宽一段、高度恒定，形成像素阶梯剪影。
  // 暮/晓的山脊刻意压低（lifts/amps 更小），保证近地平线的太阳可见。
  const ridges: Ridge[] = [];
  for (let i = 0; i < 3; i++) {
    const amp = mood.amps[i] * (0.7 + rnd() * 0.6);
    const chunks = Math.ceil(SCENE_W / CHUNK) + 1;
    // 随机游走 + 邻域平滑：起伏连绵，避免单块突刺造成的「断崖/浮块」
    const raw: number[] = [];
    let h = rnd() * amp * 0.5;
    for (let c = 0; c < chunks; c++) {
      h += (rnd() - 0.5) * amp * 0.5;
      h = Math.max(0, Math.min(amp, h));
      raw.push(h);
    }
    const heights = raw.map((v, c) => {
      const prev = raw[Math.max(0, c - 1)];
      const next = raw[Math.min(raw.length - 1, c + 1)];
      return (prev + 2 * v + next) / 4;
    });
    ridges.push({
      base: HORIZON - mood.lifts[i] - rnd() * 5,
      heights,
      color: mood.ridges[i],
    });
  }

  // 水面（夜 / 暮 / 晓）
  const lit = moon && sun ? (sun.r >= moon.r ? sun : moon) : (moon ?? sun);
  const water = mood.water
    ? {
        y: HORIZON + 2,
        base: mood.water.base,
        band: mood.water.band,
        shine: mood.water.shine,
        shimmer: Array.from({ length: 26 }, () => ({
          x: Math.floor(rnd() * SCENE_W),
          phase: rnd() * Math.PI * 2,
        })),
        reflect: lit
          ? {
              x: lit.x,
              color: lit.color,
              phase: rnd() * Math.PI * 2,
              offs: Array.from({ length: 7 }, () => Math.floor(rnd() * 5) - 2),
            }
          : null,
      }
    : null;

  // 草地（昼）
  const grass = mood.grass ? { y: HORIZON + 2, base: mood.grass.base, bands: mood.grass.bands } : null;

  // 前景像素树（昼）：phase 驱动树冠随微风摆动
  const trees: Tree[] = [];
  if (grass) {
    const n = 4 + Math.floor(rnd() * 4);
    for (let i = 0; i < n; i++) {
      trees.push({
        x: Math.floor(rnd() * SCENE_W),
        base: HORIZON + 2 + Math.floor(rnd() * 6),
        h: 9 + Math.floor(rnd() * 9),
        phase: rnd() * Math.PI * 2,
      });
    }
  }

  // 飞鸟：横穿天际 + 上下浮沉（bob）
  const birds: Bird[] = [];
  if (mood.bird) {
    const n = 1 + Math.floor(rnd() * 2);
    for (let i = 0; i < n; i++) {
      birds.push({
        x: 12 + Math.floor(rnd() * (SCENE_W - 40)),
        y: 10 + Math.floor(rnd() * 22),
        speed: (rnd() < 0.6 ? 1 : -1) * (6 + rnd() * 8),
        bob: rnd() * Math.PI * 2,
      });
    }
  }

  return {
    sky: mood.sky,
    stars,
    moon,
    sun,
    clouds,
    ridges,
    water,
    grass,
    trees,
    treeColor: mood.treeColor ?? '',
    birds,
    birdColor: mood.birdColor,
    shooting: mood.id === 'night' ? { x: Math.floor(rnd() * SCENE_W * 0.7), y: 4 + Math.floor(rnd() * 8) } : null,
  };
}

// ── 渲染 ────────────────────────────────────────────────────────────────────

/** 像素圆盘（日/月）+ 一圈呼吸光晕（phase 错开相位，t 驱动亮度缓慢起伏）。 */
function drawDisc(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  r: number,
  color: string,
  phase: number,
  t: number,
): void {
  const halo = 0.11 + 0.07 * (0.5 + 0.5 * Math.sin(t * 0.6 + phase));
  for (let dy = -r - 2; dy <= r + 2; dy++) {
    for (let dx = -r - 2; dx <= r + 2; dx++) {
      const d = dx * dx + dy * dy;
      if (d <= r * r) {
        ctx.globalAlpha = 1;
        ctx.fillStyle = color;
        ctx.fillRect(x + dx, y + dy, 1, 1);
      } else if (d <= (r + 2) * (r + 2)) {
        ctx.globalAlpha = halo;
        ctx.fillStyle = color;
        ctx.fillRect(x + dx, y + dy, 1, 1);
      }
    }
  }
  ctx.globalAlpha = 1;
}

/** 环形漂移 + 环绕：位移随时间推进，完全移出画面后才从对侧重新进入。 */
function wrapX(v: number, span: number, margin: number): number {
  return (((v % span) + span) % span) - margin;
}

/** 绘制一帧。t 为秒级时间戳，用于动画（闪烁 / 波光 / 流星）。 */
export function renderScene(ctx: CanvasRenderingContext2D, s: PixelScene, t: number): void {
  const { sky, stars, moon, sun, clouds, ridges, water, grass, trees, birds } = s;
  const w = SCENE_W;
  const h = SCENE_H;

  // 天空：逐行色带（顶部 → 地平线）
  const rows = sky.length;
  for (let y = 0; y < HORIZON; y++) {
    ctx.fillStyle = sky[Math.min(rows - 1, Math.floor((y * rows) / HORIZON))];
    ctx.fillRect(0, y, w, 1);
  }

  // 星星：亮度随时间呼吸
  ctx.fillStyle = '#ffffff';
  for (const st of stars) {
    ctx.globalAlpha = 0.25 + 0.75 * (0.5 + 0.5 * Math.sin(t * st.speed + st.phase));
    ctx.fillRect(st.x, st.y, st.s, st.s);
  }
  ctx.globalAlpha = 1;

  // 流星（夜）：约每 26s 划过一次，2.5s 内沿对角线移动
  if (s.shooting) {
    const p = (t % 26) / 2.5;
    if (p < 1) {
      const sx = s.shooting.x + Math.floor(p * 48);
      const sy = s.shooting.y + Math.floor(p * 10);
      for (let k = 0; k < 4; k++) {
        ctx.globalAlpha = 0.9 * (1 - k / 4);
        ctx.fillRect(sx - k * 3, sy - k, 1, 1);
      }
      ctx.globalAlpha = 1;
    }
  }

  // 月亮：高挂天空，绘制在山脊之前（可被远山部分遮挡，符合真实层次）
  if (moon) {
    drawDisc(ctx, moon.x, moon.y, moon.r, moon.color, moon.phase, t);
  }

  // 云：底部两行 + 下沿缩短 + 顶部预生成凸起；整朵云随风缓慢漂移、环绕进出
  for (const c of clouds) {
    const cx = wrapX(c.x + c.speed * t, w + 2 * c.len, c.len);
    ctx.fillStyle = c.color;
    ctx.fillRect(cx, c.y, c.len, 1);
    ctx.fillRect(cx, c.y + 1, c.len, 1);
    ctx.fillRect(cx + 2, c.y + 2, Math.max(1, c.len - 4), 1);
    for (const b of c.bumps) {
      ctx.fillRect(cx + b.x, c.y - 1, b.len, 1);
    }
  }

  // 三层山脊剪影（后 → 前，阶梯状）
  // 太阳夹在最后层与前两层之间：低垂的日盘永远「架在」山口上，
  // 不会被中景山脊完全吞没（暮色近地平线的太阳尤需如此）。
  for (let i = 0; i < ridges.length; i++) {
    const layer = ridges[i];
    ctx.fillStyle = layer.color;
    for (let j = 0; j < layer.heights.length; j++) {
      const top = Math.round(layer.base - layer.heights[j]);
      ctx.fillRect(j * CHUNK, top, CHUNK, h - top);
    }
    if (i === 0 && sun) {
      drawDisc(ctx, sun.x, sun.y, sun.r, sun.color, sun.phase, t);
    }
  }

  // 水面：2px 交替条纹 + 日/月倒影 + 游走的波光
  if (water) {
    let row = 0;
    for (let y = water.y; y < h; y++) {
      ctx.fillStyle = row % 2 === 0 ? water.base : water.band;
      ctx.fillRect(0, y, w, 1);
      row++;
    }

    if (water.reflect) {
      const rf = water.reflect;
      // 倒影亮度随波起伏（相位遮挡渐隐）
      ctx.globalAlpha = 0.2 + 0.14 * (0.5 + 0.5 * Math.sin(t * 1.6 + rf.phase));
      ctx.fillStyle = rf.color;
      for (let k = 0; k < rf.offs.length; k++) {
        ctx.fillRect(rf.x + rf.offs[k], water.y + 2 + k * 2, 1, 1);
      }
      ctx.globalAlpha = 1;
    }

    ctx.fillStyle = water.shine;
    for (const sh of water.shimmer) {
      // 波光横向缓移 + 纵向游走 → 光点斜向舞蹈
      const shx = (sh.x + Math.floor(t * 0.9 + sh.phase * 4)) % w;
      const phase = Math.floor(t * 1.8 + sh.phase * 3);
      for (let k = 0; k < 4; k++) {
        if ((k + phase) % 3 === 0) {
          ctx.globalAlpha = 0.45;
          ctx.fillRect(shx, water.y + 2 + k * 2, 1, 1);
        }
      }
    }
    ctx.globalAlpha = 1;
  }

  // 草地：2px 交替色带
  if (grass) {
    let row = 0;
    for (let y = grass.y; y < h; y++) {
      const tone = Math.floor(row / 2) % 3;
      ctx.fillStyle = tone === 0 ? grass.base : grass.bands[tone - 1];
      ctx.fillRect(0, y, w, 1);
      row++;
    }

    // 前景像素树：三角形树冠 + 1px 树干，树冠随风左右轻摆（±1px，极慢）
    ctx.fillStyle = s.treeColor;
    for (const tr of trees) {
      // 各树相位错开（同速不同相），像风扫过树丛而非整齐划一
      const sway = Math.round(Math.sin(t * 0.5 + tr.phase));
      for (let r = 0; r < tr.h; r++) {
        const half = 1 + Math.floor((r / tr.h) * tr.h * 0.35);
        ctx.fillRect(tr.x + sway - half, tr.base - tr.h + r, half * 2 + 1, 1);
      }
      ctx.fillRect(tr.x, tr.base - 1, 1, 2);
    }
  }

  // 飞鸟（三点 v 形）：横穿天际并循环，高度随翅膀节奏微幅浮沉
  if (birds.length > 0) {
    ctx.fillStyle = s.birdColor;
    for (const b of birds) {
      const bx = wrapX(b.x + b.speed * t, w + 60, 30);
      const by = b.y + Math.round(Math.sin(t * 1.3 + b.bob) * 1.2);
      ctx.fillRect(bx, by, 1, 1);
      ctx.fillRect(bx + 1, by - 1, 1, 1);
      ctx.fillRect(bx + 2, by, 1, 1);
    }
  }
}
