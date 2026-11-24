use super::*;

type Rect = (f32, f32, f32, f32);

pub(super) struct PopoverLayout {
    pub frame: Rect,
    pub body_height: f32,
    pub scroll_max: f32,
    pub above: bool,
}

pub(super) fn layout(
    viewport: (f32, f32),
    anchor: Rect,
    width: f32,
    fixed_height: f32,
    content_height: f32,
) -> PopoverLayout {
    let (ax, ay, aw, ah) = anchor;
    let width = width.min((viewport.0 - 16.0).max(0.0));
    let above_room = (ay - 8.0).max(0.0);
    let below_room = (viewport.1 - ay - ah - 8.0).max(0.0);
    let above = above_room >= below_room;
    let available = if above { above_room } else { below_room };
    let height = (fixed_height + content_height).min(available);
    let body_height = (height - fixed_height).max(0.0);
    let x = (ax + aw - width).clamp(8.0, (viewport.0 - width - 8.0).max(8.0));
    let y = if above { ay - height } else { ay + ah };
    PopoverLayout {
        frame: (x, y, width, height),
        body_height,
        scroll_max: (content_height - body_height).max(0.0),
        above,
    }
}

pub(super) struct Frame<'a> {
    pub open: bool,
    pub anchor: Option<Rect>,
    pub viewport: (f32, f32),
    pub cursor: (f32, f32),
    pub accounts: AccountSettings<'a>,
    pub provider: Option<AccountProvider>,
    pub claude_observations: &'a ClaudeObservations,
    pub codex_rollout: Option<&'a crate::transcript::CodexRolloutSnapshot>,
}

pub(super) struct MenuState<'a> {
    pub hits: &'a mut Vec<(AccountMenuItem, Rect)>,
    pub rect: &'a mut Option<Rect>,
    pub body_rect: &'a mut Option<Rect>,
    pub scroll: &'a mut f32,
    pub scroll_max: &'a mut f32,
}

struct AccountRow {
    id: String,
    name: String,
    email: Option<String>,
    active: bool,
    status: &'static str,
    windows: Vec<crate::UsageWindowBadge>,
    stale: bool,
}

impl AccountRow {
    fn height(&self) -> f32 {
        58.0 + self.windows.len() as f32 * crate::native_controls::QUOTA_RAIL_HEIGHT
    }
}

fn account_rows(frame: &Frame<'_>, provider: AccountProvider) -> Vec<AccountRow> {
    match provider {
        AccountProvider::Claude => frame.accounts.claude_list.iter()
            .filter(|a| !a.id.is_empty()).enumerate().map(|(i, account)| {
                let id = &account.id;
                let (badge, status) = frame.claude_observations.get(id)
                    .map(|(badge, status)| (badge.as_ref(), *status)).unwrap_or((None, "loading"));
                let windows = badge.map_or_else(Vec::new, |badge| {
                    if badge.windows.is_empty() {
                        vec![crate::UsageWindowBadge {
                            label: badge.label.clone(), pct: badge.pct, resets_at: badge.resets_at,
                        }]
                    } else {
                        badge.windows.clone()
                    }
                });
                AccountRow {
                    id: id.clone(),
                    name: crate::settings::account_display(id, &account.label, &format!("계정 {}", i + 1)),
                    email: crate::settings::auth_probe(id).map(|probe| probe.email),
                    active: frame.accounts.claude == id,
                    status,
                    windows,
                    stale: badge.is_some_and(|b| b.stale) || status == "failed",
                }
            }).collect(),
        AccountProvider::Codex => std::iter::once(("", "", "기본 로그인".to_string()))
            .chain(frame.accounts.codex_list.iter().enumerate().map(|(i, a)| {
                (a.id.as_str(), a.label.as_str(), format!("계정 {}", i + 2))
            }))
            .map(|(id, label, fallback)| {
                let signed_in = crate::settings::codex_logged_in(id) || crate::codexlimits::seeded_for_probe(id);
                let limits = signed_in.then(|| crate::codexlimits::snapshot_for(id)).flatten();
                let status = if !signed_in { "logged_out" }
                    else if limits.as_ref().is_some_and(|limits| limits.stale) { "failed" }
                    else if limits.is_some() { "ready" }
                    else if crate::codexlimits::attempted_for(id) { "failed" }
                    else { "loading" };
                AccountRow {
                    id: id.to_string(),
                    name: crate::settings::codex_account_display(id, label, &fallback),
                    email: crate::settings::codex_identity(id),
                    active: frame.accounts.codex == id,
                    status,
                    windows: limits.as_ref().map_or_else(Vec::new, crate::native_settings::codex_usage_windows),
                    stale: status == "failed",
                }
            }).collect(),
    }
}

fn text(g: &mut gpu::GpuRenderer, x: f32, y: f32, label: &str, size: f32, color: [u8; 4], bold: bool) {
    g.draw_text(x, y, label, gpu::DrawOpts { font_size: size, color, bold, italic: false });
}

fn hit(g: &mut gpu::GpuRenderer, hits: &mut Vec<(AccountMenuItem, Rect)>, rect: Rect, item: AccountMenuItem) {
    if let Some(visible) = g.clip_hit(rect) {
        hits.push((item, visible));
    }
}

fn inside(rect: Rect, (x, y): (f32, f32)) -> bool {
    x >= rect.0 && x < rect.0 + rect.2 && y >= rect.1 && y < rect.1 + rect.3
}

fn menu_button(
    g: &mut gpu::GpuRenderer,
    hits: &mut Vec<(AccountMenuItem, Rect)>,
    cursor: (f32, f32),
    rect: Rect,
    label: &str,
    item: AccountMenuItem,
    active: bool,
) {
    let hover = g.clip_hit(rect).is_some_and(|r| inside(r, cursor));
    if active || hover {
        let fill = if active { theme::lerp(theme::surface(), theme::accent(), 0.22) } else { theme::surface_active() };
        round_rect(g, rect.0, rect.1, rect.2, rect.3, rect.3 * 0.5, fill);
    }
    let color = if active { theme::enforce_contrast_at(theme::accent(), theme::surface(), 4.5) } else { theme::text() };
    let label = crate::info::fit_text(g, label, (rect.2 - 12.0).max(0.0), 12.0, active);
    g.centered_chrome_text(rect, &label, gpu::DrawOpts { font_size: 12.0, color, bold: active, italic: false });
    g.hover_pointer |= hover;
    hit(g, hits, rect, item);
}

fn row_status(row: &AccountRow) -> &'static str {
    match row.status {
        "logged_out" => "로그인 필요",
        "failed" if row.windows.is_empty() => "조회 실패",
        "failed" => "이전 조회",
        "loading" => "조회 중…",
        _ if row.windows.is_empty() => "표시할 사용량 없음",
        _ => "",
    }
}

fn paint_row(
    g: &mut gpu::GpuRenderer,
    hits: &mut Vec<(AccountMenuItem, Rect)>,
    cursor: (f32, f32),
    rect: Rect,
    provider: AccountProvider,
    row: &AccountRow,
) {
    let (x, y, w, h) = rect;
    let needs_login = row.status == "logged_out";
    let selectable = !row.active && !needs_login;
    let hover = selectable && g.clip_hit(rect).is_some_and(|r| inside(r, cursor));
    if hover {
        round_rect(g, x + 2.0, y + 2.0, w - 4.0, h - 4.0, 10.0, theme::surface_hover());
    }
    if row.active {
        round_rect(g, x + 3.0, y + 10.0, 3.0, 22.0, 1.5, theme::accent());
    }
    if selectable {
        hit(g, hits, rect, AccountMenuItem::Select(provider, row.id.clone()));
    }
    g.hover_pointer |= hover;
    let text_x = x + 15.0;
    let right = x + w - 13.0;
    let mut action_x = right;
    if needs_login {
        let label = if row.email.as_ref().is_some_and(|email| !email.is_empty()) || !row.id.is_empty() {
            "다시 로그인"
        } else { "로그인" };
        let width = crate::native_controls::button_width(g, label, 26.0, true);
        action_x -= width;
        menu_button(g, hits, cursor, (action_x, y + 8.0, width, 26.0), label,
            AccountMenuItem::Reauth(provider, row.id.clone()), true);
    } else if row.active {
        let label = "사용 중";
        action_x -= g.measure_chrome_text(label, 11.0, false);
        text(g, action_x, y + 12.0, label, 11.0, theme::text_dim(), false);
    }
    let brand = crate::account_brand::Brand::for_account(provider, &row.id);
    let name_x = text_x + brand.map_or(0.0, |brand| brand.width(14.0) + 6.0);
    if let Some(brand) = brand {
        brand.paint(g, text_x, y + 11.0, 14.0);
    }
    let label = crate::info::fit_text(g, &row.name, (action_x - name_x - 8.0).max(0.0), 13.0, true);
    text(g, name_x, y + 11.0, &label, 13.0, theme::text(), true);
    let status = row_status(row);
    let email = row.email.as_deref().filter(|email| !email.is_empty() && *email != row.name);
    let subtitle = match (email, status) {
        (Some(email), "") => email.to_string(),
        (Some(email), status) => format!("{status} · {email}"),
        (None, status) => status.to_string(),
    };
    let subtitle = crate::info::fit_text(g, &subtitle, (right - name_x).max(0.0), 10.5, false);
    text(g, name_x, y + 32.0, &subtitle, 10.5,
        if needs_login { theme::danger() } else { theme::text_dim() }, false);
    for (i, window) in row.windows.iter().enumerate() {
        crate::native_controls::usage_rail(g, (text_x, y + 52.0 + i as f32 * crate::native_controls::QUOTA_RAIL_HEIGHT,
            right - text_x, crate::native_controls::QUOTA_RAIL_HEIGHT), &window.label, window.pct, window.resets_at, row.stale);
    }
    g.rect(text_x, y + h - 1.0, right - text_x, 1.0, theme::border());
}

pub(super) fn paint(g: &mut gpu::GpuRenderer, st: MenuState<'_>, frame: &Frame<'_>) {
    st.hits.clear();
    *st.rect = None;
    *st.body_rect = None;
    *st.scroll_max = 0.0;
    let Some(anchor) = frame.anchor.filter(|_| frame.open) else { return };
    let provider = frame.provider.unwrap_or(if frame.accounts.claude_list.is_empty() {
        AccountProvider::Codex
    } else {
        AccountProvider::Claude
    });
    let rows = account_rows(frame, provider);
    let grant = (provider == AccountProvider::Claude)
        .then(|| crate::limit_reset::current(frame.accounts.claude)).flatten();
    let sync: Option<crate::session::SyncNotice> = (provider == AccountProvider::Claude)
        .then(|| crate::session::account_sync_notice(frame.accounts.claude)).flatten()
        .filter(|notice| notice.failed || notice.pending);
    let resend = sync.is_some() && rows.iter().any(|row| row.active && row.status != "logged_out");
    let width = 440.0_f32.min((frame.viewport.0 - 16.0).max(0.0));
    let sync_lines: Vec<String> = sync.as_ref().map_or_else(Vec::new, |notice| {
        std::iter::once(notice.summary.as_str()).chain(notice.details.lines())
            .flat_map(|line| crate::native_settings::wrap_words(g, line, (width - 30.0).max(0.0), 10.5))
            .collect()
    });
    let run = if provider == AccountProvider::Codex {
        frame.codex_rollout.map(codex_run_summary).filter(|summary| !summary.is_empty())
    } else { None };
    let sync_height = if sync_lines.is_empty() { 0.0 }
        else { 12.0 + sync_lines.len() as f32 * 16.0 + if resend { 31.0 } else { 0.0 } };
    let extra = (if grant.is_some() { 42.0 } else { 0.0 }) + sync_height
        + if run.is_some() { 25.0 } else { 0.0 };
    let content = rows.iter().map(AccountRow::height).sum::<f32>().max(44.0) + extra;
    let minimum_body = (frame.viewport.1 * 0.30).clamp(155.0, 260.0);
    let panel = layout(frame.viewport, anchor, width, 103.0, content.max(minimum_body));
    let rect @ (x, y, w, h) = panel.frame;
    *st.rect = Some(rect);
    *st.scroll_max = panel.scroll_max.min((content - panel.body_height).max(0.0));
    *st.scroll = st.scroll.clamp(0.0, *st.scroll_max);
    let surface = theme::surface();
    panel_rect_outlined(g, x, y, w, h, theme::radius_md(), surface);
    let join_left = x.max(anchor.0);
    let join_right = (x + w).min(anchor.0 + anchor.2);
    if join_left < join_right {
        let edge_y = if panel.above { anchor.1 - 1.0 } else { anchor.1 + anchor.3 - 1.0 };
        g.rect(join_left, edge_y, join_right - join_left, 2.0, surface);
    }
    g.push_clip(x, y, w, h);
    let _layer = theme::settings_layer(surface);
    let cursor = frame.cursor;

    let tab_width = (w - 26.0).max(0.0) * 0.5;
    for (i, tab) in [AccountProvider::Claude, AccountProvider::Codex].into_iter().enumerate() {
        menu_button(g, st.hits, cursor, (x + 11.0 + i as f32 * (tab_width + 4.0), y + 9.0, tab_width, 28.0),
            tab.label(), AccountMenuItem::Provider(tab), provider == tab);
    }
    let body = (x + 3.0, y + 46.0, (w - 6.0).max(0.0), panel.body_height);
    *st.body_rect = Some(body);
    g.rect(x + 12.0, body.1 - 2.0, w - 24.0, 1.0, theme::border());
    g.push_clip(body.0, body.1, body.2, body.3);
    let mut row_y = body.1 - *st.scroll;
    if let Some(grant) = grant {
        let width = if grant.usable_now { 100.0 } else { 0.0 };
        let note = crate::info::fit_text(g, &crate::limit_reset::label(&grant), (w - width - 30.0).max(0.0), 11.5, false);
        text(g, x + 15.0, row_y + 14.0, &note, 11.5, theme::text_dim(), false);
        if grant.usable_now {
            menu_button(g, st.hits, cursor, (x + w - width - 12.0, row_y + 6.0, width, 28.0),
                "웹에서 쓰기", AccountMenuItem::UseLimitReset, true);
        }
        row_y += 42.0;
    }
    if let Some(summary) = run {
        let summary = crate::info::fit_text(g, &summary, (w - 30.0).max(0.0), 10.5, false);
        text(g, x + 15.0, row_y + 7.0, &summary, 10.5, theme::text_dim(), false);
        row_y += 25.0;
    }
    if rows.is_empty() {
        text(g, x + 15.0, row_y + 14.0, "등록한 계정이 없어요", 12.0, theme::text_dim(), false);
        row_y += 44.0;
    }
    for row in &rows {
        paint_row(g, st.hits, cursor, (body.0, row_y, body.2, row.height()), provider, row);
        row_y += row.height();
    }
    if let Some(notice) = sync {
        let line_y = row_y + 8.0;
        for (i, line) in sync_lines.iter().enumerate() {
            text(g, x + 15.0, line_y + i as f32 * 16.0, line, 10.5,
                if i == 0 && notice.failed { theme::danger() } else { theme::text_dim() }, i == 0);
        }
        if resend {
            menu_button(g, st.hits, cursor, (x + 14.0, line_y + sync_lines.len() as f32 * 16.0, 154.0, 26.0),
                "다른 기기에 다시 보내기", AccountMenuItem::Select(provider, frame.accounts.claude.clone()), false);
        }
    }
    g.pop_clip();
    let foot = body.1 + body.3;
    crate::native_settings::paint_scroll_affordance(g, body.0, body.1, body.2, body.3, content, *st.scroll);
    g.rect(x + 12.0, foot, w - 24.0, 1.0, theme::border());
    menu_button(g, st.hits, cursor, (x + 8.0, foot + 3.0, 156.0, 25.0),
        "코딩 에이전트 설정", AccountMenuItem::ManageAccounts, false);
    let version = format!("카사텀 {}", crate::version::label());
    let version_note = if crate::install_pending() {
        "업데이트 준비됨".to_string()
    } else {
        match crate::version::state() {
            crate::version::Check::Newer(v) => format!("v{v} 받기"),
            crate::version::Check::Busy => "확인 중…".to_string(),
            crate::version::Check::Failed => "확인 다시 시도".to_string(),
            _ if crate::version::is_local_build() && !crate::version::BUILT.is_empty() =>
                format!("내 빌드 · {}", crate::version::BUILT),
            _ => version,
        }
    };
    menu_button(g, st.hits, cursor, (x + 8.0, foot + 28.0, 156.0, 25.0),
        &version_note, AccountMenuItem::CheckUpdates, false);
    g.pop_clip();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bottom_trigger_and_panel_share_an_edge() {
        let p = layout(
            (1200.0, 800.0),
            (980.0, 772.0, 140.0, 28.0),
            380.0,
            127.0,
            240.0,
        );
        assert!(p.above);
        assert_eq!(p.frame.1 + p.frame.3, 772.0);
        assert_eq!(p.frame.0 + p.frame.2, 1120.0);
        assert_eq!(p.scroll_max, 0.0);
    }

    #[test]
    fn narrow_window_keeps_the_full_panel_inside() {
        let p = layout(
            (300.0, 500.0),
            (250.0, 472.0, 42.0, 28.0),
            380.0,
            127.0,
            200.0,
        );
        assert_eq!(p.frame.0, 8.0);
        assert_eq!(p.frame.2, 284.0);
        assert!(p.frame.0 + p.frame.2 <= 292.0);
    }

    #[test]
    fn long_accounts_scroll_without_moving_fixed_chrome() {
        let p = layout(
            (800.0, 400.0),
            (620.0, 372.0, 100.0, 28.0),
            380.0,
            127.0,
            2000.0,
        );
        assert_eq!(p.frame.1, 8.0);
        assert_eq!(p.frame.3 - p.body_height, 127.0);
        assert_eq!(p.scroll_max + p.body_height, 2000.0);
    }

    #[test]
    fn upper_trigger_opens_below_without_a_gap() {
        let p = layout(
            (800.0, 600.0),
            (10.0, 50.0, 120.0, 28.0),
            380.0,
            127.0,
            200.0,
        );
        assert!(!p.above);
        assert_eq!(p.frame.1, 78.0);
        assert_eq!(p.frame.0, 8.0);
    }
}
