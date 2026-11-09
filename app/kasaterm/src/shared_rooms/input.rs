use super::*;
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta};
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

fn hit(rect: Rect, p: (f32, f32)) -> bool {
    p.0 >= rect.0 && p.0 < rect.0 + rect.2 && p.1 >= rect.1 && p.1 < rect.1 + rect.3
}

impl App {
    fn shared_redraw(&mut self) {
        self.chrome_dirty = true;
        if let Some(w) = &self.window { w.request_redraw(); }
    }

    fn shared_form(&mut self, kind: FormKind, text: String) {
        self.shared_rooms.view_sequence = self.shared_rooms.view_sequence.wrapping_add(1);
        self.ime_retarget(ImeFocus::SharedRooms);
        if let Some(text) = self.hangul.flush() { self.shared_field_insert(&text); }
        self.preedit.clear(); self.in_preedit = false;
        self.ime_retarget(ImeFocus::SharedRooms);
        crate::lineedit::forget(FIELD);
        let mut form = Form::new(kind, text);
        crate::lineedit::select_all(FIELD, &form.text, &mut form.cursor);
        self.shared_rooms.form = Some(form);
        self.shared_rooms.error = None;
    }

    pub(crate) fn shared_action(&mut self, action: Action) {
        match action {
            Action::Open(id) => { self.open_shared_room_view(Some(&id)); }
            Action::New => self.shared_form(FormKind::Create { request_id: uuid::Uuid::new_v4().to_string() }, "공유방".into()),
            Action::Join => self.shared_form(FormKind::Join, String::new()),
            Action::CloseView => { self.close_shared_room_view(); }
            Action::Back => {
                let form = self.shared_rooms.form.is_some();
                self.shared_rooms.cancel_view();
                if form { crate::lineedit::forget(FIELD); }
                else if self.shared_rooms.active.take().is_some() {
                    self.shared_rooms.reset_reader();
                    self.shared_rooms.list_scroll = 0.0;
                } else { self.close_shared_room_view(); }
            }
            Action::Cancel => {
                self.shared_rooms.cancel_view();
                crate::lineedit::forget(FIELD);
            }
            Action::Refresh => { self.shared_rooms.refreshed = None; }
            Action::SignIn => { self.open_settings_room(Some(SettingsCat::Accounts)); }
            Action::Select(id) => {
                if let Some(session) = self.shared_rooms.room().and_then(|r| r.sessions.iter().find(|s| s.id == id)).cloned() {
                    self.shared_rooms.view_sequence = self.shared_rooms.view_sequence.wrapping_add(1);
                    self.shared_rooms.reader_sequence = self.shared_rooms.reader_sequence.wrapping_add(1);
                    self.shared_rooms.reading = false;
                    self.shared_rooms.reader = Reader { room: self.shared_rooms.active.clone().unwrap_or_default(),
                        session: id, generation: session.generation, follow: true, ..Reader::default() };
                }
            }
            Action::Invite if !self.shared_rooms.pending && self.shared_rooms.owner() => {
                if let Some(room) = self.shared_rooms.active.clone() { self.shared_job(Job::Invite(room)); }
            }
            Action::CopyInvite => {
                if let Some(invite) = self.shared_rooms.current_invite() { crate::lineedit::copy_text(&invite.token); }
            }
            Action::CopyConversation => crate::lineedit::copy_text(&reader_text(&self.shared_rooms.reader, false)),
            Action::Manage => self.shared_form(FormKind::Manage, String::new()),
            Action::Rename if self.shared_rooms.owner() => {
                if let Some(room) = self.shared_rooms.room().cloned() {
                    self.shared_form(FormKind::Rename { room: room.id, revision: room.revision }, room.title);
                }
            }
            Action::ToggleFork if !self.shared_rooms.pending => {
                if let Some(form) = self.shared_rooms.form.as_mut().filter(|f| matches!(f.kind, FormKind::Create { .. })) {
                    form.allow_fork = !form.allow_fork;
                } else if self.shared_rooms.owner() && !self.shared_rooms.pending {
                    if let Some(room) = self.shared_rooms.room().cloned() {
                        self.shared_job(Job::Update(room.id, api::UpdateRoom { expected_revision: room.revision,
                            title: None, allow_fork: Some(!room.allow_fork) }));
                    }
                }
            }
            Action::ToggleProject => {
                if let Some(form) = &mut self.shared_rooms.form { form.include_project = !form.include_project; }
            }
            Action::Publish if self.shared_rooms.owner() => {
                self.shared_rooms.sources = self.shared_source_list();
                self.shared_form(FormKind::Publish, String::new());
                if let Some(source) = self.shared_rooms.sources.first().filter(|s| s.ready) {
                    let key = source.key().to_string();
                    if let Some(form) = &mut self.shared_rooms.form { form.selected.insert(key); }
                }
            }
            Action::SelectSource(key) => {
                if let Some(form) = &mut self.shared_rooms.form {
                    if !form.selected.remove(&key) { form.selected.insert(key); }
                }
            }
            Action::Fork => {
                if let Some(room) = self.shared_rooms.room().filter(|r| r.allow_fork).cloned() {
                    if let Some(session) = room.sessions.iter().find(|s| s.id == self.shared_rooms.reader.session) {
                        if let Some(checkpoint) = &session.checkpoint {
                            let cwd = self.shared_source_list().into_iter().find(|s|
                                s.address["surface_id"].as_str() == self.shared_rooms.return_pane.as_deref()).map(|s| s.cwd).unwrap_or_default();
                            self.shared_form(FormKind::Fork { room: room.id.clone(), session: session.id.clone(),
                                checkpoint: checkpoint.id.clone(), request_id: uuid::Uuid::new_v4().to_string() }, cwd);
                            if let Some(form) = &mut self.shared_rooms.form { form.include_project = checkpoint.has_project; }
                        }
                    }
                }
            }
            Action::Delete | Action::Leave | Action::RemoveMember(_) | Action::RevokeInvite(_) => {
                let label = match &action {
                    Action::Delete => "이 공유방을 삭제할까요? 원본 작업은 계속돼요.",
                    Action::Leave => "이 공유방에서 나갈까요?",
                    Action::RemoveMember(_) => "참여자를 이 방에서 제외할까요?",
                    _ => "이 초대 코드를 폐기할까요?",
                };
                self.shared_form(FormKind::Confirm { action: Box::new(action), label: label.into() }, String::new());
            }
            Action::Confirm if !self.shared_rooms.pending => {
                if let Some(Form { kind: FormKind::Confirm { action, .. }, .. }) = self.shared_rooms.form.clone() {
                    if let Some(room) = self.shared_rooms.room().cloned() {
                        match *action {
                            Action::Delete if room.my_role == "owner" => self.shared_job(Job::Delete(room.id)),
                            Action::Leave if room.my_role != "owner" => {
                                if let Some(account) = self.shared_rooms.client.as_ref().map(|c| c.account().to_owned()) {
                                    self.shared_job(Job::RemoveMember(room.id, account));
                                }
                            }
                            Action::RemoveMember(account) if room.my_role == "owner" => self.shared_job(Job::RemoveMember(room.id, account)),
                            Action::RevokeInvite(id) if room.my_role == "owner" => self.shared_job(Job::RevokeInvite(room.id, id)),
                            _ => {}
                        }
                    }
                }
            }
            Action::Submit if !self.shared_rooms.pending => {
                if let Some(text) = self.hangul.flush() { self.shared_field_insert(&text); }
                self.preedit.clear(); self.in_preedit = false;
                let Some(form) = self.shared_rooms.form.clone() else { return; };
                let value = form.text.trim().to_string();
                match form.kind {
                    FormKind::Create { request_id } if !value.is_empty() => self.shared_job(Job::Create(api::CreateRoom {
                        title: value, allow_fork: form.allow_fork, request_id: Some(request_id),
                    })),
                    FormKind::Join if !value.is_empty() => self.shared_job(Job::Join(value)),
                    FormKind::Rename { room, revision } if !value.is_empty() => self.shared_job(Job::Update(room,
                        api::UpdateRoom { expected_revision: revision, title: Some(value), allow_fork: None })),
                    FormKind::Publish => {
                        if form.selected.is_empty() { self.shared_rooms.error = Some("공유할 대화를 골라 주세요.".into()); }
                        else if let Some(room) = self.shared_rooms.active.clone() {
                            let keys: Vec<_> = form.selected.into_iter().collect();
                            match self.publish_shared_sources(&room, &keys, form.include_project) {
                                Ok(_) => { self.shared_rooms.form = None; }
                                Err(error) => self.shared_rooms.error = Some(error),
                            }
                        }
                    }
                    FormKind::Fork { room, session, checkpoint, request_id } => {
                        if form.include_project && value.is_empty() {
                            self.shared_rooms.error = Some("프로젝트 자료를 가져오려면 내 PC의 저장소 폴더를 입력해 주세요.".into());
                        } else {
                            self.shared_job(Job::Fork { room, session, request: api::CreateFork { checkpoint_id: checkpoint, request_id },
                                cwd: value, include_project: form.include_project });
                        }
                    }
                    _ => self.shared_rooms.error = Some("내용을 입력해 주세요.".into()),
                }
            }
            Action::ToggleTool(key) => {
                if !self.shared_rooms.reader.open.remove(&key) { self.shared_rooms.reader.open.insert(key); }
            }
            Action::FocusField => { self.ime_retarget(ImeFocus::SharedRooms); }
            _ => {}
        }
        self.sync_shared_navigation();
        self.shared_redraw();
        self.prime_hit_areas();
    }

    pub(crate) fn shared_field_insert(&mut self, text: &str) {
        if self.shared_rooms.pending { return; }
        if let Some(form) = &mut self.shared_rooms.form {
            let limit = if matches!(form.kind, FormKind::Fork { .. }) { 2048 } else { 512 };
            let text: String = text.chars().filter(|c| !c.is_control()).take(limit).collect();
            crate::lineedit::insert_in(crate::lineedit::line(FIELD), &mut form.text, &mut form.cursor, &text);
            form.text = form.text.chars().take(limit).collect();
            form.cursor = form.cursor.min(form.text.chars().count());
        }
    }

    pub(crate) fn shared_key(&mut self, event: &KeyEvent) -> bool {
        if !self.shared_room_active() { return false; }
        if !event.state.is_pressed() || crate::input::is_modifier_key(event) { return true; }
        let mods = crate::lineedit::Mods::of(self.modifiers);
        if mods.host {
            if let PhysicalKey::Code(code) = event.physical_key {
                if let Some(number) = crate::input::room_digit(code) { self.goto_room_number(number); return true; }
                if code == KeyCode::KeyW { self.close_shared_room_view(); return true; }
                if code == KeyCode::Comma { self.open_settings_room(None); return true; }
            }
        }
        if self.shared_rooms.form.is_some() {
            if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
                self.settings_scene.arm_activation_key(NamedKey::Escape);
                self.shared_action(Action::Cancel); return true;
            }
            if self.shared_rooms.pending { return true; }
            self.ime_retarget(ImeFocus::SharedRooms);
            #[cfg(target_os = "macos")]
            if let Some(text) = event.text.as_ref().filter(|_| !mods.chord()) {
                if let Some(c) = text.chars().next().filter(|_| text.chars().count() == 1) {
                    if (0x3130..=0x318F).contains(&(c as u32)) {
                        if let Some(done) = self.hangul.feed(c) { self.shared_field_insert(&done); }
                        self.preedit = self.hangul.preedit().unwrap_or_default(); self.in_preedit = !self.preedit.is_empty();
                        self.shared_redraw(); return true;
                    }
                }
            }
            #[cfg(target_os = "macos")]
            if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) && self.hangul.backspace() {
                self.preedit = self.hangul.preedit().unwrap_or_default(); self.in_preedit = !self.preedit.is_empty();
                self.shared_redraw(); return true;
            }
            #[cfg(target_os = "macos")]
            {
                if let Some(done) = self.hangul.flush() { self.shared_field_insert(&done); }
                self.preedit.clear(); self.in_preedit = false;
            }
            #[cfg(not(target_os = "macos"))]
            if self.in_preedit && !mods.chord() { return true; }
            let action = if let Some(form) = &mut self.shared_rooms.form {
                let action = crate::lineedit::edit_event(crate::lineedit::line(FIELD), &mut form.text, &mut form.cursor, event, mods);
                let limit = if matches!(form.kind, FormKind::Fork { .. }) { 2048 } else { 512 };
                form.text = form.text.chars().take(limit).collect(); form.cursor = form.cursor.min(form.text.chars().count());
                action
            } else { crate::lineedit::LineEditAction::Ignored };
            match action {
                crate::lineedit::LineEditAction::Submit => {
                    self.settings_scene.arm_activation_key(NamedKey::Enter);
                    if matches!(self.shared_rooms.form.as_ref().map(|f| &f.kind), Some(FormKind::Confirm { .. })) {
                        self.shared_action(Action::Cancel);
                    } else { self.shared_action(Action::Submit); }
                }
                crate::lineedit::LineEditAction::Cancel => {
                    self.settings_scene.arm_activation_key(NamedKey::Escape);
                    self.shared_action(Action::Cancel);
                }
                _ => self.shared_redraw(),
            }
        } else {
            match event.physical_key {
                PhysicalKey::Code(KeyCode::KeyC) if mods.host => {
                    if self.shared_rooms.reader.selection.is_some() { crate::lineedit::copy_text(&reader_text(&self.shared_rooms.reader, true)); }
                }
                PhysicalKey::Code(KeyCode::KeyA) if mods.host => {
                    let rows = &self.shared_rooms.reader.cache.lines;
                    if let Some(last) = rows.last() { self.shared_rooms.reader.selection = Some(((0, 0), (rows.len() - 1, last.cells.len()))); }
                    self.shared_redraw();
                }
                _ => match event.logical_key {
                    Key::Named(NamedKey::Escape) => { self.settings_scene.arm_activation_key(NamedKey::Escape); self.shared_action(Action::Back); }
                    Key::Named(NamedKey::ArrowUp) => self.shared_scroll(-self.shared_rooms.reader.cell.1 * 3.0),
                    Key::Named(NamedKey::ArrowDown) => self.shared_scroll(self.shared_rooms.reader.cell.1 * 3.0),
                    Key::Named(NamedKey::PageUp) => self.shared_scroll(-self.shared_rooms.reader.rect.map_or(200.0, |r| r.3)),
                    Key::Named(NamedKey::PageDown) => self.shared_scroll(self.shared_rooms.reader.rect.map_or(200.0, |r| r.3)),
                    Key::Named(NamedKey::Home) => { self.shared_rooms.reader.scroll = 0.0; self.shared_rooms.reader.follow = false; self.shared_redraw(); }
                    Key::Named(NamedKey::End) => { self.shared_rooms.reader.follow = true; self.shared_redraw(); }
                    _ => {}
                },
            }
        }
        true
    }

    pub(crate) fn shared_ime(&mut self, ime: &Ime) -> bool {
        if !self.shared_room_active() || !matches!(self.ime_focus, Some(ImeFocus::SharedRooms)) { return false; }
        if self.shared_rooms.pending {
            self.preedit.clear(); self.in_preedit = false;
            return true;
        }
        match ime {
            Ime::Commit(text) => { self.shared_field_insert(text); self.preedit.clear(); self.in_preedit = false; }
            Ime::Preedit(text, _) if self.shared_rooms.form.is_some() => { self.preedit = text.clone(); self.in_preedit = !text.is_empty(); }
            _ => { self.preedit.clear(); self.in_preedit = false; }
        }
        self.shared_redraw(); true
    }

    fn shared_scroll(&mut self, delta: f32) {
        let reader = &mut self.shared_rooms.reader;
        let max = reader.rect.map_or(0.0, |r| (reader.cache.lines.len() as f32 * reader.cell.1 - r.3).max(0.0));
        reader.scroll = (reader.scroll + delta).clamp(0.0, max);
        reader.follow = reader.scroll >= max - 1.0;
        self.shared_redraw();
    }

    pub(crate) fn shared_wheel(&mut self, point: (f32, f32), delta: MouseScrollDelta) -> bool {
        if !self.shared_room_active() || !self.shared_rooms.area.is_some_and(|r| hit(r, point)) { return false; }
        let dy = -match delta { MouseScrollDelta::LineDelta(_, y) => y * 36.0, MouseScrollDelta::PixelDelta(p) => p.y as f32 };
        if let Some(form) = &mut self.shared_rooms.form {
            form.scroll = (form.scroll + dy).clamp(0.0, self.shared_rooms.form_scroll_max);
            self.shared_redraw();
        } else if self.shared_rooms.list_rect.is_some_and(|r| hit(r, point)) {
            self.shared_rooms.list_scroll = (self.shared_rooms.list_scroll + dy).clamp(0.0, self.shared_rooms.list_max);
            self.shared_redraw();
        } else { self.shared_scroll(dy); }
        true
    }

    pub(crate) fn shared_pointer(&mut self, point: (f32, f32)) -> bool {
        if !self.shared_room_active() { return false; }
        if let Some(form) = &mut self.shared_rooms.form {
            if crate::lineedit::pointer_drag(FIELD, &mut form.cursor, form.text.chars().count(), point.0) {
                self.shared_redraw(); return true;
            }
        }
        if self.shared_rooms.reader.dragging {
            let end = reader_position(&self.shared_rooms.reader, point);
            if let Some((_, to)) = &mut self.shared_rooms.reader.selection { *to = end; }
            self.shared_redraw(); return true;
        }
        false
    }

    pub(crate) fn shared_mouse(&mut self, point: (f32, f32), button: MouseButton, state: ElementState) -> bool {
        if !self.shared_room_active() { return false; }
        if state == ElementState::Released {
            let field = crate::lineedit::pointer_release(FIELD);
            if self.shared_rooms.reader.dragging {
                self.shared_rooms.reader.dragging = false;
                let click = self.shared_rooms.reader.selection.is_some_and(|(a, b)| a == b);
                if click {
                    let toggle = self.shared_rooms.hits.iter().find(|(a, r)| matches!(a, Action::ToggleTool(_)) && hit(*r, point)).map(|(a, _)| a.clone());
                    if let Some(action) = toggle { self.shared_action(action); }
                }
                return true;
            }
            return field || self.shared_rooms.area.is_some_and(|r| hit(r, point));
        }
        if !self.shared_rooms.area.is_some_and(|r| hit(r, point)) { return false; }
        if button != MouseButton::Left { return true; }
        if self.shared_rooms.form.is_none() && self.shared_rooms.reader.rect.is_some_and(|r| hit(r, point)) {
            let p = reader_position(&self.shared_rooms.reader, point);
            let from = if self.modifiers.shift_key() { self.shared_rooms.reader.selection.map_or(p, |s| s.0) } else { p };
            self.shared_rooms.reader.selection = Some((from, p)); self.shared_rooms.reader.dragging = true;
            self.shared_redraw(); return true;
        }
        if self.shared_rooms.form.is_some() && crate::lineedit::pointer_hit(FIELD, point) {
            self.ime_retarget(ImeFocus::SharedRooms);
            if let Some(done) = self.hangul.flush() { self.shared_field_insert(&done); }
            self.preedit.clear(); self.in_preedit = false;
            if let Some(form) = &mut self.shared_rooms.form {
                crate::lineedit::pointer_press(FIELD, &mut form.cursor, form.text.chars().count(), point, self.modifiers.shift_key());
            }
            self.shared_redraw(); return true;
        }
        let action = self.shared_rooms.hits.iter().find(|(_, r)| hit(*r, point)).map(|(a, _)| a.clone());
        if let Some(action) = action { self.shared_action(action); }
        true
    }
}

impl App {
    pub(crate) fn shared_pointer_focus(&self) -> bool {
        self.shared_room_active() && !self.main_pointer_modal() && !self.account_menu
            && self.statusbar.popover.is_none() && self.handle_menu.is_none()
            && self.sidebar_menu.is_none() && self.info.navigation.room_menu.is_none()
            && self.info.pane_menu.is_none() && self.info.ctx_menu.is_none()
            && self.info.machine_menu.is_none() && self.file_tree.ctx_menu.is_none()
    }
}

fn reader_position(reader: &Reader, p: (f32, f32)) -> (usize, usize) {
    let Some(r) = reader.rect else { return (0, 0); };
    let row = (((p.1 - r.1 + reader.scroll) / reader.cell.1.max(1.0)).floor().max(0.0) as usize)
        .min(reader.cache.lines.len().saturating_sub(1));
    let col = (((p.0 - r.0) / reader.cell.0.max(1.0)).round().max(0.0) as usize)
        .min(reader.cache.lines.get(row).map_or(0, |l| l.cells.len()));
    (row, col)
}

fn reader_text(reader: &Reader, selected: bool) -> String {
    let range = selected.then_some(reader.selection).flatten().map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
    let mut lines = Vec::new();
    for (r, line) in reader.cache.lines.iter().enumerate() {
        if range.is_some_and(|(a, b)| r < a.0 || r > b.0) { continue; }
        let text: String = line.cells.iter().enumerate().filter(|(c, cell)| {
            cell.ch != '\0' && !cell.hidden && range.is_none_or(|(a, b)| (r, *c) >= a && (r, *c) < b)
        }).map(|(_, cell)| cell.ch).collect();
        lines.push(text.trim_end().to_string());
    }
    lines.join("\n")
}
