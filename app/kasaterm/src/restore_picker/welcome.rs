use super::*;
use crate::native_controls::{icon_button, text_button, Style};

#[derive(Debug)]
struct Layout {
    card: Rect,
    rail: Rect,
    preview: Rect,
    footer: Rect,
    pad: f32,
    dense: bool,
    slots: usize,
    tile: f32,
    frame: f32,
}

fn layout(viewport: (f32, f32), count: usize) -> Layout {
    let w = (viewport.0 - 32.0).clamp(0.0, 560.0);
    let h = (viewport.1 - 32.0).clamp(0.0, 324.0);
    let x = (viewport.0 - w) / 2.0;
    let y = (viewport.1 - h) / 2.0;
    let dense = h < 280.0;
    let pad = if w < 420.0 { 16.0 } else { 24.0 };
    let inner = (w - 2.0 * pad).max(0.0);
    let (header, footer) = if dense { (42.0, 36.0) } else { (80.0, 80.0) };
    let preview_h = if dense { 34.0 } else { 64.0 };
    let rail_h = (h - header - footer - preview_h).max(0.0);
    let tile = if dense { 48.0 } else { 64.0 };
    let frame = if dense { 40.0 } else { 56.0 };
    let plain_slots = ((inner + 8.0) / (tile + 8.0)).floor().max(1.0) as usize;
    let paged = count > plain_slots;
    let rail_w = (inner - if paged { 48.0 } else { 0.0 }).max(0.0);
    let slots = ((rail_w + 8.0) / (tile + 8.0)).floor().max(1.0) as usize;
    Layout {
        card: (x, y, w, h),
        rail: (
            x + pad + if paged { 24.0 } else { 0.0 },
            y + header,
            rail_w,
            rail_h,
        ),
        preview: (x + pad, y + header + rail_h, inner, preview_h),
        footer: (x + pad, y + h - footer, inner, footer),
        pad,
        dense,
        slots,
        tile,
        frame,
    }
}

pub(super) fn range(count: usize, focus: usize, slots: usize) -> std::ops::Range<usize> {
    let slots = slots.max(1);
    let start = focus.min(count.saturating_sub(1)) / slots * slots;
    start..(start + slots).min(count)
}

fn portrait(
    g: &mut gpu::GpuRenderer,
    row: &crate::session::restore_selection::Row,
    r: Rect,
    chosen: bool,
    focused: bool,
) {
    let color = theme::lerp(
        theme::surface(),
        theme::accent(),
        if chosen { 0.12 } else { 0.02 },
    );
    g.round_rect_fill(r.0, r.1, r.2, r.3, theme::radius_md(), color);
    if focused || chosen {
        g.round_rect_stroke(
            r.0,
            r.1,
            r.2,
            r.3,
            theme::radius_md(),
            if focused { 2.0 } else { 1.0 },
            if focused {
                theme::enforce_contrast_at(theme::accent(), color, 3.0)
            } else {
                theme::with_alpha(theme::accent(), 80)
            },
        );
    }
    let face = if r.2 >= 56.0 { 40.0 } else { 28.0 };
    let inset = (r.2 - face) / 2.0;
    if row.character.is_empty()
        || !crate::sprites::draw_student_face(g, &row.character, r.0 + inset, r.1 + inset, face)
    {
        let size = face * 0.7;
        g.queue_icon(
            row.icon,
            r.0 + (r.2 - size) / 2.0,
            r.1 + (r.3 - size) / 2.0,
            size,
            theme::text_dim(),
        );
    }
    if chosen {
        let bx = r.0 + r.2 - 10.0;
        let by = r.1 + r.3 - 10.0;
        g.round_rect_fill(bx - 2.0, by - 2.0, 18.0, 18.0, 9.0, theme::bg());
        g.round_rect_fill(bx, by, 14.0, 14.0, 7.0, theme::accent());
        g.queue_icon(
            "check",
            bx + 1.0,
            by + 1.0,
            12.0,
            theme::foreground_on(theme::accent()),
        );
    }
}

pub(super) fn paint(
    g: &mut gpu::GpuRenderer,
    state: &mut State,
    viewport: (f32, f32),
    cursor: (f32, f32),
) -> Vec<(RestoreBtn, Rect)> {
    let l = layout(viewport, state.plan.rows.len());
    state.welcome_slots = l.slots;
    let visible = range(state.plan.rows.len(), state.focus, l.slots);
    let (x, y, w, h) = l.card;
    let mut hits = Vec::new();
    g.rect(
        0.0,
        0.0,
        viewport.0,
        viewport.1,
        theme::with_alpha([0, 0, 0, 255], 125),
    );
    g.round_rect_fill(x, y, w, h, theme::radius_md(), theme::bg());
    g.round_rect_stroke(x, y, w, h, theme::radius_md(), 1.0, theme::border());
    g.push_clip(x, y, w, h);
    let title_y = y + if l.dense { 9.0 } else { 24.0 };
    label(
        g,
        (x + l.pad, title_y, w - 2.0 * l.pad - 90.0, 24.0),
        "이어서 해볼까요?",
        if l.dense { 16.0 } else { 20.0 },
        true,
        theme::text(),
    );
    let close = icon_button(
        g,
        (x + w - l.pad - 26.0, title_y - 5.0, 26.0, 28.0),
        cursor,
        "x",
        Style::default(),
    );
    hits.push((RestoreBtn::Dismiss, close));
    let details = text_button(
        g,
        (x + w - l.pad - 84.0, title_y - 5.0, 50.0, 28.0),
        cursor,
        "상세",
        Style::default(),
    );
    hits.push((RestoreBtn::Expand, details));
    if !l.dense {
        label(
            g,
            (x + l.pad, y + 54.0, w - 2.0 * l.pad, 16.0),
            &state.plan.saved_at,
            11.5,
            false,
            theme::text_dim(),
        );
    }
    let total_w = visible.len() as f32 * l.tile + visible.len().saturating_sub(1) as f32 * 8.0;
    let start_x = l.rail.0 + (l.rail.2 - total_w) / 2.0;
    let rail_y =
        l.rail.1 + ((l.rail.3 - l.frame - if l.dense { 12.0 } else { 22.0 }) / 2.0).max(0.0);
    let mut peek = state.focus;
    g.push_clip(l.rail.0, l.rail.1, l.rail.2, l.rail.3);
    for (slot, i) in visible.clone().enumerate() {
        let row = &state.plan.rows[i];
        let r = (
            start_x + slot as f32 * (l.tile + 8.0),
            rail_y,
            l.tile,
            l.frame + 20.0,
        );
        let hover = clipped(r, l.rail).is_some_and(|hit| inside(hit, cursor));
        if hover {
            peek = i;
        }
        portrait(
            g,
            row,
            (r.0 + (l.tile - l.frame) / 2.0, r.1, l.frame, l.frame),
            state.selected[i],
            hover || (state.control_focus.is_none() && state.focus == i),
        );
        let name = if row.character.is_empty() {
            &row.title
        } else {
            &row.character
        };
        let caption_size = if l.dense { 10.0 } else { 11.0 };
        let shown = crate::info::fit_text(g, name, l.tile, caption_size, false);
        let tw = g.measure_chrome_text(&shown, caption_size, false);
        label(
            g,
            (
                r.0 + (l.tile - tw) / 2.0,
                r.1 + l.frame + if l.dense { 4.0 } else { 8.0 },
                tw + 1.0,
                14.0,
            ),
            &shown,
            caption_size,
            false,
            if state.selected[i] {
                theme::text()
            } else {
                theme::text_mute()
            },
        );
        if let Some(hit) = clipped(r, l.rail) {
            hits.push((RestoreBtn::Toggle(i), hit));
            g.hover_pointer |= inside(hit, cursor);
        }
    }
    g.pop_clip();
    if state.plan.rows.len() > l.slots {
        let arrow_y = rail_y + (l.frame - 26.0) / 2.0;
        for (action, icon, rx, enabled) in [
            (
                RestoreBtn::Previous,
                "chevron-left",
                x + l.pad - 4.0,
                visible.start > 0,
            ),
            (
                RestoreBtn::Next,
                "chevron-right",
                x + w - l.pad - 22.0,
                visible.end < state.plan.rows.len(),
            ),
        ] {
            let r = icon_button(
                g,
                (rx, arrow_y, 26.0, 28.0),
                cursor,
                icon,
                Style {
                    enabled,
                    ..Style::default()
                },
            );
            if enabled {
                hits.push((action, r));
            }
        }
    }
    g.push_clip(l.preview.0, l.preview.1, l.preview.2, l.preview.3);
    if let Some(row) = state.plan.rows.get(peek) {
        label(
            g,
            (l.preview.0, l.preview.1 + 4.0, l.preview.2, 18.0),
            &row.title,
            13.0,
            true,
            theme::text(),
        );
        let chosen = if state.selected[peek] {
            "선택됨"
        } else {
            "선택 안 됨"
        };
        let detail = if l.dense {
            format!("{chosen} · {}", row.identity)
        } else {
            format!("{chosen} · {} · 방 {}", row.identity, row.group + 1)
        };
        label(
            g,
            (
                l.preview.0,
                l.preview.1 + if l.dense { 20.0 } else { 24.0 },
                l.preview.2,
                16.0,
            ),
            &detail,
            11.5,
            false,
            theme::text_dim(),
        );
        if !l.dense {
            label(
                g,
                (l.preview.0, l.preview.1 + 43.0, l.preview.2, 16.0),
                if row.folder.is_empty() {
                    "작업 폴더 기록 없음"
                } else {
                    &row.folder
                },
                11.5,
                false,
                theme::text_mute(),
            );
        }
    }
    g.pop_clip();
    if !l.dense {
        let note = format!(
            "{} / {}개 선택 · 고르지 않은 작업은 보관돼요",
            state.count(),
            state.selected.len()
        );
        label(
            g,
            (l.footer.0, l.footer.1 + 10.0, l.footer.2, 16.0),
            &note,
            11.5,
            false,
            theme::text_dim(),
        );
    }
    let by = l.footer.1 + if l.dense { 4.0 } else { 40.0 };
    let usable = (l.footer.2 - 16.0).max(0.0);
    let primary_w = (usable * 0.42).min(134.0);
    let choose_w = (usable * 0.32).min(100.0);
    let fresh_w = (usable - primary_w - choose_w).min(94.0);
    let primary_x = l.footer.0 + l.footer.2 - primary_w;
    let all = state.count() == state.selected.len();
    let toggle_label = if choose_w < 76.0 {
        if all {
            "해제"
        } else {
            "전체"
        }
    } else if all {
        "전체 해제"
    } else {
        "전체 선택"
    };
    let restore_label = if primary_w < 100.0 {
        format!("{}개 열기", state.count())
    } else {
        format!("{}개 이어가기", state.count())
    };
    for (action, label, rect, primary, enabled) in [
        (
            RestoreBtn::Fresh,
            if fresh_w < 76.0 {
                "새로"
            } else {
                "새로 시작"
            },
            (l.footer.0, by, fresh_w, 28.0),
            false,
            true,
        ),
        (
            if all {
                RestoreBtn::Clear
            } else {
                RestoreBtn::All
            },
            toggle_label,
            (primary_x - 8.0 - choose_w, by, choose_w, 28.0),
            false,
            true,
        ),
        (
            RestoreBtn::Restore,
            restore_label.as_str(),
            (primary_x, by, primary_w, 28.0),
            true,
            state.count() > 0,
        ),
    ] {
        let r = text_button(
            g,
            rect,
            cursor,
            label,
            Style {
                primary,
                enabled,
                ..Style::default()
            },
        );
        if enabled {
            hits.push((action, r));
        }
    }
    for (action, rect) in &hits {
        if state.control_focus == Some(*action) {
            crate::native_controls::focus_ring(g, *rect);
        }
    }
    g.pop_clip();
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn face_pages_cover_every_task_without_an_extra_restore_step() {
        for count in [0, 1, 6, 25, 100] {
            for width in [280.0, 390.0, 640.0, 1600.0] {
                let l = layout((width, 780.0), count);
                for focus in 0..count {
                    let visible = range(count, focus, l.slots);
                    assert!(visible.contains(&focus));
                    assert!(visible.end <= count && visible.len() <= l.slots);
                    let occupied = visible.len() as f32 * l.tile
                        + visible.len().saturating_sub(1) as f32 * 8.0;
                    assert!(occupied <= l.rail.2 + 0.01);
                }
            }
        }
    }
    #[test]
    fn welcome_stays_small_and_keeps_portraits_preview_and_actions_apart() {
        for w in [280.0, 390.0, 640.0, 1920.0] {
            for h in [200.0, 240.0, 360.0, 780.0] {
                let l = layout((w, h), 20);
                assert!(l.card.2 <= 560.0 && l.card.3 <= 324.0);
                for r in [l.card, l.rail, l.preview, l.footer] {
                    assert!(r.0 >= 0.0 && r.1 >= 0.0 && r.0 + r.2 <= w && r.1 + r.3 <= h);
                }
                assert!(l.rail.1 + l.rail.3 <= l.preview.1);
                assert!(l.preview.1 + l.preview.3 <= l.footer.1);
                assert!(l.rail.3 >= l.frame);
                assert!(l.footer.3 >= 36.0);
            }
        }
    }
}
