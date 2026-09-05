//! 네이티브 설정 화면의 방 수명 계약.
//!
//! 설정은 터미널 위에 덮는 웹뷰가 아니라 `windows` 목록 안의 내부 방이다. 다만
//! 셸을 가진 사용자 방과 달리 PTY·cwd·캐릭터·복원 기록이 전혀 없다. 이 차이를
//! 문자열 라벨이 아니라 `PaneContent::Settings`와 이 상태로 표현해야 새 기능이
//! 설정 방을 평범한 터미널 anchor로 오인하지 않는다.

use super::*;

/// BSP 트리에만 앉는 설정 화면의 유일한 leaf id. PTY id는 `%N`이라 충돌하지 않는다.
pub(crate) const SETTINGS_PANE_ID: &str =
    crate::internal_room::InternalRoomKind::Settings.pane_id();

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
    cache: crate::native_settings::SettingsCache,
    scroll: f32,
    scroll_max: f32,
    hits: Vec<crate::native_settings::Hit>,
    caret_rect: Option<crate::native_settings::Rect>,
    first_run: bool,
    onboarding: crate::native_onboarding::State,
    field_backup: Option<crate::native_settings::FieldBackup>,
    field_dirty: bool,
    inspected_theme: Option<String>,
    dynamic_at: Option<std::time::Instant>,
    picker_drag: Option<(SettingsAction, crate::native_settings::Rect)>,
    sprite_slot: Option<(String, usize)>,
    field_select_all: bool,
    media: Arc<crate::settings_media::SettingsMediaCache>,
    media_started: std::time::Instant,
    multiline_layouts: Vec<crate::native_settings::MultilineLayout>,
    motion_preview_visible: bool,
}

impl Default for SettingsScene {
    fn default() -> Self {
        Self {
            category: SettingsCat::General,
            return_pane: None,
            cache: crate::native_settings::SettingsCache::default(),
            scroll: 0.0,
            scroll_max: 0.0,
            hits: Vec::new(),
            caret_rect: None,
            first_run: crate::onboarding::launch_pending(),
            onboarding: crate::native_onboarding::State::default(),
            field_backup: None,
            field_dirty: false,
            inspected_theme: None,
            dynamic_at: None,
            picker_drag: None,
            sprite_slot: None,
            field_select_all: false,
            media: Arc::new(crate::settings_media::SettingsMediaCache::default()),
            media_started: std::time::Instant::now(),
            multiline_layouts: Vec::new(),
            motion_preview_visible: false,
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
            self.set_category(category);
        }
        if return_pane.as_deref().is_some_and(|id| id != SETTINGS_PANE_ID) {
            self.return_pane = return_pane;
        }
    }

    pub(crate) fn return_pane(&self) -> Option<&str> {
        self.return_pane.as_deref()
    }

    pub(crate) fn cache(&self) -> &crate::native_settings::SettingsCache {
        &self.cache
    }

    pub(crate) fn refresh_cache(&mut self) {
        self.cache.refresh();
    }

    pub(crate) fn refresh_palette_cache(&mut self) {
        self.cache.refresh_palette();
    }

    pub(crate) fn refresh_media_cache(&mut self, plan: &crate::settings_media::MediaPlan) {
        if let Some(media) = Arc::get_mut(&mut self.media) {
            media.ensure(plan);
        } else {
            let mut media = crate::settings_media::SettingsMediaCache::default();
            media.ensure(plan);
            self.media = Arc::new(media);
        }
        self.media_started = std::time::Instant::now();
    }

    pub(crate) fn invalidate_media_cache(&mut self) {
        if let Some(media) = Arc::get_mut(&mut self.media) {
            media.invalidate();
        } else {
            self.media = Arc::new(crate::settings_media::SettingsMediaCache::default());
        }
    }

    pub(crate) fn media(&self) -> Arc<crate::settings_media::SettingsMediaCache> {
        self.media.clone()
    }

    pub(crate) fn media_elapsed(&self) -> std::time::Duration {
        self.media_started.elapsed()
    }

    pub(crate) fn set_account_cache(
        &mut self,
        accounts: Vec<crate::native_settings::AccountChoice>,
    ) {
        self.cache.set_accounts(accounts);
    }

    pub(crate) fn category(&self) -> SettingsCat {
        self.category
    }

    pub(crate) fn set_category(&mut self, category: SettingsCat) {
        if self.category != category {
            self.category = category;
            self.scroll = 0.0;
            self.hits.clear();
            self.caret_rect = None;
        }
    }

    pub(crate) fn scroll(&self) -> f32 {
        self.scroll
    }

    pub(crate) fn scroll_by(&mut self, delta: f32) -> bool {
        let next = (self.scroll + delta).clamp(0.0, self.scroll_max);
        let changed = (next - self.scroll).abs() > f32::EPSILON;
        self.scroll = next;
        changed
    }

    pub(crate) fn hit_at(&self, x: f32, y: f32) -> Option<&crate::native_settings::Hit> {
        self.hits.iter().rev().find(|hit| {
            let (rx, ry, rw, rh) = hit.rect;
            x >= rx && x <= rx + rw && y >= ry && y <= ry + rh
        })
    }

    pub(crate) fn multiline_layout(
        &self,
        field: SettingsInput,
    ) -> Option<&crate::native_settings::MultilineLayout> {
        self.multiline_layouts
            .iter()
            .find(|layout| layout.field == field)
    }

    pub(crate) fn motion_preview_visible(&self) -> bool {
        self.motion_preview_visible
    }

    pub(crate) fn finish_paint(
        &mut self,
        hits: Vec<crate::native_settings::Hit>,
        content_h: f32,
        view_h: f32,
        caret_rect: Option<crate::native_settings::Rect>,
        multiline_layouts: Vec<crate::native_settings::MultilineLayout>,
        motion_preview_visible: bool,
    ) {
        self.scroll_max = (content_h - view_h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, self.scroll_max);
        self.hits = hits;
        self.caret_rect = caret_rect;
        self.multiline_layouts = multiline_layouts;
        self.motion_preview_visible = motion_preview_visible;
    }

    pub(crate) fn caret_rect(&self) -> Option<crate::native_settings::Rect> {
        self.caret_rect
    }

    pub(crate) fn first_run(&self) -> bool {
        self.first_run
    }

    pub(crate) fn finish_onboarding(&mut self) {
        self.first_run = false;
    }

    pub(crate) fn onboarding(&self) -> &crate::native_onboarding::State {
        &self.onboarding
    }

    pub(crate) fn onboarding_mut(&mut self) -> &mut crate::native_onboarding::State {
        &mut self.onboarding
    }

    pub(crate) fn reset_scroll(&mut self) {
        self.scroll = 0.0;
    }

    pub(crate) fn inspected_theme(&self) -> Option<&str> {
        self.inspected_theme.as_deref()
    }

    pub(crate) fn toggle_inspected_theme(&mut self, id: String) {
        if self.inspected_theme.as_deref() == Some(id.as_str()) {
            self.inspected_theme = None;
        } else {
            self.inspected_theme = Some(id);
        }
    }

    pub(crate) fn dynamic_refresh_due(&mut self) -> bool {
        let due = self
            .dynamic_at
            .is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(2));
        if due {
            self.dynamic_at = Some(std::time::Instant::now());
        }
        due
    }

    pub(crate) fn begin_picker_drag(
        &mut self,
        action: SettingsAction,
        rect: crate::native_settings::Rect,
    ) {
        self.picker_drag = Some((action, rect));
    }

    pub(crate) fn picker_drag(&self) -> Option<(SettingsAction, crate::native_settings::Rect)> {
        self.picker_drag.clone()
    }

    pub(crate) fn end_picker_drag(&mut self) -> bool {
        self.picker_drag.take().is_some()
    }

    pub(crate) fn sprite_slot(&self) -> Option<(&str, usize)> {
        self.sprite_slot.as_ref().map(|(motion, frame)| (motion.as_str(), *frame))
    }

    pub(crate) fn toggle_sprite_slot(&mut self, motion: String, frame: usize) {
        if self.sprite_slot.as_ref().is_some_and(|(picked, at)| picked == &motion && *at == frame) {
            self.sprite_slot = None;
        } else {
            self.sprite_slot = Some((motion, frame));
        }
    }

    pub(crate) fn arm_field_backup(&mut self, backup: crate::native_settings::FieldBackup) {
        self.field_backup = Some(backup);
        self.field_dirty = false;
        self.field_select_all = false;
    }

    pub(crate) fn field_backup_matches(&self, field: SettingsInput) -> bool {
        self.field_backup
            .as_ref()
            .is_some_and(|backup| backup.field == field)
    }

    pub(crate) fn field_backup(&self) -> Option<crate::native_settings::FieldBackup> {
        self.field_backup.clone()
    }

    pub(crate) fn mark_field_dirty(&mut self) {
        self.field_dirty = true;
    }

    pub(crate) fn select_all_field(&mut self) {
        self.field_select_all = true;
    }

    pub(crate) fn clear_field_selection(&mut self) {
        self.field_select_all = false;
    }

    pub(crate) fn field_select_all(&self) -> bool {
        self.field_select_all
    }

    pub(crate) fn take_field_select_all(&mut self) -> bool {
        std::mem::take(&mut self.field_select_all)
    }

    pub(crate) fn take_field_backup(
        &mut self,
    ) -> (Option<crate::native_settings::FieldBackup>, bool) {
        (self.field_backup.take(), std::mem::take(&mut self.field_dirty))
    }

    pub(crate) fn leave(&mut self) {
        self.return_pane = None;
        self.hits.clear();
        self.caret_rect = None;
        self.field_backup = None;
        self.field_dirty = false;
        self.inspected_theme = None;
        self.dynamic_at = None;
        self.picker_drag = None;
        self.sprite_slot = None;
        self.field_select_all = false;
        self.media = Arc::new(crate::settings_media::SettingsMediaCache::default());
        self.multiline_layouts.clear();
        self.motion_preview_visible = false;
    }
}

#[cfg(test)]
pub(crate) fn is_settings_layout(layout: &kasa_pty::PtyLayout) -> bool {
    crate::internal_room::is_layout(
        layout,
        crate::internal_room::InternalRoomKind::Settings,
    )
}

#[cfg(test)]
pub(crate) fn should_persist_layout(layout: &kasa_pty::PtyLayout) -> bool {
    crate::internal_room::should_persist_layout(layout)
}

/// 설정 marker를 대상으로 삼으면 안 되는 pane 구조 변경 종류.
pub(crate) use crate::internal_room::InternalMutation as SettingsMutation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsToggle {
    OpenOrFocus,
    ReturnToUser,
}

fn settings_toggle(active: bool, category: Option<SettingsCat>) -> SettingsToggle {
    if active && category.is_none() {
        SettingsToggle::ReturnToUser
    } else {
        SettingsToggle::OpenOrFocus
    }
}

#[cfg(test)]
pub(crate) fn pane_operation_allowed(pane: &str, _mutation: SettingsMutation) -> bool {
    crate::internal_room::pane_operation_allowed(pane, _mutation)
}

/// 렌더가 이미 `Workspace` lock을 잡은 채 호출할 수 있으므로 sentinel 비교만 한다.
/// Settings pane은 생성 시 이 고정 id를 쓰며 다른 id로 바뀌지 않는다.
#[cfg(test)]
pub(crate) fn is_settings_pane(pane: &str) -> bool {
    crate::internal_room::InternalRoomKind::from_pane(pane)
        == Some(crate::internal_room::InternalRoomKind::Settings)
}

/// 복원 파일에 내부 방이 잘못 실렸던 개발판도 안전하게 읽기 위한 표식 판정.
/// 정식 저장 경로는 이 노드를 애초에 쓰지 않는다.
#[cfg(test)]
pub(crate) fn is_saved_settings_window(node: &serde_json::Value) -> bool {
    crate::internal_room::is_saved_window(node)
}

impl App {
    /// 현재 세션에 있는 설정 방. marker를 찾아 매번 계산하므로 reorder에 강하다.
    pub(crate) fn settings_room_index(&self) -> Option<usize> {
        self.internal_room_index(crate::internal_room::InternalRoomKind::Settings)
    }

    pub(crate) fn settings_room_active(&self) -> bool {
        self.internal_room_active(crate::internal_room::InternalRoomKind::Settings)
    }

    #[allow(dead_code)] // 다음 단계의 native painter 진입점.
    pub(crate) fn settings_scene_snapshot(&self) -> Option<SettingsSceneSnapshot> {
        self.settings_room_active()
            .then(|| self.settings_scene.snapshot())
    }

    /// pane 구조를 바꾸기 직전에 통과해야 하는 단일 관문. enum 인자를 강제해 새
    /// 호출자가 단순 "settings 아님" 검사를 복사하지 않고 작업 종류를 명시하게 한다.
    /// 파일·웹처럼 요청 pane이 없거나 이미 사라졌으면 현재 pane으로 폴백하는
    /// 진입점용. 반환된 값은 항상 BSP의 바깥 leaf이고 Settings marker가 아니다.
    pub(crate) fn resolve_user_mutation_anchor(
        &self,
        requested: Option<&str>,
        mutation: SettingsMutation,
    ) -> anyhow::Result<String> {
        let anchor = {
            let ws = self.ws.lock().unwrap();
            let resolve = |pane: &str| {
                if ws.panes.contains_key(pane) {
                    Some(pane.to_string())
                } else {
                    ws.outer_for_pty(pane)
                }
            };
            requested
                .and_then(resolve)
                .or_else(|| {
                    ws.active_pane.as_deref().map(|pane| {
                        ws.outer_for_pty(pane)
                            .unwrap_or_else(|| pane.to_string())
                    })
                })
        }
        .ok_or_else(|| anyhow::anyhow!("기준 pane 이 없다"))?;
        self.ensure_user_mutation_target(&anchor, mutation)?;
        Ok(anchor)
    }

    pub(crate) fn user_room_count(&self) -> usize {
        self.windows.len().saturating_sub(self.internal_room_count())
    }

    /// 기어/⌘, 진입점. 이미 만든 방이 있으면 그것을 재사용하고, 없을 때만 PTY 없는
    /// marker 방을 하나 만든다.
    pub(crate) fn open_settings_room(&mut self, category: Option<SettingsCat>) -> bool {
        // 아로나/보드 자식 웹뷰가 떠 있으면 native 방 위를 계속 덮는다. 설정으로
        // 들어가는 순간 먼저 걷어 한 화면의 소유자를 하나로 만든다.
        self.close_inline_web();
        let return_pane = self.active_user_pane();
        self.settings_scene.enter(category, return_pane);
        // HTTP/모바일이 설정 방이 닫힌 동안 값을 바꿀 수 있다. `ready`는 한 번
        // 그렸다는 뜻일 뿐 최신이라는 뜻이 아니므로, 재진입마다 파일을 다시 읽는다.
        self.settings_scene.refresh_cache();
        self.refresh_native_settings_dynamic_cache();
        self.refresh_native_settings_media_cache();

        if let Some(idx) = self.settings_room_index() {
            if idx != self.active_window {
                self.switch_window(idx);
            }
            self.begin_native_onboarding();
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
        self.begin_native_onboarding();
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
        true
    }

    pub(crate) fn toggle_settings_room(&mut self, category: Option<SettingsCat>) {
        match settings_toggle(self.settings_room_active(), category) {
            SettingsToggle::ReturnToUser => {
                self.return_from_settings_room();
            }
            SettingsToggle::OpenOrFocus => {
                self.open_settings_room(category);
            }
        }
    }

    /// 기어/⌘,로 설정에서 빠져나갈 때는 방을 보관한 채 마지막 사용자 방만 다시
    /// 앞으로 가져온다. 삭제는 ⌘W·×·Esc가 `close_settings_room`으로 명시한다.
    pub(crate) fn return_from_settings_room(&mut self) -> bool {
        let Some(settings_idx) = self.settings_room_index() else {
            return false;
        };
        if settings_idx != self.active_window {
            return false;
        }
        let target = self
            .settings_scene
            .return_pane()
            .and_then(|pane| self.window_of_pane(pane))
            .filter(|idx| *idx != settings_idx)
            .or_else(|| (0..self.windows.len()).find(|idx| *idx != settings_idx));
        let Some(target) = target else {
            return false;
        };
        self.native_settings_blur();
        self.switch_window(target);
        true
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
        self.native_settings_blur();

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
        let remap = |idx: usize| {
            crate::internal_room::remap_after_removal(idx, settings_idx).unwrap_or(0)
        };
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
        assert_eq!(crate::internal_room::remap_after_removal(0, 1), Some(0));
        assert_eq!(crate::internal_room::remap_after_removal(1, 1), None);
        assert_eq!(crate::internal_room::remap_after_removal(2, 1), Some(1));
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
        for mutation in SettingsMutation::ALL {
            assert!(!pane_operation_allowed(SETTINGS_PANE_ID, mutation));
            assert!(pane_operation_allowed("%0", mutation));
        }
        assert_eq!(crate::internal_room::InternalRoomKind::Settings.label(), "설정");
    }

    #[test]
    fn settings_marker_check_is_lock_free_and_exact() {
        assert!(is_settings_pane(SETTINGS_PANE_ID));
        assert!(!is_settings_pane("%0"));
        assert!(!is_settings_pane("kasaterm-settings"));
    }

    #[test]
    fn gear_returns_without_deleting_the_singleton_room() {
        assert_eq!(settings_toggle(true, None), SettingsToggle::ReturnToUser);
        assert_eq!(settings_toggle(false, None), SettingsToggle::OpenOrFocus);
        assert_eq!(
            settings_toggle(true, Some(SettingsCat::Claude)),
            SettingsToggle::OpenOrFocus
        );
    }

    #[test]
    fn every_direct_pane_creator_passes_the_typed_settings_gate_first() {
        fn between<'a>(src: &'a str, start: &str, end: &str) -> &'a str {
            src.split_once(start)
                .unwrap_or_else(|| panic!("missing function start: {start}"))
                .1
                .split_once(end)
                .unwrap_or_else(|| panic!("missing function end: {end}"))
                .0
        }
        fn before(body: &str, gate: &str, mutation: &str) {
            let gate_at = body
                .find(gate)
                .unwrap_or_else(|| panic!("missing settings gate: {gate}"));
            let mutation_at = body
                .find(mutation)
                .unwrap_or_else(|| panic!("missing mutation: {mutation}"));
            assert!(
                gate_at < mutation_at,
                "settings gate must run before {mutation}"
            );
        }

        let session = include_str!("session.rs");
        before(
            between(session, "pub(crate) fn spawn_remote_pane", "pub(crate) fn mirror_remote_pane"),
            "SettingsMutation::RemotePane",
            "remote::connect(",
        );
        before(
            between(session, "pub(crate) fn mirror_remote_pane", "pub(crate) fn reveal_pane_tab"),
            "SettingsMutation::RemoteMirror",
            "remote::connect_view(",
        );
        before(
            between(session, "pub(crate) fn open_file(", "pub(crate) fn open_markdown_window"),
            "SettingsMutation::FilePreview",
            "panes.insert(new_id.clone(), ps)",
        );

        let webpane = include_str!("webpane.rs");
        before(
            between(webpane, "pub(crate) fn open_web_pane", "pub(crate) fn sync_web_hosts"),
            "SettingsMutation::WebPane",
            "spawn_web_host",
        );
    }

    #[test]
    fn scene_paint_contract_clamps_scroll_and_uses_topmost_hit() {
        use crate::native_settings::{Hit, HitCursor, Target};

        let mut scene = SettingsScene::default();
        scene.finish_onboarding();
        scene.set_category(SettingsCat::Appearance);
        let bottom = Hit {
            target: Target::Category(SettingsCat::General),
            rect: (0.0, 0.0, 20.0, 20.0),
            cursor: HitCursor::Pointer,
        };
        let top = Hit {
            target: Target::Category(SettingsCat::Claude),
            rect: (5.0, 5.0, 10.0, 10.0),
            cursor: HitCursor::Pointer,
        };
        scene.finish_paint(
            vec![bottom, top],
            300.0,
            100.0,
            Some((4.0, 5.0, 2.0, 12.0)),
            vec![crate::native_settings::MultilineLayout {
                field: SettingsInput::FeedbackBody,
                rect: (10.0, 20.0, 200.0, 100.0),
                rows: vec![crate::native_settings::VisualRow {
                    start: 0,
                    len: 1,
                    caret_xs: vec![0.0, 12.0],
                }],
                first_line: 0,
                visible_lines: 4,
            }],
            true,
        );
        assert!(scene.scroll_by(500.0));
        assert_eq!(scene.scroll(), 200.0);
        assert_eq!(scene.category(), SettingsCat::Appearance);
        assert_eq!(scene.caret_rect(), Some((4.0, 5.0, 2.0, 12.0)));
        assert!(scene.motion_preview_visible());
        assert!(scene.multiline_layout(SettingsInput::FeedbackBody).is_some());
        assert!(matches!(
            scene.hit_at(8.0, 8.0).map(|hit| &hit.target),
            Some(Target::Category(SettingsCat::Claude))
        ));
        assert!(!scene.first_run());

        scene.arm_field_backup(crate::native_settings::FieldBackup {
            field: SettingsInput::Shell,
            value: "/bin/zsh".to_string(),
            caret: 9,
        });
        assert!(scene.field_backup_matches(SettingsInput::Shell));
        scene.mark_field_dirty();
        let (backup, dirty) = scene.take_field_backup();
        assert!(dirty);
        assert_eq!(backup.map(|value| value.value), Some("/bin/zsh".to_string()));
    }

    #[test]
    fn persisted_internal_marker_is_ignored_without_rejecting_old_user_leaves() {
        let internal = serde_json::json!({ "leaf": { "internal": "settings" } });
        let old_user = serde_json::json!({ "leaf": { "cwd": "/repo", "was_claude": false } });
        assert!(is_saved_settings_window(&internal));
        assert!(!is_saved_settings_window(&old_user));
    }
}
