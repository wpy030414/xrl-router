//! 环形旋转动画（shadcn/ui 风格：细线 + muted 轨道 + primary 弧段）。

use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Transform};

/// 窗口尺寸（奇数保证环形严格像素居中）。
pub const SIZE: u32 = 49;

/// 环形外半径。
const RING_OUTER: f32 = 20.0;
/// 环形内半径（stroke 3px）。
const RING_INNER: f32 = 17.0;
/// shadcn/ui spinner 转速：每秒一圈（360°/s）。
const SPEED_DEG_PER_SEC: f32 = 360.0;
/// 弧段覆盖角度。
const ARC_SWEEP: f32 = 240.0;

/// 轨道色 — shadcn dark `--muted`，极淡白。
fn track_color() -> Color { Color::from_rgba8(255, 255, 255, 20) }
/// 弧段色 — shadcn `--ring` 默认 hue 200°。
fn arc_color() -> Color { Color::from_rgba8(48, 171, 232, 255) }

pub fn premultiply_alpha(buf: &mut [u8]) {
    for chunk in buf.chunks_exact_mut(4) {
        let a = chunk[3] as f32 / 255.0;
        chunk[0] = (chunk[0] as f32 * a) as u8;
        chunk[1] = (chunk[1] as f32 * a) as u8;
        chunk[2] = (chunk[2] as f32 * a) as u8;
    }
}

/// RGBA → BGRA 原地交换（Windows 32bpp DIB 与 swapchain 均为 BGRA 字节序）。
#[cfg(target_os = "windows")]
pub fn to_bgra_inplace(buf: &mut [u8]) {
    for chunk in buf.chunks_exact_mut(4) {
        chunk.swap(0, 2);
    }
}

pub fn render_frame(buf: &mut [u8], anim_t: f64) {
    let w = SIZE;
    let h = SIZE;
    let mut pixmap = Pixmap::new(w, h).expect("create pixmap");
    pixmap.fill(Color::from_rgba8(0, 0, 0, 0));

    let cx = w as f32 / 2.0;
    let cy = h as f32 / 2.0;

    // 轨道：完整细环
    draw_ring(&mut pixmap, cx, cy, RING_OUTER, RING_INNER, 0.0, 360.0, track_color());

    // 旋转弧段：0° = 3 点方向，顺时针（y 向下坐标系），每秒一圈；
    // 起始角对匀速旋转无视觉意义，仅需覆盖 ARC_SWEEP
    let start = (anim_t * SPEED_DEG_PER_SEC as f64) as f32;
    draw_ring(&mut pixmap, cx, cy, RING_OUTER, RING_INNER, start, ARC_SWEEP, arc_color());

    buf.copy_from_slice(pixmap.data());
}

fn draw_ring(pixmap: &mut Pixmap, cx: f32, cy: f32,
    outer_r: f32, inner_r: f32, start_deg: f32, sweep_deg: f32, color: Color,
) {
    if sweep_deg <= 0.0 { return; }
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;

    let s = start_deg.to_radians();
    let e = (start_deg + sweep_deg).to_radians();
    let steps = (sweep_deg.abs() as usize).max(24);

    let mut pb = PathBuilder::new();
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let a = s + t * (e - s);
        let (x, y) = (cx + outer_r * a.cos(), cy + outer_r * a.sin());
        if i == 0 { pb.move_to(x, y); } else { pb.line_to(x, y); }
    }
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let a = e - t * (e - s);
        let (x, y) = (cx + inner_r * a.cos(), cy + inner_r * a.sin());
        pb.line_to(x, y);
    }
    pb.close();
    pixmap.fill_path(&pb.finish().unwrap(), &paint, FillRule::Winding, Transform::identity(), None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke() {
        let mut buf = vec![0u8; (SIZE * SIZE * 4) as usize];
        for t in [0.0, 0.5, 1.0, 2.0] {
            render_frame(&mut buf, t);
        }
        assert!(buf.iter().any(|&b| b != 0));
    }

    #[test]
    fn premultiply() {
        let mut buf = vec![255u8, 255, 255, 128];
        premultiply_alpha(&mut buf);
        assert_eq!(buf[0], 128);
        assert_eq!(buf[1], 128);
        assert_eq!(buf[2], 128);
        assert_eq!(buf[3], 128);
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn bgra_swap() {
        // arc_color = (48, 171, 232)（蓝）：R/B 交换后首字节应为 232
        let mut buf = vec![48u8, 171, 232, 255];
        to_bgra_inplace(&mut buf);
        assert_eq!(buf, vec![232, 171, 48, 255]);
    }
}