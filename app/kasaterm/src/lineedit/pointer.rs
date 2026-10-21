use super::{selection, with_state, BAND_ALPHA};
use crate::{gpu, theme};
use std::{cell::RefCell, collections::HashMap};

type Rect = (f32, f32, f32, f32);

#[derive(Default)]
struct Pointer {
    view: Option<View>,
    scroll: f32,
    dragging: bool,
}

thread_local! {
    // Painting a background field must not change keyboard focus or its undo history.
    static POINTERS: RefCell<HashMap<String, Pointer>> = RefCell::new(HashMap::new());
}

fn with_pointer<R>(id: &str, f: impl FnOnce(&mut Pointer) -> R) -> R {
    POINTERS.with_borrow_mut(|states| f(states.entry(id.to_owned()).or_default()))
}

pub(super) fn forget(id: &str) {
    POINTERS.with_borrow_mut(|states| {
        states.remove(id);
    });
}

struct View {
    rect: Rect,
    positions: Vec<f32>,
}

impl View {
    fn hit(&self, (x, y): (f32, f32)) -> bool {
        x >= self.rect.0
            && x < self.rect.0 + self.rect.2
            && y >= self.rect.1
            && y < self.rect.1 + self.rect.3
    }

    fn index(&self, x: f32) -> usize {
        self.positions
            .windows(2)
            .position(|w| x < (w[0] + w[1]) * 0.5)
            .unwrap_or(self.positions.len().saturating_sub(1))
    }
}

#[derive(Clone)]
pub(crate) struct SingleLine {
    pub(crate) id: &'static str,
    pub(crate) text: String,
    pub(crate) cursor: usize,
    pub(crate) preedit: String,
    pub(crate) caret_on: bool,
}

impl SingleLine {
    pub(crate) fn paint(
        &self,
        g: &mut gpu::GpuRenderer,
        rect: Rect,
        size: f32,
        bold: bool,
        clip: Option<Rect>,
    ) {
        if rect.2 < 8.0 || rect.3 <= 0.0 {
            return;
        }
        let len = self.text.chars().count();
        let cursor = self.cursor.min(len);
        let picked = selection(self.id, cursor, len);
        let (from, to) = if self.preedit.is_empty() {
            (cursor, cursor)
        } else {
            picked.unwrap_or((cursor, cursor))
        };
        let shown = format!(
            "{}{}{}",
            self.text.chars().take(from).collect::<String>(),
            self.preedit,
            self.text.chars().skip(to).collect::<String>()
        );
        let caret = from + self.preedit.chars().count();
        let mut prefix = String::new();
        let mut widths = vec![0.0];
        for ch in shown.chars() {
            prefix.push(ch);
            widths.push(g.measure_chrome_text(&prefix, size, bold));
        }
        let content = (rect.0 + 4.0, rect.1 + (rect.3 - size) * 0.5, rect.2 - 8.0);
        let caret_x = widths[caret];
        let scroll = with_pointer(self.id, |s| {
            let scroll = &mut s.scroll;
            *scroll =
                (*scroll).min((widths.last().copied().unwrap_or(0.0) - content.2 + 2.0).max(0.0));
            if caret_x < *scroll {
                *scroll = caret_x;
            }
            if caret_x > *scroll + content.2 - 2.0 {
                *scroll = (caret_x - content.2 + 2.0).max(0.0);
            }
            *scroll
        });
        let positions: Vec<f32> = widths.iter().map(|x| content.0 + x - scroll).collect();
        g.rect(rect.0, rect.1, rect.2, rect.3, theme::surface_hover());
        g.round_rect_stroke(rect.0, rect.1, rect.2, rect.3, 3.0, 1.0, theme::accent());
        g.push_clip(rect.0 + 1.0, rect.1, (rect.2 - 2.0).max(0.0), rect.3);
        if self.preedit.is_empty() {
            if let Some((start, end)) = picked {
                g.rect(
                    positions[start],
                    content.1 - 1.0,
                    positions[end] - positions[start],
                    size + 2.0,
                    theme::with_alpha(theme::accent(), BAND_ALPHA),
                );
            }
        } else {
            g.rect(
                positions[from],
                content.1 + size + 1.0,
                positions[caret] - positions[from],
                1.5,
                theme::accent(),
            );
        }
        g.draw_text(
            content.0 - scroll,
            content.1,
            &shown,
            gpu::DrawOpts {
                font_size: size,
                color: theme::text(),
                bold,
                italic: false,
            },
        );
        if self.caret_on {
            g.rect(
                positions[caret],
                content.1 - 1.0,
                1.4,
                size + 2.0,
                theme::text(),
            );
        }
        g.pop_clip();
        with_pointer(self.id, |s| {
            s.view = visible_rect(rect, clip).map(|rect| View { rect, positions })
        });
    }
}

fn visible_rect(rect: Rect, clip: Option<Rect>) -> Option<Rect> {
    let clip = clip.unwrap_or(rect);
    let (x, y) = (rect.0.max(clip.0), rect.1.max(clip.1));
    let right = (rect.0 + rect.2).min(clip.0 + clip.2);
    let bottom = (rect.1 + rect.3).min(clip.1 + clip.3);
    (right > x && bottom > y).then_some((x, y, right - x, bottom - y))
}

// Geometry belongs to the painted frame, so hiding or moving a card must not
// leave an invisible text target at its previous position.
pub(crate) fn clear_geometry(id: &str) {
    with_pointer(id, |s| s.view = None);
}

pub(crate) fn pointer_hit(id: &str, point: (f32, f32)) -> bool {
    with_pointer(id, |s| s.view.as_ref().is_some_and(|v| v.hit(point)))
}

pub(crate) fn pointer_press(
    id: &str,
    cursor: &mut usize,
    len: usize,
    point: (f32, f32),
    shift: bool,
) -> bool {
    with_pointer(id, |pointer| {
        let Some(v) = pointer.view.as_ref().filter(|v| v.hit(point)) else {
            return false;
        };
        let at = v.index(point.0).min(len);
        with_state(id, |s| {
            if shift {
                s.anchor.get_or_insert(*cursor);
            } else {
                s.anchor = Some(at);
            }
            s.typing = false;
        });
        *cursor = at;
        pointer.dragging = true;
        true
    })
}

pub(crate) fn pointer_drag(id: &str, cursor: &mut usize, len: usize, x: f32) -> bool {
    with_pointer(id, |s| {
        if !s.dragging {
            return false;
        }
        if let Some(v) = &s.view {
            *cursor = v.index(x).min(len);
        }
        true
    })
}

pub(crate) fn pointer_release(id: &str) -> bool {
    with_pointer(id, |s| std::mem::take(&mut s.dragging))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry(id: &str) {
        with_pointer(id, |s| {
            s.view = Some(View {
                rect: (10.0, 20.0, 80.0, 24.0),
                positions: vec![14.0, 34.0, 54.0, 64.0],
            })
        });
    }

    #[test]
    fn pointer_uses_character_boundaries_and_shift_anchor() {
        let id = "pointer-hangul";
        geometry(id);
        let mut c = 3;
        assert!(pointer_press(id, &mut c, 3, (28.0, 25.0), false));
        assert_eq!(c, 1);
        assert!(pointer_drag(id, &mut c, 3, 80.0));
        assert_eq!(selection(id, c, 3), Some((1, 3)));
        assert!(pointer_release(id));
        assert!(!pointer_drag(id, &mut c, 3, 10.0));
        assert!(pointer_press(id, &mut c, 3, (12.0, 25.0), true));
        assert_eq!(selection(id, c, 3), Some((0, 1)));
        assert!(!pointer_press(id, &mut c, 3, (12.0, 50.0), false));
    }

    #[test]
    fn cursor_click_does_not_clear_undo_history() {
        let id = "pointer-undo";
        let (mut text, mut cursor) = ("가나".to_string(), 2);
        super::super::insert_in(super::super::line(id), &mut text, &mut cursor, "x");
        geometry(id);
        pointer_press(id, &mut cursor, 3, (14.0, 25.0), false);
        with_state(id, |s| {
            assert_eq!(s.undo.last().map(|(t, _)| t.as_str()), Some("가나"))
        });
        clear_geometry(id);
        assert!(!pointer_hit(id, (14.0, 25.0)));
    }

    #[test]
    fn scrolled_out_text_does_not_steal_header_clicks() {
        let field = (10.0, 20.0, 80.0, 24.0);
        assert_eq!(
            visible_rect(field, Some((0.0, 30.0, 200.0, 100.0))),
            Some((10.0, 30.0, 80.0, 14.0))
        );
        assert_eq!(visible_rect(field, Some((0.0, 45.0, 200.0, 100.0))), None);
    }

    #[test]
    fn background_geometry_does_not_reset_another_fields_selection_or_undo() {
        let (mut text, mut cursor) = ("keep".to_string(), 4);
        super::super::insert_in(
            super::super::line("other-field"),
            &mut text,
            &mut cursor,
            "x",
        );
        super::super::select_all("other-field", &text, &mut cursor);
        geometry("background-room");
        assert!(pointer_hit("background-room", (20.0, 25.0)));
        clear_geometry("background-room");
        assert_eq!(selection("other-field", cursor, text.len()), Some((0, 5)));
        with_state("other-field", |s| {
            assert_eq!(s.undo.last().map(|(s, _)| s.as_str()), Some("keep"))
        });
    }
}
