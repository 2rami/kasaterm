//! 네이티브 설정 화면의 방 수명 계약.
//!
//! 설정은 터미널 위에 덮는 웹뷰가 아니라 `windows` 목록 안의 내부 방이다. 다만
//! 셸을 가진 사용자 방과 달리 PTY·cwd·캐릭터·복원 기록이 전혀 없다. 이 차이를
//! 문자열 라벨이 아니라 `PaneContent::Settings`와 이 상태로 표현해야 새 기능이
//! 설정 방을 평범한 터미널 anchor로 오인하지 않는다.

use super::*;

/// BSP 트리에만 앉는 설정 화면의 유일한 leaf id. PTY id는 `%N`이라 충돌하지 않는다.
pub(crate) const SETTINGS_PANE_ID: &str = "\0kasaterm-settings";
pub(crate) const SETTINGS_LABEL: &str = "설정";

/// 다음 렌더 단계가 웹 문맥 없이 읽을 수 있는 설정 화면 스냅샷.
#[allow(dead_code)] // 다음 단계의 native painter가 이 typed snapshot을 소비한다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SettingsSceneSnapshot {
    pub(crate) category: SettingsCat,
}

/// 설정 방 고유 상태. 방 인덱스는 일부러 저장하지 않는다.
///
/// 방 순서를 바꾸거나 앞쪽 방을 닫으면 인덱스는 흔들리지만 marker leaf는 그대로다.
/// 돌아갈 자리도 같은 이유로 window index가 아니라 pane id를 기억한다.
#[derive(Clone, Debug)]
pub(crate) struct SettingsScene {
    category: SettingsCat,
    return_pane: Option<String>,
}

impl Default for SettingsScene {
    fn default() -> Self {
        Self {
            category: SettingsCat::General,
            return_pane: None,
        }
    }
}

impl SettingsScene {
    #[allow(dead_code)] // 다음 단계의 native painter 진입점.
    pub(crate) fn snapshot(&self) -> SettingsSceneSnapshot {
        SettingsSceneSnapshot {
            category: self.category,
        }
    }

    pub(crate) fn enter(&mut self, category: Option<SettingsCat>, return_pane: Option<String>) {
        if let Some(category) = category {
            self.category = category;
        }
        if return_pane.as_deref().is_some_and(|id| id != SETTINGS_PANE_ID) {
            self.return_pane = return_pane;
        }
    }

    pub(crate) fn return_pane(&self) -> Option<&str> {
        self.return_pane.as_deref()
    }

    pub(crate) fn leave(&mut self) {
        self.return_pane = None;
    }
}

pub(crate) fn is_settings_layout(layout: &kasa_pty::PtyLayout) -> bool {
    let leaves = layout.leaves();
    leaves.len() == 1 && leaves[0] == SETTINGS_PANE_ID
}

pub(crate) fn should_persist_layout(layout: &kasa_pty::PtyLayout) -> bool {
    !is_settings_layout(layout)
}

fn remap_after_removal(index: usize, removed: usize) -> Option<usize> {
    if index == removed {
        None
    } else {
        Some(index.saturating_sub(usize::from(index > removed)))
    }
}

pub(crate) fn pane_operation_allowed(pane: &str) -> bool {
    pane != SETTINGS_PANE_ID
}

/// 복원 파일에 내부 방이 잘못 실렸던 개발판도 안전하게 읽기 위한 표식 판정.
/// 정식 저장 경로는 이 노드를 애초에 쓰지 않는다.
pub(crate) fn is_saved_settings_window(node: &serde_json::Value) -> bool {
    node.get("leaf").is_some_and(|leaf| {
        leaf.get("internal").and_then(|v| v.as_str()) == Some("settings")
            || leaf.get("pane_id").and_then(|v| v.as_str()) == Some(SETTINGS_PANE_ID)
    })
}

impl App {
    /// 현재 세션에 있는 설정 방. marker를 찾아 매번 계산하므로 reorder에 강하다.
    pub(crate) fn settings_room_index(&self) -> Option<usize> {
        (0..self.windows.len()).find(|&idx| {
            let layout = if idx == self.active_window {
                self.pty_layout.as_ref()
            } else {
                self.windows.get(idx).and_then(Option::as_ref)
            };
            layout.is_some_and(is_settings_layout)
        })
    }

    pub(crate) fn settings_room_active(&self) -> bool {
        self.settings_room_index() == Some(self.active_window)
    }

    #[allow(dead_code)] // 다음 단계의 native painter 진입점.
    pub(crate) fn settings_scene_snapshot(&self) -> Option<SettingsSceneSnapshot> {
        self.settings_room_active()
            .then(|| self.settings_scene.snapshot())
    }

    pub(crate) fn pane_is_settings(&self, pane: &str) -> bool {
        !pane_operation_allowed(pane)
            || self
                .ws
                .lock()
                .ok()
                .and_then(|ws| ws.panes.get(pane).map(|state| state.settings()))
                .unwrap_or(false)
    }

    pub(crate) fn user_room_count(&self) -> usize {
        self.windows
            .len()
            .saturating_sub(usize::from(self.settings_room_index().is_some()))
    }

    fn active_user_pane(&self) -> Option<String> {
        if self.settings_room_active() {
            return None;
        }
        self.ws
            .lock()
            .ok()
            .and_then(|ws| ws.active_pane.clone())
            .filter(|pane| pane != SETTINGS_PANE_ID)
    }

    /// 기어/⌘, 진입점. 이미 만든 방이 있으면 그것을 재사용하고, 없을 때만 PTY 없는
    /// marker 방을 하나 만든다.
    pub(crate) fn open_settings_room(&mut self, category: Option<SettingsCat>) -> bool {
        // 아로나/보드 자식 웹뷰가 떠 있으면 native 방 위를 계속 덮는다. 설정으로
        // 들어가는 순간 먼저 걷어 한 화면의 소유자를 하나로 만든다.
        self.close_inline_web();
        let return_pane = self.active_user_pane();
        self.settings_scene.enter(category, return_pane);

        if let Some(idx) = self.settings_room_index() {
            if idx != self.active_window {
                self.switch_window(idx);
            }
            self.chrome_dirty = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
            return true;
        }

        if self.windows.is_empty() {
            // 정상 부팅은 언제나 사용자 방 하나를 먼저 만든다. 이 가드는 설정 방만
            // 남는 상태를 만들지 않는 마지막 안전망이다.
            return false;
        }

        self.commit_room_rename();
        self.zoomed_pane = None;
        self.windows[self.active_window] = self.pty_layout.take();
        self.windows.push(None);
        self.active_window = self.windows.len() - 1;
        self.win_tab_reveal(self.active_window);

        let mut tab = PaneTab::default();
        tab.content = PaneContent::Settings;
        tab.title = Some("Settings".to_string());
        tab.title_pinned = true;
        let pane = PaneState {
            tabs: vec![tab],
            dirty: true,
            ..Default::default()
        };
        {
            let mut ws = self.ws.lock().unwrap();
            ws.panes.insert(SETTINGS_PANE_ID.to_string(), pane);
            ws.active_pane = Some(SETTINGS_PANE_ID.to_string());
        }
        self.pty_layout = Some(kasa_pty::PtyLayout::single(SETTINGS_PANE_ID));
        self.window_labels_at = None;
        self.chrome_dirty = true;
        self.publish_pty_layout();
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
        true
    }

    pub(crate) fn toggle_settings_room(&mut self, category: Option<SettingsCat>) {
        if self.settings_room_active() && category.is_none() {
            self.close_settings_room();
        } else {
            self.open_settings_room(category);
        }
    }

    /// 설정 방만 즉시 걷고 마지막 사용자 pane의 방으로 돌아간다. 작업 확인창을 띄울
    /// 대상도, 죽일 PTY도 없다.
    pub(crate) fn close_settings_room(&mut self) -> bool {
        let Some(settings_idx) = self.settings_room_index() else {
            return false;
        };
        if self.user_room_count() == 0 {
            return false;
        }

        let was_active = settings_idx == self.active_window;
        let return_idx = self
            .settings_scene
            .return_pane()
            .and_then(|pane| self.window_of_pane(pane))
            .filter(|idx| *idx != settings_idx)
            .or_else(|| (0..self.windows.len()).find(|idx| *idx != settings_idx));

        if was_active {
            self.pty_layout.take();
        }
        self.windows.remove(settings_idx);
        {
            let mut ws = self.ws.lock().unwrap();
            ws.panes.remove(SETTINGS_PANE_ID);
            ws.pid_to_pane.remove(SETTINGS_PANE_ID);
            ws.pane_room.remove(SETTINGS_PANE_ID);
            ws.pane_character.remove(SETTINGS_PANE_ID);
            ws.pane_window.remove(SETTINGS_PANE_ID);
        }

        if was_active {
            let old_target = return_idx.unwrap_or(0);
            let target = old_target.saturating_sub(usize::from(old_target > settings_idx));
            self.active_window = target.min(self.windows.len().saturating_sub(1));
            self.pty_layout = self.windows[self.active_window].take();
            let preferred = self.settings_scene.return_pane().map(str::to_string);
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
        } else if settings_idx < self.active_window {
            self.active_window -= 1;
        }

        // 설정 방 뒤의 사용자 인덱스가 한 칸 당겨지므로 인덱스를 키로 쓰는 chrome
        // 상태도 같은 규칙으로 옮긴다.
        let remap = |idx: usize| remap_after_removal(idx, settings_idx).unwrap_or(0);
        self.window_name_override = std::mem::take(&mut self.window_name_override)
            .into_iter()
            .filter(|(idx, _)| *idx != settings_idx)
            .map(|(idx, name)| (remap(idx), name))
            .collect();
        self.window_alert = std::mem::take(&mut self.window_alert)
            .into_iter()
            .filter(|idx| *idx != settings_idx)
            .map(remap)
            .collect();
        self.expanded_windows = std::mem::take(&mut self.expanded_windows)
            .into_iter()
            .filter(|idx| *idx != settings_idx)
            .map(remap)
            .collect();
        self.expand_anim = self
            .expand_anim
            .filter(|(idx, _, _)| *idx != settings_idx)
            .map(|(idx, opening, at)| (remap(idx), opening, at));
        for closed in &mut self.closed_panes {
            closed.window = remap(closed.window);
        }

        self.settings_scene.leave();
        self.window_labels_at = None;
        self.win_tab_reveal(self.active_window);
        self.chrome_dirty = true;
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_finds_singleton_after_room_reorder() {
        let user_a = kasa_pty::PtyLayout::single("%1");
        let settings = kasa_pty::PtyLayout::single(SETTINGS_PANE_ID);
        let user_b = kasa_pty::PtyLayout::single("%2");
        let mut rooms = vec![user_a, settings, user_b];
        assert_eq!(rooms.iter().position(is_settings_layout), Some(1));
        rooms.swap(0, 2);
        assert_eq!(rooms.iter().position(is_settings_layout), Some(1));
        rooms.swap(1, 2);
        assert_eq!(rooms.iter().position(is_settings_layout), Some(2));
        assert_eq!(rooms.iter().filter(|r| is_settings_layout(r)).count(), 1);
    }

    #[test]
    fn settings_window_is_omitted_and_user_windows_keep_their_order() {
        let layouts = [
            kasa_pty::PtyLayout::single("%1"),
            kasa_pty::PtyLayout::single(SETTINGS_PANE_ID),
            kasa_pty::PtyLayout::single("%2"),
        ];
        let saved: Vec<_> = layouts.iter().filter(|layout| should_persist_layout(layout)).collect();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0].leaves(), vec!["%1"]);
        assert_eq!(saved[1].leaves(), vec!["%2"]);
    }

    #[test]
    fn close_remaps_only_rooms_after_the_internal_room() {
        assert_eq!(remap_after_removal(0, 1), Some(0));
        assert_eq!(remap_after_removal(1, 1), None);
        assert_eq!(remap_after_removal(2, 1), Some(1));
    }

    #[test]
    fn scene_keeps_return_pane_and_deep_link_category() {
        let mut scene = SettingsScene::default();
        scene.enter(Some(SettingsCat::Appearance), Some("%7".into()));
        assert_eq!(scene.snapshot().category, SettingsCat::Appearance);
        assert_eq!(scene.return_pane(), Some("%7"));

        // 설정 방에서 카테고리만 바꿀 때 marker를 돌아갈 pane로 덮지 않는다.
        scene.enter(Some(SettingsCat::Claude), Some(SETTINGS_PANE_ID.into()));
        assert_eq!(scene.snapshot().category, SettingsCat::Claude);
        assert_eq!(scene.return_pane(), Some("%7"));
        scene.leave();
        assert_eq!(scene.return_pane(), None);
    }

    #[test]
    fn settings_leaf_is_never_a_user_operation_anchor() {
        assert!(!pane_operation_allowed(SETTINGS_PANE_ID));
        assert!(pane_operation_allowed("%0"));
        assert_eq!(SETTINGS_LABEL, "설정");
    }

    #[test]
    fn persisted_internal_marker_is_ignored_without_rejecting_old_user_leaves() {
        let internal = serde_json::json!({ "leaf": { "internal": "settings" } });
        let old_user = serde_json::json!({ "leaf": { "cwd": "/repo", "was_claude": false } });
        assert!(is_saved_settings_window(&internal));
        assert!(!is_saved_settings_window(&old_user));
    }
}
