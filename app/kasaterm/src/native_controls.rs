use crate::{gpu, theme};
mod probe;

type Rect = (f32, f32, f32, f32);

pub(crate) const CONTROL_HEIGHT: f32 = 32.0;
pub(crate) const COMPACT_HEIGHT: f32 = 26.0;
pub(crate) const FONT_SIZE: f32 = 13.0;
pub(crate) const CONTROL_PADDING_X: f32 = 12.0;
pub(crate) const TOOLBAR_SIZE: f32 = 26.0;
pub(crate) const TOOLBAR_GAP: f32 = 4.0;

pub(crate) fn toolbar_button(g: &mut gpu::GpuRenderer, rect: Rect, cursor: (f32, f32), icon: &str, active: bool) {
    let hover = contains(rect, cursor);
    if hover || active {
        let fill = if hover { theme::surface_hover() } else { theme::surface_active() };
        g.round_rect_fill(rect.0, rect.1, rect.2, rect.3, theme::radius_sm(), fill);
    }
    g.hover_pointer |= hover;
    let size = theme::ICON_SIZE;
    g.queue_icon(icon, rect.0 + (rect.2 - size) / 2.0, rect.1 + (rect.3 - size) / 2.0,
        size, if hover || active { theme::text() } else { theme::text_dim() });
}

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
        g.round_rect_stroke(
            rect.0,
            rect.1,
            rect.2,
            rect.3,
            theme::radius_sm(),
            theme::border_w().max(2.0),
            theme::enforce_contrast_at(theme::accent(), theme::bg(), 3.0),
        );
        if rect.2 > 8.0 && rect.3 > 8.0 {
            g.round_rect_stroke(
                rect.0 + 2.0,
                rect.1 + 2.0,
                rect.2 - 4.0,
                rect.3 - 4.0,
                (theme::radius_sm() - 2.0).max(0.0),
                1.0,
                theme::foreground_on(theme::accent()),
            );
        }
    }
}

fn colors(style: Style, hover: bool) -> ([u8; 4], [u8; 4], [u8; 4]) {
    if !style.enabled {
        (theme::surface(), theme::border(), theme::text_mute())
    } else if style.danger {
        let fill = theme::lerp(
            theme::surface(),
            theme::danger(),
            if hover { 0.18 } else { 0.08 },
        );
        (
            fill,
            theme::with_alpha(theme::danger(), 150),
            button_ink(theme::danger(), fill),
        )
    } else if style.primary {
        let fill = if hover {
            theme::lerp(theme::accent(), theme::text(), 0.10)
        } else {
            theme::accent()
        };
        (
            fill,
            theme::with_alpha(theme::foreground_on(fill), 45),
            theme::foreground_on(fill),
        )
    } else if style.active {
        let fill = theme::lerp(
            theme::surface(),
            theme::accent(),
            if hover { 0.18 } else { 0.12 },
        );
        (
            fill,
            theme::with_alpha(theme::accent(), 110),
            button_ink(theme::accent(), fill),
        )
    } else {
        let fill = if hover {
            theme::surface_hover()
        } else {
            theme::lerp(theme::surface(), theme::bg(), 0.16)
        };
        (
            fill,
            theme::with_alpha(theme::edge_on(fill), if hover { 170 } else { 100 }),
            button_ink(theme::text(), fill),
        )
    }
}

fn button_ink(color: [u8; 4], background: [u8; 4]) -> [u8; 4] {
    theme::enforce_contrast_at(color, background, 4.5)
}

pub(crate) fn font_size(height: f32) -> f32 {
    if height >= 30.0 {
        FONT_SIZE
    } else {
        12.0
    }
}
fn horizontal_padding(height: f32) -> f32 {
    if height >= 30.0 {
        CONTROL_PADDING_X
    } else {
        10.0
    }
}

pub(crate) fn button_width(g: &mut gpu::GpuRenderer, label: &str, height: f32, bold: bool) -> f32 {
    (g.chrome_text_metrics(label, font_size(height), bold)
        .width()
        + 2.0 * horizontal_padding(height))
    .ceil()
    .max(height)
}

pub(crate) fn content_button(
    g: &mut gpu::GpuRenderer,
    slot: Rect,
    cursor: (f32, f32),
    label: &str,
    style: Style,
) -> Rect {
    content_button_aligned(
        g,
        slot,
        cursor,
        label,
        style,
        crate::native_layout::Align::Start,
    )
}

pub(crate) fn content_button_aligned(
    g: &mut gpu::GpuRenderer,
    slot: Rect,
    cursor: (f32, f32),
    label: &str,
    style: Style,
    align: crate::native_layout::Align,
) -> Rect {
    let slot = bounds(slot);
    let width = button_width(g, label, slot.3, style.primary || style.active);
    text_button(
        g,
        crate::native_layout::fit_width(slot, width, align),
        cursor,
        label,
        style,
    )
}

pub(crate) fn panel(g: &mut gpu::GpuRenderer, r: Rect, fill: [u8; 4]) {
    let radius = theme::radius_md();
    g.round_rect_fill(r.0, r.1, r.2, r.3, radius, fill);
    g.round_rect_stroke(
        r.0,
        r.1,
        r.2,
        r.3,
        radius,
        theme::border_w().max(1.0),
        theme::with_alpha(theme::edge_on(fill), 75),
    );
}

pub(crate) fn field(g: &mut gpu::GpuRenderer, r: Rect, focused: bool) {
    let fill = theme::lerp(theme::surface(), theme::bg(), 0.4);
    g.round_rect_fill(r.0, r.1, r.2, r.3, theme::radius_sm(), fill);
    g.round_rect_stroke(
        r.0,
        r.1,
        r.2,
        r.3,
        theme::radius_sm(),
        if focused { 2.0 } else { 1.0 },
        if focused {
            theme::enforce_contrast_at(theme::accent(), fill, 3.0)
        } else {
            theme::with_alpha(theme::edge_on(fill), 100)
        },
    );
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
    let (fill, line, ink) = colors(style, hover);
    g.round_rect_fill(rect.0, rect.1, rect.2, rect.3, theme::radius_sm(), fill);
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
    let font = font_size(rect.3);
    let shown = crate::info::fit_text(
        g,
        label,
        (rect.2 - 2.0 * horizontal_padding(rect.3)).max(0.0),
        font,
        bold,
    );
    g.centered_chrome_text(
        rect,
        &shown,
        gpu::DrawOpts {
            font_size: font,
            color: ink,
            bold,
            italic: false,
        },
    );
    g.hover_pointer |= hover;
    g.note_control(rect, hover, style.enabled);
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
    let (fill, line, ink) = colors(style, hover);
    let ink = if style.primary && !hover && !style.active {
        button_ink(theme::accent(), theme::bg())
    } else {
        ink
    };
    if hover || style.active {
        g.round_rect_fill(rect.0, rect.1, rect.2, rect.3, theme::radius_sm(), fill);
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
    g.note_control(rect, hover, style.enabled);
    rect
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_and_danger_labels_remain_readable_in_every_builtin_palette() {
        for (_, _, palette) in theme::THEME_PRESETS {
            for color in [palette.danger, [122, 162, 247, 255], [255, 240, 120, 255]] {
                let ink = button_ink(color, palette.bg);
                use kasa_gridview::palette::{contrast_of, luminance};
                assert!(contrast_of(luminance(ink), luminance(palette.bg)) >= 4.49);
            }
        }
    }

    #[test]
    fn visual_and_hit_bounds_share_the_same_centered_control() {
        let rect = bounds((10.0, 20.0, 100.0, 40.0));
        assert_eq!(rect, (10.0, 24.0, 100.0, 32.0));
        assert!(!contains(rect, (20.0, 23.0)));
        assert!(contains(rect, (20.0, 28.0)));
        assert!(!contains(rect, (20.0, 56.0)));
    }
}
