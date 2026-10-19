use super::*;

pub(super) struct Badge {
    provider: &'static str,
    name: String,
    windows: Vec<(String, Option<f32>)>,
    note: &'static str,
    stale: bool,
}

pub(super) fn badges(f: &Frame<'_>) -> Vec<Badge> {
    f.prefs.order.iter().filter_map(|provider| {
        if !f.prefs.visible(provider) { return None; }
        let (id, name, windows, note, stale) = match provider.as_str() {
            "claude" => {
                let observation = f.claude_observations.get(f.accounts.claude);
                let badge = observation.and_then(|(badge, _)| badge.as_ref());
                let state = observation.map_or("unselected", |(_, state)| *state);
                let active = crate::claude_auth::runtime_dir_for_cached(f.accounts.claude, f.accounts.claude)
                    .map_or(String::new(), |path| path.to_string_lossy().into_owned());
                let switching = badge.is_some_and(|badge| badge.account_dir != active);
                let windows = if state == "ready" {
                    selected_status_usage_windows(badge, f.prefs, "claude", switching)
                } else { Vec::new() };
                let note = match state {
                    "ready" => "", "unselected" => "계정 선택", "logged_out" => "로그인 필요",
                    "failed" => "조회 실패", _ => "확인 중",
                };
                ("claude", claude_account_label(f.accounts.claude, f.accounts.claude_list), windows, note, badge.is_some_and(|b| b.stale))
            }
            "codex" => {
                let id = f.accounts.codex.as_str();
                let logged_in = crate::settings::codex_logged_in(id);
                if !codex_statusbar_visible(f.viewport.0, logged_in, !id.is_empty() || !f.accounts.codex_list.is_empty()) { return None; }
                let snapshot = crate::codexlimits::snapshot_for(id);
                let raw = codex_windows_for(id);
                let windows = if logged_in {
                    selected_codex_windows(raw.as_deref().unwrap_or_default(), f.prefs)
                        .into_iter().map(|(label, pct)| (label, Some(pct))).collect()
                } else { Vec::new() };
                let note = if !logged_in { "로그인 필요" } else if raw.is_none() && f.prefs.wants_usage("codex") { "확인 중" } else { "" };
                ("codex", codex_account_label(id, f.accounts.codex_list), windows, note, snapshot.is_some_and(|s| s.stale))
            }
            _ => return None,
        };
        let other_names: Vec<String> = if id == "codex" {
            std::iter::once(codex_account_name("", f.accounts.codex_list))
                .chain(f.accounts.codex_list.iter().map(|account| codex_account_name(&account.id, f.accounts.codex_list))).collect()
        } else { f.accounts.claude_list.iter().map(|account| account.label.clone()).collect() };
        let mut identity = if f.prefs.has_usage_field(id, "account") { statusbar_account_short(&name, &other_names) } else { String::new() };
        if f.prefs.has_usage_field(id, "email") {
            let email = if id == "codex" { crate::settings::codex_identity(f.accounts.codex) }
                else { crate::settings::auth_probe(f.accounts.claude).map(|p| p.email) };
            if let Some(email) = email.filter(|email| !email.is_empty() && !identity.contains(email)) {
                if !identity.is_empty() { identity.push(' '); }
                identity.push_str(&email);
            }
        }
        Some(Badge { provider: id, name: identity, windows, note, stale })
    }).collect()
}

pub(super) fn budget(width: f32, tools: usize, providers: usize) -> f32 {
    if providers == 0 { return 0.0; }
    let room = (width - 24.0 - tools as f32 * 22.0).max(0.0);
    (width * 0.42).min(600.0).min(room).max((providers as f32 * 18.0).min((width - 24.0).max(0.0)))
}

fn chosen_windows(windows: &[(String, Option<f32>)], width: f32, mut measure: impl FnMut(&str) -> f32, stale: bool) -> Vec<(String, Option<f32>)> {
    let mut priority: Vec<_> = windows.iter().enumerate().collect();
    priority.sort_by(|a, b| b.1.1.unwrap_or(-1.0).total_cmp(&a.1.1.unwrap_or(-1.0)));
    let mut room = width;
    let mut selected = Vec::new();
    for (index, (label, pct)) in priority {
        let text = usage_text(label, *pct, stale);
        let needed = measure(&text) + if selected.is_empty() { 0.0 } else { 10.0 };
        if needed <= room { selected.push((index, text, *pct)); room -= needed; }
    }
    selected.sort_by_key(|(index, _, _)| *index);
    selected.into_iter().map(|(_, text, pct)| (text, pct)).collect()
}

fn usage_text(label: &str, pct: Option<f32>, stale: bool) -> String {
    pct.map_or_else(|| format!("{label} …"), |pct| format!("{label} {}{pct:.0}%", if stale { "~" } else { "" }))
}

pub(super) fn paint(g: &mut gpu::GpuRenderer, f: &Frame<'_>, badges: &[Badge], bounds: (f32, f32, f32, f32)) -> f32 {
    if badges.is_empty() || bounds.2 < 12.0 { return 0.0; }
    let gap = if badges.len() > 1 { 14.0 } else { 0.0 };
    let slot = ((bounds.2 - gap * (badges.len() - 1) as f32) / badges.len() as f32).max(0.0);
    let y = bounds.1 + (bounds.3 - 11.0) / 2.0 - 1.0;
    let mut used: f32 = 0.0;
    for (index, badge) in badges.iter().enumerate() {
        let start = bounds.0 + index as f32 * (slot + gap);
        let right = start + slot;
        if slot < 12.0 { continue; }
        g.queue_icon(badge.provider, start, bounds.1 + (bounds.3 - 12.0) / 2.0, 12.0, f.prefs.color(badge.provider, theme::text_dim()));
        let mut x = start + 18.0;
        let gauge = slot >= 200.0;
        let labels = chosen_windows(&badge.windows, (right - x).max(0.0), |text| g.measure_chrome_text(text, 11.0, false) + if gauge { 34.0 } else { 0.0 }, badge.stale);
        for (text, pct) in labels {
            if gauge {
                let gy = bounds.1 + (bounds.3 - 5.0) / 2.0;
                round_rect(g, x, gy, 28.0, 5.0, 2.5, theme::with_alpha(theme::text_dim(), 70));
                if let Some(pct) = pct { round_rect(g, x, gy, (28.0 * pct.clamp(0.0, 100.0) / 100.0).max(0.0), 5.0, 2.5, theme::text_dim()); }
                x += 34.0;
            }
            g.draw_text(x, y, &text, gpu::DrawOpts { font_size: 11.0, color: pct.map_or_else(theme::text_dim, usage_pct_color), bold: false, italic: false });
            x += g.measure_chrome_text(&text, 11.0, false) + 10.0;
        }
        let value = if badge.note.is_empty() { badge.name.as_str() } else { badge.note };
        if badge.note == "로그인 필요" && right - x >= 70.0 {
            x += status_dot(g, x, bounds.1, bounds.3, ChipState::Bad) + 4.0;
        }
        let room = (right - x).max(0.0);
        if !value.is_empty() && room >= 35.0 {
            let text = crate::info::fit_text(g, value, room, 11.0, false);
            g.draw_text(x, y, &text, gpu::DrawOpts { font_size: 11.0, color: f.prefs.color(badge.provider, theme::text_dim()), bold: false, italic: false });
            x += g.measure_chrome_text(&text, 11.0, false);
        }
        used = used.max((x.min(right) - bounds.0).max(start + 12.0 - bounds.0));
    }
    used.min(bounds.2)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scarce_space_keeps_the_highest_real_limit_without_cutting_a_number() {
        let windows = vec![("5h".into(), Some(12.0)), ("7d".into(), Some(95.0))];
        let measure = |s: &str| s.chars().count() as f32;
        assert_eq!(chosen_windows(&windows, 7.0, measure, false), vec![("7d 95%".into(), Some(95.0))]);
        assert!(chosen_windows(&windows, 4.0, measure, false).is_empty());
        assert_eq!(chosen_windows(&windows, 30.0, measure, false).len(), 2);
        assert!(chosen_windows(&[], 100.0, measure, false).is_empty());
    }
}
