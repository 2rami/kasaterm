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

/// 자른 글 줄과 그 폭. 사이드바 글은 장마다 다시 그리는데, 자를 자리를 찾느라 글자 하나씩 폭을 재서 사용자 앱
/// 사이드바 표본의 약 1/5 이 글자 재기였다(2026-11-18, 칸 이름표·기기 이름·방 이름). 같은 글·폭·크기면 한 번 잰 것을 쓴다.
struct Fitted {
    title: String,
    width: u32,
    size: u32,
    bold: bool,
    lines: usize,
    probe: u32,
    fit: Vec<(String, f32)>,
}

thread_local! {
    static FITTED: std::cell::RefCell<std::collections::HashMap<u64, Fitted>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

/// 배율·글꼴이 바뀌면 같은 글도 폭이 달라진다 — 두 글자의 폭을 열쇠에 넣어 옛 자르기를 안 쓴다.
const PROBE: &str = "가A";
/// 끌어 넓히는 동안 폭마다 하나씩 쌓이므로 이만큼에서 비운다.
const FITTED_MAX: usize = 512;

/// `width` 에 맞춰 자른 줄들(과 그 폭)로 `draw` 를 부른다. `lines == 0` 이면 [`crate::info::fit_text`] 한 줄,
/// 아니면 [`crate::info::fit_text_lines`] 로 그 줄 수까지.
pub(crate) fn with_fitted<R>(
    g: &mut gpu::GpuRenderer,
    title: &str,
    width: f32,
    size: f32,
    bold: bool,
    lines: usize,
    draw: impl FnOnce(&mut gpu::GpuRenderer, &[(String, f32)]) -> R,
) -> R {
    let probe = g.measure_chrome_text(PROBE, size, bold).to_bits();
    let key = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (title, width.to_bits(), size.to_bits(), bold, lines, probe).hash(&mut hasher);
        hasher.finish()
    };
    let same = |f: &Fitted| f.title == title && f.width == width.to_bits() && f.size == size.to_bits()
        && f.bold == bold && f.lines == lines && f.probe == probe;
    if !FITTED.with(|kept| kept.borrow().get(&key).is_some_and(same)) {
        let cut = if lines == 0 {
            vec![crate::info::fit_text(g, title, width, size, bold)]
        } else {
            crate::info::fit_text_lines(g, title, width, size, bold, lines, false)
        };
        let fit = cut.into_iter().map(|line| {
            let w = g.measure_chrome_text(&line, size, bold);
            (line, w)
        }).collect();
        FITTED.with(|kept| {
            let mut kept = kept.borrow_mut();
            if kept.len() >= FITTED_MAX {
                kept.clear();
            }
            kept.insert(key, Fitted { title: title.to_string(), width: width.to_bits(), size: size.to_bits(), bold, lines, probe, fit });
        });
    }
    FITTED.with(|kept| {
        let kept = kept.borrow();
        draw(g, kept.get(&key).map_or(&[][..], |fitted| &fitted.fit))
    })
}

pub(crate) fn paint_title(g: &mut gpu::GpuRenderer, rect: Rect, title: &str) {
    let layout = layout(rect);
    let Some((x, y, w, h)) = layout.title else { return };
    g.push_clip(x, y, w, h);
    with_fitted(g, title, w, FONT, false, layout.lines, |g, fit| {
        let top = y + (h - fit.len() as f32 * LINE) * 0.5;
        for (i, (line, width)) in fit.iter().enumerate() {
            let left = if layout.horizontal { x } else { x + (w - width).max(0.0) * 0.5 };
            g.draw_text(left, top + i as f32 * LINE, line,
                gpu::DrawOpts { font_size: FONT, color: theme::text(), bold: false, italic: false });
        }
    });
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
