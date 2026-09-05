//! 네이티브 보드의 PTY 없는 싱글턴 방 수명.

use super::*;

pub(crate) const BOARD_PANE_ID: &str =
    crate::internal_room::InternalRoomKind::Board.pane_id();
#[cfg(test)]
pub(crate) const BOARD_LABEL: &str = crate::internal_room::InternalRoomKind::Board.label();

#[cfg(test)]
pub(crate) fn is_board_layout(layout: &kasa_pty::PtyLayout) -> bool {
    crate::internal_room::is_layout(layout, crate::internal_room::InternalRoomKind::Board)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BoardToggle {
    OpenOrFocus,
    ReturnToUser,
}

fn board_toggle(active: bool) -> BoardToggle {
    if active {
        BoardToggle::ReturnToUser
    } else {
        BoardToggle::OpenOrFocus
    }
}

impl App {
    pub(crate) fn board_room_index(&self) -> Option<usize> {
        self.internal_room_index(crate::internal_room::InternalRoomKind::Board)
    }

    pub(crate) fn board_room_active(&self) -> bool {
        self.internal_room_active(crate::internal_room::InternalRoomKind::Board)
    }

    pub(crate) fn open_board_room(&mut self) -> bool {
        self.close_inline_web();
        let return_pane = self.active_user_pane().or_else(|| {
            self.settings_room_active()
                .then(|| self.settings_scene.return_pane().map(str::to_string))
                .flatten()
        });
        let target_window = return_pane
            .as_deref()
            .and_then(|pane| self.window_of_pane(pane))
            .unwrap_or(self.active_window);
        let target_cwd = return_pane
            .as_deref()
            .and_then(|pane| self.pane_current_cwd(pane))
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.board_scene
            .enter(return_pane, target_window, target_cwd);

        if let Some(idx) = self.board_room_index() {
            if idx != self.active_window {
                self.switch_window(idx);
            }
            self.request_native_board_refresh();
            self.chrome_dirty = true;
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            return true;
        }
        if self.user_room_count() == 0 {
            return false;
        }

        self.commit_room_rename();
        self.zoomed_pane = None;
        self.windows[self.active_window] = self.pty_layout.take();
        self.windows.push(None);
        self.active_window = self.windows.len() - 1;
        self.win_tab_reveal(self.active_window);

        let mut tab = PaneTab::default();
        tab.content = PaneContent::Board;
        tab.title = Some("Board".to_string());
        tab.title_pinned = true;
        let pane = PaneState {
            tabs: vec![tab],
            dirty: true,
            ..Default::default()
        };
        {
            let mut ws = self.ws.lock().unwrap();
            ws.panes.insert(BOARD_PANE_ID.to_string(), pane);
            ws.active_pane = Some(BOARD_PANE_ID.to_string());
        }
        self.pty_layout = Some(kasa_pty::PtyLayout::single(BOARD_PANE_ID));
        self.window_labels_at = None;
        self.chrome_dirty = true;
        self.publish_pty_layout();
        self.request_native_board_refresh();
        if let Some(window) = &self.window {
            window.request_redraw();
        }
        true
    }

    pub(crate) fn toggle_board_room(&mut self) {
        match board_toggle(self.board_room_active()) {
            BoardToggle::ReturnToUser => {
                self.return_from_board_room();
            }
            BoardToggle::OpenOrFocus => {
                self.open_board_room();
            }
        }
    }

    pub(crate) fn return_from_board_room(&mut self) -> bool {
        let Some(board_idx) = self.board_room_index() else {
            return false;
        };
        if board_idx != self.active_window {
            return false;
        }
        let target = self
            .board_scene
            .return_pane()
            .and_then(|pane| self.window_of_pane(pane))
            .filter(|idx| *idx != board_idx)
            .or_else(|| {
                (0..self.windows.len())
                    .find(|idx| *idx != board_idx && self.internal_room_kind_at(*idx).is_none())
            });
        let Some(target) = target else {
            return false;
        };
        self.switch_window(target);
        true
    }

    pub(crate) fn close_board_room(&mut self) -> bool {
        let Some(board_idx) = self.board_room_index() else {
            return false;
        };
        if self.user_room_count() == 0 {
            return false;
        }
        let was_active = board_idx == self.active_window;
        let return_idx = self
            .board_scene
            .return_pane()
            .and_then(|pane| self.window_of_pane(pane))
            .filter(|idx| *idx != board_idx)
            .or_else(|| {
                (0..self.windows.len())
                    .find(|idx| *idx != board_idx && self.internal_room_kind_at(*idx).is_none())
            });
        if was_active {
            self.pty_layout.take();
        }
        self.windows.remove(board_idx);
        {
            let mut ws = self.ws.lock().unwrap();
            ws.panes.remove(BOARD_PANE_ID);
            ws.pid_to_pane.remove(BOARD_PANE_ID);
            ws.pane_room.remove(BOARD_PANE_ID);
            ws.pane_character.remove(BOARD_PANE_ID);
            ws.pane_window.remove(BOARD_PANE_ID);
        }
        if was_active {
            let old_target = return_idx.unwrap_or(0);
            let target = old_target.saturating_sub(usize::from(old_target > board_idx));
            self.active_window = target.min(self.windows.len().saturating_sub(1));
            self.pty_layout = self.windows[self.active_window].take();
            let preferred = self.board_scene.return_pane().map(str::to_string);
            let focus = preferred.filter(|pane| {
                self.pty_layout
                    .as_ref()
                    .is_some_and(|layout| layout.leaves().contains(&pane.as_str()))
            });
            let focus = focus.or_else(|| {
                self.pty_layout
                    .as_ref()
                    .and_then(|layout| layout.leaves().first().map(|pane| pane.to_string()))
            });
            self.ws.lock().unwrap().active_pane = focus;
            self.handoff_ime_to_active_surface();
        } else if board_idx < self.active_window {
            self.active_window -= 1;
        }

        let remap = |idx: usize| {
            crate::internal_room::remap_after_removal(idx, board_idx).unwrap_or(0)
        };
        self.window_name_override = std::mem::take(&mut self.window_name_override)
            .into_iter()
            .filter(|(idx, _)| *idx != board_idx)
            .map(|(idx, name)| (remap(idx), name))
            .collect();
        self.window_alert = std::mem::take(&mut self.window_alert)
            .into_iter()
            .filter(|idx| *idx != board_idx)
            .map(remap)
            .collect();
        self.expanded_windows = std::mem::take(&mut self.expanded_windows)
            .into_iter()
            .filter(|idx| *idx != board_idx)
            .map(remap)
            .collect();
        self.expand_anim = self
            .expand_anim
            .filter(|(idx, _, _)| *idx != board_idx)
            .map(|(idx, opening, at)| (remap(idx), opening, at));
        for closed in &mut self.closed_panes {
            closed.window = remap(closed.window);
        }

        self.board_scene.leave();
        self.window_labels_at = None;
        self.win_tab_reveal(self.active_window);
        self.chrome_dirty = true;
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(window) = &self.window {
            window.request_redraw();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_marker_is_a_singleton_internal_layout() {
        let board = kasa_pty::PtyLayout::single(BOARD_PANE_ID);
        assert!(is_board_layout(&board));
        assert!(!crate::internal_room::should_persist_layout(&board));
        assert_eq!(BOARD_LABEL, "보드");
    }

    #[test]
    fn view_menu_returns_without_deleting_the_board_room() {
        assert_eq!(board_toggle(true), BoardToggle::ReturnToUser);
        assert_eq!(board_toggle(false), BoardToggle::OpenOrFocus);
    }

    #[test]
    fn desktop_board_has_no_wry_route_and_arona_keeps_its_route() {
        let chrome = include_str!("chrome.rs");
        assert!(!chrome.contains("InlineWebKind::Board"));
        assert!(!chrome.contains("panel=board"));
        assert!(chrome.contains("self.toggle_board_room()"));
        assert!(chrome.contains("InlineWebKind::Arona"));
        assert!(chrome.contains("view=classroom"));
    }
}
