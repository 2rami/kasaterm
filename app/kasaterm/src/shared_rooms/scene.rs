use super::*;
use crate::native_controls::{self as controls, Style};

fn text(g: &mut gpu::GpuRenderer, rect: Rect, value: &str, size: f32, dim: bool, bold: bool) {
    let value = crate::info::fit_text(g, value, rect.2.max(0.0), size, bold);
    g.draw_text(rect.0, rect.1, &value, gpu::DrawOpts { font_size: size,
        color: if dim { theme::text_dim() } else { theme::text() }, bold, italic: false });
}

fn button(g: &mut gpu::GpuRenderer, state: &mut State, cursor: (f32, f32), rect: Rect,
    label: &str, action: Action, style: Style) -> Rect {
    let hit = controls::content_button(g, rect, cursor, label, style);
    if style.enabled { if let Some(hit) = g.clip_hit(hit) { state.hits.push((action, hit)); } }
    hit
}

fn buttons(g: &mut gpu::GpuRenderer, state: &mut State, cursor: (f32, f32), area: Rect,
    items: Vec<(&str, Action, Style)>) -> f32 {
    let widths: Vec<_> = items.iter().map(|(label, _, style)|
        controls::button_width(g, label, 32.0, style.primary || style.active)).collect();
    let (rects, bottom) = button_rows(area, &widths);
    for ((label, action, style), rect) in items.into_iter().zip(rects) {
        button(g, state, cursor, rect, label, action, style);
    }
    bottom
}

fn button_rows(area: Rect, widths: &[f32]) -> (Vec<Rect>, f32) {
    let mut x = area.0;
    let mut y = area.1;
    let mut rects = Vec::new();
    for width in widths {
        let width = width.min(area.2.max(0.0));
        if x > area.0 && x + width > area.0 + area.2 { x = area.0; y += 40.0; }
        rects.push((x, y, width, 32.0));
        x += width + 8.0;
    }
    (rects, y + 40.0)
}

fn form_regions(area: Rect, footer_widths: &[f32]) -> (Rect, Rect) {
    let (_, height) = button_rows((0.0, 0.0, area.2, 0.0), footer_widths);
    let footer_y = (area.1 + area.3 - height + 8.0).max(area.1);
    ((area.0, area.1, area.2, (footer_y - area.1 - 8.0).max(0.0)),
        (area.0, footer_y, area.2, (area.1 + area.3 - footer_y).max(0.0)))
}

pub(crate) fn paint(g: &mut gpu::GpuRenderer, state: &mut State, area: Rect, cursor: (f32, f32),
    preedit: &str, caret_on: bool) {
    state.area = Some(area);
    state.hits.clear();
    state.reader.rect = None;
    state.list_rect = None;
    state.form_scroll_max = 0.0;
    crate::lineedit::clear_geometry(FIELD);
    g.rect(area.0, area.1, area.2, area.3, theme::bg());
    g.push_clip(area.0, area.1, area.2, area.3);
    let pad = if area.2 < 500.0 { 12.0 } else { 24.0 };
    let x = area.0 + pad;
    let w = (area.2 - 2.0 * pad).max(0.0);
    let room = state.room().cloned();
    let title = room.as_ref().map_or("공유방", |r| r.title.as_str());
    text(g, (x, area.1 + 16.0, w - 34.0, 24.0), title, 21.0, false, true);
    let close = (area.0 + area.2 - pad - 26.0, area.1 + 14.0, 26.0, 26.0);
    controls::toolbar_button(g, close, cursor, "x", false);
    if let Some(r) = g.clip_hit(close) { state.hits.push((Action::CloseView, r)); }
    let mut y = area.1 + 52.0;
    if let Some(error) = state.error.as_ref() {
        text(g, (x, y, w, 18.0), error, 12.0, true, false);
        y += 24.0;
    } else if let Some(note) = state.note.as_ref() {
        text(g, (x, y, w, 18.0), note, 12.0, true, false);
        y += 24.0;
    }
    if state.pending {
        text(g, (x, y, w, 18.0), "처리 중…", 12.0, true, false);
        y += 24.0;
    }
    if state.client.is_none() {
        text(g, (x, y + 16.0, w, 22.0), if state.fetching { "공유방에 연결하는 중이에요" } else { "KASA 계정으로 공유방에 참여할 수 있어요" }, 14.0, false, false);
        button(g, state, cursor, (x, y + 54.0, w, 32.0), "계정 열기", Action::SignIn,
            Style { primary: true, ..Style::default() });
        g.pop_clip();
        return;
    }
    if let Some(form) = state.form.clone() {
        paint_form(g, state, &form, (x, y, w, (area.1 + area.3 - y - pad).max(0.0)), cursor, preedit, caret_on);
        g.pop_clip();
        return;
    }
    let enabled = !state.pending;
    if let Some(room) = room {
        text(g, (x, y, w, 18.0), &format!("{} · {}명 · 읽기 전용", room.owner_name, room.members.len()), 12.0, true, false);
        y += 24.0;
        let mut actions = vec![("목록", Action::Back, Style::default()), ("새로고침", Action::Refresh, Style::default())];
        if room.my_role == "owner" {
            actions.push(("대화 공유", Action::Publish, Style { primary: true, enabled, ..Style::default() }));
            actions.push(("초대", Action::Invite, Style { enabled, ..Style::default() }));
        }
        actions.push(("관리", Action::Manage, Style { enabled, ..Style::default() }));
        y = buttons(g, state, cursor, (x, y, w, 32.0), actions);
        if let Some(invite) = state.current_invite().cloned() {
            text(g, (x, y + 2.0, (w - 110.0).max(0.0), 22.0), &invite.token, 13.0, false, false);
            button(g, state, cursor, (x + (w - 104.0).max(0.0), y, w.min(104.0), 32.0), "초대 코드 복사", Action::CopyInvite, Style::default());
            y += 40.0;
        }
        if room.sessions.is_empty() {
            text(g, (x, y + 24.0, w, 22.0), "아직 공유된 대화가 없어요", 15.0, false, false);
            text(g, (x, y + 54.0, w, 20.0), if room.my_role == "owner" { "내 PC에서 대화를 골라 이 방에 게시해 주세요." }
                else { "소유자가 대화를 게시하면 여기에 보여요." }, 12.0, true, false);
            g.pop_clip(); return;
        }
        let bottom = area.1 + area.3 - pad;
        let side = if w >= 700.0 { 204.0 } else { 0.0 };
        let list_h = if side > 0.0 { (bottom - y).max(0.0) } else { (room.sessions.len() as f32 * 40.0).min(120.0) };
        let list = (x, y, if side > 0.0 { side } else { w }, list_h);
        state.list_rect = Some(list);
        state.list_max = (room.sessions.len() as f32 * 40.0 - list_h).max(0.0);
        state.list_scroll = state.list_scroll.clamp(0.0, state.list_max);
        g.push_clip(list.0, list.1, list.2, list.3);
        for (i, session) in room.sessions.iter().enumerate() {
            let row = (list.0, list.1 + i as f32 * 40.0 - state.list_scroll, list.2, 38.0);
            if !g.clip_visible(row.0, row.1, row.2, row.3) { continue; }
            let selected = session.id == state.reader.session;
            if selected { g.round_rect_fill(row.0, row.1, row.2, row.3, 6.0, theme::surface_active()); }
            text(g, (row.0 + 8.0, row.1 + 5.0, row.2 - 16.0, 17.0), &session.title, 12.5, false, selected);
            text(g, (row.0 + 8.0, row.1 + 22.0, row.2 - 16.0, 13.0), &session.harness, 10.0, true, false);
            if let Some(hit) = g.clip_hit(row) { state.hits.push((Action::Select(session.id.clone()), hit)); }
        }
        g.pop_clip();
        let rx = x + if side > 0.0 { side + 16.0 } else { 0.0 };
        let rw = w - if side > 0.0 { side + 16.0 } else { 0.0 };
        let ry = y + if side > 0.0 { 0.0 } else { list_h + 8.0 };
        let can_fork = room.allow_fork && state.session().is_some_and(|s| s.checkpoint.is_some());
        let reader_y = buttons(g, state, cursor, (rx, ry, rw, 32.0), vec![
            ("내 세션으로 포크", Action::Fork, Style { primary: true, enabled: enabled && can_fork, ..Style::default() }),
            ("대화 복사", Action::CopyConversation, Style { enabled: !state.reader.cache.lines.is_empty(), ..Style::default() }),
        ]);
        paint_reader(g, state, (rx, reader_y, rw, (bottom - reader_y).max(0.0)));
    } else {
        y = buttons(g, state, cursor, (x, y, w, 32.0), vec![
            ("새 공유방", Action::New, Style { primary: true, enabled, ..Style::default() }),
            ("초대 코드로 참여", Action::Join, Style { enabled, ..Style::default() }),
            ("새로고침", Action::Refresh, Style::default()),
        ]);
        let rooms = state.rooms.clone();
        if rooms.is_empty() { text(g, (x, y + 22.0, w, 22.0), "함께 볼 대화를 새 공유방에 담아 보세요", 14.0, true, false); }
        let list = (x, y, w, (area.1 + area.3 - y - pad).max(0.0));
        state.list_rect = Some(list);
        state.list_max = (rooms.len() as f32 * 76.0 - list.3).max(0.0);
        state.list_scroll = state.list_scroll.clamp(0.0, state.list_max);
        y -= state.list_scroll;
        g.push_clip(list.0, list.1, list.2, list.3);
        for room in rooms.iter() {
            let row = (x, y, w, 66.0);
            if let Some(hit) = g.clip_hit(row) {
                g.round_rect_fill(row.0, row.1, row.2, row.3, theme::radius_md(), theme::surface_active());
                text(g, (x + 12.0, y + 10.0, w - 24.0, 20.0), &room.title, 15.0, false, true);
                text(g, (x + 12.0, y + 38.0, w - 24.0, 18.0),
                    &format!("{} · 대화 {} · {}", room.owner_name, room.sessions.len(), if room.my_role == "owner" { "내가 공유" } else { "참여 중" }),
                    12.0, true, false);
                state.hits.push((Action::Open(room.id.clone()), hit));
            }
            y += 76.0;
        }
        g.pop_clip();
    }
    g.pop_clip();
}

fn paint_reader(g: &mut gpu::GpuRenderer, state: &mut State, rect: Rect) {
    if rect.2 < 1.0 || rect.3 < 1.0 { return; }
    let reader = &mut state.reader;
    let cw = g.cell_w.max(1.0);
    let ch = g.cell_h.max(1.0);
    let cols = (rect.2 / cw).floor().max(8.0) as usize;
    reader.cache.build(&reader.conversation, reader.version, cols, &reader.open,
        &crate::chat_view::classic::Look { face: false, student: None, banner: None });
    let max = (reader.cache.lines.len() as f32 * ch - rect.3).max(0.0);
    reader.scroll = if reader.follow { max } else { reader.scroll.clamp(0.0, max) };
    reader.rect = Some(rect); reader.cell = (cw, ch);
    let start = (reader.scroll / ch).floor() as usize;
    let count = (rect.3 / ch).ceil() as usize + 1;
    let mut rows: Vec<_> = reader.cache.lines.iter().skip(start).take(count).map(|l| l.cells.clone()).collect();
    let bg = theme::bg();
    if u32::from(bg[0]) + u32::from(bg[1]) + u32::from(bg[2]) > 384 {
        use kasa_bridge::screen::Color;
        let rgb = |c: [u8; 4]| Color::Rgb(c[0], c[1], c[2]);
        let tint = |c: [u8; 4]| Color::Rgb(
            ((bg[0] as u16 * 7 + c[0] as u16) / 8) as u8,
            ((bg[1] as u16 * 7 + c[1] as u16) / 8) as u8,
            ((bg[2] as u16 * 7 + c[2] as u16) / 8) as u8);
        for cell in rows.iter_mut().flatten() {
            cell.bg = match cell.bg {
                Color::Idx(237) => rgb(theme::surface_active()), Color::Idx(16) => rgb(bg),
                Color::Idx(22) => tint(theme::success()), Color::Idx(52) => tint(theme::danger()),
                _ => cell.bg.clone(),
            };
            cell.fg = match cell.fg {
                Color::Idx(153) => rgb(theme::accent()), Color::Idx(77) => rgb(theme::success()),
                Color::Idx(203) => rgb(theme::danger()), _ => cell.fg.clone(),
            };
        }
    }
    if let Some((a, b)) = reader.selection {
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        let color = theme::surface_hover();
        for (r, row) in rows.iter_mut().enumerate() {
            for (c, cell) in row.iter_mut().enumerate() {
                if (start + r, c) >= a && (start + r, c) < b { cell.bg = kasa_bridge::screen::Color::Rgb(color[0], color[1], color[2]); }
            }
        }
    }
    g.push_clip(rect.0, rect.1, rect.2, rect.3);
    let scale = g.scale();
    g.draw_cells(&[gpu::PaneSlot { rows: &rows,
        origin_px: (rect.0 * scale, (rect.1 - reader.scroll % ch) * scale),
        font_scale: 1.0, dim: false, links: Vec::new(), default_fg: theme::text(), source: None }]);
    for (i, line) in reader.cache.lines.iter().skip(start).take(count).enumerate() {
        if let Some(key) = line.toggle {
            if let Some(r) = g.clip_hit((rect.0, rect.1 + i as f32 * ch - reader.scroll % ch, rect.2, ch)) {
                state.hits.push((Action::ToggleTool(key), r));
            }
        }
    }
    if reader.cache.lines.is_empty() {
        text(g, (rect.0 + 8.0, rect.1 + 16.0, rect.2 - 16.0, 20.0),
            if state.reading { "공유 대화를 받는 중이에요" } else { "공유된 내용이 여기에 보여요" }, 13.0, true, false);
    }
    if max > 0.0 {
        let bh = (rect.3 * rect.3 / (max + rect.3)).max(20.0).min(rect.3);
        g.round_rect_fill(rect.0 + rect.2 - 4.0, rect.1 + (rect.3 - bh) * reader.scroll / max,
            3.0, bh, 1.5, theme::text_mute());
    }
    g.pop_clip();
}

fn paint_form(g: &mut gpu::GpuRenderer, state: &mut State, form: &Form, area: Rect, cursor: (f32, f32), preedit: &str, caret_on: bool) {
    let enabled = !state.pending;
    let mut footer = Vec::new();
    if matches!(form.kind, FormKind::Confirm { .. }) { footer.push(("확인", Action::Confirm, Style { primary: true, enabled, danger: true, ..Style::default() })); }
    else if !matches!(form.kind, FormKind::Manage) { footer.push(("완료", Action::Submit, Style { primary: true, enabled, ..Style::default() })); }
    footer.push(("돌아가기", Action::Cancel, Style::default()));
    let widths: Vec<_> = footer.iter().map(|(label, _, style)| controls::button_width(g, label, 32.0, style.primary)).collect();
    let (body, footer_area) = form_regions(area, &widths);
    g.push_clip(body.0, body.1, body.2, body.3);
    let area = (body.0, body.1 - form.scroll, body.2, body.3);
    let heading = match &form.kind {
        FormKind::Create { .. } => "새 공유방", FormKind::Join => "초대 코드로 참여", FormKind::Rename { .. } => "방 이름 바꾸기",
        FormKind::Publish => "공유할 대화 고르기", FormKind::Fork { .. } => "내 PC에서 이어가기",
        FormKind::Confirm { label, .. } => label.as_str(), FormKind::Manage => "공유방 관리",
    };
    text(g, (area.0, area.1 + 4.0, area.2, 25.0), heading, 17.0, false, true);
    let mut y = area.1 + 42.0;
    if matches!(form.kind, FormKind::Create { .. } | FormKind::Join | FormKind::Rename { .. } | FormKind::Fork { .. }) {
        let field = (area.0, y, area.2.min(640.0), 36.0);
        crate::lineedit::SingleLine { id: FIELD, text: form.text.clone(), cursor: form.cursor,
            preedit: preedit.into(), caret_on }.paint(g, field, 13.0, false, Some(body));
        if let Some(r) = g.clip_hit(field) { state.hits.push((Action::FocusField, r)); }
        y += 48.0;
    }
    match &form.kind {
        FormKind::Create { .. } => {
            button(g, state, cursor, (area.0, y, area.2, 32.0),
                if form.allow_fork { "포크 허용: 켜짐" } else { "포크 허용: 꺼짐" }, Action::ToggleFork,
                Style { active: form.allow_fork, enabled, ..Style::default() });
            text(g, (area.0, y + 42.0, area.2, 18.0), "포크한 사본은 공유를 끝내도 받는 사람에게 남아요.", 12.0, true, false);
            y += 76.0;
        }
        FormKind::Join => {
            text(g, (area.0, y, area.2, 20.0), "받은 초대 코드를 붙여 넣어 주세요.", 12.0, true, false); y += 36.0;
        }
        FormKind::Fork { room, session, .. } => {
            text(g, (area.0, y, area.2, 20.0), "내 프로젝트 폴더 · 비우면 별도 작업 폴더를 만들어요", 12.0, true, false); y += 28.0;
            let checkpoint = state.rooms.iter().find(|r| r.id == *room).and_then(|r| r.sessions.iter().find(|s| s.id == *session))
                .and_then(|s| s.checkpoint.clone());
            if let Some(checkpoint) = checkpoint {
                text(g, (area.0, y, area.2, 20.0), &checkpoint.boundary_label, 12.0, true, false); y += 28.0;
                if checkpoint.has_project {
                    button(g, state, cursor, (area.0, y, area.2, 32.0),
                        if form.include_project { "프로젝트 자료도 가져오기: 켜짐" } else { "대화만 가져오기" },
                        Action::ToggleProject, Style { active: form.include_project, enabled, ..Style::default() }); y += 40.0;
                }
                for omission in checkpoint.omissions.iter().take(4) {
                    text(g, (area.0, y, area.2, 18.0), omission, 11.5, true, false); y += 22.0;
                }
            }
        }
        FormKind::Publish => {
            button(g, state, cursor, (area.0, y, area.2, 32.0),
                if form.include_project { "프로젝트 변경 파일 포함: 켜짐" } else { "프로젝트 변경 파일 포함: 꺼짐" }, Action::ToggleProject,
                Style { active: form.include_project, enabled, ..Style::default() }); y += 40.0;
            let sources = state.sources.clone();
            for (i, source) in sources.iter().enumerate() {
                let selected = form.selected.contains(source.key());
                button(g, state, cursor, (area.0, y + i as f32 * 42.0, area.2, 34.0),
                    &format!("{} · {}", source.title, source.harness), Action::SelectSource(source.key().to_string()),
                    Style { active: selected, enabled: enabled && source.ready, ..Style::default() });
            }
            if sources.is_empty() { text(g, (area.0, y + 10.0, area.2, 22.0), "이 PC에서 실행 중인 Claude·Codex 대화가 없어요.", 12.0, true, false); }
            y += sources.len().max(1) as f32 * 42.0;
        }
        FormKind::Manage => {
            let room = state.room().cloned();
            if let Some(room) = room {
                if room.my_role == "owner" {
                    y = buttons(g, state, cursor, (area.0, y, area.2, 32.0), vec![
                        ("이름 변경", Action::Rename, Style { enabled, ..Style::default() }),
                        (if room.allow_fork { "포크 허용 끄기" } else { "포크 허용 켜기" }, Action::ToggleFork, Style { enabled, ..Style::default() }),
                        ("공유방 삭제", Action::Delete, Style { enabled, danger: true, ..Style::default() }),
                    ]);
                } else {
                    y = buttons(g, state, cursor, (area.0, y, area.2, 32.0), vec![("이 방 나가기", Action::Leave, Style { enabled, ..Style::default() })]);
                }
                let rows = (area.0, y, area.2, body.3);
                let mut ry = rows.1;
                for member in &room.members {
                    text(g, (rows.0, ry + 9.0, (rows.2 - 80.0).max(0.0), 20.0),
                        &format!("{} · {}", member.display_name, if member.role == "owner" { "소유자" } else { "참여자" }), 12.0, false, false);
                    if room.my_role == "owner" && member.role != "owner" {
                        button(g, state, cursor, (rows.0 + (rows.2 - 72.0).max(0.0), ry + 2.0, 72.0_f32.min(rows.2), 32.0),
                            "제외", Action::RemoveMember(member.account.clone()), Style { enabled, danger: true, ..Style::default() });
                    }
                    ry += 42.0;
                }
                for invite in &room.invites {
                    text(g, (rows.0, ry + 9.0, (rows.2 - 80.0).max(0.0), 20.0),
                        &format!("초대 · {} / {}회 사용", invite.uses, invite.max_uses), 12.0, true, false);
                    button(g, state, cursor, (rows.0 + (rows.2 - 72.0).max(0.0), ry + 2.0, 72.0_f32.min(rows.2), 32.0),
                        "폐기", Action::RevokeInvite(invite.id.clone()), Style { enabled, danger: true, ..Style::default() });
                    ry += 42.0;
                }
                y = ry;
            }
        }
        _ => {}
    }
    state.form_scroll_max = (y + 8.0 + form.scroll - (body.1 + body.3)).max(0.0);
    if let Some(form) = &mut state.form { form.scroll = form.scroll.min(state.form_scroll_max); }
    g.pop_clip();
    buttons(g, state, cursor, footer_area, footer);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_forms_reserve_all_wrapped_footer_rows() {
        for width in [100.0, 240.0, 400.0] {
            let area = (12.0, 70.0, width, 170.0);
            let widths = [68.0, 96.0];
            let (body, footer) = form_regions(area, &widths);
            let (buttons, _) = button_rows(footer, &widths);
            assert!(body.3 > 0.0);
            assert!(body.1 + body.3 <= footer.1 - 8.0);
            for button in buttons {
                assert!(button.1 >= body.1 + body.3);
                assert!(button.1 + button.3 <= area.1 + area.3);
                assert!(button.0 + button.2 <= area.0 + area.2);
            }
        }
    }
}
