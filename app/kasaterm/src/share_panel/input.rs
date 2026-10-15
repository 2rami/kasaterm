use super::*;

impl App {
    pub(crate) fn sync_share_ime_cursor(&mut self) {
        if !self.git.col_visible
            || !self.info.shared_page()
            || !self.share.search_focus
            || self.internal_room_active_any()
        {
            return;
        }
        let (Some(window), Some((x, y, _, h))) = (self.window.as_ref(), self.share.caret_rect)
        else {
            return;
        };
        let scale = self.effective_scale();
        let position = ((x * scale).round() as i32, (y * scale).round() as i32);
        if self.ime_cursor_px == Some(position) {
            return;
        }
        self.ime_cursor_px = Some(position);
        window.set_ime_cursor_area(
            winit::dpi::PhysicalPosition::new(position.0, position.1),
            winit::dpi::PhysicalSize::new(1, (h * scale).max(1.0) as u32),
        );
    }

    pub(crate) fn share_search_insert(&mut self, text: &str) {
        self.share.insert(text);
        self.chrome_dirty = true;
    }

    pub(crate) fn share_panel_event(&mut self, event: &WindowEvent) -> bool {
        let active = self.git.col_visible
            && self.info.shared_page()
            && !self.internal_room_active_any();
        // A click can change focus before the platform delivers the old field's commit.
        if !(active && self.share.search_focus) {
            if let WindowEvent::Ime(ime) = event {
                if self.share.deferred_ime(ime) {
                    self.preedit.clear();
                    self.in_preedit = false;
                    self.os_ime_surface = None;
                    self.chrome_dirty = true;
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                    return true;
                }
            }
        }
        if !self.git.col_visible
            || !self.info.shared_page()
            || self.internal_room_active_any()
        {
            self.share.scroll_drag = None;
            return false;
        }
        let scrolling = match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self
                .share
                .scroll_track
                .is_some_and(|rect| contains(rect, self.cursor_px)) =>
            {
                if let Some(thumb) = self.share.thumb() {
                    self.share.scroll_drag = Some(
                        if contains(
                            (thumb.0 - 2.0, thumb.1, thumb.2 + 4.0, thumb.3),
                            self.cursor_px,
                        ) {
                            self.cursor_px.1 - thumb.1
                        } else {
                            thumb.3 * 0.5
                        },
                    );
                    if let Some(text) = self.hangul.flush() {
                        self.share.insert(&text);
                    }
                    self.share.search_focus = false;
                    self.share.keyboard_focus = None;
                    self.preedit.clear();
                    self.in_preedit = false;
                    self.share.drag_scroll(self.cursor_px.1);
                }
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.share.scroll_drag.take().is_some(),
            WindowEvent::CursorMoved { .. } if self.share.scroll_drag.is_some() => {
                self.share.drag_scroll(self.cursor_px.1);
                true
            }
            WindowEvent::Focused(false) => {
                self.share.scroll_drag = None;
                false
            }
            _ => false,
        };
        if scrolling {
            self.chrome_dirty = true;
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            return true;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            if self.cursor_px.0 < self.git_col_x() || self.cursor_px.1 <= TITLE_HEIGHT {
                self.share.keyboard_focus = None;
            }
            if self.share.search_focus
                && !self
                    .share
                    .search_rect
                    .is_some_and(|rect| contains(rect, self.cursor_px))
            {
                if let Some(text) = self.hangul.flush() {
                    self.share.insert(&text);
                }
                self.share.search_focus = false;
                self.preedit.clear();
                self.in_preedit = false;
            }
        }
        if let WindowEvent::KeyboardInput { event, .. } = event {
            use winit::keyboard::{Key, NamedKey};
            if self.share.keyboard_focus.is_some()
                && event.state.is_pressed()
                && matches!(event.logical_key, Key::Named(NamedKey::Tab))
                && !self.in_preedit
            {
                let actions: Vec<_> = self
                    .share
                    .hits
                    .iter()
                    .map(|(action, _)| action.clone())
                    .collect();
                if !actions.is_empty() {
                    let current = actions
                        .iter()
                        .position(|action| Some(action) == self.share.keyboard_focus.as_ref())
                        .unwrap_or(0);
                    let next = if self.modifiers.shift_key() {
                        (current + actions.len() - 1) % actions.len()
                    } else {
                        (current + 1) % actions.len()
                    };
                    let action = actions[next].clone();
                    if let Some(text) = self.hangul.flush() {
                        self.share.insert(&text);
                    }
                    self.share.search_focus = action == Action::Search;
                    if self.share.search_focus {
                        self.ime_retarget(ImeFocus::ShareSearch);
                    }
                    self.share.keyboard_focus = Some(action);
                }
                self.chrome_dirty = true;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
                return true;
            }
            if !self.share.search_focus && self.share.keyboard_focus.is_some() {
                if !event.state.is_pressed() {
                    return true;
                }
                if self.host_mod()
                    && matches!(&event.logical_key, Key::Character(key) if key.eq_ignore_ascii_case("q"))
                {
                    return false;
                }
                match event.logical_key {
                    Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) if !event.repeat => {
                        if let Some(action) = self.share.keyboard_focus.clone() {
                            self.share_action(action);
                        }
                    }
                    Key::Named(NamedKey::Escape) => {
                        self.share.keyboard_focus = None;
                        self.chrome_dirty = true;
                    }
                    _ => {}
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
                return true;
            }
        }
        if !self.share.search_focus {
            if self.share.keyboard_focus.is_some() {
                if let WindowEvent::Ime(ime) = event {
                    self.discard_modal_ime(ime);
                    return true;
                }
            }
            return false;
        }
        match event {
            WindowEvent::Ime(ime) => match ime {
                Ime::Enabled => self.ime_active = true,
                Ime::Disabled => {
                    self.share.pending_search_commit = false;
                    self.ime_active = false;
                    self.preedit.clear();
                    self.in_preedit = false;
                }
                Ime::Preedit(text, _) => {
                    self.share.pending_search_commit = !text.is_empty();
                    self.preedit = text.clone();
                    self.in_preedit = !text.is_empty();
                    self.ime_active = true;
                }
                Ime::Commit(text) => {
                    self.share.pending_search_commit = false;
                    self.share.insert(text);
                    self.preedit.clear();
                    self.in_preedit = false;
                }
            },
            WindowEvent::KeyboardInput { event, .. } => {
                if !event.state.is_pressed() {
                    return true;
                }
                use winit::keyboard::{Key, NamedKey};
                let command = if cfg!(target_os = "macos") {
                    self.modifiers.super_key()
                } else {
                    self.modifiers.control_key()
                };
                if command {
                    match &event.logical_key {
                        Key::Character(key) if key.eq_ignore_ascii_case("q") => return false,
                        Key::Character(key) if key.eq_ignore_ascii_case("a") => {
                            self.share.select_all = true
                        }
                        Key::Character(key) if key.eq_ignore_ascii_case("v") => {
                            if let Ok(text) = arboard::Clipboard::new()
                                .and_then(|mut clipboard| clipboard.get_text())
                            {
                                self.share.insert(&text);
                            }
                        }
                        Key::Character(key) if key.eq_ignore_ascii_case("c") => {
                            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                let _ = clipboard.set_text(self.share.query.clone());
                            }
                        }
                        _ => {}
                    }
                } else {
                    if !cfg!(target_os = "macos") && self.in_preedit {
                        return true;
                    }
                    #[cfg(target_os = "macos")]
                    if let Some(character) = event
                        .text
                        .as_deref()
                        .filter(|text| text.chars().count() == 1)
                        .and_then(|text| text.chars().next())
                    {
                        if (0x3130..=0x318f).contains(&(character as u32)) {
                            if let Some(text) = self.hangul.feed(character) {
                                self.share.insert(&text);
                            }
                            self.preedit = self.hangul.preedit().unwrap_or_default();
                            self.in_preedit = !self.preedit.is_empty();
                            self.chrome_dirty = true;
                            if let Some(window) = &self.window {
                                window.request_redraw();
                            }
                            return true;
                        }
                    }
                    if matches!(event.logical_key, Key::Named(NamedKey::Backspace))
                        && self.hangul.backspace()
                    {
                        self.preedit = self.hangul.preedit().unwrap_or_default();
                        self.in_preedit = !self.preedit.is_empty();
                    } else {
                        if let Some(text) = self.hangul.flush() {
                            self.share.insert(&text);
                        }
                        self.preedit.clear();
                        self.in_preedit = false;
                        let edit = if self.share.select_all
                            && matches!(
                                event.logical_key,
                                Key::Named(NamedKey::Backspace | NamedKey::Delete)
                            ) {
                            self.share.query.clear();
                            self.share.caret = 0;
                            self.share.select_all = false;
                            crate::lineedit::LineEditAction::Edited
                        } else if self.share.select_all
                            && matches!(
                                event.logical_key,
                                Key::Named(
                                    NamedKey::ArrowLeft
                                        | NamedKey::ArrowRight
                                        | NamedKey::Home
                                        | NamedKey::End
                                )
                            )
                        {
                            self.share.caret = if matches!(
                                event.logical_key,
                                Key::Named(NamedKey::ArrowLeft | NamedKey::Home)
                            ) {
                                0
                            } else {
                                self.share.query.chars().count()
                            };
                            self.share.select_all = false;
                            crate::lineedit::LineEditAction::Moved
                        } else if let Key::Character(text) = &event.logical_key {
                            self.share.insert(text);
                            crate::lineedit::LineEditAction::Edited
                        } else {
                            crate::lineedit::key(
                                &mut self.share.query,
                                &mut self.share.caret,
                                &event.logical_key,
                            )
                        };
                        match edit {
                            crate::lineedit::LineEditAction::Cancel
                            | crate::lineedit::LineEditAction::Submit => {
                                self.share.search_focus = false;
                                self.share.keyboard_focus = None;
                                self.share.select_all = false;
                            }
                            crate::lineedit::LineEditAction::Edited => {
                                self.share.scroll = 0.0;
                                self.share.rebuild();
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => return false,
        }
        self.chrome_dirty = true;
        if let Some(window) = &self.window {
            window.request_redraw();
        }
        true
    }
}
