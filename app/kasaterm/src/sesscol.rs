//! 우측 칼럼의 「세션 기록」 탭 — 과거 대화를 골라 그 자리에서 잇는다.
//!
//! 살아 있는 tmux 세션을 보여주는 `/sessions` 패널과 다르다. 이쪽은 각 하네스가
//! 디스크에 남긴 기록(claude jsonl · codex · agy)이고, 목록을 만들려면 그 저장소를
//! 통째로 stat 해야 한다 — 그래서 수집은 `App::pump_sessions_col` 이 워커 스레드로
//! 뺀다(Info 탭이 `ps`/`lsof` 를 GUI 스레드 밖으로 뺀 것과 같은 이유).
//!
//! 별도 OS 창이 아니라 칼럼의 세 번째 탭인 건, 창을 옮기면 흩어지는 게 참고본
//! (Orca 의 상주 레일)과 정반대여서다. 칼럼은 폭 예약(`effective_right_chrome_w`
//! → `window_cells`)·드래그 리사이즈·창 리사이즈 추종을 이미 갖고 있어, 탭을 하나
//! 더 다는 것만으로 그 전부가 따라온다. wry 를 메인 창에 자식으로 붙이는 길은
//! Metal layer 충돌로 이미 접힌 판정이다(커밋 7893e95).
use super::*;
use std::sync::atomic::Ordering::Relaxed;
mod model;
mod paint;
pub(crate) use paint::draw_sessions_col;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Mode(bool),
    Scope(bool),
    Filter(Option<String>),
    Refresh,
    Select(String, String),
    Resume(String, String),
    ClosedSelect(String),
    Reopen(String),
    AskDiscard(String),
    Discard(String),
    CancelDiscard,
}

/// 재수집 간격. 기록은 대화가 끝나야 바뀌므로 Info 탭(1.5초)보다 훨씬 성기다 —
/// 저장소 전체를 stat 하는 비용이라 자주 돌 이유가 없다.
const REFRESH_MS: u64 = 8000;

impl App {
    /// 탭이 보일 때만 워커를 깨워 세션 목록을 새로 고친다. 닫혀 있으면 아무 일도
    /// 안 한다 — 안 보이는 목록 때문에 디스크를 훑을 이유가 없다.
    pub(crate) fn pump_sessions_col(&mut self) {
        if self.info.tab != state::SideTab::Sessions || !self.git.col_visible {
            return;
        }
        if crate::verification_run()
            && std::env::var("KASATERM_AUTOSESSION_PROBE").is_ok_and(|v| {
                matches!(
                    v.as_str(),
                    "retained" | "history" | "history-empty" | "history-check"
                )
            })
        {
            return;
        }
        let cwd = (!self.sessions_col.scope_all)
            .then(|| self.active_pane_cwd())
            .flatten();
        let scope_changed = self.sessions_col.loaded_scope != Some(self.sessions_col.scope_all)
            || cwd != self.sessions_col.cwd;
        if scope_changed {
            self.sessions_col.generation.fetch_add(1, Relaxed);
            self.sessions_col.cwd = cwd.clone();
            self.sessions_col.loaded_scope = Some(self.sessions_col.scope_all);
            self.sessions_col.view.clear();
            if let Ok(mut snap) = self.sessions_col.snap.lock() {
                snap.clear();
            }
            self.sessions_col.selected = None;
            self.sessions_col.keyboard_focus = None;
            self.sessions_col.actions.clear();
            self.sessions_col.scroll = 0.0;
            self.sessions_col.last_refresh = None;
        }
        // 워커가 새 목록을 올렸을 때만 렌더용 사본으로 옮긴다. Info 탭과 달리
        // 커서 동결을 두지 않는 건 이 목록이 8초에 한 번, 그것도 대화가 끝났을
        // 때만 바뀌어서다 — 누르려는 찰나에 행이 밀릴 일이 사실상 없다.
        let rev = self.sessions_col.rev.load(Relaxed);
        if rev != self.sessions_col.seen_rev {
            if let Ok(g) = self.sessions_col.snap.lock() {
                self.sessions_col.view = g.clone();
            }
            self.sessions_col.seen_rev = rev;
        }
        if self.sessions_col.busy.load(Relaxed) {
            return;
        }
        let fresh = self
            .sessions_col
            .last_refresh
            .is_some_and(|t: Instant| t.elapsed() < std::time::Duration::from_millis(REFRESH_MS));
        if fresh {
            return;
        }
        self.sessions_col.last_refresh = Some(Instant::now());
        self.sessions_col.busy.store(true, Relaxed);
        let snap = self.sessions_col.snap.clone();
        let busy = self.sessions_col.busy.clone();
        let rev = self.sessions_col.rev.clone();
        let proxy = self.proxy.clone();
        let scope_all = self.sessions_col.scope_all;
        let generation = self.sessions_col.generation.clone();
        let requested = generation.load(Relaxed);
        std::thread::spawn(move || {
            let mut next = match (scope_all, cwd.as_deref()) {
                (true, _) => kasa_socket::sessions::recent_all_sessions(40),
                (false, None) => Vec::new(),
                (false, Some(p)) => kasa_socket::sessions::recent_sessions_here(p, 40),
            };
            // 누가 맡았던 대화인가. 스폰이 이미 남기고 있는 세션→캐릭터 표를 그대로
            // 읽는다 — claude 를 띄울 때 쓰는 `--session-id` 가 곧 jsonl 파일 이름이라
            // 목록의 `id` 로 바로 조회된다(실측: 이 레포 222개 중 182개가 이미 매핑돼
            // 있다). 그래서 기록을 새로 심지 않았다: 새 저장소를 만들면 오늘 이후
            // 세션만 얼굴이 붙고, 이미 있는 표는 그대로 놀게 된다.
            //
            // 목록을 만드는 `kasa-socket` 이 아니라 여기서 채우는 건 순환 때문이다 —
            // 표를 쥔 `kasa-mcp` 가 `kasa-socket` 을 의존한다.
            for s in next.iter_mut() {
                // codex 갈래 대화는 자기 id 로는 표에 없다 — 부모를 맡은 학생이 곧
                // 이 대화의 학생이다(2026-09-08 지시 「세션탭에 코덱스도 학생이미지」).
                let found = kasa_mcp::character::session_character(&s.id).or_else(|| {
                    (!s.parent.is_empty())
                        .then(|| kasa_mcp::character::session_character(&s.parent))
                        .flatten()
                });
                if let Some(n) = found {
                    s.student = n;
                }
                // 살아 있는 claude 세션은 `/rename` 이름을 라벨로 — 명부가 정본이다
                // (2026-09-08 지시 「클로드세션도 /rename해놓은거 뜨게」).
                //
                // 단 아무도 개명하지 않은 세션의 명부 이름은 셰임이 부팅 때 심는 **세션
                // 주소**(`yuzu-p0-4iz` 꼴)다 — 그것으로 덮으면 원래 라벨이 사라지고 목록이
                // 주소 나열이 된다(2026-09-14 지시 「자동으로 붙는거 아예 없애도돼」).
                if s.harness == "claude" {
                    if let Some(n) = crate::screenread::peer_name_by_sid(&s.id)
                        .filter(|n| !crate::screenread::label_is_roster_agent(n))
                    {
                        s.label = n;
                    }
                }
            }
            // RecentSession 에 PartialEq 가 없어 (하네스, id, mtime, 학생) 으로 비교한다 —
            // 목록이 그대로면 깨우지 않아야 앱이 idle 에 잠든 채로 있는다. 학생까지 보는
            // 건 방금 시작한 세션의 바인딩이 목록보다 늦게 자리잡는 경우가 있어서다.
            let key = |v: &[kasa_socket::backend::RecentSession]| {
                v.iter()
                    .map(|s| {
                        (
                            s.harness.clone(),
                            s.id.clone(),
                            s.mtime,
                            s.student.clone(),
                            s.label.clone(),
                            s.preview.clone(),
                            s.cwd.clone(),
                        )
                    })
                    .collect::<Vec<_>>()
            };
            let changed = match snap.lock() {
                Ok(mut g) => {
                    let differs = generation.load(Relaxed) == requested && key(&g) != key(&next);
                    if differs {
                        *g = next;
                    }
                    differs
                }
                Err(_) => false,
            };
            busy.store(false, Relaxed);
            if changed {
                rev.fetch_add(1, Relaxed);
            }
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }

    /// 활성 pane 의 cwd. 「이 방」범위가 어느 프로젝트를 뜻하는지의 기준이다.
    /// MCP 탭도 같은 기준을 쓴다 — claude 쪽 꺼짐이 폴더마다 다르다.
    pub(crate) fn active_pane_cwd(&self) -> Option<std::path::PathBuf> {
        let active = self
            .ws
            .lock()
            .ok()
            .and_then(|w| w.active_pane.as_deref().map(|pane| w.active_tab_pid(pane)))?;
        self.pane_view_cwd
            .get(&active)
            .or_else(|| self.pane_cwd_cache.get(&active))
            .cloned()
    }

    pub(crate) fn sessions_col_click(&mut self, x: f32, y: f32) -> bool {
        let action = self
            .sessions_col
            .actions
            .iter()
            .find(|(_, r)| inside((x, y), r))
            .map(|(a, _)| a.clone());
        if let Some(action) = action {
            self.sessions_col.keyboard_focus = None;
            self.sessions_col_action(action);
            return true;
        }
        false
    }

    fn sessions_col_action(&mut self, action: Action) {
        use Action::*;
        match action {
            Mode(retained) => {
                self.sessions_col.retained_mode = retained;
                self.sessions_col.scroll = 0.0;
                self.sessions_col.confirm_discard = None;
            }
            Scope(all) => {
                if self.sessions_col.scope_all != all {
                    self.sessions_col.scope_all = all;
                    self.sessions_col.last_refresh = None;
                    self.pump_sessions_col();
                }
            }
            Filter(filter) => {
                self.sessions_col.filter = filter;
                self.sessions_col.scroll = 0.0;
                self.sessions_col.selected = None;
            }
            Refresh => self.sessions_col.last_refresh = None,
            Select(harness, id) => {
                let key = (harness, id);
                self.sessions_col.selected =
                    (self.sessions_col.selected.as_ref() != Some(&key)).then_some(key);
            }
            Resume(harness, id) => {
                let key = (harness, id);
                if self.sessions_col.selected.as_ref() != Some(&key) {
                    return;
                }
                if let Some(s) = self.sessions_col.view.iter().find(|s| model::key(s) == key) {
                    let _ = self.proxy.send_event(UserEvent::ResumeSession {
                        id: s.id.clone(),
                        cwd: (!s.cwd.is_empty()).then(|| s.cwd.clone()),
                        newroom: true,
                        attach: false,
                        harness: s.harness.clone(),
                        reply: None,
                    });
                    self.set_toast("새 방에서 대화를 여는 중…".into());
                    self.sessions_col.selected = None;
                }
            }
            ClosedSelect(key) => {
                self.sessions_col.selected_closed =
                    (self.sessions_col.selected_closed.as_ref() != Some(&key)).then_some(key);
                self.sessions_col.confirm_discard = None;
            }
            Reopen(key) => {
                if let Some(i) = model::closed_index(&self.closed_panes, &key) {
                    self.reopen_closed_pane_at(i);
                }
                self.sessions_col.selected_closed = None;
                self.sessions_col.confirm_discard = None;
            }
            AskDiscard(key) => self.sessions_col.confirm_discard = Some(key),
            CancelDiscard => self.sessions_col.confirm_discard = None,
            Discard(key) => {
                if self.sessions_col.confirm_discard.as_ref() != Some(&key) {
                    return;
                }
                if let Some(i) = model::closed_index(&self.closed_panes, &key) {
                    self.discard_closed_pane_at(i);
                }
                self.sessions_col.selected_closed = None;
                self.sessions_col.confirm_discard = None;
            }
        }
        self.sessions_col.actions.clear();
        self.chrome_dirty = true;
    }

    pub(crate) fn sessions_panel_event(&mut self, event: &WindowEvent) -> bool {
        if !self.git.col_visible
            || self.info.tab != state::SideTab::Sessions
            || self.internal_room_active_any()
        {
            self.sessions_col.keyboard_focus = None;
            self.sessions_col.scroll_drag = None;
            return false;
        }
        let mut scrolling = false;
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self
                .sessions_col
                .scroll_track
                .is_some_and(|r| inside(self.cursor_px, &r)) =>
            {
                let (_, top, _, height) = self.sessions_col.scroll_track.unwrap();
                if let Some((ty, th)) = model::scroll_thumb(
                    top,
                    height,
                    self.sessions_col.content_h,
                    self.sessions_col.scroll,
                ) {
                    self.sessions_col.scroll_drag =
                        Some(if self.cursor_px.1 >= ty && self.cursor_px.1 <= ty + th {
                            self.cursor_px.1 - ty
                        } else {
                            th / 2.0
                        });
                    self.sessions_drag_scroll(self.cursor_px.1);
                }
                scrolling = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                scrolling = self.sessions_col.scroll_drag.take().is_some();
            }
            WindowEvent::CursorMoved { position, .. }
                if self.sessions_col.scroll_drag.is_some() =>
            {
                self.sessions_drag_scroll((position.y / self.effective_scale() as f64) as f32);
                scrolling = true;
            }
            WindowEvent::Focused(false) => {
                self.sessions_col.scroll_drag = None;
                self.sessions_col.keyboard_focus = None;
            }
            _ => {}
        }
        if scrolling {
            self.sessions_col.keyboard_focus = None;
            self.chrome_dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return true;
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        ) && self.cursor_px.0 < self.git_col_x()
        {
            self.sessions_col.keyboard_focus = None;
        }
        if self.sessions_col.keyboard_focus.is_some() {
            if let WindowEvent::Ime(ime) = event {
                self.discard_modal_ime(ime);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
                return true;
            }
        }
        let WindowEvent::KeyboardInput { event, .. } = event else {
            return false;
        };
        self.sessions_panel_key(&event.logical_key, event.state.is_pressed(), event.repeat)
    }

    fn sessions_panel_key(
        &mut self,
        key: &winit::keyboard::Key,
        pressed: bool,
        repeat: bool,
    ) -> bool {
        use winit::keyboard::{Key, NamedKey};
        let active = self.sessions_col.keyboard_focus.is_some();
        let entering =
            matches!(key, Key::Named(NamedKey::Tab)) && self.cursor_px.0 >= self.git_col_x();
        if !active && !entering {
            return false;
        }
        if !active && (self.in_preedit || self.hangul.preedit().is_some()) {
            return false;
        }
        if self.modifiers.super_key()
            && matches!(key, Key::Character(s) if s.eq_ignore_ascii_case("q"))
        {
            return false;
        }
        if !pressed {
            return true;
        }
        match key {
            Key::Named(NamedKey::Tab) => {
                let actions = &self.sessions_col.actions;
                if !actions.is_empty() {
                    let index = actions
                        .iter()
                        .position(|(a, _)| Some(a) == self.sessions_col.keyboard_focus.as_ref());
                    let next = match (index, self.modifiers.shift_key()) {
                        (Some(i), true) => (i + actions.len() - 1) % actions.len(),
                        (Some(i), false) => (i + 1) % actions.len(),
                        (None, true) => actions.len() - 1,
                        (None, false) => 0,
                    };
                    self.sessions_col.keyboard_focus = Some(actions[next].0.clone());
                }
            }
            Key::Named(key @ (NamedKey::Enter | NamedKey::Space)) if !repeat => {
                self.settings_scene.arm_activation_key(*key);
                if let Some(action) = self.sessions_col.keyboard_focus.clone() {
                    if self.sessions_col.actions.iter().any(|(a, _)| a == &action) {
                        self.sessions_col_action(action);
                    }
                }
            }
            Key::Character(value) if value.as_str() == " " && !repeat => {
                self.settings_scene.arm_activation_key(NamedKey::Space);
                if let Some(action) = self.sessions_col.keyboard_focus.clone() {
                    if self.sessions_col.actions.iter().any(|(a, _)| a == &action) {
                        self.sessions_col_action(action);
                    }
                }
            }
            Key::Named(NamedKey::Escape) => {
                self.settings_scene.arm_activation_key(NamedKey::Escape);
                self.sessions_col.keyboard_focus = None;
                self.sessions_col.confirm_discard = None;
            }
            Key::Named(NamedKey::PageDown | NamedKey::PageUp) => {
                let delta = self.sessions_col.body_rect.3
                    * if matches!(key, Key::Named(NamedKey::PageUp)) {
                        -0.8
                    } else {
                        0.8
                    };
                self.sessions_col.scroll = (self.sessions_col.scroll + delta).max(0.0);
                self.sessions_col.keyboard_focus = Some(Action::Refresh);
            }
            _ => {}
        }
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        true
    }

    fn sessions_drag_scroll(&mut self, y: f32) {
        let sc = &mut self.sessions_col;
        let (Some((_, top, _, height)), Some(grab)) = (sc.scroll_track, sc.scroll_drag) else {
            return;
        };
        let Some((_, thumb_h)) = model::scroll_thumb(top, height, sc.content_h, sc.scroll) else {
            return;
        };
        sc.scroll = if height > thumb_h {
            ((y - top - grab) / (height - thumb_h)).clamp(0.0, 1.0) * (sc.content_h - height)
        } else {
            0.0
        };
        sc.actions.clear();
    }

    pub(crate) fn verify_session_history_controls(&mut self) {
        if !crate::verification_run() {
            return;
        }
        let panes = self.pty.len();
        let room = self.active_window;
        let click = |app: &mut App, wanted: Action| {
            let (_, r) = app
                .sessions_col
                .actions
                .iter()
                .find(|(a, _)| a == &wanted)
                .unwrap_or_else(|| panic!("missing history fixture action: {wanted:?}"))
                .clone();
            assert!(app.sessions_col_click(r.0 + r.2 / 2.0, r.1 + r.3 / 2.0));
            app.render_frame();
        };
        click(self, Action::Filter(Some("claude".into())));
        assert_eq!(self.sessions_col.filter.as_deref(), Some("claude"));
        assert!(self
            .sessions_col
            .actions
            .iter()
            .all(|(a, _)| !matches!(a, Action::Select(h, _) if h != "claude")));
        click(self, Action::Select("claude".into(), "fixture-1".into()));
        assert_eq!(
            self.sessions_col.selected,
            Some(("claude".into(), "fixture-1".into()))
        );
        assert!(self.sessions_col.actions.iter().any(
            |(a, _)| matches!(a, Action::Resume(h, id) if h == "claude" && id == "fixture-1")
        ));
        self.sessions_col.view.reverse();
        self.render_frame();
        assert_eq!(
            self.sessions_col.selected,
            Some(("claude".into(), "fixture-1".into()))
        );
        click(self, Action::Mode(true));
        assert!(!self
            .sessions_col
            .actions
            .iter()
            .any(|(a, _)| matches!(a, Action::Resume(..))));
        self.restore_closed_history(&serde_json::json!({"stashed_panes":[
            {"pane_id":"%990","folder":"검증용 폴더","stashed":false,"rec":{"title":"검증용 종료된 셸"}}
        ]}));
        self.render_frame();
        let closed_key = model::closed_key(self.closed_panes.last().unwrap());
        let count = self.closed_panes.len();
        click(self, Action::ClosedSelect(closed_key.clone()));
        click(self, Action::AskDiscard(closed_key.clone()));
        assert_eq!(self.closed_panes.len(), count, "asking to discard is not confirmation");
        click(self, Action::CancelDiscard);
        assert_eq!(self.closed_panes.len(), count);
        click(self, Action::AskDiscard(closed_key.clone()));
        click(self, Action::Discard(closed_key));
        assert_eq!(self.closed_panes.len(), count - 1);
        click(self, Action::Mode(false));
        click(self, Action::Filter(None));
        use winit::event::Ime;
        use winit::keyboard::{Key, NamedKey};
        self.sessions_col.keyboard_focus = Some(Action::Refresh);
        assert!(
            self.sessions_panel_event(&WindowEvent::Ime(Ime::Preedit("가".into(), Some((0, 3)))))
        );
        assert!(self.sessions_panel_event(&WindowEvent::Ime(Ime::Commit(
            "패널의 입력은 터미널로 보내지 않음".into()
        ))));
        assert!(self.preedit.is_empty() && self.os_ime_surface.is_none());
        assert!(self.sessions_panel_key(&Key::Named(NamedKey::PageDown), true, false));
        assert!(self.sessions_panel_key(&Key::Named(NamedKey::PageDown), true, true));
        assert!(self.sessions_col.keyboard_focus.is_some());
        for key in [NamedKey::Enter, NamedKey::Space, NamedKey::Escape] {
            self.sessions_col.keyboard_focus = Some(Action::Refresh);
            self.sessions_panel_key(&Key::Named(key), true, false);
            assert!(self.settings_scene.consume_activation_key(key, true, true));
            assert!(self
                .settings_scene
                .consume_activation_key(key, false, false));
        }
        self.sessions_col.keyboard_focus = Some(Action::Refresh);
        self.sessions_panel_key(&Key::Character(" ".into()), true, false);
        assert!(self
            .settings_scene
            .consume_activation_key(NamedKey::Space, true, true));
        assert!(self
            .settings_scene
            .consume_activation_key(NamedKey::Space, false, false));
        self.sessions_col.keyboard_focus = None;
        self.cursor_px.0 = self.git_col_x() + 30.0;
        let _ = self.hangul.feed('ㄱ');
        let _ = self.hangul.feed('ㅏ');
        let composing = self.hangul.preedit();
        assert!(composing.is_some());
        assert!(!self.sessions_panel_key(&Key::Named(NamedKey::Tab), true, false));
        assert_eq!(self.hangul.preedit(), composing);
        self.discard_modal_ime(&Ime::Disabled);
        self.sessions_col.keyboard_focus = None;
        self.sessions_col.scroll = 0.0;
        self.render_frame();
        assert_eq!(
            panes,
            self.pty.len(),
            "browsing history must not launch a session"
        );
        assert_eq!(room, self.active_window);
        eprintln!("[session-probe] PASS history filter, selection, reorder, modes, confirmed discard, IME, held keys, Hangul and no launch while browsing");
    }
}
/// 하네스별 색. 테마 강조색에서 파생해 라이트/다크·강조색 변경을 따라간다 —
/// 고정 RGB 로 박으면 테마를 바꿨을 때 이 세 줄만 남의 색이 된다.
fn harness_color(harness: &str) -> [u8; 4] {
    match harness {
        "codex" => theme::accent_variant(theme::accent(), 1),
        "agy" => theme::accent_variant(theme::accent(), 2),
        _ => theme::accent(),
    }
}

/// 하네스 로고.
fn harness_icon(harness: &str) -> &'static str {
    match harness {
        "codex" => "codex",
        "agy" => "antigravity",
        _ => "claude",
    }
}

fn inside(cursor: (f32, f32), r: &(f32, f32, f32, f32)) -> bool {
    cursor.0 >= r.0 && cursor.0 < r.0 + r.2 && cursor.1 >= r.1 && cursor.1 < r.1 + r.3
}

#[cfg(test)]
mod harness_icon_tests {
    use super::harness_icon;
    use crate::gpu::GpuRenderer;

    /// 이름이 어긋나면 화면을 봐야만 안다 — 등록되지 않은 이름은 `icon_svg` 가
    /// `None` 을 주고 `queue_icon` 이 아무것도 그리지 않은 채 조용히 돌아간다.
    /// 행에서 로고만 빠지고 나머지는 멀쩡해 회귀로 보이지도 않는다.
    #[test]
    fn every_harness_maps_to_a_registered_icon() {
        for h in ["claude", "codex", "agy", "처음 보는 하네스"] {
            let name = harness_icon(h);
            assert!(
                GpuRenderer::icon_svg(name).is_some(),
                "{h} → \"{name}\" 이 icon_svg 목록에 없다"
            );
        }
    }

    /// antigravity 는 원본 워드마크에서 심볼만 떼어내느라 viewBox 원점이 0 이
    /// 아니다(`14 14 84 84`). 래스터라이저가 그 원점을 접어 넣지 못하면 잉크가
    /// 캔버스 밖으로 밀려 **빈 이미지**가 되는데, 그래도 예외는 안 나고 아이콘만
    /// 사라진다. 그려지는 것과 잘리지 않은 것을 함께 본다.
    #[test]
    fn an_offset_viewbox_still_lands_inside_the_canvas() {
        const PX: u32 = 32;
        for name in ["antigravity", "claude", "codex"] {
            let svg = GpuRenderer::icon_svg(name).expect("등록된 아이콘");
            let buf = GpuRenderer::rasterize_icon(svg, PX).expect("래스터");
            let alpha = |x: u32, y: u32| buf[((y * PX + x) * 4 + 3) as usize];
            let inked = (0..PX * PX)
                .filter(|i| buf[(*i as usize) * 4 + 3] > 8)
                .count();
            let ratio = inked as f32 / (PX * PX) as f32;
            assert!(
                (0.05..0.9).contains(&ratio),
                "{name}: 잉크 비율 {ratio:.3} — 빈 이미지이거나 캔버스를 다 덮었다"
            );
            // 가장자리 한 줄이 통째로 차 있으면 그 방향으로 잘려 나간 것이다.
            let full_row = |y: u32| (0..PX).all(|x| alpha(x, y) > 8);
            let full_col = |x: u32| (0..PX).all(|y| alpha(x, y) > 8);
            assert!(
                !full_row(0) && !full_row(PX - 1) && !full_col(0) && !full_col(PX - 1),
                "{name}: 잉크가 캔버스 가장자리에 꽉 닿았다 — viewBox 밖으로 넘쳤을 수 있다"
            );
        }
    }
}
