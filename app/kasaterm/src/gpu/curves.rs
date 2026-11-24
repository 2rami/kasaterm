use super::{CellInstance, GpuRenderer, srgb_rgba_to_linear};
use resvg::tiny_skia::{LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};

fn raster(dx: i32, dy: i32, width: f32) -> Option<(Vec<u8>, u32, u32, f32)> {
    if !width.is_finite() || width <= 0.0 || (dx == 0 && dy == 0) { return None; }
    let pad = (width / 2.0 + 1.0).ceil();
    let w = dx.unsigned_abs().checked_add(2 * pad as u32)?;
    let h = dy.unsigned_abs().checked_add(2 * pad as u32)?;
    if w > 4096 || h > 4096 { return None; }
    let mut pix = Pixmap::new(w, h)?;
    let (x, y) = (pad + (-dx).max(0) as f32, pad + (-dy).max(0) as f32);
    let (tx, ty) = (x + dx as f32, y + dy as f32);
    let mut path = PathBuilder::new();
    path.move_to(x, y);
    path.cubic_to(x, y + dy as f32 * 0.5, tx, ty - dy as f32 * 0.5, tx, ty);
    let mut paint = Paint::default();
    paint.set_color_rgba8(255, 255, 255, 255);
    paint.anti_alias = true;
    pix.stroke_path(&path.finish()?, &paint,
        &Stroke { width, line_cap: LineCap::Butt, ..Stroke::default() }, Transform::identity(), None);
    Some((pix.take(), w, h, pad))
}

impl GpuRenderer {
    pub(crate) fn curve_stroke(&mut self, from: (f32, f32), to: (f32, f32), width: f32, color: [u8; 4]) {
        let scale = self.grid.scale;
        if ![from.0, from.1, to.0, to.1, width].iter().all(|v| v.is_finite()) { return; }
        let (x, y) = ((from.0 * scale).round(), (from.1 * scale).round());
        let (tx, ty) = ((to.0 * scale).round(), (to.1 * scale).round());
        let (dx, dy, width) = ((tx - x) as i32, (ty - y) as i32, width * scale);
        if width <= 0.0 || (dx == 0 && dy == 0) { return; }
        let pad = (width / 2.0 + 1.0).ceil();
        let (w, h) = (dx.unsigned_abs() as f32 + 2.0 * pad, dy.unsigned_abs() as f32 + 2.0 * pad);
        // 위치·색을 캐시 열쇠에서 빼야 스크롤·호버 때 같은 곡선을 다시 굽지 않는다.
        let key = format!("__curve:{dx}:{dy}:{}", width.to_bits());
        if !self.grid.images.contains_key(&key) {
            let Some((rgba, w, h, _)) = raster(dx, dy, width) else { return; };
            self.upload_image(&key, &rgba, w, h);
        }
        self.grid.icon_quads.push((key, CellInstance {
            cell_px: [x.min(tx) - pad, y.min(ty) - pad, w, h],
            uv_min: [0.0, 0.0], uv_max: [1.0, 1.0],
            fg_rgba: srgb_rgba_to_linear(color), flags: CellInstance::FLAG_ICON,
            ..Default::default()
        }, self.grid.chrome.len() as u32, self.cur_clip_phys()));
    }
}

#[cfg(test)]
mod tests {
    use super::raster;

    #[test]
    fn graph_curves_are_continuous_and_antialiased_at_each_scale() {
        for scale in [1.0_f32, 1.25, 1.5, 2.0] {
            for direction in [-1, 1] {
                let (bytes, w, h, pad) = raster(direction * (28.0 * scale).round() as i32,
                    (18.0 * scale).round() as i32, 2.0 * scale).unwrap();
                assert!(bytes.chunks_exact(4).any(|px| px[3] > 0 && px[3] < 255));
                for y in pad as usize..h as usize - pad as usize {
                    assert!(bytes[y * w as usize * 4..(y + 1) * w as usize * 4]
                        .chunks_exact(4).any(|px| px[3] > 100), "gap at scale={scale}, y={y}");
                }
                assert!(bytes[..w as usize * 4].iter().all(|b| *b == 0));
            }
        }
    }
}
