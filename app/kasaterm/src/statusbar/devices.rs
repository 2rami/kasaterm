use super::*;

type Rect = (f32, f32, f32, f32);

fn text(g: &mut gpu::GpuRenderer, x: f32, y: f32, value: &str, width: f32, size: f32, color: [u8; 4], bold: bool) {
    let value = crate::info::fit_text(g, value, width.max(0.0), size, bold);
    g.draw_text(x, y, &value, gpu::DrawOpts { font_size: size, color, bold, italic: false });
}

fn action(g: &mut gpu::GpuRenderer, sb: &mut state::StatusbarState, cursor: (f32, f32), slot: Rect, label: &str, target: state::StatusbarHit) {
    let rect = crate::native_controls::content_button(g, slot, cursor, label, Default::default());
    if let Some(rect) = g.clip_hit(rect) { sb.popover_hits.push((target, rect)); }
}

/// 계정 줄 높이 — 계정 한 줄, 막힌 사유가 있으면 한 줄 더.
fn account_h(account: Option<&crate::device_accounts::DeviceAccount>) -> f32 {
    account.map_or(0.0, |a| if a.warning.is_some() { 34.0 } else { 17.0 })
}

/// 기기 이름 아래에 그 기기의 Claude 계정과 막힌 사유를 그린다.
fn account_lines(g: &mut gpu::GpuRenderer, x: f32, y: f32, width: f32, account: Option<&crate::device_accounts::DeviceAccount>) {
    let Some(account) = account else { return };
    text(g, x, y, &account.line, width, 10.5, theme::text_dim(), false);
    if let Some(warning) = &account.warning {
        text(g, x, y + 17.0, warning, width, 10.5, theme::danger(), false);
    }
}

pub(super) fn paint(g: &mut gpu::GpuRenderer, sb: &mut state::StatusbarState, cursor: (f32, f32), anchor: Rect, viewport: (f32, f32)) {
    let machines = kasa_device::machines::snapshot();
    let entries = kasa_device::machines::entries();
    let w = 420.0_f32.min((viewport.0 - 16.0).max(1.0));
    let stacked = w < 350.0;
    let local_account = sb.local_account.clone();
    let local_h = if stacked { 96.0 } else { 66.0 } + account_h(local_account.as_ref());
    let rows: Vec<_> = machines.iter().map(|machine| {
        let label = machine["label"].as_str().unwrap_or_default();
        let online = machine["online"].as_bool() == Some(true);
        let differs = online && machine["build"].as_str().is_some_and(|b| !b.is_empty()) && machine["build_match"].as_bool() == Some(false);
        let install = differs.then(|| super::build_send_target(label, &entries)).flatten();
        let base = machine["base"].as_str().unwrap_or_default();
        if online && machine["guest"].as_bool() != Some(true) {
            crate::device_accounts::poll(base);
        }
        let account = crate::device_accounts::get(base);
        (machine, label, online, differs, install, account)
    }).collect();
    let row_h = |install: bool, account: Option<&crate::device_accounts::DeviceAccount>| {
        (if stacked && install { 94.0 } else { 62.0 }) + account_h(account)
    };
    let body = local_h + rows.iter().map(|(_, _, _, _, install, account)| row_h(install.is_some(), account.as_ref())).sum::<f32>();
    let h = (44.0 + body + 48.0).min((anchor.1 - 14.0).min(viewport.1 - 16.0).max(1.0));
    let x = (anchor.0 + anchor.2 - w).clamp(8.0, (viewport.0 - w - 8.0).max(8.0));
    let y = (anchor.1 - h - 6.0).max(8.0);
    sb.popover_rect = Some((x, y, w, h));
    panel_rect_outlined(g, x, y, w, h, theme::radius_md(), theme::surface());
    text(g, x + 14.0, y + 14.0, "기기 · 업데이트", w - 28.0, 13.0, theme::text(), true);
    let summary = crate::machinescol::status_links_summary(&crate::machinescol::status_links());
    if w >= 390.0 && !summary.is_empty() {
        let label = crate::info::fit_text(g, &summary, w - 190.0, 10.0, false);
        let sw = g.measure_chrome_text(&label, 10.0, false);
        text(g, x + w - 14.0 - sw, y + 16.0, &label, sw, 10.0, theme::text_dim(), false);
    }
    let inner = (h - 92.0).max(0.0);
    sb.popover_scroll = sb.popover_scroll.clamp(0.0, (body - inner).max(0.0));
    g.push_clip(x + 6.0, y + 44.0, (w - 12.0).max(0.0), inner);
    let mut ry = y + 44.0 - sb.popover_scroll;
    let button_w = crate::native_controls::button_width(g, "업데이트 확인", 32.0, false).min(w - 28.0);
    let label_w = if stacked { w - 28.0 } else { w - button_w - 40.0 };
    text(g, x + 14.0, ry + 6.0, "이 기기", label_w, 12.0, theme::text(), true);
    let current = format!("v{}{}", crate::version::CURRENT, if crate::version::is_local_build() { " · 개발 빌드" } else { "" });
    text(g, x + 14.0, ry + 27.0, &current, label_w, 11.0, theme::text_dim(), false);
    let lines_y = if stacked { ry + 88.0 } else { ry + 47.0 };
    account_lines(g, x + 14.0, lines_y, w - 28.0, local_account.as_ref());
    let button = if stacked { (x + 14.0, ry + 51.0, w - 28.0, 32.0) } else { (x + w - button_w - 14.0, ry + 10.0, button_w, 32.0) };
    action(g, sb, cursor, button, "업데이트 확인", state::StatusbarHit::CheckUpdates);
    ry += local_h;
    for (machine, label, online, differs, install, account) in rows {
        g.rect(x + 14.0, ry, (w - 28.0).max(0.0), 1.0, theme::border());
        let button_w = if install.is_some() { crate::native_controls::button_width(g, "업데이트", 32.0, false) } else { 0.0 };
        let available = if stacked || install.is_none() { w - 28.0 } else { w - button_w - 40.0 };
        text(g, x + 14.0, ry + 10.0, &crate::render::pane_identity::device_name(label), available, 12.0, theme::text(), true);
        let link = crate::machinescol::machine_link(machine);
        let mut state = if !online { "끊김".to_string() } else if machine["online_via"].as_str() == Some("direct") { "직통".into() } else { "중계".into() };
        if online {
            if let Some(ms) = machine["rtt_ms"].as_u64() { state.push_str(&format!(" · {ms}ms")); }
            if differs { state.push_str(" · 버전 다름"); }
        } else if !link.reason.is_empty() { state.push_str(&format!(" · {}", link.reason)); }
        text(g, x + 14.0, ry + 32.0, &state, available, 10.5, theme::text_dim(), false);
        let height = row_h(install.is_some(), account.as_ref());
        let lines_y = if stacked && install.is_some() { ry + 92.0 } else { ry + 50.0 };
        account_lines(g, x + 14.0, lines_y, if stacked { w - 28.0 } else { available }, account.as_ref());
        if let Some(target) = install {
            let button = if stacked { (x + 14.0, ry + 55.0, w - 28.0, 32.0) } else { (x + w - button_w - 14.0, ry + 15.0, button_w, 32.0) };
            action(g, sb, cursor, button, "업데이트", state::StatusbarHit::BuildSend(label.into(), target));
        }
        ry += height;
    }
    g.pop_clip();
    if body > inner && inner > 0.0 {
        let thumb = (inner * inner / body).max(16.0).min(inner);
        let top = y + 44.0 + (inner - thumb) * sb.popover_scroll / (body - inner);
        round_rect(g, x + w - 5.0, top, 3.0, thumb, 1.5, theme::text_mute());
    }
    action(g, sb, cursor, (x + 14.0, y + h - 40.0, (w - 28.0).max(0.0), 32.0), "기기 설정", state::StatusbarHit::OpenMachines);
}
