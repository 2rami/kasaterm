//! 덮어도 마저 하기 팝오버 — 켜고 끄기와 지금 하는 일. 최대 시간 같은 세부는 설정 「기기」에 둔다.

use super::*;
use crate::lid_away::Glance;

const HEAD: f32 = 60.0;
const LINE: f32 = 15.0;
const FOOT: f32 = 36.0;

pub(super) fn paint(
    g: &mut gpu::GpuRenderer,
    sb: &mut state::StatusbarState,
    cursor: (f32, f32),
    anchor: (f32, f32, f32, f32),
    win_w: f32,
) {
    let glance = crate::lid_away::glance();
    let w = 300.0_f32.min((win_w - 16.0).max(200.0));
    let on = !matches!(glance, Glance::Off);
    let status = crate::lid_away::status_line().unwrap_or_default();
    let note = crate::lid_away::note(on, crate::lid_away::sudo_ready(), status);
    let lines = crate::native_settings::wrap_words(g, note, w - 2.0 * PADX, 10.5);
    let h = HEAD + 10.0 + lines.len() as f32 * LINE + 6.0 + FOOT;
    let x = (anchor.0 + anchor.2 - w).clamp(8.0, (win_w - w - 8.0).max(8.0));
    let y = (anchor.1 - h - 6.0).max(8.0);
    sb.popover_rect = Some((x, y, w, h));
    panel_rect_outlined(g, x, y, w, h, theme::radius_md(), theme::surface());
    let text = |g: &mut gpu::GpuRenderer, xx, yy, value: &str, size, color, bold| {
        g.draw_text(xx, yy, value, gpu::DrawOpts { font_size: size, color, bold, italic: false });
    };
    g.queue_icon("moon", x + 16.0, y + 15.0, 18.0, theme::accent());
    text(g, x + 42.0, y + 15.0, "덮어도 마저 하기", 13.0, theme::text(), true);
    let status_color = match glance {
        Glance::On | Glance::Awake => theme::success(),
        Glance::NeedsSetup => theme::attention(),
        Glance::Off | Glance::Checking => theme::text_mute(),
    };
    round_rect(g, x + 43.0, y + 38.0, 5.0, 5.0, 2.5, status_color);
    text(g, x + 54.0, y + 35.0, glance.label(), 10.0, status_color, false);
    let sw = (x + w - 52.0, y + 15.0, 36.0, 20.0);
    crate::settings::toggle(g, sw, on, cursor);
    sb.popover_hits.push((state::StatusbarHit::ToggleLidAway, sw));

    g.rect(x + PADX, y + HEAD, w - 2.0 * PADX, 1.0, theme::border());
    let mut ly = y + HEAD + 10.0;
    for line in &lines {
        text(g, x + PADX, ly, line, 10.5, theme::text_dim(), false);
        ly += LINE;
    }

    let row = (x + 7.0, y + h - FOOT + 2.0, w - 14.0, FOOT - 8.0);
    let hovered = hit(cursor, &row);
    if hovered {
        hover_rect(g, row.0, row.1, row.2, row.3, theme::radius_sm());
    }
    g.queue_icon("settings-2", x + PADX, row.1 + 8.0, 12.0, theme::text_dim());
    text(g, x + PADX + 19.0, row.1 + 8.0, "기기 설정 열기", 10.5, theme::text(), false);
    g.hover_pointer |= hovered;
    sb.popover_hits.push((state::StatusbarHit::OpenMachines, row));
}
