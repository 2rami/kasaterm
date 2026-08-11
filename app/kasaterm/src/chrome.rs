//! 사이드바·git col·파일트리 토글·패널·줌/폰트·toast 등 chrome UI 메서드.
use super::*;

/// How long a completion notification pulses a pane header / sidebar done-dot.
const NOTIFY_FLASH_MS: u128 = 1800;

/// 권한 대기 토스트 — 완료 토스트와 같은 원칙(캐릭터 고정값 + hook reason,
/// 미현존이면 reason 만). Notification hook 경로.
fn format_attention_toast(character: Option<&str>, reason: &str) -> String {
    let reason = reason.trim();
    match (character, reason.is_empty()) {
        (Some(c), true) => format!("⚠ {c} — 권한 대기중"),
        (Some(c), false) => format!("⚠ {c} — {reason}"),
        (None, true) => "⚠ 권한 대기중".to_string(),
        (None, false) => format!("⚠ 권한 대기중 — {reason}"),
    }
}

impl App {
    /// pane 의 표시용 학생 — "터미널은 파싱만"(거노): claude sessionId 바인딩이 정본,
    /// agents/attach 뷰 pane 은 파싱 전 스폰 랜덤(ws.pane_character)을 보여주지 않는다
    /// (거노: 세션 진입 직후 다른 학생으로 보임 — 뷰 pane 의 로컬 배정은 무의미한 잔재
    /// 라 None 으로 두면 학생 시각 요소가 중립으로 남는다). 일반 pane 은 스폰 배정
    /// 폴백 유지(첫 프레임부터 학생 표시). render 의 프사·타이틀바·테두리가 공유한다.
    ///
    /// **탭 접기는 여기서 한다** — 부르는 쪽은 BSP leaf(outer)를 들고 있는데 학생 상태는
    /// 탭 pid 로 기록된다. 접지 않으면 탭으로 띄운 학생이 화면에 아예 안 나온다.
    pub(crate) fn display_pane_char(&self, ws: &Workspace, id: &str) -> Option<String> {
        let id = &ws.active_tab_pid(id);
        self.pane_claude_sid
            .get(id)
            .and_then(|sid| kasa_mcp::character::session_character(sid))
            .or_else(|| {
                let view =
                    self.pty.get(id).map(|p| p.is_claude_agents()).unwrap_or(false);
                if view {
                    None
                } else {
                    ws.pane_character.get(id).cloned()
                }
            })
    }

    /// pane 에 **학생색을 입힐지**의 정본. 이름(`display_pane_char`)과 달리
    /// 「지금 에이전트가 도는가」 관문을 지난다 — 순수 셸 pane 에 남의 학생색이
    /// 둘리면 「저기 누가 있다」로 잘못 읽히기 때문이고, 메인 그리드의 pane 테두리가
    /// 이미 그 규칙이다(`render.rs` 의 `claude_panes` 필터).
    ///
    /// 별도창(터미널·방)이 이걸 안 쓰고 `ws.pane_character` 를 날로 읽던 동안,
    /// 같은 pane 이 창마다 다른 대접을 받았다 — 셸 pane 이 별도창에선 학생색·이름을
    /// 달고 메인에선 무채색이라, 되돌리면 「학생 테마가 깨졌다」로 보였다(거노).
    /// 관문은 이름과 같은 키(**탭 pid**)로 본다 — 탭에서 도는 학생을 놓치지 않게.
    pub(crate) fn pane_accent(&self, ws: &Workspace, id: &str) -> Option<[u8; 4]> {
        let tab = ws.active_tab_pid(id);
        self.pty.get(tab.as_str()).and_then(|p| p.active_agent())?;
        let name = self.display_pane_char(ws, id)?;
        crate::theme::character_accent_n(&name, crate::theme::character_ordinal(&ws.pane_character, id))
    }

    /// A pane's claude finished (Stop hook → `kasaterm-cli notify` → socket →
    /// `UserEvent::Notify`). Flash the pane's header and, unless the user is
    /// already looking at that exact pane (our window focused + it's the
    /// active pane), raise a desktop alert. cmux-style suppression keeps the
    /// alert for the cases that actually need attention (background window or
    /// a sibling pane).
    pub(crate) fn handle_notify(&mut self, surface_id: &str, title: &str, body: &str) {
        let now = std::time::Instant::now();
        let is_active_pane = self
            .ws
            .lock()
            .unwrap()
            .active_pane
            .as_deref()
            == Some(surface_id);
        // claude's Stop hook fired → this pane's turn is DONE. Trust this push
        // over the glyph heuristic: force the pane idle right now so the working
        // bar can't linger on a stale "✻ Churned for 42s" line, and drop the
        // busy-grace timer. The glyph working→idle path in
        // `refresh_pane_activity` then sees the pane is already idle.
        //
        // 완료 화면 토스트는 제거(거노 2026-07-27) — 학생이 많아 턴마다 떠서 시야를
        // 가린다. 완료 신호는 탭 펄스·dock 배지·백그라운드 데스크톱 알림(아래)으로
        // 전달된다.
        self.pane_last_busy.remove(surface_id);
        self.pane_activity
            .entry(surface_id.to_string())
            .and_modify(|a| a.status = "idle".to_string())
            .or_insert_with(|| crate::stream::PaneStatusView {
                status: "idle".to_string(),
                ..Default::default()
            });
        self.notify_flash.insert(surface_id.to_string(), now);
        // A pane in a *background* window finished — pulse that window's sidebar
        // tab until the user switches to it (switch_window clears the entry).
        if let Some(wi) = self.window_of_pane(surface_id) {
            if wi != self.active_window {
                self.window_alert.insert(wi);
            }
        }
        self.chrome_dirty = true;
        // 읽음 처리(=dock 배지)만 지금 보고 있는 pane 을 뺀다. 데스크톱 알림 자체는
        // 그 pane 을 보고 있어도 쏜다 — 거노 2026-08-11 "pane별로 그냥 다오게하자".
        // 학생이 여럿이면 어느 창을 보고 있든 나머지가 끝난 걸 놓치는 쪽이 손해다.
        if !(self.window_focused && is_active_pane) {
            self.unread_panes.insert(surface_id.to_string());
        }
        let who = self.pane_character_if_known(surface_id);
        // 누구 알림인지는 프사(오른쪽 썸네일)와 제목 둘 다로 말한다 — 캐릭터가 없는
        // 순정 pane 은 프사가 안 붙으므로 제목만 남는다.
        let titled = match who.as_deref() {
            Some(c) => format!("{c} · {title}"),
            None => title.to_string(),
        };
        notify_desktop(&titled, body, who.as_deref());
    }

    /// A pane's claude is blocked on a permission / input prompt (its
    /// `Notification` hook → `kasaterm-cli attention` → `UserEvent::Attention`).
    /// Toast + pulse the pane, and unless the user is already looking at that
    /// exact pane (our window focused + it's the active pane), raise a desktop
    /// alert. Same suppression as `handle_notify`, but this is the *attention*
    /// case — the agent isn't done, it's stuck waiting on you. The board's
    /// `waiting` flag is set separately in `collab_board` (socket thread, off
    /// the shared attention map); here we only own the GUI-side surfacing.
    pub(crate) fn handle_attention(&mut self, surface_id: &str, reason: &str) {
        let now = std::time::Instant::now();
        let is_active_pane = self.ws.lock().unwrap().active_pane.as_deref() == Some(surface_id);
        // 캐릭터명(pane 고정) + hook reason(완료 순간). OSC 작업명은 안 쓴다.
        let character = self.pane_character_if_known(surface_id);
        let reason = reason.trim();
        self.notify_flash.insert(surface_id.to_string(), now);
        // Attention raised in a background window — pulse its sidebar tab too.
        if let Some(wi) = self.window_of_pane(surface_id) {
            if wi != self.active_window {
                self.window_alert.insert(wi);
            }
        }
        self.chrome_dirty = true;
        // 이미 sticky 승인 토스트(칩 포함)가 이 pane 으로 떠 있으면 hook 의
        // 중복 알림으로 텍스트를 덮지 않는다.
        if self.collab.toast_action.as_deref() != Some(surface_id) {
            self.collab.toast =
                Some((format_attention_toast(character.as_deref(), reason), now));
            self.collab.toast_rect = None;
        }
        if !(self.window_focused && is_active_pane) {
            self.unread_panes.insert(surface_id.to_string());
        }
        // 완료 알림과 같은 이유로 억제하지 않는다 — 막혀 선 학생은 더더욱 놓치면
        // 안 되는 쪽이다(그 pane 을 보고 있었다면 어차피 화면에도 토스트가 떠 있다).
        let who = character.as_deref().unwrap_or("pane");
        let body = if reason.is_empty() {
            who.to_string()
        } else {
            format!("{who} — {reason}")
        };
        notify_desktop("⚠ 권한 필요", &body, character.as_deref());
    }

    /// pane 이 현존하고 캐릭터가 배정됐으면 그 이름(고정값) — 토스트 "누가" 소스.
    /// 미현존(resume/재사용으로 surface_id 가 stale)이거나 순정 pane 이면 None →
    /// 호출부는 hook 정보만으로 폴백(토스트를 드롭하지 않는다).
    pub(crate) fn pane_character_if_known(&self, id: &str) -> Option<String> {
        let ws = self.ws.lock().unwrap();
        let known = ws.panes.contains_key(id) || self.pty.contains_key(id);
        let key = ws.active_tab_pid(id);
        known.then(|| ws.pane_character.get(&key).cloned()).flatten()
    }

    /// claude 가 떠 있는 pane 을 기억해 둔다 — 얼굴을 내보일 자격.
    ///
    /// pane 이 사라지면 같이 잊는다. surface id 는 재사용되므로, 안 지우면 새로 난
    /// 셸 pane 이 남의 자격을 물려받아 켜지도 않은 학생을 달고 뜬다.
    pub(crate) fn note_claude_panes(&mut self) {
        // 판정은 **OSC 제목** 이 먼저다. claude 는 뜨자마자 「✳ Claude Code」를
        // 보내는데, 프로세스 이름 쪽은 셸의 직계 자식을 500ms 캐시로 훑는 경로라
        // 헤드리스 실측에서 claude 가 떠 있는데도 계속 `zsh` 를 돌려줬다.
        let seen: Vec<String> = self
            .pty
            .iter()
            .filter(|(_, p)| {
                p.osc_title()
                    .is_some_and(|t| t.contains("Claude") || t.contains("Codex"))
                    || p.active_agent().is_some()
            })
            .map(|(id, _)| id.clone())
            .collect();
        self.pane_claude_seen.extend(seen);
        self.pane_claude_seen.retain(|id| self.pty.contains_key(id));
    }

    /// 밖에 나간 방을 메인으로 되돌린다 — 사이드바가 부르는 쪽 입구.
    ///
    /// `dock_window_room` 은 **aux 창 인덱스**를 받는데 사이드바는 방 인덱스로만
    /// 말한다. 그 둘을 그대로 넘기면 엉뚱한 창이 닫히므로 여기서 한 번 옮긴다.
    pub(crate) fn dock_room_back(&mut self, win: usize) {
        let Some(aux) =
            self.aux_windows.iter().position(|a| a.room_window() == Some(win))
        else {
            return;
        };
        self.dock_window_room(aux);
        self.chrome_dirty = true;
    }

    /// 이 pane 에 학생 얼굴을 내보여도 되나 — claude 를 한 번이라도 띄웠는가.
    pub(crate) fn pane_claude_ready(&self, id: &str) -> bool {
        self.pane_claude_seen.contains(id)
    }

    /// Drop the focused pane from the unread set (the user is now looking at
    /// it) and push the count to the Dock badge when it changes. Called every
    /// tick from `about_to_wait` — cheap unless the count actually moves.
    pub(crate) fn sync_dock_badge(&mut self) {
        if self.window_focused {
            if let Some(active) = self.ws.lock().unwrap().active_pane.clone() {
                self.unread_panes.remove(&active);
            }
        }
        let n = self.unread_panes.len();
        if n != self.dock_badge_n {
            self.dock_badge_n = n;
            set_dock_badge(n);
        }
    }

    /// Flash strength (1.0 → 0.0) for `id`'s completion pulse, or `None` when
    /// it isn't flashing. Drives the header pulse and the sidebar done-dot;
    /// both fade over `NOTIFY_FLASH_MS`.
    /// pane 하나의 상태를 색 하나로. 사이드바가 방을 열지 않고도 "누가 나를
    /// 기다리는지"를 말하는 근거다 — 대기가 먼저다: 작업 중은 놔두면 끝나지만
    /// 대기는 내가 손대야 풀리므로, 둘이 겹치면 급한 쪽을 보여야 한다.
    pub(crate) fn pane_state_color(&self, id: &str) -> [u8; 4] {
        let st = self.pane_activity.get(id);
        if st.is_some_and(|a| a.status == "waiting") {
            theme::attention()
        } else if self.notify_flash_factor(id).is_some() {
            theme::success()
        } else if st.is_some_and(|a| a.status != "idle" && !a.status.is_empty())
            // 파란 점은 **지금 보고 있는 pane** 에만 준다. 작업 중인 pane 이 여럿이면
            // 목록이 온통 파래져 정작 손이 필요한 빨강·끝난 초록이 묻혔다 — 남의
            // 진행은 배너 바가 이미 말하고 있으니 여기서 한 번 더 외칠 자리가 아니다.
            && self.ws.lock().unwrap().active_pane.as_deref() == Some(id)
        {
            theme::accent()
        } else {
            theme::with_alpha(theme::text_mute(), 0x66)
        }
    }
    pub(crate) fn notify_flash_factor(&self, id: &str) -> Option<f32> {
        self.notify_flash.get(id).and_then(|t| {
            let age = t.elapsed().as_millis();
            (age < NOTIFY_FLASH_MS).then(|| 1.0 - age as f32 / NOTIFY_FLASH_MS as f32)
        })
    }

    /// Whether any pane is mid-flash — `about_to_wait` pumps ~30fps frames
    /// while this is true so the pulse fades smoothly instead of freezing on
    /// the last painted frame.
    pub(crate) fn any_notify_flash(&self) -> bool {
        self.notify_flash
            .values()
            .any(|t| t.elapsed().as_millis() < NOTIFY_FLASH_MS)
    }

    /// Sidebar width that layout math should actually use: the full
    /// `SIDEBAR_W` when the strip is shown, 0 when collapsed. Every
    /// origin_x / window_cells / hit-test calc routes through here so a
    /// single `sidebar_visible` flip reflows the whole grid.
    pub(crate) fn effective_sidebar_w(&self) -> f32 {
        self.tab_strip_w() + self.file_tree_col_w()
    }
    /// Width of the session-tab strip alone (0 when collapsed). With top tabs
    /// the strip never opens — the tabs live in the title bar instead.
    pub(crate) fn tab_strip_w(&self) -> f32 {
        if self.sidebar_visible && !self.tabs_on_top {
            self.sidebar_w_logical
        } else {
            0.0
        }
    }
    /// Shift the windowed tab strip so window `idx` is visible. Called on
    /// window switch/create — a keyboard- or click-driven switch must never
    /// land on a tab scrolled out of the strip. Free wheel-scrolling is left
    /// alone otherwise (sidebar_layout only clamps, never follows).
    pub(crate) fn win_tab_reveal(&mut self, idx: usize) {
        let vis = self.win_tab_vis.max(1);
        if idx < self.win_tab_first {
            self.win_tab_first = idx;
        } else if idx >= self.win_tab_first.saturating_add(vis) {
            self.win_tab_first = idx + 1 - vis;
        }
    }
    /// File-tree column width (0 when hidden). Independent of the tab strip.
    pub(crate) fn file_tree_col_w(&self) -> f32 {
        if self.file_tree.visible {
            self.file_tree.w_logical
        } else {
            0.0
        }
    }
    /// Left edge (logical x) of the file-tree column — right after the tab
    /// strip. The column sits between the tabs and the cell grid.
    pub(crate) fn file_tree_col_x(&self) -> f32 {
        self.tab_strip_w()
    }
    /// Right-hand chrome width (the git column), mirroring `effective_sidebar_w`
    /// on the left. Folded into `window_cells` so the cell grid reflows and no
    /// pane ever overlaps the column.
    pub(crate) fn effective_right_chrome_w(&self) -> f32 {
        self.git_col_w()
    }
    /// Git-column width (0 when hidden).
    pub(crate) fn git_col_w(&self) -> f32 {
        if self.git.col_visible {
            self.git.col_w_logical
        } else {
            0.0
        }
    }
    /// Left edge (logical x) of the git column — flush against the window's
    /// right edge. 0 before the window exists (no paint yet).
    pub(crate) fn git_col_x(&self) -> f32 {
        let w = self.git_col_w();
        self.window.as_ref().map_or(0.0, |win| {
            let scale = self.effective_scale();
            win.inner_size().width as f32 / scale - w
        })
    }
    /// Whether `id`'s per-pane status bar is shown. The global default
    /// (`set_footer_default`) decides it unless this pane sits in an exception
    /// set: `shown` forces it on, `hidden` forces it off.
    pub(crate) fn statusbar_visible(&self, id: &str) -> bool {
        if self.statusbar.shown.contains(id) {
            true
        } else if self.statusbar.hidden.contains(id) {
            false
        } else {
            self.set_footer_default
        }
    }
    /// Logical-px footer band `id` reserves for its status bar — `PANE_FOOTER_HEIGHT`
    /// when shown, 0 when collapsed. Mirrors the header band in `resize_backend`
    /// and the render clip so the PTY grid stops exactly above the bar.
    pub(crate) fn statusbar_px(&self, id: &str) -> f32 {
        if self.statusbar_visible(id) {
            PANE_FOOTER_HEIGHT
        } else {
            0.0
        }
    }
    /// 헤더 띠 높이(logical px) — image/md pane만 30, 그 외 0. resize_backend
    /// 처럼 ws 미잠금 지점에서 id로 조회한다(PaneState::header_px 위임).
    pub(crate) fn pane_header_px(&self, id: &str) -> f32 {
        self.ws
            .lock()
            .ok()
            .and_then(|w| w.panes.get(id).map(|p| p.header_px()))
            .unwrap_or(0.0)
    }
    /// Git-column-toggle button rect, parked at the right end of the title
    /// strip (mirrors the file-tree toggle on the left). Needs the window
    /// width, so it returns `None` before the first paint.
    pub(crate) fn git_col_toggle_rect(&self) -> Option<(f32, f32, f32, f32)> {
        let w = 26.0;
        let h = 22.0;
        let win_w = self.window.as_ref().map(|win| {
            let scale = self.effective_scale();
            win.inner_size().width as f32 / scale
        })?;
        let x = win_w - w - 8.0;
        // Windows paints min/max/close at the right edge; keep this toggle left
        // of that cluster so render (render.rs) and this hit-test agree.
        #[cfg(windows)]
        let x = Self::win_control_rects(win_w)[0].0 - 2.0 - w;
        let y = (TITLE_HEIGHT - h) / 2.0;
        Some((x, y, w, h))
    }
    /// Show/hide the git column. Same reflow path as `toggle_sidebar`: flip the
    /// flag, resize the PTYs to the new usable cols, repaint. Publishes the
    /// active cwd so the poller has something to refresh the moment it opens.
    pub(crate) fn toggle_git_col(&mut self) {
        self.git.col_visible = !self.git.col_visible;
        if self.git.col_visible {
            self.publish_git_col_cwd();
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Show/hide pane `id`'s status bar. Collapsing returns the footer rows to
    /// the cell grid, so the PTY is reshaped; an open dropdown on that bar is
    /// dismissed. `resize_backend` reads `statusbar_px` per leaf, so the toggle
    /// is all the state it needs.
    pub(crate) fn toggle_statusbar(&mut self, id: &str) {
        // Record this pane as an exception to the global default — drop any
        // stale membership first, then file it under the set opposite to its
        // new state (off → hidden, on → shown).
        let was_visible = self.statusbar_visible(id);
        self.statusbar.hidden.remove(id);
        self.statusbar.shown.remove(id);
        if was_visible {
            self.statusbar.hidden.insert(id.to_string());
            if self.statusbar.menu.as_ref().map(|(p, _)| p == id).unwrap_or(false) {
                self.statusbar.menu = None;
            }
        } else {
            self.statusbar.shown.insert(id.to_string());
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
    }
    /// Flip the pane's top bar (header band), pinning the choice so it stops
    /// following the automatic rule (tabs>1 / image / md).
    ///
    /// Must resize the backend: the band eats a row off the cell grid, and
    /// render / hit-test / PTY all derive that from `header_px()`. Skip this and
    /// the PTY keeps its old row count while the renderer draws the new one —
    /// clicks land a row off, which is the same class of bug as the zoom
    /// mapping. `chrome_dirty` alone would repaint but not re-measure.
    pub(crate) fn toggle_pane_header(&mut self, id: &str) {
        {
            let mut ws = self.ws.lock().unwrap();
            let Some(pane) = ws.panes.get_mut(id) else { return };
            let now = pane.has_header();
            pane.header_override = Some(!now);
            pane.dirty = true;
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
    }
    /// Open (or toggle) a status-bar dropdown for pane `id`. The list is
    /// snapshotted now — `read_dir` / `git_branches` block, so they can't run on
    /// the render path. A second click on the same chip closes the menu.
    pub(crate) fn open_statusbar_menu(&mut self, id: &str, kind: StatusbarMenu) {
        if self.statusbar.menu.as_ref() == Some(&(id.to_string(), kind)) {
            self.statusbar.menu = None;
            self.chrome_dirty = true;
            return;
        }
        let cwd = self.pane_cwd_cache.get(id).cloned();
        self.statusbar.menu_dirs.clear();
        self.statusbar.menu_branches.clear();
        self.statusbar.menu_scroll = 0.0;
        self.statusbar.menu_search.clear();
        match kind {
            StatusbarMenu::Path => {
                if let Some(cwd) = cwd.as_ref() {
                    // `..` first, then child entries (folders before files, each
                    // alpha-sorted) — a quick-nav picker, so files show too, not
                    // just directories. Dotfiles (and `.git`) stay hidden here.
                    if let Some(parent) = cwd.parent() {
                        self.statusbar.menu_dirs.push(parent.to_path_buf());
                    }
                    if let Ok(rd) = std::fs::read_dir(cwd) {
                        let mut entries: Vec<(bool, std::path::PathBuf)> = rd
                            .filter_map(|e| e.ok())
                            .filter(|e| {
                                e.file_name()
                                    .to_str()
                                    .map(|s| !s.starts_with('.'))
                                    .unwrap_or(false)
                            })
                            .map(|e| {
                                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                                (is_dir, e.path())
                            })
                            .collect();
                        entries.sort_by(|a, b| {
                            b.0.cmp(&a.0).then_with(|| {
                                a.1.file_name()
                                    .map(|s| s.to_ascii_lowercase())
                                    .cmp(&b.1.file_name().map(|s| s.to_ascii_lowercase()))
                            })
                        });
                        self.statusbar.menu_dirs.extend(entries.into_iter().map(|(_, p)| p));
                    }
                }
            }
            StatusbarMenu::Branch => {
                if let Some(cwd) = cwd.as_ref() {
                    self.statusbar.menu_branches = kasa_mcp::git::git_branches(cwd);
                }
            }
        }
        self.statusbar.menu = Some((id.to_string(), kind));
        self.chrome_dirty = true;
    }
    /// Indices into `statusbar_menu_dirs` that survive the live search query
    /// (case-insensitive substring on the entry name; the `..` parent row at
    /// index 0 always shows). Drives both the dropdown render and Enter-to-open.
    pub(crate) fn statusbar_menu_filtered(&self) -> Vec<usize> {
        let q = self.statusbar.menu_search.to_lowercase();
        self.statusbar.menu_dirs
            .iter()
            .enumerate()
            .filter(|(i, p)| {
                if q.is_empty() || *i == 0 {
                    return true;
                }
                p.file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| nfc_hangul(s).to_lowercase().contains(&q))
                    .unwrap_or(false)
            })
            .map(|(i, _)| i)
            .collect()
    }
    /// Enter on the path dropdown: open the first real match (folder → cd, file
    /// → preview pane). With an active query the `..` parent is skipped so Enter
    /// commits to a searched entry, not the parent.
    pub(crate) fn statusbar_menu_activate_first(&mut self) {
        let Some((pid, _)) = self.statusbar.menu.clone() else { return };
        let idxs = self.statusbar_menu_filtered();
        let target = if self.statusbar.menu_search.is_empty() {
            idxs.first().copied()
        } else {
            idxs.iter().find(|&&i| i != 0).or_else(|| idxs.first()).copied()
        };
        if let Some(path) = target.and_then(|i| self.statusbar.menu_dirs.get(i).cloned()) {
            if path.is_dir() {
                self.statusbar_cd(&pid, &path);
            } else {
                self.statusbar.menu = None;
                self.open_file_split(path);
            }
        }
    }
    /// `cd` pane `id`'s shell into `dir` (status-bar path picker). Sent straight
    /// to that pane's PTY — single-quoted so spaces survive — and the dropdown
    /// closes. The cwd sniffer repaints the bar once the shell reports the move.
    pub(crate) fn statusbar_cd(&mut self, id: &str, dir: &std::path::Path) {
        self.statusbar.menu = None;
        // 현재 추적 중인 cwd의 부모로 가는 경우엔 셸 상대경로 `cd ..` 를 쓴다.
        // (PowerShell 등에서 kasaterm이 잡은 절대경로가 부정확해도 한 칸 위로는 항상 정확.)
        let is_parent = self.pane_cwd_cache.get(id).and_then(|c| c.parent()) == Some(dir);
        let cmd = if is_parent {
            "cd ..\r".to_string()
        } else {
            let q = dir.to_string_lossy().replace('\'', "'\\''");
            format!("cd '{q}'\r")
        };
        if let Some(pty) = self.pty.get(id) {
            let _ = pty.send_bytes(cmd.as_bytes());
        }
        self.chrome_dirty = true;
    }
    /// Check out `branch` in pane `id`'s repo (status-bar branch switcher). Runs
    /// inline so the result can become a toast: a dirty tree makes git refuse
    /// (the silent failure that read as "branch switch doesn't work"), so we
    /// surface its message instead of dropping it. We don't stash/force — same
    /// no-surprises stance as the git column.
    pub(crate) fn statusbar_checkout(&mut self, id: &str, branch: String) {
        self.statusbar.menu = None;
        self.chrome_dirty = true;
        let Some(cwd) = self.pane_cwd_cache.get(id).cloned() else { return };
        let res = kasa_mcp::git::git_checkout(&cwd, &branch);
        let ok = res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        let msg = if ok {
            format!("{branch} 브랜치로 전환")
        } else {
            let out = res.get("output").and_then(|v| v.as_str()).unwrap_or("");
            if out.contains("would be overwritten") {
                "변경사항 때문에 전환 불가 — 커밋하거나 stash 먼저".to_string()
            } else if out.is_empty() {
                "브랜치 전환 실패".to_string()
            } else {
                format!("전환 실패: {}", out.lines().next().unwrap_or(""))
            }
        };
        self.collab.toast = Some((msg, std::time::Instant::now()));
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Surface a transient top-right toast (reuses the collab toast slot).
    pub(crate) fn set_toast(&mut self, msg: String) {
        self.collab.toast = Some((msg, std::time::Instant::now()));
        self.collab.toast_rect = None;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Run a file-tree right-click menu item. The target is the primary
    /// selection the right-click pinned. New*/Rename open an inline input row;
    /// CopyPath/Reveal/Delete act immediately.
    pub(crate) fn run_ft_menu_action(&mut self, action: crate::FtMenuAction) {
        use crate::FtMenuAction as A;
        let target = self.file_tree.selected.clone();
        match action {
            A::NewFile | A::NewFolder => {
                // Folder → inside it; file → its parent; nothing → tree root.
                let parent = target.as_ref().and_then(|p| {
                    if p.is_dir() {
                        Some(p.clone())
                    } else {
                        p.parent().map(|x| x.to_path_buf())
                    }
                });
                if let Some(par) = parent.clone() {
                    self.file_tree.expanded.insert(par);
                    self.rebuild_file_tree_nodes();
                }
                self.file_tree.new_parent = parent;
                self.file_tree.new = Some((matches!(action, A::NewFolder), String::new()));
                self.file_tree.rename = None;
                self.file_tree.search_active = false;
                self.file_tree.scroll = 0.0;
            }
            A::Rename => {
                if let Some(p) = target {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    // 이름을 싣고 여는 유일한 칸이라 커서를 여기서 끝에 찍는다 —
                    // 0 으로 두면 고치려던 확장자 앞이 아니라 이름 맨 앞에 선다.
                    self.file_tree.edit_cursor = name.chars().count();
                    self.file_tree.rename = Some((p, name));
                    self.file_tree.new = None;
                    self.file_tree.search_active = false;
                }
            }
            A::CopyPath => {
                if let Some(p) = target {
                    let s = p.to_string_lossy().to_string();
                    match arboard::Clipboard::new() {
                        Ok(mut cb) => {
                            if cb.set_text(s).is_ok() {
                                self.set_toast("경로 복사됨".to_string());
                            }
                        }
                        Err(e) => eprintln!("[kasaterm] clipboard open failed: {e}"),
                    }
                }
            }
            A::Reveal => {
                if let Some(p) = target {
                    self.reveal_in_file_manager(&p);
                }
            }
            A::OpenWith(i) => {
                // 인덱스는 이 프레임에 메뉴를 그린 목록에서 왔고 그 목록은
                // 프로세스당 한 번만 만들어지므로 어긋날 수 없다. 그래도 get 으로
                // 받는 건, 목록이 비었을 때 패닉 대신 아무 일도 안 일어나게.
                if let (Some(p), Some((_, target))) =
                    (target, crate::proc::open_with_apps().get(i))
                {
                    crate::proc::open_path_with(target, &p);
                }
            }
            A::OpenDefault => {
                if let Some(p) = target {
                    crate::proc::open_path_default(&p);
                }
            }
            A::Delete => self.delete_tree_selection(),
        }
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Reveal a path in the OS file manager (macOS Finder reveal · Windows
    /// Explorer select · Linux opens the parent folder via xdg-open).
    pub(crate) fn reveal_in_file_manager(&self, path: &std::path::Path) {
        #[cfg(target_os = "macos")]
        {
            let _ = crate::proc::command("open").arg("-R").arg(path).spawn();
        }
        #[cfg(target_os = "windows")]
        {
            let _ = crate::proc::command("explorer")
                .arg(format!("/select,{}", path.display()))
                .spawn();
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let dir = if path.is_dir() {
                path
            } else {
                path.parent().unwrap_or(path)
            };
            let _ = crate::proc::command("xdg-open").arg(dir).spawn();
        }
    }
    /// Info 패널의 포트 칩 클릭 — 기본 브라우저로 `http://localhost:<port>`.
    /// https 를 시도하지 않는 건 로컬 dev 서버가 거의 평문이기 때문이다(TLS 인
    /// 서버는 브라우저가 리다이렉트해준다).
    pub(crate) fn open_localhost(&self, port: u16) {
        let url = format!("http://localhost:{port}");
        #[cfg(target_os = "macos")]
        let _ = crate::proc::command("open").arg(&url).spawn();
        #[cfg(target_os = "windows")]
        let _ = crate::proc::command("cmd").args(["/C", "start", "", &url]).spawn();
        #[cfg(all(unix, not(target_os = "macos")))]
        let _ = crate::proc::command("xdg-open").arg(&url).spawn();
    }

    /// Expand/collapse a file's inline unified diff in the git panel. The diff
    /// is parsed once on first expand and cached; `git diff` for a single file
    /// is cheap but not render-loop cheap, so it must not run per frame.
    pub(crate) fn toggle_git_diff(&mut self, staged: bool, path: String) {
        let key = (staged, path.clone());
        if self.git.col_expanded.remove(&key) {
            self.chrome_dirty = true;
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
            return;
        }
        if !self.git.col_diff_cache.contains_key(&key) {
            if let Some(cwd) = self.git.col_data.lock().ok().and_then(|g| g.cwd.clone()) {
                let rows = kasa_mcp::git::git_file_diff(&cwd, &path, staged);
                self.git.col_diff_cache.insert(key.clone(), rows);
            }
        }
        self.git.col_expanded.insert(key);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Double-click on a recent-commit row: expand/collapse its changed-file
    /// list inline (only one commit open at a time). On expand the file list is
    /// fetched once and cached.
    pub(crate) fn toggle_git_commit(&mut self, hash: String) {
        if self.git.col_commit_expanded.as_deref() == Some(hash.as_str()) {
            self.git.col_commit_expanded = None;
        } else {
            if !self.git.col_commit_files_cache.contains_key(&hash) {
                if let Some(cwd) = self.git.col_data.lock().ok().and_then(|g| g.cwd.clone()) {
                    let files = kasa_mcp::git::git_commit_files(&cwd, &hash);
                    self.git.col_commit_files_cache.insert(hash.clone(), files);
                }
            }
            self.git.col_commit_expanded = Some(hash);
        }
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Click a file row inside an expanded commit: expand/collapse that file's
    /// diff. The diff is fetched once and cached, like `toggle_git_diff`.
    pub(crate) fn toggle_git_commit_file(&mut self, hash: String, path: String) {
        let key = (hash.clone(), path.clone());
        if self.git.col_commit_file_expanded.remove(&key) {
            self.chrome_dirty = true;
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
            return;
        }
        if !self.git.col_commit_diff_cache.contains_key(&key) {
            if let Some(cwd) = self.git.col_data.lock().ok().and_then(|g| g.cwd.clone()) {
                let rows = kasa_mcp::git::git_commit_file_diff(&cwd, &hash, &path);
                self.git.col_commit_diff_cache.insert(key.clone(), rows);
            }
        }
        self.git.col_commit_file_expanded.insert(key);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Collapse + drop cached diffs after a stage/unstage/commit changes the
    /// tree — the cached rows (and which side a file lives on) are now stale, so
    /// closing them is the no-surprise reset; the user re-expands for fresh diff.
    pub(crate) fn invalidate_git_diffs(&mut self) {
        self.git.col_expanded.clear();
        self.git.col_diff_cache.clear();
    }
    /// Open the git column for pane `id`'s repo (status-bar diff chip click).
    /// Focuses that pane so the column follows it (auto-track), then opens the
    /// column if it's hidden. A second click on an already-open column for the
    /// same pane closes it (toggle).
    pub(crate) fn open_git_panel_for(&mut self, id: &str) {
        let already = self.git.col_visible
            && self
                .ws
                .lock()
                .ok()
                .and_then(|w| w.active_pane.clone())
                .as_deref()
                == Some(id);
        if already {
            self.toggle_git_col();
            return;
        }
        if let Ok(mut w) = self.ws.lock() {
            w.active_pane = Some(id.to_string());
        }
        self.git.col_pinned_cwd = None;
        if self.git.col_visible {
            self.publish_git_col_cwd();
            self.chrome_dirty = true;
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        } else {
            self.toggle_git_col();
        }
    }
    /// Feed every pane's cwd into the badge poller (`git_poll_cwds`) so each
    /// pane's status bar shows its own repo's branch/diff, not just the active
    /// one. The poller dedups + rate-limits, so a flat overwrite each frame is
    /// fine. Skipped entirely when no pane shows a bar (nothing to refresh).
    pub(crate) fn publish_pane_git_cwds(&self) {
        // Always feed every pane's cwd. Besides the native status bars, the BA
        // GUI — now opened in an external browser tab — consumes per-pane badges
        // through `/layout`, and the GUI can't tell whether that tab is open. The
        // poller dedups by cwd and only wakes on a change, so an idle feed is one
        // cheap git call per distinct repo per interval (no repaint).
        let cwds: Vec<std::path::PathBuf> = self.pane_cwd_cache.values().cloned().collect();
        if let Ok(mut guard) = self.git_poll_cwds.lock() {
            *guard = cwds;
        }
    }
    /// Publish each pane's cwd + git badge into `pane_status_pub` so the socket
    /// thread's `/layout` can stamp them onto every `PaneRect` — the BA GUI draws
    /// a Warp-style cwd/branch/diff bar on plain (non-claude) terminal tiles from
    /// it. Reads only the already-resolved `pane_cwd_cache` + `window_git` caches,
    /// so nothing here touches the lsof/git hot path.
    pub(crate) fn publish_pane_status(&self) {
        let badges = self.window_git.lock().ok();
        let mut map: HashMap<String, PaneStatus> = HashMap::new();
        for (id, cwd) in &self.pane_cwd_cache {
            let badge = badges.as_ref().and_then(|g| g.get(cwd).cloned());
            // Share the PTY's OSC 133 block store (cheap Arc clone) so the
            // socket `/blocks` can read it without reaching into App.pty.
            let blocks = self.pty.get(id).map(|p| p.blocks_arc());
            map.insert(
                id.clone(),
                PaneStatus {
                    cwd: cwd.clone(),
                    badge,
                    blocks,
                },
            );
        }
        if let Ok(mut guard) = self.pane_status_pub.lock() {
            *guard = map;
        }
    }
    /// Push the active pane's cwd into the shared `git_col_cwd` so the git
    /// poller refreshes the right repo. Cheap string clone; called from the
    /// render right before the column paints (mirrors `git_poll_cwds`).
    pub(crate) fn publish_git_col_cwd(&self) {
        if !self.git.col_visible {
            return;
        }
        // A user-pinned repo (picked from the path dropdown) overrides the
        // active-pane follow — the column stays on that repo until unpinned.
        if let Some(pinned) = self.git.col_pinned_cwd.clone() {
            if let Ok(mut guard) = self.git.col_cwd.lock() {
                *guard = Some(pinned);
            }
            return;
        }
        let active = self.ws.lock().ok().and_then(|w| w.active_pane.clone());
        let resolved = active
            .as_ref()
            .and_then(|id| self.pane_cwd_cache.get(id).cloned());
        if let Ok(mut guard) = self.git.col_cwd.lock() {
            match resolved {
                // A confidently-resolved pane cwd always wins.
                Some(cwd) => *guard = Some(cwd),
                // Cache miss (e.g. right after a pane switch, before the cwd
                // sniffer catches up): keep the last good cwd instead of
                // flashing the launch dir — which is often a non-repo and
                // would read as "not a repo". Seed from current_dir only on
                // the very first frame, when nothing is known yet.
                None if guard.is_none() => *guard = std::env::current_dir().ok(),
                None => {}
            }
        }
    }
    /// Run a git-column button off a worker thread so the UI never blocks on
    /// git/network. Pull/Push sync the branch; Commit
    /// commits the STAGED changes with the panel's message (VSCode model). All
    /// read the column's repo from the poller's snapshot so the action always
    /// targets what the user sees.
    pub(crate) fn run_git_col_action(&mut self, btn: GitColBtn) {
        let cwd = self.git.col_data.lock().ok().and_then(|g| g.cwd.clone());
        let Some(cwd) = cwd else { return };
        match btn {
            GitColBtn::Pull => {
                self.git.op = Some("Pulling");
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    let _ = kasa_mcp::git::git_pull(&cwd);
                    // GitOpDone clears the spinner; the poller's next tick
                    // repaints ahead/behind.
                    let _ = proxy.send_event(UserEvent::GitOpDone);
                });
            }
            GitColBtn::Push => {
                self.git.op = Some("Pushing");
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    let _ = kasa_mcp::git::git_push(&cwd);
                    let _ = proxy.send_event(UserEvent::GitOpDone);
                });
            }
            GitColBtn::Commit => {
                // Commit the STAGED changes with the panel's message (VSCode
                // model — commit -m, no add). Empty message → focus the input
                // instead of a silent no-op, so the user sees where to type.
                let msg = self.git.commit_msg.trim().to_string();
                if msg.is_empty() {
                    self.git.commit_focused = true;
                    self.chrome_dirty = true;
                    return;
                }
                self.git.op = Some("Committing");
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    let _ = kasa_mcp::git::git_commit_staged(&cwd, &msg);
                    let _ = proxy.send_event(UserEvent::GitOpDone);
                });
                self.git.commit_msg.clear();
                self.git.commit_cursor = 0;
                self.git.commit_focused = false;
                self.chrome_dirty = true;
            }
        }
    }
    /// Open the cursor-style Commit modal: pre-fill nothing, focus the message
    /// box, default to including unstaged changes (the toggle in the modal).
    pub(crate) fn open_commit_modal(&mut self) {
        self.git.commit_menu_open = false;
        self.git.commit_modal_open = true;
        self.git.commit_focused = true;
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    pub(crate) fn close_commit_modal(&mut self) {
        self.git.commit_modal_open = false;
        self.git.commit_focused = false;
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Run the modal's commit. `push` also pushes after. Honors the
    /// include-unstaged toggle: when on, stage everything first (`git add -A`),
    /// else commit only what's already staged. Empty message is a no-op.
    pub(crate) fn run_commit_modal(&mut self, push: bool) {
        let msg = self.git.commit_msg.trim().to_string();
        if msg.is_empty() {
            self.git.commit_focused = true;
            self.chrome_dirty = true;
            return;
        }
        let Some(cwd) = self.git.col_data.lock().ok().and_then(|g| g.cwd.clone()) else { return };
        let include = self.git.commit_modal_include_unstaged;
        self.git.op = Some(if push { "Pushing" } else { "Committing" });
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            if include {
                // Stage everything, then commit all staged.
                let _ = kasa_mcp::git::git_commit_all(&cwd, &msg);
            } else {
                let _ = kasa_mcp::git::git_commit_staged(&cwd, &msg);
            }
            if push {
                let _ = kasa_mcp::git::git_push(&cwd);
            }
            let _ = proxy.send_event(UserEvent::GitOpDone);
        });
        self.git.commit_msg.clear();
        self.git.commit_cursor = 0;
        self.git.commit_focused = false;
        self.git.commit_modal_open = false;
        self.invalidate_git_diffs();
        self.chrome_dirty = true;
    }
    /// `gh pr create --web` for the column's repo (Commit-menu → Create PR).
    pub(crate) fn create_git_pr(&mut self) {
        self.git.commit_menu_open = false;
        let Some(cwd) = self.git.col_data.lock().ok().and_then(|g| g.cwd.clone()) else { return };
        std::thread::spawn(move || {
            let _ = crate::proc::command("gh")
                .args(["pr", "create", "--web"])
                .current_dir(&cwd)
                .spawn();
        });
        self.collab.toast = Some(("gh pr create --web 실행".to_string(), std::time::Instant::now()));
        self.chrome_dirty = true;
    }
    /// Expand/restore the git column width (header ⤢ button). Toggles between a
    /// wide reading width and the normal sidebar width; reshapes the PTYs.
    pub(crate) fn toggle_git_col_expand(&mut self) {
        let wide = 620.0_f32;
        let normal = 340.0_f32;
        self.git.col_w_logical = if self.git.col_w_logical >= wide - 1.0 { normal } else { wide };
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
    }
    /// Check out `branch` in the column's repo (off-thread). A dirty tree makes
    /// git refuse with a clear message — we don't stash/force, just let the
    /// poller repaint whatever git did. Closes the branch dropdown.
    pub(crate) fn run_git_checkout(&mut self, branch: String) {
        self.git.branch_menu_open = false;
        let cwd = self.git.col_data.lock().ok().and_then(|g| g.cwd.clone());
        let Some(cwd) = cwd else { return };
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let _ = kasa_mcp::git::git_checkout(&cwd, &branch);
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }
    /// Persist the current window frame (logical size + physical position).
    /// Called from `exiting` and from the Moved/Resized debounce in
    /// `about_to_wait` — the debounce keeps the frame safe across a crash.
    pub(crate) fn save_window_frame(&self) {
        // **검증 실행은 저장하지 않는다.** 위치·크기를 env 로 강제했다는 건 그 창이
        // 사람이 쓰던 창이 아니라 하네스가 띄운 창이라는 뜻인데, 설정 파일은 인스턴스
        // 사이에 공유돼서 그 값이 그대로 거노 앱의 다음 크기가 된다(실사고 2026-08-06:
        // 좁은 화면 재현으로 430x700 를 띄웠더니 `window.json` 이 그 값으로 덮여,
        // 재시작하면 앱이 구석에 손바닥만 하게 뜰 뻔했다).
        if crate::verification_run() {
            return;
        }
        let Some(win) = self.window.as_ref() else { return };
        let scale = win.scale_factor().max(0.5);
        let sz = win.inner_size();
        let pos = win
            .outer_position()
            .ok()
            .map(|p| (p.x as f64, p.y as f64));
        crate::socket::write_window_frame(
            sz.width as f64 / scale,
            sz.height as f64 / scale,
            pos,
        );
    }

    /// Hit-test a press against the window-tab strip controls, in paint order:
    /// close-× (sits on top of a tab) → tab → "+" new-window button. Shared by
    /// the side sidebar strip and the top-tabs title strip — the top strip
    /// previously had no click gate at all, so its tabs painted but never
    /// switched/closed. Returns true when the click was handled.
    /// 그 방의 목록이 얼마나 펴져 있나 — 0(접힘)..1(다 폄).
    ///
    /// 애니메이션이 없으면 0 이나 1 이고, 도는 중이면 그 사이다. 목록 높이·행
    /// 투명도가 이 하나를 같이 보므로 밀림과 나타남이 어긋나지 않는다.
    pub(crate) fn expand_progress(&self, idx: usize) -> f32 {
        let target = if self.expanded_windows.contains(&idx) { 1.0 } else { 0.0 };
        let Some((ai, opening, at)) = self.expand_anim else { return target };
        if ai != idx {
            return target;
        }
        let t = (at.elapsed().as_secs_f32() / EXPAND_ANIM_SECS).clamp(0.0, 1.0);
        // ease-out — 손을 뗀 직후가 가장 빠르고 끝에서 가라앉는다. 선형은 멈추는
        // 순간이 툭 끊겨 목록이 "튄" 것처럼 보인다.
        let e = 1.0 - (1.0 - t).powi(3);
        if opening { e } else { 1.0 - e }
    }

    /// 방을 펴거나 접는다 — 상태와 애니메이션을 같이 세우는 유일한 입구.
    pub(crate) fn toggle_window_expand(&mut self, idx: usize) {
        let opening = !self.expanded_windows.contains(&idx);
        if opening {
            self.expanded_windows.insert(idx);
        } else {
            self.expanded_windows.remove(&idx);
        }
        self.expand_anim = Some((idx, opening, std::time::Instant::now()));
        self.chrome_dirty = true;
    }

    /// 방 탭 카드 안 **펼치기 버튼**의 사각 — 상태 점 왼쪽의 삼각형 자리.
    /// `tab` 은 그 방 카드의 사각.
    ///
    /// 렌더와 클릭 판정이 이 하나를 같이 본다. 예전엔 클릭 쪽이 "아랫줄 오른쪽
    /// 100px" 이라는 자기 공식을 따로 갖고 있어서, 눈에는 삼각형 하나만 보이는데
    /// 그 옆 점들까지 눌러도 방 전환이 안 됐다 — 버튼이 어디까지인지 화면이
    /// 말해 주지 않는 상태였다(거노: "접기 버튼이 따로 있어야, 누르면 전환은
    /// 되고"). pane 이 하나뿐인 방은 펼쳐도 그 하나뿐이라 버튼을 두지 않는다.
    pub(crate) fn window_expand_rect(
        &self,
        idx: usize,
        tab: (f32, f32, f32, f32),
    ) -> Option<(f32, f32, f32, f32)> {
        let n = self.window_leaves(idx).len();
        // pane 이 하나뿐인 방도 편다. 예전엔 `n < 2` 로 막았는데 — 한 줄짜리 목록은
        // 펼 값어치가 없다는 판단이었다 — 그 한 줄이 **누가 거기 있고 무슨 상태인지**
        // 다. 학생 하나를 방 하나에 두고 쓰면 사이드바에서 그 학생을 볼 길이 통째로
        // 사라졌다(거노: "방하나에 학생하나면 펼치기가 없어서 학생목록이 안보이네").
        if n == 0 {
            return None;
        }
        // 삼각형 하나짜리 18px 칩은 눌러 보기에 너무 작았다(거노). pane 개수를
        // 같이 담아 pill 로 키우면 타깃이 두 배 넘게 커지고, 방을 펴지 않고도
        // 몇 개짜리 방인지 읽힌다 — 커진 자리에 정보가 같이 들어온 셈이다.
        let w = if n >= 10 { 44.0 } else { 37.0 };
        Some((tab.0 + tab.2 - 8.0 - w, tab.1 + 26.0, w, 20.0))
    }

    pub(crate) fn window_strip_click(&mut self, cx: f32, cy: f32) -> bool {
        let inside =
            |r: &(f32, f32, f32, f32)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3;
        // 밖에 나간 방의 되돌리기 버튼이 가장 먼저다. × 히트렉트는 그려지지 않는
        // 탭에도 남아 있고 자리가 정확히 겹쳐서, 뒤에 두면 되돌리려던 클릭이
        // 「이 방 닫을까요」로 새어 나갔다(실측: 방이 통째로 사라짐).
        if let Some(idx) = self
            .window_dock_rects
            .iter()
            .find(|(_, r)| inside(r))
            .map(|(i, _)| *i)
        {
            self.dock_room_back(idx);
            return true;
        }
        if let Some(idx) = self
            .window_tab_close_rects
            .iter()
            .find(|(_, r)| inside(r))
            .map(|(i, _)| *i)
        {
            // close_window 를 직접 부르면 그 창의 claude 가 돌고 있어도 말없이
            // 죽는다. 같은 동작의 가운데 클릭은 confirm_or_close_session 으로
            // 물어보는데, 이 ×(사이드바/상단 strip 공용)만 확인을 건너뛰고 있었다.
            self.confirm_or_close_session(idx);
            return true;
        }
        // 펼쳐 둔 pane 줄이 먼저다 — 줄은 탭 카드 **안에** 그려지므로, 탭을 먼저
        // 검사하면 줄을 눌러도 방 전환만 되고 학생에게는 영영 못 간다.
        if let Some((wi, pane)) = self
            .sidebar_row_rects
            .iter()
            .find(|(_, _, r)| inside(r))
            .map(|(i, p, _)| (*i, p.clone()))
        {
            if wi != self.active_window {
                self.switch_window(wi);
            }
            self.focus_pane(&pane);
            // 포커스는 누르는 즉시(목록에서 pane 을 고르는 게 이 줄의 본업이다),
            // 옮기기는 여기서 장전만. 문턱을 못 넘으면 release 가 그냥 버린다.
            self.sidebar_row_drag = Some(crate::SidebarRowDrag {
                pane,
                start: (cx, cy),
                active: false,
                target: None,
            });
            self.chrome_dirty = true;
            return true;
        }
        if let Some(idx) = self
            .window_tab_rects
            .iter()
            .find(|(_, r)| inside(r))
            .map(|(i, _)| *i)
        {
            // 펼치기 버튼만 전환의 예외다 — 그 삼각형 하나 크기.
            let tab = self.window_tab_rects.iter().find(|(i, _)| *i == idx).map(|(_, r)| *r);
            if let Some(r) = tab.and_then(|t| self.window_expand_rect(idx, t)) {
                if inside(&r) {
                    self.toggle_window_expand(idx);
                    return true;
                }
            }
            // 이미 열려 있는 방을 **천천히** 다시 누르면 이름 편집(Finder 규칙).
            // 전환보다 먼저 본다 — 전환은 같은 방이면 어차피 무동작이다.
            let now = std::time::Instant::now();
            if starts_room_rename(self.room_rename.last_click, idx, self.active_window, now) {
                // 손으로 붙인 이름이 없으면 **지금 화면에 보이는 라벨**로 시작한다.
                // 빈칸으로 열면 cwd 에서 파생된 이름이 눈앞에서 사라져, 고치려던
                // 사람이 이름을 통째로 다시 쳐야 한다(Finder 는 현 이름을 채워 준다).
                let cur = self
                    .window_name_override
                    .get(&idx)
                    .cloned()
                    .or_else(|| self.window_labels.get(idx).map(|(n, _)| n.clone()))
                    .unwrap_or_default();
                self.room_rename.cursor = cur.chars().count();
                self.room_rename.editing = Some((idx, cur));
                self.room_rename.last_click = None;
                let _ = self.hangul.flush();
                self.mark_room_label_dirty();
                return true;
            }
            self.room_rename.last_click = Some((idx, now));
            // 다른 방을 누르면 편집은 확정하고 넘어간다(바깥 클릭 = 확정).
            self.commit_room_rename();
            self.switch_window(idx);
            // 전환은 누르는 즉시(브라우저 탭과 같다 — 방 전환은 트리 스왑이라
            // 싸다), 재배치는 여기서 장전만. 문턱을 못 넘으면 release 가 그냥
            // 버리므로 평범한 클릭의 감각은 그대로다.
            self.win_tab_drag = Some(WinTabDrag {
                from: idx,
                start: (cx, cy),
                active: false,
                target: idx,
            });
            return true;
        }
        if self.new_window_btn_rect.map(|r| inside(&r)).unwrap_or(false) {
            // 피커 항목은 Windows 설치 셸뿐 — macOS/Linux 는 목록이 비므로
            // 메뉴 대신 즉시 기본 셸 새 윈도우("Claude 학생" 항목은 폐기 —
            // split+claude 수동 부팅으로 충분, 거노).
            if crate::available_shells().is_empty() {
                self.new_window();
            } else {
                self.shell_menu_open = !self.shell_menu_open;
            }
            self.chrome_dirty = true;
            return true;
        }
        false
    }

    /// Preview a changed file from the git column, resolved against the
    /// column's repo cwd. `open_file` does its own extension branching
    /// (image viewer / md render / raw code editor) and focuses an existing
    /// pane instead of duplicating — same path as a file-tree double-click.
    /// A native diff view is still phase 2; opening the file is the useful v1.
    pub(crate) fn open_git_file(&mut self, rel: &str) {
        let cwd = self.git.col_data.lock().ok().and_then(|g| g.cwd.clone());
        let Some(cwd) = cwd else { return };
        self.open_file(cwd.join(rel), None, false);
    }
    /// 하단바가 pane 그리드에서 먹는 높이(0 이면 바 자체가 없다).
    ///
    /// 예약과 그리기가 서로 다른 조건을 보면 바가 마지막 셀 줄 위에 겹치거나
    /// 빈 띠만 남는다 — 판단은 여기 한 곳에서만 한다.
    ///
    /// 닫은 pane 은 여기 안 센다. 되살리기는 Info 의 「되살리기」 섹션이 맡는다 —
    /// 하단바에 두면 pane 을 하나 닫을 때마다 그리드가 40px 줄면서 화면 전체가
    /// 재배치되고, 그 띠가 포커스 테두리 아랫변까지 덮었다(거노).
    ///
    /// 접어 둔 별도창은 **센다**. 그건 사용자가 그 순간 직접 접은 것이라 띠가 생기는
    /// 게 결과로 읽히고, 무엇보다 되살릴 손잡이가 여기 말고는 없다.
    pub(crate) fn dock_reserve_h(&self) -> f32 {
        if self.docked.is_empty() && self.zoomed_pane.is_none() && self.hidden_aux.is_empty() {
            0.0
        } else {
            DOCK_HEIGHT
        }
    }

    /// 사이드바 하단에 붙박인 트레이 — 새 세션(`+`)과 앱 전역 버튼(피드백·설정).
    /// 반환은 `(구분선 y, +, 피드백, 설정)`, 세로 사이드바가 없으면 `None`.
    ///
    /// 셋 다 원래는 세션 목록 *뒤에* 줄줄이 붙어 있었다. 그러면 세션이 늘 때마다
    /// 아래로 밀려서, 늘 같은 버튼을 누르는데 자리가 매번 달라진다. 트레이는 목록
    /// 길이와 무관하게 바닥에 고정이라 근육기억이 선다.
    pub(crate) fn sidebar_tray_rects(
        &self,
        win_h: f32,
    ) -> Option<(f32, (f32, f32, f32, f32), (f32, f32, f32, f32), (f32, f32, f32, f32))> {
        if self.tabs_on_top || !self.sidebar_visible {
            return None;
        }
        let dock_h = if self.docked.is_empty() { 0.0 } else { DOCK_HEIGHT };
        let b = 28.0_f32;
        let line_y = (win_h - dock_h - SIDEBAR_TRAY_H).max(TITLE_HEIGHT);
        let y = line_y + (SIDEBAR_TRAY_H - b) / 2.0;
        let left = SIDEBAR_TAB_INSET + 4.0;
        let right = (self.sidebar_w_logical - SIDEBAR_TAB_INSET - 4.0 - b).max(left);
        Some((
            line_y,
            (left, y, b, b),
            (right - 4.0 - b, y, b, b),
            (right, y, b, b),
        ))
    }
    /// 사이드바 토글 버튼 rect(논리 px).
    ///
    /// 자리가 상태에 따라 갈린다. 접혀 있으면 신호등 오른쪽 — 열 것이 아직 없으니
    /// 창의 버튼이다. 펴져 있으면 사이드바 자신의 오른쪽 위 — 닫는 버튼은 닫힐
    /// 판 위에 있어야 무엇을 닫는지가 자리로 설명된다. 사이드바를 좁게 끌면
    /// 신호등을 침범하니 거기서 멈춘다.
    pub(crate) fn sidebar_toggle_rect(&self) -> (f32, f32, f32, f32) {
        let w = 26.0;
        let h = 22.0;
        #[cfg(not(windows))]
        let home = TRAFFIC_LIGHT_WIDTH + 6.0;
        // Windows is frameless with no traffic-light cluster to clear — start
        // the toggles at the left edge instead of reserving the macOS width.
        #[cfg(windows)]
        let home = 10.0;
        let y = (TITLE_HEIGHT - h) / 2.0;
        if self.tabs_on_top || !self.sidebar_visible {
            return (home, y, w, h);
        }
        ((self.sidebar_w_logical - SIDEBAR_TAB_INSET - w).max(home), y, w, h)
    }
    /// 파일트리 토글 rect. 사이드바 토글을 따라다닌다 — 접혀 있으면 그 오른쪽,
    /// 펴져 있으면 사이드바 밖(본문 쪽 첫 자리)이다. 토글이 판 안으로 들어간
    /// 마당에 트리 버튼까지 넣으면 세션 목록 머리가 버튼 줄이 된다.
    pub(crate) fn file_tree_toggle_rect(&self) -> (f32, f32, f32, f32) {
        let (sx, sy, sw, sh) = self.sidebar_toggle_rect();
        if self.tabs_on_top {
            return (sx, sy, sw, sh);
        }
        if self.sidebar_visible {
            return (self.sidebar_w_logical + 10.0, sy, sw, sh);
        }
        (sx + sw + 2.0, sy, sw, sh)
    }
    /// Windows-only frameless window controls (minimize / maximize / close),
    /// parked at the right end of the title strip. macOS keeps the native
    /// traffic lights, so this exists only where we drop OS decorations.
    /// Returns `[minimize, maximize, close]` left→right; close is the
    /// right-most so it lands where Windows users reach for it. Same chip
    /// size as the sidebar toggle to read as one button family.
    #[cfg(windows)]
    pub(crate) fn win_control_rects(win_w_logical: f32) -> [(f32, f32, f32, f32); 3] {
        let w = 26.0;
        let h = 22.0;
        let gap = 2.0;
        let right_pad = 8.0;
        let y = (TITLE_HEIGHT - h) / 2.0;
        let close_x = win_w_logical - right_pad - w;
        let max_x = close_x - gap - w;
        let min_x = max_x - gap - w;
        [(min_x, y, w, h), (max_x, y, w, h), (close_x, y, w, h)]
    }
    /// Show/hide the left window-tab sidebar. The cell grid reflows to the
    /// new usable width (every layout calc reads `effective_sidebar_w()`),
    /// so we just flip the flag, resize the PTYs to the new cols/rows, and
    /// repaint.
    /// 편집 중이면 버퍼를 방 이름으로 확정한다. **빈 문자열이면 override 를 지워**
    /// 기본 라벨(캐릭터 이름)로 되돌린다 — 빈 이름을 저장하면 방이 무명이 된다.
    pub(crate) fn commit_room_rename(&mut self) {
        let Some((idx, mut buf)) = self.room_rename.editing.take() else { return };
        // 조합 중이던 마지막 글자도 이름의 일부다 — 안 흘리면 "가나다" 를 치고
        // Enter 를 눌렀을 때 "가나" 만 남는다.
        if let Some(tail) = self.hangul.flush() {
            buf.push_str(&tail);
        }
        self.end_room_rename_ime();
        let name = buf.trim().to_string();
        if name.is_empty() {
            self.window_name_override.remove(&idx);
        } else {
            self.window_name_override.insert(idx, name);
        }
        self.window_labels_at = None;
        self.mark_room_label_dirty();
    }

    /// 편집을 버린다(Esc).
    pub(crate) fn cancel_room_rename(&mut self) {
        if self.room_rename.editing.take().is_some() {
            let _ = self.hangul.flush();
            self.end_room_rename_ime();
            self.window_labels_at = None;
            self.mark_room_label_dirty();
        }
    }

    /// 편집이 끝났으니 조합 상태를 걷는다. `ime_focus` 를 비워 두지 않으면 다음에
    /// pane 으로 치는 한글이 `ime_retarget` 에서 사라진 편집칸으로 흘러간다.
    fn end_room_rename_ime(&mut self) {
        if matches!(self.ime_focus, Some(crate::ImeFocus::RoomRename(_))) {
            self.ime_focus = None;
        }
        self.preedit.clear();
        self.in_preedit = false;
    }

    /// 조합이 끝난 글자를 커서 자리에 넣는다(`ime_retarget` 도 여기로 흘린다).
    pub(crate) fn room_rename_insert(&mut self, text: &str) {
        let cursor = &mut self.room_rename.cursor;
        if let Some((_, buf)) = self.room_rename.editing.as_mut() {
            crate::lineedit::insert(buf, cursor, text);
            self.chrome_dirty = true;
        }
    }

    /// 편집 중인 방에 키를 넣는다. 처리했으면 true — 호출부가 그 키를 pane 으로
    /// 흘리지 않게 한다(편집 중 타이핑이 셸에 새면 안 된다).
    ///
    /// **한글은 자체 조합기(`self.hangul`)를 태운다.** macOS 는 OS IME 를 꺼 두고
    /// (`set_ime_allowed(false)`) 자모를 `KeyboardInput.text` 로 직접 받으므로, 여기서
    /// 조합하지 않으면 "안녕"이 "ㅇㅏㄴㄴㅕㅇ"으로 박힌다 — 거노: "이름 바꾸는 거
    /// 이상한데". git 커밋 칸(`git_commit_input`)이 같은 이유로 같은 경로를 탄다.
    pub(crate) fn room_rename_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::keyboard::{Key, NamedKey};
        let Some(idx) = self.room_rename.editing.as_ref().map(|(i, _)| *i) else { return false };
        if crate::input::is_modifier_key(event) {
            return true;
        }
        // Cmd/Ctrl 조합은 삼키되 버퍼엔 안 넣는다. 앞단에서 안 잡힌 조합(Cmd+C 등)이
        // 여기 오면 글자만 박히고, 흘려보내면 편집 중인데 셸이 그 키를 먹는다.
        if self.modifiers.super_key() || self.modifiers.control_key() {
            return true;
        }
        self.ime_retarget(crate::ImeFocus::RoomRename(idx));
        #[cfg(target_os = "macos")]
        if let Some(t) = &event.text {
            if let Some(c) = t.chars().next().filter(|_| t.chars().count() == 1) {
                if (0x3130..=0x318F).contains(&(c as u32)) {
                    if let Some(done) = self.hangul.feed(c) {
                        self.room_rename_insert(&done);
                    }
                    self.preedit = self.hangul.preedit().unwrap_or_default();
                    self.in_preedit = !self.preedit.is_empty();
                    self.mark_room_label_dirty();
                    return true;
                }
            }
        }
        // 조합 중이던 자모를 지우는 백스페이스가 먼저다 — 완성 글자를 지우기 전에
        // 조합기 안의 것부터 물린다.
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) && self.hangul.backspace() {
            self.preedit = self.hangul.preedit().unwrap_or_default();
            self.in_preedit = !self.preedit.is_empty();
            self.mark_room_label_dirty();
            return true;
        }
        if let Some(flushed) = self.hangul.flush() {
            self.room_rename_insert(&flushed);
        }
        self.preedit.clear();
        self.in_preedit = false;
        let cursor = &mut self.room_rename.cursor;
        let act = match self.room_rename.editing.as_mut() {
            Some((_, buf)) => crate::lineedit::key(buf, cursor, &event.logical_key),
            None => crate::lineedit::LineEditAction::Ignored,
        };
        match act {
            crate::lineedit::LineEditAction::Submit => self.commit_room_rename(),
            crate::lineedit::LineEditAction::Cancel => self.cancel_room_rename(),
            _ => {}
        }
        self.mark_room_label_dirty();
        true
    }

    /// 방 라벨을 이번 프레임에 다시 짓게 한다. `refresh_window_labels` 는 1초 캐시라
    /// 이걸 안 깨면 **타이핑이 1초씩 뭉쳐 나온다**(거노: "버벅여"). 편집 중인 방의
    /// 라벨은 캐시 밖에서 버퍼로 덮으므로 재계산 자체는 안 돌지만, 편집을 끝낸 뒤
    /// 원래 이름으로 돌아가려면 캐시를 한 번 비워야 한다.
    fn mark_room_label_dirty(&mut self) {
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    pub(crate) fn toggle_sidebar(&mut self) {
        self.sidebar_visible = !self.sidebar_visible;
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Show/hide the file-tree column. Same reflow path as `toggle_sidebar`.
    pub(crate) fn toggle_file_tree(&mut self) {
        self.file_tree.visible = !self.file_tree.visible;
        if self.file_tree.visible {
            self.refresh_file_tree();
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Drop a word off the end of the input buffer (for Ctrl-W /
    /// Alt-Backspace): eat trailing spaces, then non-spaces.
    pub(crate) fn buf_pop_word(&mut self) {
        while self.input_buf.ends_with(' ') {
            self.input_buf.pop();
        }
        while let Some(c) = self.input_buf.chars().last() {
            if c == ' ' {
                break;
            }
            self.input_buf.pop();
        }
    }
    /// Recompute the inline suggestion against the live grid. Called once
    /// per frame from the render path (so the grid reflects the latest
    /// shell echo). Only runs at a shell prompt (active pane, not
    /// alt-screen).
    ///
    /// Two ways to find the editable command line:
    ///   1. **OSC 133 mark** (primary) — the shell's precmd hook emits a
    ///      `B` mark at prompt end; pty-backend tags the cursor there. We
    ///      read the grid from that column to the cursor, which is the
    ///      ground truth: it survives Tab-completion, paste, RPROMPT and
    ///      wide (CJK) chars that the typed-buffer heuristic can't see.
    ///   2. **typed buffer** (fallback) — when there's no usable mark yet
    ///      (tmux backend, pre-first-prompt, or a scrolled-away mark), we
    ///      trust `input_buf` but only if it's still the tail of the
    ///      cursor row, which auto-suppresses on edits we can't track.
    pub(crate) fn update_suggestion(&mut self) {
        if !self.autosuggest.enabled() || !self.preedit.is_empty() {
            self.current_suggestion = None;
            return;
        }
        let line: Option<String> = {
            let ws = self.ws.lock().unwrap();
            match ws.active().and_then(|p| p.term()) {
                Some(t) if !t.alt_screen => {
                    let crow = t.cursor_row as usize;
                    let ccol = t.cursor_col as usize;
                    let row_cells = t.cells.get(crow);
                    let cell_str = |r: &[GridCell], from: usize, to: usize| -> String {
                        r.iter()
                            .take(to)
                            .skip(from)
                            .map(|c| if c.ch == '\0' { ' ' } else { c.ch })
                            .collect()
                    };
                    // Primary: OSC 133 mark still on the cursor's row.
                    let from_mark = match t.prompt_end {
                        Some((pr, pc))
                            if pr as usize == crow && (pc as usize) <= ccol =>
                        {
                            row_cells.map(|r| cell_str(r, pc as usize, ccol))
                        }
                        _ => None,
                    };
                    if from_mark.is_some() {
                        from_mark
                    } else if !self.input_buf.is_empty() {
                        let synced = row_cells
                            .map(|r| cell_str(r, 0, ccol).ends_with(&self.input_buf))
                            .unwrap_or(false);
                        synced.then(|| self.input_buf.clone())
                    } else {
                        None
                    }
                }
                _ => None,
            }
        };
        let Some(line) = line else {
            self.current_suggestion = None;
            return;
        };
        // Nothing to complete from an empty / whitespace-only line.
        if line.trim().is_empty() {
            self.current_suggestion = None;
            return;
        }
        self.autosuggest.maybe_refresh();
        self.current_suggestion = self.autosuggest.suggest(&line);
    }
    /// Build banner shown bottom-right on launch: `v<pkg>·<git rev>`
    /// (rev carries a trailing '+' when built dirty). Stamped at compile
    /// time by build.rs.
    pub(crate) fn version_label() -> String {
        format!(
            "v{}·{}",
            env!("CARGO_PKG_VERSION"),
            env!("KASATERM_GIT_REV")
        )
    }
    /// 0.0..1.0 opacity for the launch banner: solid through
    /// VERSION_HOLD_MS, then a linear fade across VERSION_FADE_MS, then
    /// gone. Also the single source of truth for "is the banner still
    /// animating" (alpha > 0).
    pub(crate) fn version_alpha(&self) -> f32 {
        let e = self.version_anim_start.elapsed().as_millis();
        if e < VERSION_HOLD_MS {
            1.0
        } else if e < VERSION_HOLD_MS + VERSION_FADE_MS {
            1.0 - (e - VERSION_HOLD_MS) as f32 / VERSION_FADE_MS as f32
        } else {
            0.0
        }
    }
    /// 0.0..1.0 opacity for the "복사됨" copy toast: solid for a brief hold
    /// after a block copy, then a quick fade. Mirrors `version_alpha`.
    pub(crate) fn copy_toast_alpha(&self) -> f32 {
        const HOLD: u128 = 900;
        const FADE: u128 = 500;
        let Some(at) = self.copy_toast_at else { return 0.0 };
        let e = at.elapsed().as_millis();
        if e < HOLD {
            1.0
        } else if e < HOLD + FADE {
            1.0 - (e - HOLD) as f32 / FADE as f32
        } else {
            0.0
        }
    }
    /// 0.0..1.0 opacity for a collab completion toast: a longer hold than the
    /// copy toast (a sibling finishing is worth a real glance) then a fade.
    /// Returns 0 with no active toast, so callers gate paint + frame-loop wake.
    pub(crate) fn collab_toast_alpha(&self) -> f32 {
        const HOLD: u128 = 2400;
        const FADE: u128 = 600;
        let Some((_, at)) = self.collab.toast.as_ref() else { return 0.0 };
        // 승인 토스트(칩 포함)는 사용자가 응답하거나 프롬프트가 풀릴 때까지
        // 고정 — 시간 페이드 없음. (해제는 route_approval_prompts/클릭 핸들러)
        if self.collab.toast_action.is_some() {
            return 1.0;
        }
        let e = at.elapsed().as_millis();
        if e < HOLD {
            1.0
        } else if e < HOLD + FADE {
            1.0 - (e - HOLD) as f32 / FADE as f32
        } else {
            0.0
        }
    }
    /// Copy a detected code block's text to the clipboard and arm the
    /// toast. Reuses arboard like `copy_selection`. Best-effort: a
    /// clipboard failure just logs (the toast still fires on success).
    pub(crate) fn copy_block_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match arboard::Clipboard::new() {
            Ok(mut cb) => {
                if let Err(e) = cb.set_text(text.to_string()) {
                    eprintln!("[kasaterm] clipboard write failed: {e}");
                    return;
                }
            }
            Err(e) => {
                eprintln!("[kasaterm] clipboard open failed: {e}");
                return;
            }
        }
        self.copy_toast_at = Some(Instant::now());
    }
    /// Open the session panel in its own OS window. Mirrors open_git_panel:
    /// the page polls `/sessions` on the MCP server. Best-effort — any failure
    /// just logs and leaves the terminal untouched.
    pub(crate) fn open_session_panel(&mut self, event_loop: &ActiveEventLoop) {
        if self.session_panel_window.is_some() {
            return;
        }
        let port = mcp_panel_port();
        let attrs = WindowAttributes::default()
            .with_title("sessions")
            .with_theme(Some(Theme::Dark))
            .with_inner_size(LogicalSize::new(260.0, 360.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[session-panel] window create failed: {e}");
                return;
            }
        };
        let html = SESSION_PANEL_HTML
            .replace("__PORT__", &port)
            .replace("__TOKEN__", kasa_mcp::session_token());
        // build_as_child for the same use-after-free reason as the git panel.
        let webview = match wry::WebViewBuilder::new()
            .with_html(html)
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                size: wry::dpi::LogicalSize::new(260.0, 360.0).into(),
            })
            .build_as_child(window.as_ref())
        {
            Ok(wv) => wv,
            Err(e) => {
                eprintln!("[session-panel] webview build failed: {e}");
                return;
            }
        };
        eprintln!("[session-panel] open; polling 127.0.0.1:{port}/sessions");
        self.session_panel_window = Some(window);
        self.session_panel_webview = Some(webview);
    }
    /// Toggle the session panel from the menu: close if open, open if not.
    pub(crate) fn toggle_session_panel(&mut self, event_loop: &ActiveEventLoop) {
        if self.session_panel_window.is_some() {
            // Drop the webview before the window it borrows from.
            self.session_panel_webview = None;
            self.session_panel_window = None;
        } else {
            self.open_session_panel(event_loop);
        }
    }
    /// Open the board panel in its own OS window. Mirrors open_session_panel:
    /// the page polls `/board` on the MCP server. Best-effort — any failure
    /// just logs and leaves the terminal untouched.
    pub(crate) fn open_board_panel(&mut self, event_loop: &ActiveEventLoop) {
        if self.board_panel_window.is_some() {
            return;
        }
        let port = mcp_panel_port();
        let attrs = WindowAttributes::default()
            .with_title("board")
            .with_theme(Some(Theme::Dark))
            .with_inner_size(LogicalSize::new(320.0, 440.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[board-panel] window create failed: {e}");
                return;
            }
        };
        let html = BOARD_PANEL_HTML
            .replace("__PORT__", &port)
            .replace("__TOKEN__", kasa_mcp::session_token());
        // build_as_child for the same use-after-free reason as the git panel.
        let webview = match wry::WebViewBuilder::new()
            .with_html(html)
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                size: wry::dpi::LogicalSize::new(320.0, 440.0).into(),
            })
            .build_as_child(window.as_ref())
        {
            Ok(wv) => wv,
            Err(e) => {
                eprintln!("[board-panel] webview build failed: {e}");
                return;
            }
        };
        eprintln!("[board-panel] open; polling 127.0.0.1:{port}/board");
        self.board_panel_window = Some(window);
        self.board_panel_webview = Some(webview);
    }
    /// Toggle the board panel from the menu: close if open, open if not.
    pub(crate) fn toggle_board_panel(&mut self, event_loop: &ActiveEventLoop) {
        if self.board_panel_window.is_some() {
            // Drop the webview before the window it borrows from.
            self.board_panel_webview = None;
            self.board_panel_window = None;
        } else {
            self.open_board_panel(event_loop);
        }
    }
    /// Open the arona full UI in its own OS window. Unlike the
    /// HTML-string panels this loads the arona-ui dist over the MCP HTTP
    /// server (`/arona-ui/`) — same-origin with the API the page fetches, and
    /// the in-window wry embed is off the table anyway (Metal layer conflict).
    pub(crate) fn open_arona_panel(&mut self, event_loop: &ActiveEventLoop) {
        // shim OFF(순정 모드)면 아로나 GUI 자체를 안 띄운다 — board/미러 hook 이 전무해
        // 빈 웹뷰가 뜨는 어색함 방지(arona_btn_rect 도 None 이라 버튼부터 숨겨진다).
        // 이건 전역 shim 축 — 아래 방 모드 게이트 부재와는 다른 결이다.
        if !crate::socket::read_shim_inject() {
            return;
        }
        if self.arona_panel_window.is_some() {
            return;
        }
        // (방) 모드 게이트 없음: solo/미설정 방에서도 창은 연다. 모드 안내·전환은
        // 웹 쪽 ModePicker 담당(GET /mode 로 분기) — 네이티브가 차단하면
        // ModePicker 에 도달 자체가 불가한 설계 모순이었다(거노 실측).
        let port = mcp_panel_port();
        // 처음부터 보이게 띄운다 — 배경을 교실 다크톤으로 칠해(아래 with_background_color)
        // 흰 플래시가 없고, 무엇보다 webview 로드가 실패해도(포트 stale 등) 창이 영영
        // 숨겨지는 단일 실패점을 없앤다. 옛 "Finished 후에만 set_visible(true)"는 로드가
        // 안 끝나면 "버튼 눌러도 안 열림"이 됐다(멀티 인스턴스 포트 race). 완료 시 focus 만.
        let attrs = WindowAttributes::default()
            .with_title("아로나 — 샬레 교실")
            .with_theme(Some(Theme::Dark))
            .with_visible(true)
            .with_inner_size(LogicalSize::new(1100.0, 720.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[arona-panel] window create failed: {e}");
                return;
            }
        };
        // launch 별 캐시버스트 — webview 가 옛 index.html 을 캐시해도 새 URL 이라
        // 무조건 새로 받는다(서버 no-store 와 이중 방어). relaunch 마다 값이 바뀜.
        let cb = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        // 로드 끝나면 포커스를 주는 핸들러(창은 이미 보임). webview2 가 UI(메인)
        // 스레드에서 콜백하므로 winit 호출이 안전하다.
        let win_show = window.clone();
        // 이미지 드롭은 HTML5 onDrop(입력창/패널)이 처리한다 — dragover preventDefault 로
        // WKWebView 가 드롭을 웹콘텐츠에 넘겨 ondrop+files 가 뜬다. wry 네이티브
        // drag_drop_handler 를 설치하면 그게 드롭을 가로채(active_pty 로 오배송) HTML 경로를
        // 막아 첨부가 안 됐다(거노 실측) → 설치 안 함.
        // build_as_child for the same use-after-free reason as the git panel.
        let webview = match wry::WebViewBuilder::new()
            .with_url(format!("http://127.0.0.1:{port}/arona-ui/?v={cb}"))
            // 로딩 중 노출되는 빈 배경을 교실 다크톤으로 — 흰 플래시 제거.
            .with_background_color((20, 22, 28, 255))
            .with_on_page_load_handler(move |event, _url| {
                if matches!(event, wry::PageLoadEvent::Finished) {
                    win_show.focus_window();
                }
            })
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                size: wry::dpi::LogicalSize::new(1100.0, 720.0).into(),
            })
            .build_as_child(window.as_ref())
        {
            Ok(wv) => Some(wv),
            Err(e) => {
                // webview 생성 실패해도 window 는 살려둔다(아래 항상 저장) — 옛 `return`
                // 은 로컬 window 를 drop 해 "검은창 떴다 사라짐"을 냈다. 실패 원인이
                // 보이게 창 제목을 에러로 바꾸고 webview 는 None 으로 둔다.
                eprintln!("[arona-panel] webview build failed: {e}");
                window.set_title(&format!("아로나 — 웹뷰 로드 실패: {e}"));
                None
            }
        };
        eprintln!("[arona-panel] open; http://127.0.0.1:{port}/arona-ui/");
        self.arona_panel_window = Some(window);
        self.arona_panel_webview = webview;
        // BA GUI 는 세션을 건드리지 않는다(거노 06-17 방향전환: "시각 레이어"). 자동 통솔
        // 자체가 폐기됐다(솔로 확정 06-18) — 아로나/SCHALE OS 는 관찰·시각 레이어일 뿐
        // 세션을 통솔하지 않는다(활성 pane 승격 호출 제거).
        // 제품 동작: 교실(BA UI)과 터미널을 둘 다 띄워 나란히 연동한다 — BA UI 의
        // 포커스/입력/상태가 메인 터미널 창과 양방향으로 묶인다. 옛 "교실이 화면을
        // 인수(터미널 숨김)"는 KASATERM_ARONA_SOLO_VIEW 몰입 옵션으로 강등.
        if std::env::var_os("KASATERM_ARONA_SOLO_VIEW").is_some() {
            if let Some(w) = &self.window {
                w.set_visible(false);
            }
        }
    }
    /// Close the arona window and bring the hidden main terminal back. The
    /// single close path — menu toggle, the window's X button, and
    /// `POST /arona-close` (ModePicker "터미널로") all route here so none of
    /// them can forget the reveal and strand the terminal hidden. No-op when
    /// the window isn't open.
    pub(crate) fn close_arona_panel(&mut self) {
        if self.arona_panel_window.is_none() {
            return;
        }
        // Drop the webview before the window it borrows from.
        self.arona_panel_webview = None;
        self.arona_panel_window = None;
        // 교실에서 나옴 — 숨겨둔 메인 터미널 창 복귀(+숨김 동안 못 받은
        // redraw 직접 청구).
        if let Some(w) = &self.window {
            w.set_visible(true);
            w.focus_window();
            w.request_redraw();
        }
        eprintln!("[arona-panel] closed; terminal revealed");
    }
    /// 거노: "터미널 보기"를 누르면 화면을 2분할 — 터미널(왼쪽)·아로나 교실(오른쪽).
    /// 두 네이티브 창을 현재 모니터 작업영역의 좌/우 절반에 타일링한다. 둘 다 떠
    /// 있을 때만 의미가 있어, 아로나 창이 없으면(순수 터미널) no-op.
    pub(crate) fn tile_terminal_arona_split(&self) {
        let (Some(term), Some(arona)) = (self.window.as_ref(), self.arona_panel_window.as_ref())
        else {
            return;
        };
        // 사용자가 보고 있는 화면 기준 — 떠 있는 아로나 창의 모니터.
        let Some(monitor) = arona.current_monitor().or_else(|| term.current_monitor()) else {
            return;
        };
        let mpos = monitor.position(); // 가상 데스크톱 물리좌표(멀티모니터 오프셋)
        let msize = monitor.size(); // 모니터 해상도(물리 px)
        // macOS 상단 메뉴바를 가리지 않게 인셋. 다른 OS는 0.
        let top_inset: i32 = if cfg!(target_os = "macos") {
            (28.0 * monitor.scale_factor()) as i32
        } else {
            0
        };
        let half_w = (msize.width / 2) as i32;
        let usable_h = ((msize.height as i32 - top_inset).max(200)) as u32;
        let y = mpos.y + top_inset;
        // 왼쪽: 터미널(frameless 라 inner≈outer).
        term.set_outer_position(winit::dpi::PhysicalPosition::new(mpos.x, y));
        let _ = term.request_inner_size(winit::dpi::PhysicalSize::new(half_w as u32, usable_h));
        term.set_visible(true);
        term.request_redraw();
        // 오른쪽: 아로나 교실(타이틀바 높이만큼 아래로 밀려도 허용).
        arona.set_outer_position(winit::dpi::PhysicalPosition::new(mpos.x + half_w, y));
        let _ = arona.request_inner_size(winit::dpi::PhysicalSize::new(
            msize.width - half_w as u32,
            usable_h,
        ));
    }
    /// BA GUI 버튼: 없으면 열고, 뒤/최소화 상태면 앞으로 가져오고, 이미 맨 앞이면 닫는다.
    /// 순수 토글이던 시절엔 창이 다른 창 뒤로 내려가 있어도 버튼이 "있음→닫기"라 두 번
    /// 눌러야 다시 떴다(거노: 내려간 창 다시 누르면 꺼져 불편). has_focus 로 분기.
    pub(crate) fn toggle_arona_panel(&mut self, event_loop: &ActiveEventLoop) {
        // 기본: arona-ui 를 기본 웹브라우저 탭으로 연다(거노 06-26). wry 임베드(별도 OS
        // 창)는 Metal layer 충돌을 피하려던 우회였고 지금은 비활성 — KASATERM_ARONA_WRY=1
        // 로만 복귀한다. 버튼·메뉴·키보드 3경로가 다 이 함수를 거치므로 여기서만 분기.
        if std::env::var_os("KASATERM_ARONA_WRY").is_none() {
            self.open_arona_in_browser();
            return;
        }
        if self.arona_panel_window.is_none() {
            self.open_arona_panel(event_loop);
            return;
        }
        // 떠 있음: 맨 앞이면 닫고, 뒤/숨김/최소화면 앞으로(raise+focus). borrow 를 블록에
        // 가둬 self.close_arona_panel(&mut self) 와 충돌하지 않게 focus 여부만 빼낸다.
        let focused = {
            let w = self.arona_panel_window.as_ref().unwrap();
            let f = w.has_focus();
            if !f {
                w.set_minimized(false);
                w.set_visible(true);
                w.focus_window();
            }
            f
        };
        if focused {
            self.close_arona_panel();
        }
    }

    /// arona-ui 를 기본 브라우저 탭으로 연다. MCP HTTP 서버가 같은 포트로 `/arona-ui/`
    /// 와 API 를 동일 origin 서빙하므로 페이지 fetch 가 그대로 동작한다. 캐시버스트(`?v=`)는
    /// 안 붙인다 — 같은 URL 이면 브라우저가 기존 탭을 재사용할 수 있다(중복 탭 방지).
    pub(crate) fn open_arona_in_browser(&self) {
        let port = mcp_panel_port();
        let url = format!("http://127.0.0.1:{port}/arona-ui/");
        open_url_in_browser(&url);
        eprintln!("[arona-browser] open {url}");
    }

    /// 거노: 새 방(윈도우) + 첫 pane 캐릭터 지정. 방별 collab 격리로 room slug 를
    /// 셸 env(KASATERM_ROOM)로 주입하고(spawn_session_pane 이 pending_room 을 읽음),
    /// 첫 pane 캐릭터를 지정값으로 강제한다(pending_character). 사용자가 그 pane 에서
    /// claude 를 치면 shim 이 persona·session-id 를 입히고, 추가 split pane 은 랜덤 배정.
    pub(crate) fn new_room_with_character(&mut self, character: &str) {
        let room = format!("room-{}", self.next_room_seq);
        self.next_room_seq += 1;
        self.pending_room = Some(room);
        self.pending_character = Some(character.to_string()); // 첫 pane = 지정 캐릭터
        self.new_window();
        // 좌측 방 라벨 = 선택 캐릭터 이름(방 구분 시각 라벨).
        self.window_name_override
            .insert(self.active_window, format!("● {character}"));
    }
    /// Effective render scale = DPI scale × whole-UI zoom. Everything that
    /// converts logical↔physical (cell metrics, chrome coords, cursor px,
    /// window→cols) routes through this so a single `ui_zoom` change scales
    /// the entire UI uniformly.
    pub(crate) fn effective_scale(&self) -> f32 {
        let dpi = self
            .window
            .as_ref()
            .map(|w| w.scale_factor() as f32)
            .unwrap_or(1.0);
        dpi * self.ui_zoom
    }
    /// "빠른 파일" 고정 섹션 목록: (라벨, 경로, 아이콘 이름). ① 개인 CLAUDE.md
    /// (~/.claude/CLAUDE.md) 는 항상, ② 프로젝트 CLAUDE.md(트리 root/CLAUDE.md)·
    /// ③ 프로젝트 메모리(root/.memory/MEMORY.md, symlink 허용→exists) 는 있을 때만.
    /// codex 짝(개인 ~/.codex/AGENTS.md · 프로젝트 root/AGENTS.md)도 있을 때만 넣는다 —
    /// codex pane 도 claude 처럼 자기 지시 파일을 한 번에 열게.
    /// ⚠️ 아이콘 "codex" 는 codex.svg 가 아직 없으면 gpu.rs match 에서 None 으로
    /// 빠져 아이콘만 안 뜬다(빌드는 안 깨진다). svg 들어오면 gpu.rs 에 arm 추가 필요.
    pub(crate) fn quick_files(&self) -> Vec<(&'static str, std::path::PathBuf, &'static str)> {
        let mut out: Vec<(&'static str, std::path::PathBuf, &'static str)> = Vec::new();
        if let Ok(home) = std::env::var("HOME") {
            let home = std::path::PathBuf::from(home);
            out.push(("개인 CLAUDE.md", home.join(".claude/CLAUDE.md"), "claude"));
            let agents = home.join(".codex/AGENTS.md");
            if agents.exists() {
                out.push(("개인 AGENTS.md", agents, "codex"));
            }
        }
        if let Some(root) = self.file_tree.root.as_ref() {
            let proj = root.join("CLAUDE.md");
            if proj.exists() {
                out.push(("프로젝트 CLAUDE.md", proj, "claude"));
            }
            let agents = root.join("AGENTS.md");
            if agents.exists() {
                out.push(("프로젝트 AGENTS.md", agents, "codex"));
            }
            let mem = root.join(".memory/MEMORY.md");
            if mem.exists() {
                out.push(("프로젝트 메모리", mem, "braces"));
            }
        }
        out
    }
    /// Adjust the whole-UI zoom by `delta` (additive on the multiplier).
    /// Clamped to a sane range; chrome + sidebar + every pane scale together.
    pub(crate) fn change_ui_zoom(&mut self, delta: f32) {
        let new = (self.ui_zoom + delta).clamp(0.5, 3.0);
        if (new - self.ui_zoom).abs() < 0.01 {
            return;
        }
        self.ui_zoom = new;
        self.apply_effective_scale();
    }
    /// Reset whole-UI zoom to native (1.0).
    pub(crate) fn reset_ui_zoom(&mut self) {
        if (self.ui_zoom - 1.0).abs() < 0.01 {
            return;
        }
        self.ui_zoom = 1.0;
        self.apply_effective_scale();
    }
    /// Push the current effective scale into the GPU renderer and reflow the
    /// cell grid + PTY size. Shared by zoom changes and (future) DPI
    /// scale-factor changes when the window moves between monitors.
    pub(crate) fn apply_effective_scale(&mut self) {
        let eff = self.effective_scale();
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.set_scale(eff);
            let (cw, ch) = gpu.set_font_size(self.font_size);
            self.cell = CellGeom { w: cw, h: ch, baseline: 0.0 };
        }
        if self.window.is_some() {
            let (cols, rows) = self.window_cells();
            self.resize_backend(cols, rows);
            self.chrome_dirty = true;
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
    }
    /// Rebuild everything the renderer derives from the display, without
    /// touching a single PTY. Cmd+Shift+R and the pane ⋮ menu's rotate icon.
    ///
    /// This exists because a terminal is the one app you cannot just restart
    /// to fix — every pane in it is live work. The automatic invalidation
    /// (`set_scale` → oversample + repack, `maintain_atlas` on a full atlas)
    /// should make this unnecessary; it is here for the display state we
    /// failed to notice, so a wrong-looking window is never a dead end.
    ///
    /// Order matters: re-seat the NSView onto the window's content rect,
    /// reconfigure the swapchain to the size we are actually at, re-derive
    /// scale/font metrics/PTY grid from the *current* monitor, and queue the
    /// atlas repack last so it lands at the next frame boundary with the new
    /// scale in place.
    ///
    /// The view step is the one that matters for a window that came back wrong
    /// from another monitor — see `gpu::ensure_view_fills_window`. It must run
    /// first: everything below reads `inner_size()`, which is derived from the
    /// view, so a shrunken view would quietly poison all of it. The swapchain
    /// must then be re-jammed with the size read *now*, not with the stored
    /// config — a refresh is needed precisely when that config is what drifted.
    pub(crate) fn refresh_renderer(&mut self) {
        if let Some(w) = self.window.as_ref() {
            gpu::ensure_view_fills_window(w);
            // 뷰가 멀쩡한데도 화면이 어긋나 새로고침을 누르는 경우가 있다 —
            // 레이어 backing scale 이 옛 모니터에 남은 상태. 아래 resize 로
            // drawable 을 다시 잡기 전에 짝부터 맞춰 둔다.
            gpu::ensure_layer_scale_matches(w);
            let size = w.inner_size();
            if let Some(gpu) = self.gpu.as_mut() {
                gpu.resize(size.width, size.height);
            }
        }
        self.apply_effective_scale();
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.force_atlas_reset();
        }
        self.chrome_dirty = true;
        if let Ok(mut ws) = self.ws.lock() {
            for pane in ws.panes.values_mut() {
                pane.dirty = true;
            }
        }
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        self.set_toast("화면 새로고침".to_string());
    }
    /// Adjust the focused pane's font multiplier (pane-local zoom). Only that
    /// pane's glyphs + PTY grid change; the BSP layout and other panes stay
    /// put. Delta is additive on the multiplier; clamped to a sane range.
    pub(crate) fn change_pane_font(&mut self, delta: f32) {
        let Some(active) = self.target_pane() else { return };
        let cur = self.pane_font_scales.get(&active).copied().unwrap_or(1.0);
        let new = (cur + delta).clamp(0.5, 3.0);
        if (new - cur).abs() < 0.01 {
            return;
        }
        self.pane_font_scales.insert(active, new);
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Reset the focused pane's font multiplier to match the rest of the UI.
    pub(crate) fn reset_pane_font(&mut self) {
        let Some(active) = self.target_pane() else { return };
        if self.pane_font_scales.remove(&active).is_none() {
            return;
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// True when the cursor block should be visible this frame.
    /// Solid for `BLINK_PAUSE_AFTER_INPUT_MS` after any input event, then
    /// toggles every `BLINK_HALF_PERIOD_MS`.
    pub(crate) fn cursor_blink_on(&self, now: Instant) -> bool {
        // Debug: KASATERM_NOBLINK=1 keeps the cursor solid so a
        // screenshot can verify cursor position/visibility without
        // racing the blink phase.
        if std::env::var_os("KASATERM_NOBLINK").is_some() {
            return true;
        }
        let since_input = now.saturating_duration_since(self.last_input_at);
        if since_input.as_millis() < BLINK_PAUSE_AFTER_INPUT_MS as u128 {
            return true;
        }
        let elapsed = since_input.as_millis() - BLINK_PAUSE_AFTER_INPUT_MS as u128;
        (elapsed / BLINK_HALF_PERIOD_MS as u128) % 2 == 0
    }
    /// "Host modifier" chord that opens the kasaterm shortcut layer
    /// (split / close / focus / copy-paste). macOS conventions reserve
    /// Cmd for this; Windows and Linux terminals overwhelmingly use
    /// Ctrl+Shift instead so Ctrl+letter stays free to deliver control
    /// bytes to the shell.
    pub(crate) fn host_mod(&self) -> bool {
        if cfg!(target_os = "macos") {
            self.modifiers.super_key()
        } else {
            self.modifiers.control_key() && self.modifiers.shift_key()
        }
    }
    /// Secondary modifier that flips a host shortcut into its alternate
    /// behavior (e.g. `Cmd+Shift+D` = stacked split on macOS). The host
    /// chord on Windows/Linux already owns Shift, so Alt fills the same
    /// role there.
    pub(crate) fn host_mod_alt(&self) -> bool {
        if cfg!(target_os = "macos") {
            self.modifiers.shift_key()
        } else {
            self.modifiers.alt_key()
        }
    }
    /// The foreground job name of `pid`'s session if it's something other than
    /// a plain login shell — what makes closing it worth a confirmation.
    /// `None` when the pane is idle at a shell prompt or has no session.
    pub(crate) fn pid_busy(&self, pid: &str) -> Option<String> {
        let name = self.pty.get(pid)?.active_process_name()?;
        if name.is_empty() || is_shell_name(&name) {
            None
        } else {
            Some(decorate_process_name(&name))
        }
    }
    /// First running job across every pane/tab — drives the window-close
    /// confirmation ("close the whole app while claude is mid-run?").
    fn any_pane_busy(&self) -> Option<String> {
        let pids: Vec<String> = {
            let ws = self.ws.lock().unwrap();
            ws.panes
                .values()
                .flat_map(|p| p.tabs.iter().filter_map(|t| t.pid.clone()))
                .collect()
        };
        pids.iter().find_map(|p| self.pid_busy(p))
    }
    /// First running job in sidebar session `idx` only — drives the per-session
    /// close confirm (sidebar tab ×). Mirrors `close_window`'s layout pick: the
    /// active window's tree lives in `pty_layout`, the rest in `windows[idx]`.
    fn window_busy(&self, idx: usize) -> Option<String> {
        let layout = if idx == self.active_window {
            self.pty_layout.as_ref()
        } else {
            self.windows.get(idx).and_then(|w| w.as_ref())
        }?;
        let pids: Vec<String> = {
            let ws = self.ws.lock().unwrap();
            layout
                .leaves()
                .iter()
                .filter_map(|leaf| ws.panes.get(*leaf))
                .flat_map(|p| p.tabs.iter().filter_map(|t| t.pid.clone()))
                .collect()
        };
        pids.iter().find_map(|p| self.pid_busy(p))
    }
    /// Cmd+W / header ×: close tab `idx` of `pane`. A multi-tab pane drops just
    /// that tab; the last tab drops the pane (no-op on a single-pane window, so
    /// we skip it there and leave the OS close button to quit). If the tab is
    /// running a real job, raise the confirm modal instead of closing now.
    pub(crate) fn confirm_or_close_tab(&mut self, pane: &str, idx: usize) {
        let (tabs_len, pid) = {
            let ws = self.ws.lock().unwrap();
            match ws.panes.get(pane) {
                Some(p) => (p.tabs.len(), p.tabs.get(idx).and_then(|t| t.pid.clone())),
                // PaneState 가 **없는 게 정상**인 pane 이 있다 — split leaf 는 보조 탭이
                // 생길 때까지 `ws.panes` 에 안 들어간다(main.rs `pane_font_scales` 주석이
                // 같은 사실을 말한다). 여기서 return 하면 그런 pane 은 Cmd+W 가 통째로
                // 죽는다(거노: "커맨드 W 해도 무반응"). 항목이 없다 = 탭 하나짜리 pane.
                None => (1, None),
            }
        };
        let action = if tabs_len > 1 {
            PendingClose::Tab { pane: pane.to_string(), idx }
        } else {
            let leaves = self.pty_layout.as_ref().map_or(0, |t| t.leaves().len());
            if leaves <= 1 {
                // 이 방의 마지막 pane. 방이 여럿이면 **방을 닫는 것**으로 잇는다 —
                // 전에는 여기서 그냥 return 이라 Cmd+W 가 죽은 키였다(거노).
                // 방이 하나뿐이면 그건 앱 종료라 OS 닫기 버튼에 맡기고 no-op.
                let idx = self.active_window;
                if self.windows.len() <= 1 {
                    return;
                }
                let action = PendingClose::Session(idx);
                // 바쁨·미저장이 있으면 그쪽 대화가 무엇을 잃는지까지 말해 주므로 먼저다.
                if self.guard_dirty(&action) {
                    return;
                }
                match self.window_busy(idx) {
                    Some(proc) => self.open_confirm_close(proc, action),
                    None => self.raise_confirm(ConfirmClose { why: CloseWhy::LastPane, action }),
                }
                return;
            }
            PendingClose::Pane { pane: pane.to_string() }
        };
        if self.guard_dirty(&action) {
            return;
        }
        match pid.as_deref().and_then(|p| self.pid_busy(p)) {
            Some(proc) => self.open_confirm_close(proc, action),
            None => self.do_close(action),
        }
    }
    /// CloseRequested (red light / Cmd+Q): returns true when a job is running
    /// and the confirm modal was raised — the caller must NOT exit yet. Returns
    /// false when nothing's running, so the caller exits immediately.
    pub(crate) fn confirm_or_close_window(&mut self) -> bool {
        if self.guard_dirty(&PendingClose::Window) {
            return true;
        }
        match self.any_pane_busy() {
            Some(proc) => {
                self.open_confirm_close(proc, PendingClose::Window);
                true
            }
            None => false,
        }
    }
    /// Sidebar session (window `idx`) close: raise the confirm modal if any pane
    /// in that session is running a job, else close it now. The app stays open —
    /// this is the per-session path, distinct from the whole-app quit above.
    pub(crate) fn confirm_or_close_session(&mut self, idx: usize) {
        if self.guard_dirty(&PendingClose::Session(idx)) {
            return;
        }
        match self.window_busy(idx) {
            Some(proc) => self.open_confirm_close(proc, PendingClose::Session(idx)),
            None => self.do_close(PendingClose::Session(idx)),
        }
    }
    fn open_confirm_close(&mut self, proc: String, action: PendingClose) {
        self.raise_confirm(ConfirmClose { why: CloseWhy::Busy(proc), action });
    }
    fn raise_confirm(&mut self, dlg: ConfirmClose) {
        self.confirm_close = Some(dlg);
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Every unsaved editor that `action` would destroy, with the file name to
    /// show. Empty means nothing would be lost.
    fn dirty_docs(&self, action: &PendingClose) -> Vec<(DirtyDoc, String)> {
        // 별도창 편집기는 pane 트리 밖에 산다 — 앱 종료와 그 창 자체를 닫을
        // 때만 걸린다.
        let aux = |want: Option<winit::window::WindowId>| -> Vec<(DirtyDoc, String)> {
            self.aux_windows
                .iter()
                .filter(|a| want.is_none_or(|w| a.window.id() == w))
                .filter_map(|a| {
                    let m = a.editor().filter(|m| m.modified)?;
                    Some((DirtyDoc::Aux(a.window.id()), doc_name(&m.doc.path)))
                })
                .collect()
        };
        let panes: Vec<String> = match action {
            PendingClose::Tab { pane, idx } => {
                let ws = self.ws.lock().unwrap();
                return ws
                    .panes
                    .get(pane)
                    .and_then(|p| p.tabs.get(*idx))
                    .and_then(|t| t.markdown().filter(|m| m.modified))
                    .map(|m| {
                        vec![(
                            DirtyDoc::Tab { pane: pane.clone(), tab: *idx },
                            doc_name(&m.doc.path),
                        )]
                    })
                    .unwrap_or_default();
            }
            PendingClose::Pane { pane } => vec![pane.clone()],
            PendingClose::Session(i) => {
                let layout = if *i == self.active_window {
                    self.pty_layout.as_ref()
                } else {
                    self.windows.get(*i).and_then(|w| w.as_ref())
                };
                layout.map_or_else(Vec::new, |t| t.leaves().iter().map(|l| l.to_string()).collect())
            }
            PendingClose::AuxEditor(id) => return aux(Some(*id)),
            // 앱 종료는 세션 전부 + 별도창 전부.
            PendingClose::Window => {
                let mut all: Vec<String> = self
                    .windows
                    .iter()
                    .flatten()
                    .chain(self.pty_layout.as_ref())
                    .flat_map(|t| t.leaves().into_iter().map(|l| l.to_string()))
                    .collect();
                all.sort();
                all.dedup();
                all
            }
        };
        let ws = self.ws.lock().unwrap();
        let mut out: Vec<(DirtyDoc, String)> = panes
            .iter()
            .filter_map(|id| Some((id, ws.panes.get(id)?)))
            .flat_map(|(id, p)| {
                p.tabs.iter().enumerate().filter_map(move |(t, tab)| {
                    let m = tab.markdown().filter(|m| m.modified)?;
                    Some((DirtyDoc::Tab { pane: id.clone(), tab: t }, doc_name(&m.doc.path)))
                })
            })
            .collect();
        drop(ws);
        if matches!(action, PendingClose::Window) {
            out.extend(aux(None));
        }
        out
    }

    /// Raise the unsaved-changes dialog if `action` would throw work away.
    /// Returns true when the caller must stop and wait for the answer.
    pub(crate) fn guard_dirty(&mut self, action: &PendingClose) -> bool {
        let docs = self.dirty_docs(action);
        if docs.is_empty() {
            return false;
        }
        // 별도창을 닫으려는데 확인은 메인 창에 뜬다 — 안 띄우면 사용자는 창이
        // 그냥 안 닫히는 것으로 본다.
        if matches!(action, PendingClose::AuxEditor(_)) {
            if let Some(w) = &self.window {
                w.focus_window();
            }
        }
        self.raise_confirm(ConfirmClose { why: CloseWhy::Dirty(docs), action: action.clone() });
        true
    }

    /// Save every listed editor. False means at least one write failed — the
    /// caller must abort the close rather than lose those edits.
    pub(crate) fn save_dirty_docs(&mut self, docs: &[(DirtyDoc, String)]) -> bool {
        let mut ok = true;
        for (doc, name) in docs {
            let Some((text, path)) = self.doc_text(doc) else { continue };
            if let Err(e) = crate::markdown::write_atomic(&path, &text) {
                eprintln!("[editor] 저장 실패 {path}: {e}");
                self.set_toast(format!("⚠ {name} 저장 실패: {e}"));
                ok = false;
                continue;
            }
            self.mark_doc_clean(doc);
        }
        ok
    }

    /// Drop the listed editors' changes. Clearing `modified` is what makes the
    /// close go through — the re-entered guard then sees nothing to lose.
    pub(crate) fn discard_dirty_docs(&mut self, docs: &[(DirtyDoc, String)]) {
        for (doc, _) in docs {
            self.mark_doc_clean(doc);
        }
    }

    /// Write every editor whose typing has gone quiet for the autosave delay,
    /// and report when the next one comes due so the caller can park a timer
    /// on it (the loop sleeps completely when idle — without a deadline the
    /// last edit would sit unwritten until something else woke us).
    ///
    /// Silent by design: no toast, and a failure only logs. Autosave the user
    /// didn't ask for shouldn't interrupt them; the unsaved dot stays up and
    /// the close guard still catches it, which is the honest signal.
    pub(crate) fn run_editor_autosave(&mut self) -> Option<Instant> {
        let Some(delay) = self.set_autosave else { return None };
        // "저장 / 저장 안 함" 을 묻는 중에 몰래 쓰면 '저장 안 함' 이 거짓말이 된다.
        // 대화창이 닫힐 때까지 미룬다(취소하면 그때 정상 만기로 다시 걸린다).
        if matches!(
            self.confirm_close.as_ref().map(|c| &c.why),
            Some(CloseWhy::Dirty(_))
        ) {
            return None;
        }
        let now = Instant::now();
        let mut next: Option<Instant> = None;
        // (문서 위치, 마지막 타자 시각) 을 먼저 모은다 — 저장은 ws 락 밖에서.
        let mut ready: Vec<DirtyDoc> = Vec::new();
        {
            let ws = self.ws.lock().unwrap();
            for (id, pane) in ws.panes.iter() {
                for (t, tab) in pane.tabs.iter().enumerate() {
                    let Some(at) = tab.markdown().and_then(|m| m.edited_at) else { continue };
                    if now.duration_since(at) >= delay {
                        ready.push(DirtyDoc::Tab { pane: id.clone(), tab: t });
                    } else {
                        let due = at + delay;
                        next = Some(next.map_or(due, |n: Instant| n.min(due)));
                    }
                }
            }
        }
        for a in self.aux_windows.iter() {
            let Some(at) = a.editor().and_then(|m| m.edited_at) else { continue };
            if now.duration_since(at) >= delay {
                ready.push(DirtyDoc::Aux(a.window.id()));
            } else {
                let due = at + delay;
                next = Some(next.map_or(due, |n: Instant| n.min(due)));
            }
        }
        if !ready.is_empty() {
            self.save_dirty_docs_quiet(&ready);
            self.chrome_dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        next
    }

    /// `save_dirty_docs` without the failure toast — see `run_editor_autosave`.
    fn save_dirty_docs_quiet(&mut self, docs: &[DirtyDoc]) {
        for doc in docs {
            let job = self.doc_text(doc);
            let Some((text, path)) = job else { continue };
            match crate::markdown::write_atomic(&path, &text) {
                Ok(()) => self.mark_doc_clean(doc),
                Err(e) => eprintln!("[editor] 자동 저장 실패 {path}: {e}"),
            }
        }
    }

    fn doc_text(&self, doc: &DirtyDoc) -> Option<(String, String)> {
        match doc {
            DirtyDoc::Tab { pane, tab } => {
                let ws = self.ws.lock().unwrap();
                ws.panes
                    .get(pane)
                    .and_then(|p| p.tabs.get(*tab))
                    .and_then(|t| t.markdown())
                    .map(|m| (m.edit_lines.join("\n"), m.doc.path.clone()))
            }
            DirtyDoc::Aux(id) => self
                .aux_windows
                .iter()
                .find(|a| a.window.id() == *id)
                .and_then(|a| a.editor())
                .map(|m| (m.edit_lines.join("\n"), m.doc.path.clone())),
        }
    }

    fn mark_doc_clean(&mut self, doc: &DirtyDoc) {
        match doc {
            DirtyDoc::Tab { pane, tab } => {
                let mut ws = self.ws.lock().unwrap();
                let Some(p) = ws.panes.get_mut(pane) else { return };
                if let Some(m) = p.tabs.get_mut(*tab).and_then(|t| t.markdown_mut()) {
                    m.mark_saved();
                    // 미저장 점이 사라지려면 이 pane 이 다시 그려져야 한다.
                    p.dirty = true;
                }
            }
            DirtyDoc::Aux(id) => {
                let Some(a) = self.aux_windows.iter_mut().find(|a| a.window.id() == *id)
                else {
                    return;
                };
                if let Some(m) = a.editor_mut() {
                    m.mark_saved();
                    a.window.request_redraw();
                }
            }
        }
    }

    /// Run a non-window close action immediately. `Window` is left to the
    /// caller (it needs the event loop to exit).
    pub(crate) fn do_close(&mut self, action: PendingClose) {
        match action {
            PendingClose::Tab { pane, idx } => {
                self.close_tab(&pane, idx);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            PendingClose::Pane { pane } => self.close_pane(&pane),
            PendingClose::Session(idx) => {
                if let Err(e) = self.close_window(idx) {
                    eprintln!("[window] close failed: {e:#}");
                }
            }
            PendingClose::AuxEditor(id) => {
                if let Some(i) = self.aux_windows.iter().position(|a| a.window.id() == id) {
                    self.close_aux_window(i);
                }
            }
            PendingClose::Window => {}
        }
    }
}

/// File name for the dialog — the full path would blow the card's width.
fn doc_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string()
}

/// Raise a macOS desktop notification. Inside the signed `.app` bundle we use
/// `UNUserNotificationCenter` so the alert carries kasaterm's own app icon (and
/// gets the native sound/click affordances). The bare `cargo run` binary has no
/// bundle identifier and can't obtain notification authorization, so there we
/// fall back to `osascript` — which shows the Script Editor icon (dev-only).
#[cfg(target_os = "macos")]
pub(crate) fn notify_desktop(title: &str, body: &str, character: Option<&str>) {
    if is_bundled() {
        notify_native(title, body, character);
    } else {
        notify_osascript(title, body);
    }
}

/// 알림에 붙일 그 학생의 프사 파일.
///
/// 이미지는 `include_bytes!` 로 바이너리에 박혀 있어 경로가 없는데, 첨부가 받는
/// 것은 **파일 URL 뿐**이다. 그래서 슬러그마다 한 번씩 임시 파일로 떨궈 두고 그
/// 경로를 재사용한다. 로스터에 없는 커스텀 캐릭터는 슬러그가 없어 None 이다.
#[cfg(target_os = "macos")]
fn student_profile_file(character: &str) -> Option<std::path::PathBuf> {
    let slug = crate::theme::character_slug(character)?;
    let path = std::env::temp_dir()
        .join("kasaterm-notify-icons")
        .join(format!("{slug}.png"));
    if path.exists() {
        return Some(path);
    }
    let png = crate::render::student_profile_png(slug)?;
    std::fs::create_dir_all(path.parent()?).ok()?;
    std::fs::write(&path, png).ok()?;
    Some(path)
}

/// 알림 권한 판정 — 0=아직 답 없음 · 1=허용 · 2=거부.
///
/// 전에는 `requestAuthorization` 의 콜백이 **빈 블록**이라 거부돼도 아무도 몰랐다:
/// 요청은 그대로 native 로 나가고 시스템이 조용히 버려, 화면에는 "알림이 안 온다"
/// 만 남았다. 답을 여기 남겨 두면 다음 알림부터 osascript 로 돌릴 수 있다.
#[cfg(target_os = "macos")]
static NOTIFY_AUTH: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// True when running from a `.app` bundle (has a `CFBundleIdentifier`). Native
/// `UNUserNotificationCenter` requires this; the bare binary returns `None`.
#[cfg(target_os = "macos")]
fn is_bundled() -> bool {
    objc2_foundation::NSBundle::mainBundle()
        .bundleIdentifier()
        .is_some()
}

/// Request alert/sound authorization once per process. The system shows the
/// permission prompt on first call; the grant persists across launches.
#[cfg(target_os = "macos")]
pub(crate) fn ensure_notification_authorization() {
    use objc2_user_notifications::{UNAuthorizationOptions, UNUserNotificationCenter};
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if !is_bundled() {
            return;
        }
        let handler = block2::RcBlock::new(
            |granted: objc2::runtime::Bool, err: *mut objc2_foundation::NSError| {
                let ok = granted.as_bool();
                NOTIFY_AUTH.store(
                    if ok { 1 } else { 2 },
                    std::sync::atomic::Ordering::Relaxed,
                );
                if !ok {
                    let why = unsafe { err.as_ref() }
                        .map(|e| e.localizedDescription().to_string())
                        .unwrap_or_else(|| "사유 없음".to_string());
                    eprintln!("[notify] 데스크톱 알림 권한 없음 — osascript 로 돌린다: {why}");
                }
            },
        );
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let opts = UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound;
        center.requestAuthorizationWithOptions_completionHandler(opts, &handler);
    });
}

#[cfg(target_os = "macos")]
fn notify_native(title: &str, body: &str, character: Option<&str>) {
    use objc2_foundation::{NSArray, NSString, NSURL};
    use objc2_user_notifications::{
        UNMutableNotificationContent, UNNotificationAttachment, UNNotificationRequest,
        UNUserNotificationCenter,
    };
    ensure_notification_authorization();
    // 거부가 확정났으면 native 는 요청을 받아 놓고 버린다 — 그 자리에서 돌린다.
    if NOTIFY_AUTH.load(std::sync::atomic::Ordering::Relaxed) == 2 {
        notify_osascript(title, body);
        return;
    }
    // Unique id per request so rapid completions don't replace each other.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    // 학생 프사를 오른쪽 썸네일로 — 알림이 여럿 겹쳐도 누구 것인지 그림으로 갈린다.
    // **왼쪽 작은 아이콘은 번들 아이콘 고정**이라 여기서 못 바꾼다(그건 앱 아이콘).
    if let Some(p) = character.and_then(student_profile_file) {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy()));
        let aid = NSString::from_str(&format!("kasaterm-icon-{seq}"));
        if let Ok(att) = unsafe {
            UNNotificationAttachment::attachmentWithIdentifier_URL_options_error(&aid, &url, None)
        } {
            content.setAttachments(&NSArray::from_retained_slice(&[att]));
        }
    }
    let ident = NSString::from_str(&format!("kasaterm-notify-{seq}"));
    let request =
        UNNotificationRequest::requestWithIdentifier_content_trigger(&ident, &content, None);
    let center = UNUserNotificationCenter::currentNotificationCenter();
    // 배달이 실패하면(권한 회수·첨부 거부 등) 그 자리에서 osascript 로 돌린다.
    // 실패를 삼키면 "알림이 안 온다" 만 남고 이유는 어디에도 안 남는다.
    let (t, b) = (title.to_string(), body.to_string());
    let done = block2::RcBlock::new(move |err: *mut objc2_foundation::NSError| {
        if let Some(e) = unsafe { err.as_ref() } {
            eprintln!(
                "[notify] native 배달 실패 — osascript 로 돌린다: {}",
                e.localizedDescription()
            );
            notify_osascript(&t, &b);
        }
    });
    center.addNotificationRequest_withCompletionHandler(&request, Some(&done));
}

#[cfg(target_os = "macos")]
fn notify_osascript(title: &str, body: &str) {
    let script = format!(
        "display notification {} with title {}",
        applescript_quote(body),
        applescript_quote(title),
    );
    let _ = crate::proc::command("osascript")
        .arg("-e")
        .arg(script)
        .spawn();
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn notify_desktop(_title: &str, _body: &str, _character: Option<&str>) {}

/// Set (or clear, when 0) the Dock tile badge to the unread-notification count.
#[cfg(target_os = "macos")]
fn set_dock_badge(count: usize) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSString;
    let Some(mtm) = MainThreadMarker::new() else { return };
    let app = NSApplication::sharedApplication(mtm);
    let label = (count > 0).then(|| NSString::from_str(&count.to_string()));
    app.dockTile().setBadgeLabel(label.as_deref());
}
#[cfg(not(target_os = "macos"))]
fn set_dock_badge(_count: usize) {}

#[cfg(not(target_os = "macos"))]
pub(crate) fn ensure_notification_authorization() {}

/// Wrap `s` in an AppleScript string literal, escaping `"` and `\` so a pane
/// title with quotes can't break out of the `display notification` command.
#[cfg(target_os = "macos")]
fn applescript_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// 기본 브라우저로 URL 열기 — macOS `open`, Windows `cmd /C start`, 그 외 `xdg-open`.
/// BA GUI 버튼이 arona-ui 를 외부 탭으로 띄울 때 쓴다(wry 임베드 비활성 대체).
pub(crate) fn open_url_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let _ = crate::proc::command("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = crate::proc::command("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let _ = crate::proc::command("xdg-open").arg(url).spawn();
}

#[cfg(test)]
mod toast_tests {
    use super::*;

    // 완료 토스트 = 캐릭터(pane 고정) + hook title(완료 순간). title 앞 "✓ " 중복 제거.
    #[test]
    fn attention_toast_combines_character_and_reason() {
        assert_eq!(
            format_attention_toast(Some("미도리"), "권한 요청"),
            "⚠ 미도리 — 권한 요청"
        );
    }

    // 미현존/미배정 pane → 캐릭터 없이 hook title 만(정보 보존, 드롭 안 함).
    #[test]
    fn attention_toast_falls_back_when_no_character() {
        assert_eq!(
            format_attention_toast(None, "권한 요청"),
            "⚠ 권한 대기중 — 권한 요청"
        );
        assert_eq!(format_attention_toast(None, ""), "⚠ 권한 대기중");
    }

    // 권한 대기 토스트 — 캐릭터·reason 유무 4갈래.
    #[test]
    fn attention_toast_variants() {
        assert_eq!(
            format_attention_toast(Some("아루"), "Bash 실행 권한"),
            "⚠ 아루 — Bash 실행 권한"
        );
        assert_eq!(format_attention_toast(Some("아루"), ""), "⚠ 아루 — 권한 대기중");
        assert_eq!(
            format_attention_toast(None, "Bash 실행 권한"),
            "⚠ 권한 대기중 — Bash 실행 권한"
        );
        assert_eq!(format_attention_toast(None, ""), "⚠ 권한 대기중");
    }
}


/// 이 간격 안에 다시 누르면 더블클릭이지 이름 편집이 아니다. 헤드리스 하네스도
/// 이 값을 봐야 하므로(문턱을 두 벌로 두면 하네스가 조용히 어긋난다) 밖에 둔다.
pub(crate) const ROOM_RENAME_DOUBLE_CLICK_MS: u128 = 500;

/// 「느린 더블클릭」인가 — **이미 열려 있는 방**의 줄을, **더블클릭 문턱보다 늦게**
/// 다시 누른 경우.
///
/// 셋을 다 봐야 한다: ①같은 줄 ②그 방이 지금 활성(=첫 클릭이 전환이 아니라 선택이었다)
/// ③직전 클릭에서 문턱 초과. ③이 없으면 진짜 더블클릭이 편집을 열고, ②가 없으면
/// 다른 방으로 전환하려던 두 번째 클릭이 편집을 연다.
pub(crate) fn starts_room_rename(
    last: Option<(usize, std::time::Instant)>,
    idx: usize,
    active: usize,
    now: std::time::Instant,
) -> bool {
    // 너무 오래 지난 클릭은 "다시 누른 것"이 아니라 새 클릭이다 — 몇 분 전 클릭이
    // 편집을 열면 사용자는 이유를 못 찾는다.
    const STALE_MS: u128 = 5_000;
    let Some((prev_idx, at)) = last else { return false };
    let ms = now.duration_since(at).as_millis();
    prev_idx == idx && idx == active && ms > ROOM_RENAME_DOUBLE_CLICK_MS && ms <= STALE_MS
}

#[cfg(test)]
mod room_rename_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn at(ms: u64) -> (Option<(usize, Instant)>, Instant) {
        let t0 = Instant::now();
        (Some((1, t0)), t0 + Duration::from_millis(ms))
    }

    #[test]
    fn 느리게_다시_누르면_편집이다() {
        let (last, now) = at(700);
        assert!(starts_room_rename(last, 1, 1, now));
    }

    #[test]
    fn 진짜_더블클릭은_편집이_아니다() {
        // 문턱 안(300ms) — 여기서 열면 더블클릭 동작과 겹쳐 둘 다 오작동한다.
        let (last, now) = at(300);
        assert!(!starts_room_rename(last, 1, 1, now));
    }

    #[test]
    fn 다른_방으로_전환하는_두번째_클릭은_편집이_아니다() {
        // 누른 줄(2)이 활성(1)이 아니다 = 첫 클릭이 선택이 아니라 전환이었다.
        let (last, now) = at(700);
        assert!(!starts_room_rename(last, 2, 1, now));
    }

    #[test]
    fn 한참_뒤_클릭은_새_클릭이다() {
        let (last, now) = at(9_000);
        assert!(!starts_room_rename(last, 1, 1, now));
    }

    #[test]
    fn 직전_클릭이_없으면_편집이_아니다() {
        assert!(!starts_room_rename(None, 1, 1, Instant::now()));
    }
}
