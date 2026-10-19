use super::*;

pub(super) fn paint(g: &mut gpu::GpuRenderer, sb: &mut state::StatusbarState, cursor: (f32, f32), anchor: (f32, f32, f32, f32), viewport: (f32, f32)) {
    let Some(identity) = sb.usage_inspected.clone() else { return };
    if sb.usage_process_armed.as_ref().is_some_and(|(_, at)| at.elapsed().as_secs() >= 5) { sb.usage_process_armed = None; }
    let rows = crate::process_usage::descendants(&sb.usage_processes, &identity);
    let w = 420.0_f32.min((viewport.0 - 16.0).max(1.0));
    let body = rows.len().max(1) as f32 * 44.0 + 8.0;
    let h = (44.0 + body).min((anchor.1 - 14.0).min(viewport.1 - 16.0).max(1.0));
    let x = (anchor.0 + anchor.2 - w).clamp(8.0, (viewport.0 - w - 8.0).max(8.0));
    let y = (anchor.1 - h - 6.0).max(8.0);
    sb.popover_rect = Some((x, y, w, h));
    panel_rect_outlined(g, x, y, w, h, theme::radius_md(), theme::surface());
    let back = (x + 8.0, y + 6.0, 32.0, 32.0);
    g.queue_icon("chevron-left", back.0 + 8.0, back.1 + 8.0, 16.0, theme::text());
    sb.popover_hits.push((state::StatusbarHit::UsageBack, back));
    g.hover_pointer |= hit(cursor, &back);
    let title = rows.first().map_or("종료된 실행".into(), |row| format!("{} · 실행 {}개", row.name, rows.len()));
    let title = crate::info::fit_text(g, &title, (w - 56.0).max(0.0), 12.0, true);
    g.draw_text(x + 44.0, y + 14.0, &title, gpu::DrawOpts { font_size: 12.0, color: theme::text(), bold: true, italic: false });
    let inner = (h - 44.0).max(0.0);
    sb.popover_scroll = sb.popover_scroll.clamp(0.0, (body - inner).max(0.0));
    g.push_clip(x + 6.0, y + 44.0, (w - 12.0).max(0.0), inner);
    let mut ry = y + 48.0 - sb.popover_scroll;
    if rows.is_empty() {
        g.draw_text(x + 14.0, ry + 8.0, "이 실행은 종료됐거나 바뀌었어요", gpu::DrawOpts { font_size: 11.0, color: theme::text_dim(), bold: false, italic: false });
    }
    for row in &rows {
        let can_stop = crate::process_usage::stoppable(row, &sb.usage_processes);
        let can_open = row.owner.as_ref().is_some_and(|owner| owner.shell == row.id.pid && owner.needs_open);
        let armed = sb.usage_process_armed.as_ref().is_some_and(|(id, _)| id == &row.id);
        let button_label = if can_open { "열기" } else if armed { "종료 확인" } else { "종료" };
        let button_w = if can_stop || can_open { crate::native_controls::button_width(g, button_label, 26.0, false) } else { 0.0 };
        let value = if sb.usage_tab == state::UsageTab::Cpu { format!("{:.1}%", row.cpu) }
            else if row.rss >= 1024 * 1024 * 1024 { format!("{:.1}G", row.rss as f64 / 1073741824.0) }
            else { format!("{:.0}M", row.rss as f64 / 1048576.0) };
        let value_w = g.measure_chrome_text(&value, 11.0, true);
        let right = x + w - 14.0 - if can_stop || can_open { button_w + 8.0 } else { 0.0 };
        let name_w = (right - value_w - x - 24.0).max(0.0);
        let name = crate::info::fit_text(g, &format!("{} · {}", row.name, row.id.pid), name_w, 11.0, false);
        g.draw_text(x + 14.0, ry + 4.0, &name, gpu::DrawOpts { font_size: 11.0, color: theme::text(), bold: false, italic: false });
        g.draw_text(right - value_w, ry + 4.0, &value, gpu::DrawOpts { font_size: 11.0, color: theme::text(), bold: true, italic: false });
        let detail = if let Some(owner) = &row.owner {
            if let Some(cwd) = row.cwd.as_deref() { format!("{} · {cwd}", owner.label) } else { owner.label.clone() }
        } else { row.cwd.clone().unwrap_or_else(|| row.id.exe.clone()) };
        let detail = crate::info::fit_text(g, &detail, (right - x - 14.0).max(0.0), 10.0, false);
        g.draw_text(x + 14.0, ry + 23.0, &detail, gpu::DrawOpts { font_size: 10.0, color: theme::text_dim(), bold: false, italic: false });
        if can_stop || can_open {
            let rect = crate::native_controls::content_button(g, (x + w - 14.0 - button_w, ry + 7.0, button_w, 26.0), cursor, button_label,
                crate::native_controls::Style { danger: armed, ..Default::default() });
            if let Some(rect) = g.clip_hit(rect) { sb.popover_hits.push((if can_open { state::StatusbarHit::OpenUsageShell(row.id.clone()) } else { state::StatusbarHit::StopProcess(row.id.clone()) }, rect)); }
        }
        ry += 44.0;
    }
    g.pop_clip();
    if body > inner && inner > 0.0 {
        let thumb = (inner * inner / body).max(16.0).min(inner);
        let top = y + 44.0 + (inner - thumb) * sb.popover_scroll / (body - inner);
        round_rect(g, x + w - 5.0, top, 3.0, thumb, 1.5, theme::text_mute());
    }
}
