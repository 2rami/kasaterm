use super::*;

const PREVIEW_H: f32 = 204.0;
pub(super) const FEATURED_PALETTES: [&str; 4] = ["frost-light", "frost-dark", "graphite", "paper"];

fn palette_strip(x: f32, y: f32, w: f32) -> ([Rect; 4], f32) {
    let columns = if w >= 560.0 { 4 } else { 2 };
    let item_w = (w - (columns - 1) as f32 * 8.0) / columns as f32;
    let rects = std::array::from_fn(|index| {
        (
            x + (index % columns) as f32 * (item_w + 8.0),
            y + (index / columns) as f32 * 104.0,
            item_w,
            96.0,
        )
    });
    (rects, (4 / columns) as f32 * 104.0)
}

pub(super) fn paint_palette_strip(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    x: f32,
    y: f32,
    w: f32,
) -> f32 {
    let (rects, height) = palette_strip(x, y, w);
    for (key, rect) in FEATURED_PALETTES.into_iter().zip(rects) {
        let Some((_, label, palette)) = theme::THEME_PRESETS.iter().find(|(id, _, _)| *id == key)
        else {
            continue;
        };
        let colors = palette.surface_colors();
        let selected = s.theme == key;
        let hover = contains(rect, s.cursor);
        let radius = theme::radius_md();
        round_rect(g, rect.0, rect.1, rect.2, rect.3, radius,
            if selected { theme::lerp(theme::surface(), theme::accent(), 0.12) } else { theme::surface_hover() });
        let preview = (rect.0 + 8.0, rect.1 + 8.0, (rect.2 - 16.0).max(0.0), 54.0);
        round_rect(g, preview.0, preview.1, preview.2, preview.3, theme::radius_sm(), colors.sidebar);
        let win = (preview.0 + preview.2 * 0.17, preview.1 + 9.0, preview.2 * 0.69, 36.0);
        round_rect(g, win.0, win.1, win.2, win.3, theme::radius_sm().min(6.0), colors.pane);
        for (line, portion) in [0.64, 0.42, 0.51].into_iter().enumerate() {
            g.rect(win.0 + 8.0, win.1 + 10.0 + line as f32 * 7.0, (win.2 - 16.0).max(0.0) * portion, 2.0, palette.text_dim);
        }
        let label = fit(g, label, (rect.2 - 32.0).max(0.0), 12.0, selected);
        draw_text(g, rect.0 + 12.0, rect.1 + 74.0, &label, 12.0, theme::text(), selected);
        if selected {
            g.queue_icon("check", rect.0 + rect.2 - 24.0, rect.1 + 73.0, 14.0,
                theme::enforce_contrast_at(theme::accent(), theme::surface(), 4.5));
        }
        if selected || hover {
            stroke_round(g, rect, radius, if selected { theme::accent() } else { theme::edge_on(theme::surface_hover()) });
        }
        register_clipped(g, hits, Target::Setting(SettingsAction::ThemeMode(key.into())), rect, HitCursor::Pointer);
        g.hover_pointer |= hover;
    }
    height
}

pub(super) fn paint_accents(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, y: f32, w: f32) -> f32 {
    let columns = ((w + 8.0) / 64.0).floor().max(1.0) as usize;
    let cell_w = ((w - (columns - 1) as f32 * 8.0) / columns as f32).min(76.0);
    for (i, (name, color)) in theme::ACCENT_PRESETS.iter().enumerate() {
        let rect = (x + (i % columns) as f32 * (cell_w + 8.0), y + (i / columns) as f32 * 64.0, cell_w, 58.0);
        let hot = contains(rect, s.cursor);
        let selected = s.accent == *name;
        let cx = rect.0 + (cell_w - 36.0) * 0.5;
        round_rect(g, cx, rect.1, 36.0, 36.0, 18.0, *color);
        if selected {
            g.queue_icon("check", cx + 10.0, rect.1 + 10.0, 16.0, theme::foreground_on(*color));
        }
        if selected || hot {
            stroke_round(g, (cx - 3.0, rect.1 - 3.0, 42.0, 42.0), 21.0, theme::enforce_contrast_at(*color, theme::surface(), 3.0));
        }
        let label = preset_label(name);
        let width = g.measure_chrome_text(label, 10.5, false);
        draw_text(g, rect.0 + (cell_w - width) * 0.5, rect.1 + 44.0, label, 10.5, theme::text_dim(), false);
        register_clipped(g, hits, Target::Setting(SettingsAction::Accent(name.to_string())), rect, HitCursor::Pointer);
        g.hover_pointer |= hot;
    }
    theme::ACCENT_PRESETS.len().div_ceil(columns) as f32 * 64.0
}

fn regions(x: f32, y: f32, w: f32) -> [(usize, Rect); 5] {
    let header = crate::native_controls::CONTROL_HEIGHT;
    let sidebar = (w * 0.22).floor();
    let panel = (w * 0.24).floor();
    let center = w - sidebar - panel;
    [
        (30, (x, y, w, header)),
        (29, (x, y + header, sidebar, PREVIEW_H - header)),
        (28, (x + sidebar, y + header, center, header)),
        (
            27,
            (
                x + sidebar,
                y + header * 2.0,
                center,
                PREVIEW_H - header * 2.0,
            ),
        ),
        (31, (x + w - panel, y + header, panel, PREVIEW_H - header)),
    ]
}

fn text(g: &mut gpu::GpuRenderer, rect: Rect, value: &str, ink: [u8; 4], bold: bool) {
    let value = fit(g, value, (rect.2 - 20.0).max(0.0), 10.5, bold);
    draw_text(g, rect.0 + 10.0, rect.1 + 8.0, &value, 10.5, ink, bold);
}

pub(super) fn paint(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    x: f32,
    y: f32,
    w: f32,
) -> f32 {
    let selected = match s.input {
        Some(SettingsInput::PaletteHex(index)) => Some(index),
        _ => None,
    };
    let areas = regions(x, y, w);
    for (slot, rect) in areas {
        let fill = match slot {
            27 => theme::pane_bg(),
            28 => theme::header_bg(),
            29 => theme::sidebar_bg(),
            30 => theme::titlebar_bg(),
            _ => theme::side_panel_bg(),
        };
        g.rect(rect.0, rect.1, rect.2, rect.3, fill);
        register_clipped(
            g,
            hits,
            Target::Setting(SettingsAction::FocusPaletteHex(slot)),
            rect,
            HitCursor::Pointer,
        );
        let hover = contains(rect, s.cursor);
        g.hover_pointer |= hover;
        if selected == Some(slot) || hover {
            stroke_rect(
                g,
                (
                    rect.0 + 1.0,
                    rect.1 + 1.0,
                    (rect.2 - 2.0).max(0.0),
                    (rect.3 - 2.0).max(0.0),
                ),
                theme::accent(),
            );
        }
    }
    let header_ink = theme::enforce_contrast_at(theme::text(), theme::header_bg(), 4.5);
    let title_ink = theme::enforce_contrast_at(theme::text(), theme::titlebar_bg(), 4.5);
    let side_ink = theme::enforce_contrast_at(theme::text(), theme::sidebar_bg(), 4.5);
    let panel_ink = theme::enforce_contrast_at(theme::text(), theme::side_panel_bg(), 4.5);
    let pane_ink = theme::enforce_min_contrast(theme::fg(), theme::pane_bg());
    text(g, areas[0].1, "KASA · 상단바", title_ink, true);
    let left = areas[1].1;
    for (line, label) in ["사이드바", "작업 방", "프로젝트", "터미널"]
        .iter()
        .enumerate()
    {
        text(
            g,
            (left.0, left.1 + line as f32 * 30.0, left.2, 26.0),
            label,
            side_ink,
            line == 0,
        );
    }
    text(g, areas[2].1, "터미널 · main", header_ink, true);
    let body = areas[3].1;
    text(g, body, "$ git status", pane_ink, false);
    text(
        g,
        (body.0, body.1 + 24.0, body.2, 26.0),
        "프로젝트의 변경 사항",
        pane_ink,
        false,
    );
    let split_y = body.1 + body.3 * 0.58;
    g.rect(body.0, split_y, body.2, 1.0, theme::border());
    text(
        g,
        (body.0, split_y + 8.0, body.2, 26.0),
        "$ 새 터미널",
        pane_ink,
        false,
    );
    let right = areas[4].1;
    for (line, label) in ["Git · Info", "변경 파일", "src/main.rs"]
        .iter()
        .enumerate()
    {
        text(
            g,
            (right.0, right.1 + line as f32 * 30.0, right.2, 26.0),
            label,
            panel_ink,
            line == 0,
        );
    }
    stroke_rect(g, (x, y, w, PREVIEW_H), theme::border());
    g.rect(left.0 + left.2, left.1, 1.0, left.3, theme::border());
    g.rect(right.0, right.1, 1.0, right.3, theme::border());
    let options = [
        (0, "창 바탕"),
        (1, "터미널 글자"),
        (6, "화면 글자"),
        (5, "구분선"),
    ];
    let columns = if w >= 440.0 { 4 } else { 2 };
    let button_w = (w - (columns - 1) as f32 * 8.0) / columns as f32;
    for (index, (slot, label)) in options.into_iter().enumerate() {
        button(
            g,
            s,
            hits,
            (
                x + (index % columns) as f32 * (button_w + 8.0),
                y + PREVIEW_H + 12.0 + (index / columns) as f32 * 34.0,
                button_w,
                26.0,
            ),
            label,
            Target::Setting(SettingsAction::FocusPaletteHex(slot)),
            selected == Some(slot),
        );
    }
    PREVIEW_H + 12.0 + (4 / columns) as f32 * 34.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn featured_palettes_use_four_columns_or_two_compact_rows() {
        for width in [280.0, 430.0, 560.0, 800.0] {
            let (rects, height) = palette_strip(20.0, 40.0, width);
            assert_eq!(height, if width >= 560.0 { 104.0 } else { 208.0 });
            for (index, rect) in rects.iter().enumerate() {
                assert!(rect.0 >= 20.0 && rect.0 + rect.2 <= 20.0 + width);
                assert!(rect.1 >= 40.0 && rect.1 + rect.3 <= 40.0 + height);
                for other in rects.iter().skip(index + 1) {
                    let overlap_w = (rect.0 + rect.2).min(other.0 + other.2) - rect.0.max(other.0);
                    let overlap_h = (rect.1 + rect.3).min(other.1 + other.3) - rect.1.max(other.1);
                    assert!(overlap_w <= 0.0 || overlap_h <= 0.0);
                }
            }
        }
    }

    #[test]
    fn preview_regions_stay_inside_the_frame_without_overlapping() {
        for width in [280.0, 430.0, 600.0, 800.0] {
            let areas = regions(20.0, 40.0, width);
            for (index, (_, a)) in areas.iter().enumerate() {
                assert!(a.0 >= 20.0 && a.1 >= 40.0 && a.2 > 0.0 && a.3 >= 26.0);
                assert!(a.0 + a.2 <= 20.0 + width && a.1 + a.3 <= 40.0 + PREVIEW_H);
                for (_, b) in areas.iter().skip(index + 1) {
                    let overlap_w = (a.0 + a.2).min(b.0 + b.2) - a.0.max(b.0);
                    let overlap_h = (a.1 + a.3).min(b.1 + b.3) - a.1.max(b.1);
                    assert!(overlap_w <= 0.0 || overlap_h <= 0.0);
                }
            }
        }
    }

    #[test]
    fn each_surface_has_an_independent_editor_target() {
        let areas = regions(20.0, 40.0, 600.0);
        assert_eq!(areas.map(|(slot, _)| slot), [30, 29, 28, 27, 31]);
        for (slot, rect) in areas {
            let center = (rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0);
            let matches: Vec<_> = areas.iter().filter(|(_, area)| contains(*area, center)).map(|(slot, _)| *slot).collect();
            assert_eq!(matches, [slot]);
        }
    }
}
