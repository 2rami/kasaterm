use crate::{gpu, theme};

type Rect = (f32, f32, f32, f32);

pub(crate) const CONTROL_HEIGHT: f32 = 26.0;
pub(crate) const CONTROL_PADDING_X: f32 = 10.0;

#[derive(Clone, Copy)]
pub(crate) struct Style {
    pub primary: bool,
    pub active: bool,
    pub enabled: bool,
    pub danger: bool,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            primary: false,
            active: false,
            enabled: true,
            danger: false,
        }
    }
}

pub(crate) fn bounds(rect: Rect) -> Rect {
    let h = rect.3.min(CONTROL_HEIGHT).max(0.0);
    (
        rect.0,
        rect.1 + ((rect.3 - h) / 2.0).floor(),
        rect.2.max(0.0),
        h,
    )
}

fn contains(rect: Rect, cursor: (f32, f32)) -> bool {
    cursor.0 >= rect.0
        && cursor.0 < rect.0 + rect.2
        && cursor.1 >= rect.1
        && cursor.1 < rect.1 + rect.3
}

pub(crate) fn focus_ring(g: &mut gpu::GpuRenderer, rect: Rect) {
    if rect.2 > 0.0 && rect.3 > 0.0 {
        g.round_rect_stroke(rect.0, rect.1, rect.2, rect.3, theme::radius_sm(),
            theme::border_w().max(2.0), theme::accent());
    }
}

fn colors(style: Style, hover: bool) -> ([u8; 4], [u8; 4]) {
    if !style.enabled {
        (theme::border(), theme::text_mute())
    } else if style.danger {
        (theme::danger(), theme::danger())
    } else if style.primary || style.active {
        (theme::accent(), theme::accent())
    } else if hover {
        (theme::text_dim(), theme::text())
    } else {
        (theme::border(), theme::text_dim())
    }
}

pub(crate) fn text_button(
    g: &mut gpu::GpuRenderer,
    rect: Rect,
    cursor: (f32, f32),
    label: &str,
    style: Style,
) -> Rect {
    let rect = bounds(rect);
    let hover = style.enabled && contains(rect, cursor);
    let (line, ink) = colors(style, hover);
    g.round_rect_stroke(
        rect.0,
        rect.1,
        rect.2,
        rect.3,
        theme::radius_sm(),
        theme::border_w().max(1.0),
        line,
    );
    let bold = style.primary || style.active;
    let shown = crate::info::fit_text(g, label, (rect.2 - 2.0 * CONTROL_PADDING_X).max(0.0), 12.0, bold);
    let width = g.measure_chrome_text(&shown, 12.0, bold);
    g.draw_text(
        rect.0 + (rect.2 - width) / 2.0,
        rect.1 + (rect.3 - 12.0) / 2.0 - 0.5,
        &shown,
        gpu::DrawOpts {
            font_size: 12.0,
            color: ink,
            bold,
            italic: false,
        },
    );
    g.hover_pointer |= hover;
    rect
}

pub(crate) fn icon_button(
    g: &mut gpu::GpuRenderer,
    rect: Rect,
    cursor: (f32, f32),
    icon: &str,
    style: Style,
) -> Rect {
    let rect = bounds(rect);
    let hover = style.enabled && contains(rect, cursor);
    let (line, ink) = colors(style, hover);
    if hover || style.active {
        g.round_rect_stroke(
            rect.0,
            rect.1,
            rect.2,
            rect.3,
            theme::radius_sm(),
            theme::border_w().max(1.0),
            line,
        );
    }
    g.queue_icon(
        icon,
        rect.0 + (rect.2 - 14.0) / 2.0,
        rect.1 + (rect.3 - 14.0) / 2.0,
        14.0,
        ink,
    );
    g.hover_pointer |= hover;
    rect
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visual_and_hit_bounds_share_the_same_centered_control() {
        let rect = bounds((10.0, 20.0, 100.0, 40.0));
        assert_eq!(rect, (10.0, 27.0, 100.0, 26.0));
        assert!(!contains(rect, (20.0, 23.0)));
        assert!(contains(rect, (20.0, 28.0)));
        assert!(!contains(rect, (20.0, 54.0)));
    }
}
