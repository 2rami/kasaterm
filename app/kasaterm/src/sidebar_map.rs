use super::*;

type Rect = (f32, f32, f32, f32);
const FACE: f32 = 26.0;
const FONT: f32 = 12.0;
const LINE: f32 = 16.0;

pub(crate) struct Layout {
    pub face: (f32, f32, f32),
    title: Option<Rect>,
    lines: usize,
    horizontal: bool,
}

pub(crate) fn layout((x, y, w, h): Rect) -> Layout {
    let footer = if crate::render::minimap_has_bar(w, h) { 8.0 } else { 0.0 };
    let available = (h - footer).max(0.0);
    let face = FACE.min((w - 12.0).max(0.0)).min((available - 4.0).max(0.0));
    if w >= 80.0 && (36.0..64.0).contains(&h) {
        let lines = if h >= 52.0 { 2 } else { 1 };
        let height = lines as f32 * LINE;
        Layout {
            face: (x + 8.0, y + (available - face) * 0.5, face),
            title: Some((x + face + 16.0, y + (available - height) * 0.5, w - face - 24.0, height)),
            lines,
            horizontal: true,
        }
    } else if w >= 64.0 && h >= 64.0 {
        let lines = if h >= 88.0 { 2 } else { 1 };
        let height = lines as f32 * LINE;
        let gap = if lines == 2 { 6.0 } else { 2.0 };
        let top = y + (available - face - gap - height).max(0.0) * 0.5;
        Layout {
            face: (x + (w - face) * 0.5, top, face),
            title: Some((x + 6.0, top + face + gap, w - 12.0, height)),
            lines,
            horizontal: false,
        }
    } else {
        Layout {
            face: (x + (w - face) * 0.5, y + (available - face) * 0.5, face),
            title: None,
            lines: 0,
            horizontal: false,
        }
    }
}

pub(crate) fn has_title(width: f32, height: f32) -> bool {
    layout((0.0, 0.0, width, height)).title.is_some()
}

pub(crate) fn paint_title(g: &mut gpu::GpuRenderer, rect: Rect, title: &str) {
    let layout = layout(rect);
    let Some((x, y, w, h)) = layout.title else { return };
    let lines = crate::info::fit_text_lines(g, title, w, FONT, false, layout.lines, false);
    let top = y + (h - lines.len() as f32 * LINE) * 0.5;
    g.push_clip(x, y, w, h);
    for (i, line) in lines.iter().enumerate() {
        let left = if layout.horizontal { x } else { x + (w - g.measure_chrome_text(line, FONT, false)).max(0.0) * 0.5 };
        g.draw_text(left, top + i as f32 * LINE, line,
            gpu::DrawOpts { font_size: FONT, color: theme::text(), bold: false, italic: false });
    }
    g.pop_clip();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn growing_room_cards_does_not_grow_the_portrait() {
        for width in [120.0, 240.0, 360.0, 520.0] {
            for height in [40.0, 46.0, 64.0, 96.0, 230.0, 600.0] {
                assert_eq!(layout((0.0, 0.0, width, height)).face.2, FACE);
            }
        }
    }

    #[test]
    fn names_have_their_own_space_in_compact_and_expanded_maps() {
        for (w, h) in [(200.0, 40.0), (200.0, 46.0), (105.0, 53.0), (200.0, 70.0), (80.0, 70.0), (100.0, 130.0), (480.0, 400.0)] {
            let map = layout((0.0, 0.0, w, h));
            let (x, y, width, height) = map.title.unwrap();
            let (fx, fy, size) = map.face;
            assert!(x >= 0.0 && y >= 0.0 && x + width <= w && y + height <= h - 8.0);
            assert!(x >= fx + size || y >= fy + size);
        }
        assert!(has_title(200.0, 46.0));
        assert!(!has_title(30.0, 46.0));
    }
}
