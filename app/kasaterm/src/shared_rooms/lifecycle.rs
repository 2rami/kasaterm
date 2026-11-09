use super::*;

impl State {
    pub(crate) fn return_pane(&self) -> Option<&str> { self.return_pane.as_deref() }
}

impl App {
    pub(crate) fn open_shared_room_view(&mut self, room: Option<&str>) -> bool {
        use crate::internal_room::InternalRoomKind;
        if self.windows.is_empty() { return false; }
        self.close_inline_web();
        if !self.shared_room_active() {
            self.shared_rooms.return_pane = self.active_user_pane().or_else(||
                self.internal_room_kind_at(self.active_window)
                    .and_then(|kind| self.internal_return_pane(kind)).map(str::to_owned));
        }
        if let Some(index) = self.internal_room_index(InternalRoomKind::SharedRooms) {
            if index != self.active_window { self.switch_window(index); }
        } else {
            self.commit_room_rename();
            self.zoomed_pane = None;
            self.windows[self.active_window] = self.pty_layout.take();
            self.windows.push(None);
            self.active_window = self.windows.len() - 1;
            let mut tab = PaneTab::default();
            tab.content = PaneContent::SharedRooms;
            tab.title = Some("공유방".into());
            tab.title_pinned = true;
            let mut ws = self.ws.lock().unwrap();
            ws.panes.insert(PANE.into(), PaneState { tabs: vec![tab], dirty: true, ..PaneState::default() });
            ws.active_pane = Some(PANE.into());
            drop(ws);
            self.pty_layout = Some(kasa_pty::PtyLayout::single(PANE));
        }
        self.ime_retarget(ImeFocus::SharedRooms);
        if let Some(room) = room { self.shared_select_room(room); }
        self.shared_rooms.refreshed = None;
        self.window_labels_at = None;
        self.sync_shared_navigation();
        self.chrome_dirty = true;
        self.publish_pty_layout();
        if let Some(w) = &self.window { w.request_redraw(); }
        self.prime_hit_areas();
        true
    }

    pub(crate) fn close_shared_room_view(&mut self) -> bool {
        use crate::internal_room::InternalRoomKind;
        let Some(index) = self.internal_room_index(InternalRoomKind::SharedRooms) else { return false; };
        if self.user_room_count() == 0 { return false; }
        self.shared_rooms.cancel_view();
        self.shared_rooms.reset_reader();
        crate::lineedit::forget(FIELD);
        let active = index == self.active_window;
        let back = self.shared_rooms.return_pane.as_deref().and_then(|id| self.window_of_pane(id))
            .filter(|i| *i != index)
            .or_else(|| (0..self.windows.len()).find(|i| *i != index && self.internal_room_kind_at(*i).is_none()));
        if active { self.pty_layout.take(); }
        self.windows.remove(index);
        {
            let mut ws = self.ws.lock().unwrap();
            ws.panes.remove(PANE); ws.pid_to_pane.remove(PANE); ws.pane_room.remove(PANE);
            ws.pane_character.remove(PANE); ws.pane_window.remove(PANE);
        }
        if active {
            let old = back.unwrap_or(0);
            self.active_window = old.saturating_sub(usize::from(old > index)).min(self.windows.len() - 1);
            self.pty_layout = self.windows[self.active_window].take();
            let preferred = self.shared_rooms.return_pane.clone();
            let focus = preferred.filter(|p| self.pty_layout.as_ref().is_some_and(|l| l.leaves().contains(&p.as_str())))
                .or_else(|| self.pty_layout.as_ref().and_then(|l| l.leaves().first().map(|p| p.to_string())));
            self.ws.lock().unwrap().active_pane = focus;
            self.handoff_ime_to_active_surface();
        } else if index < self.active_window { self.active_window -= 1; }
        self.remap_removed_window_metadata(index);
        self.sync_shared_navigation();
        self.window_labels_at = None;
        self.chrome_dirty = true;
        self.publish_pty_layout();
        if let Some(w) = &self.window { w.request_redraw(); }
        true
    }
}
