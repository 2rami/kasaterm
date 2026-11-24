use super::*;

pub(super) fn preview_height(available: f32) -> f32 {
    (available * 0.32).clamp(100.0, 360.0)
}
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
    let inset = 8.0;
    let gap = 4.0;
    let inner_w = (w - inset * 2.0).max(0.0);
    let count = theme::ACCENT_PRESETS.len();
    let columns = (((inner_w + gap) / 64.0).floor() as usize).clamp(1, count);
    let cell_w = ((inner_w - (columns - 1) as f32 * gap) / columns as f32).min(72.0);
    let rows = count.div_ceil(columns);
    let height = rows as f32 * 64.0 + inset * 2.0;
    let tray = (x, y, columns as f32 * cell_w + (columns - 1) as f32 * gap + inset * 2.0, height);
    let surface = theme::surface();
    round_rect(g, tray.0, tray.1, tray.2, tray.3, theme::radius_md(), surface);
    stroke_round(g, tray, theme::radius_md(), theme::edge_on(surface));
    for (i, (name, color)) in theme::ACCENT_PRESETS.iter().enumerate() {
        let rect = (x + inset + (i % columns) as f32 * (cell_w + gap), y + inset + (i / columns) as f32 * 64.0, cell_w, 58.0);
        let hot = contains(rect, s.cursor);
        let selected = s.accent == *name;
        let cx = rect.0 + (cell_w - 36.0) * 0.5;
        circle_rect(g, cx, rect.1, 36.0, *color);
        if selected {
            g.queue_icon("check", cx + 10.0, rect.1 + 10.0, 16.0, theme::foreground_on(*color));
        }
        if selected || hot {
            stroke_round(g, (cx - 3.0, rect.1 - 3.0, 42.0, 42.0), 21.0 * theme::roundness(), theme::enforce_contrast_at(*color, theme::surface(), 3.0));
        }
        let label = preset_label(name);
        let width = g.measure_chrome_text(label, 10.5, false);
        draw_text(g, rect.0 + (cell_w - width) * 0.5, rect.1 + 44.0, label, 10.5, theme::text_dim(), false);
        register_clipped(g, hits, Target::AccentPreset(name), rect, HitCursor::Pointer);
        g.hover_pointer |= hot;
    }
    height
}

pub(super) fn accent_column(width: f32) -> Option<f32> {
    (width >= 850.0).then(|| (width * 0.35).clamp(285.0, 360.0))
}

pub(super) fn paint(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    x: f32,
    y: f32,
    w: f32,
    height: f32,
) -> f32 {
    let accent = s.input == Some(SettingsInput::AccentHex);
    let side = if accent { accent_column(w) } else { None };
    let pane_width = side.map_or(w, |column| w - column - 16.0);
    let pane_height = paint_shell(g, s, hits, x, y, pane_width, height);
    if !accent { return pane_height; }
    let (panel_x, panel_y, panel_w) = side.map_or((x, y + pane_height + 10.0, w),
        |column| (x + pane_width + 16.0, y, column));
    draw_text(g, panel_x, panel_y + 8.0, "강조색", 12.0, theme::text(), true);
    let picker_end = panel_y + CTL_H + 8.0 + paint_color_picker(
        g, s, hits, caret, panel_x, panel_y + CTL_H + 8.0, panel_w,
        SettingsInput::AccentHex, "강조색", &s.palette_edit,
        s.eyedropper.then_some(SettingsAction::ColorEyedropper(SettingsInput::AccentHex)),
    );
    pane_height.max(picker_end - y)
}

fn paint_shell(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    x: f32,
    y: f32,
    w: f32,
    height: f32,
) -> f32 {
    let shell = &s.appearance_shell;
    let label = if shell.running { "셸 닫기" } else { "셸 열기" };
    let target = if shell.running { Target::ClosePreviewShell } else { Target::PreviewShell };
    let width = crate::native_controls::button_width(g, &crate::native_strings::text(label), CTL_H, false);
    button(g, s, hits, (x + (w - width).max(0.0), y, width.min(w), CTL_H), label, target, false);
    let shown = fit(g, "터미널 미리보기", (w - width - 14.0).max(0.0), 12.0, true);
    draw_text(g, x, y + 8.0, &shown, 12.0, theme::text(), true);
    let top = y + CTL_H + 8.0;
    let pane = (x, top, w, height);
    let app_layer = theme::without_settings_layer();
    crate::native_controls::panel(g, pane, theme::pane_bg());
    let ink = theme::enforce_min_contrast(theme::fg(), theme::pane_bg());
    if shell.focused || s.input == Some(SettingsInput::AccentHex) {
        stroke_round(g, pane, theme::radius_md(), theme::accent());
    }
    let body = (x + 12.0, top + 9.0, (w - 24.0).max(0.0), height - 18.0);
    register_clipped(g, hits, Target::PreviewShell, pane, HitCursor::Text);

    let (cw, ch) = (g.cell_w.max(1.0), g.cell_h.max(1.0));
    let cols = (body.2 / cw).floor().max(1.0) as u16;
    let rows = (body.3 / ch).floor().max(1.0) as u16;
    PAINT_FEEDBACK.with(|feedback| feedback.borrow_mut().preview_shell_size = Some((cols, rows)));
    if let Some((px, py, pw, ph)) = g.clip_hit(pane).filter(|r| r.2 >= 32.0 && r.3 >= 32.0) {
        PAINT_FEEDBACK.with(|feedback| feedback.borrow_mut().weather_preview = Some(crate::weather::SettingsPreview {
            rect: [px, py, pw, ph], phase: s.weather_phase, popup: None,
        }));
    }
    if let Some(visible) = g.clip_hit(body) {
        let first = ((visible.1 - body.1) / ch).ceil().max(0.0) as usize;
        let end = ((visible.1 + visible.3 - body.1) / ch).floor().max(0.0) as usize;
        let end = end.min(rows as usize).min(shell.rows.len());
        if first < end {
            let scale = g.scale();
            let lines = &shell.rows[first..end];
            let clipped;
            let lines = if lines.iter().any(|line| line.len() > cols as usize) {
                clipped = lines.iter().map(|line| line[..line.len().min(cols as usize)].to_vec()).collect::<Vec<_>>();
                clipped.as_slice()
            } else { lines };
            g.draw_cells(&[gpu::PaneSlot {
                rows: lines, origin_px: (body.0 * scale, (body.1 + first as f32 * ch) * scale),
                font_scale: 1.0, dim: false, links: Vec::new(), default_fg: ink, source: None,
            }]);
        }
        if shell.focused {
            if let Some((row, col)) = shell.cursor {
                let cursor = (body.0 + col as f32 * cw, body.1 + row as f32 * ch, cw, ch);
                if row < rows && col < cols && g.clip_hit(cursor) == Some(cursor) {
                    stroke_rect(g, cursor, theme::cursor_on_pane(theme::cursor()));
                }
            }
        }
    }
    if !shell.running && shell.rows.is_empty() {
        let message = shell.error.as_deref().unwrap_or("셸을 열고 입력을 테스트하세요");
        let message = fit(g, message, (w - 28.0).max(0.0), 12.0, false);
        g.centered_chrome_text(pane, &message, gpu::DrawOpts { font_size: 12.0, color: ink, bold: false, italic: false });
    }
    drop(app_layer);

    let controls = [
        ("터미널 바탕", Target::Setting(SettingsAction::FocusPaletteHex(27)), s.input == Some(SettingsInput::PaletteHex(27))),
        ("터미널 글자", Target::Setting(SettingsAction::FocusPaletteHex(1)), s.input == Some(SettingsInput::PaletteHex(1))),
        ("색 고르기", Target::OpenAccentPicker, s.input == Some(SettingsInput::AccentHex)),
    ];
    let buttons = button_row(g, s, hits, (x, top + height + 10.0, w, 0.0), &controls, crate::native_layout::Align::Start);
    CTL_H + 8.0 + height + 10.0 + buttons
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
    fn accent_picker_uses_the_side_only_when_the_pane_can_stay_wide() {
        assert!(accent_column(800.0).is_none());
        for width in [850.0, 960.0, 1120.0] {
            let picker = accent_column(width).unwrap();
            assert!(picker >= 285.0);
            assert!(width - picker - 16.0 >= 500.0);
        }
    }

    #[test]
    fn preview_stays_readable_on_compact_and_four_k_layouts() {
        for scale in [1.0, 1.5, 2.0] {
            let width = 3840.0 / scale;
            let visible_height = 2160.0 / scale - 100.0;
            let (x, pane_width) = appearance_content_column(0.0, width, 208.0);
            assert_eq!(pane_width, 1120.0);
            assert!(x >= 208.0 && x + pane_width < width);
            assert!(preview_height(visible_height) + 252.0 < visible_height);
        }
        let (x, width) = appearance_content_column(0.0, 390.0, 0.0);
        assert_eq!((x, width), (16.0, 358.0));
        assert_eq!(preview_height(150.0), 100.0);
    }
}
