use super::*;

const PREVIEW_H: f32 = 204.0;
pub(super) const FEATURED_PALETTES: [&str; 4] = ["graphite", "ink", "paper", "mist"];

fn palette_strip(x: f32, y: f32, w: f32) -> ([Rect; 4], f32) {
    let columns = if w >= 560.0 { 4 } else { 2 };
    let item_w = (w - (columns - 1) as f32 * 8.0) / columns as f32;
    let rects = std::array::from_fn(|index| {
        (
            x + (index % columns) as f32 * (item_w + 8.0),
            y + (index / columns) as f32 * 72.0,
            item_w,
            64.0,
        )
    });
    (rects, (4 / columns) as f32 * 72.0)
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
        let preview = (rect.0, rect.1, rect.2, 32.0);
        g.rect(preview.0, preview.1, preview.2, preview.3, colors.pane);
        g.rect(preview.0, preview.1, preview.2, 8.0, colors.header);
        let sidebar = (preview.2 * 0.22).floor();
        g.rect(preview.0, preview.1 + 8.0, sidebar, 24.0, colors.sidebar);
        g.rect(
            preview.0 + preview.2 - sidebar,
            preview.1 + 8.0,
            sidebar,
            24.0,
            colors.sidebar,
        );
        g.rect(preview.0, preview.1 + 8.0, preview.2, 1.0, palette.border);
        for (line, portion) in [0.40, 0.26].into_iter().enumerate() {
            g.rect(
                preview.0 + sidebar + 8.0,
                preview.1 + 15.0 + line as f32 * 7.0,
                preview.2 * portion,
                1.5,
                palette.text_dim,
            );
        }
        let hover = contains(preview, s.cursor);
        stroke_round(
            g,
            preview,
            theme::radius_sm(),
            if selected {
                theme::accent()
            } else if hover {
                theme::text_dim()
            } else {
                theme::border()
            },
        );
        register_clipped(
            g,
            hits,
            Target::Setting(SettingsAction::ThemeMode(key.into())),
            preview,
            HitCursor::Pointer,
        );
        g.hover_pointer |= hover;
        button(
            g,
            s,
            hits,
            (rect.0, rect.1 + 38.0, rect.2, 26.0),
            label,
            Target::Setting(SettingsAction::ThemeMode(key.into())),
            selected,
        );
    }
    height
}

fn regions(x: f32, y: f32, w: f32) -> [(usize, Rect); 5] {
    let header = crate::native_controls::CONTROL_HEIGHT;
    let sidebar = (w * 0.22).floor();
    let panel = (w * 0.24).floor();
    let center = w - sidebar - panel;
    [
        (28, (x, y, w, header)),
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
        (29, (x + w - panel, y + header, panel, PREVIEW_H - header)),
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
            _ => theme::sidebar_bg(),
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
    let header_ink = theme::text();
    let side_ink = theme::text();
    let pane_ink = theme::enforce_min_contrast(theme::fg(), theme::pane_bg());
    text(g, areas[0].1, "kasaterm · 탭과 헤더", header_ink, true);
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
            side_ink,
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
            assert_eq!(height, if width >= 560.0 { 72.0 } else { 144.0 });
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
}
