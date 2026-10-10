//! 방(창)과 초점 — 방 만들기·고르기·순서 바꾸기·닫기, 방 이름 짓기, 칸·탭 초점 옮기기.
use super::*;

pub(super) fn forward_backend_focus<T>(pane_id: String, send: impl FnOnce(String) -> T) -> T {
    send(pane_id)
}

pub(super) fn apply_workspace_focus(
    ws: &mut Workspace,
    requested: &str,
    exact_surface: bool,
) -> Option<bool> {
    let outer = ws
        .outer_for_pty(requested)
        .unwrap_or_else(|| requested.to_string());
    if !ws.panes.contains_key(&outer) {
        let changed = ws.active_pane.as_deref() != Some(outer.as_str());
        ws.active_pane = Some(outer);
        return Some(changed);
    }
    let tab = if exact_surface || requested != outer {
        Some(ws.panes.get(&outer)?.tabs.iter().position(|tab| {
            tab.pid.as_deref() == Some(requested)
                || (requested == outer && tab.pid.is_none())
        })?)
    } else {
        None
    };
    let pane_changed = ws.active_pane.as_deref() != Some(outer.as_str());
    let pane = ws.panes.get_mut(&outer)?;
    let changed = pane_changed || tab.is_some_and(|idx| pane.active_tab != idx);
    if let Some(idx) = tab {
        pane.active_tab = idx;
        pane.dirty = true;
    }
    ws.active_pane = Some(outer);
    Some(changed)
}

impl App {
    /// Create a new window inside the *current* session: stash the visible
    /// window's layout, then bring up a fresh window with a single new pane.
    /// The new pane's PTY joins the session's shared `pty` map and runs in the
    /// same `ws`, so it's a sibling of the existing windows — switching between
    /// them never tears a pane down. Windows are this session's tmux-style
    /// "windows"; the session list one level up is tmux "sessions".
    pub(crate) fn new_window(&mut self) {
        self.close_inline_web();
        self.session_touched = true;
        // Active window's slot is None — its layout lives in pty_layout. Park
        // it back into the slot before opening a new window.
        self.windows[self.active_window] = self.pty_layout.take();
        self.windows.push(None);
        self.active_window = self.windows.len() - 1;
        self.win_tab_reveal(self.active_window);
        // spawn_session_pane sets pty_layout to a fresh single-pane tree,
        // inserts the PTY into the shared map, and points ws.active_pane at it.
        if let Err(e) = self.spawn_session_pane() {
            eprintln!("[window] new window pane spawn failed: {e:#}");
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Switch the visible window to `idx` within the current session: park the
    /// visible window's layout, swap the target's in. `pty`/`ws` are shared
    /// across the session's windows, so no PTY is touched — only which BSP tree
    /// the renderer draws. Focus lands on the target window's first pane.
    /// Which window owns `pane` (as one of its leaves). The active window's tree
    /// lives in `pty_layout` (its `windows` slot is None); the rest carry their
    /// own layout. Mirrors the sidebar `sb_busy`/`sb_done` lookup.
    pub(crate) fn window_of_pane(&self, pane: &str) -> Option<usize> {
        // 탭 pid 는 BSP leaf 가 아니다 — 화면을 든 건 그 탭이 사는 바깥 pane 이다.
        // 접지 않으면 「그 surface 가 어느 창에 있나」가 탭에 대해 항상 None 이 되고,
        // 그걸 존재 판정으로 쓰는 소켓 split 이 「없는 pane」이라며 거절했다. 그래서
        // 학생들이 split 을 포기하고 탭으로 우회했다(사용자 2026-08-07: "갑자기 애들
        // 왜 탭안에 생성하지"). 접는 규칙은 `outer_for_pty` 한 곳에만 둔다.
        let pane = self
            .ws
            .lock()
            .ok()
            .and_then(|w| w.outer_for_pty(pane))
            .unwrap_or_else(|| pane.to_string());
        (0..self.windows.len()).find(|&i| {
            let layout = if i == self.active_window {
                self.pty_layout.as_ref()
            } else {
                self.windows[i].as_ref()
            };
            layout.is_some_and(|l| l.leaves().contains(&pane.as_str()))
        })
    }

    pub(super) fn room_selection_changes(requested: usize, total: usize, active: usize) -> Option<bool> {
        (requested < total).then_some(requested != active)
    }

    /// 보기 창 생성 여부가 번호를 바꾸지 않도록 원본 기기의 방을 정본으로 삼는다.
    pub(crate) fn remote_room_navigation(&self) -> Vec<(String, Option<u64>, String)> {
        let labels = crate::sidebar_navigation::section_labels(&self.info);
        labels.iter().filter_map(|label| self.info.machines_col.machines.iter().find(|m| &m.label == label))
            .flat_map(|m| crate::sidebar_navigation::rooms(m).into_iter()
                .map(|(name, rows)| (m.label.clone(), rows[0].window, name)))
            .collect()
    }

    pub(crate) fn room_navigation_count(&self) -> usize {
        (0..self.windows.len()).filter(|&i| self.remote_view_of_window(i).is_none()).count()
            + self.remote_room_navigation().len()
    }

    pub(crate) fn room_number_for_window(&self, window: usize) -> Option<usize> {
        let local: Vec<_> = (0..self.windows.len()).filter(|&i| self.remote_view_of_window(i).is_none()).collect();
        if let Some(n) = local.iter().position(|&i| i == window) { return Some(n); }
        let (label, ids) = self.remote_view_of_window(window)?;
        let machine = self.info.machines_col.machines.iter().find(|m| m.label == label)?;
        let source = machine.remote.iter().chain(&machine.mirrored).find(|row| ids.contains(&row.remote_id))?;
        self.remote_room_navigation().iter().position(|(l, w, name)| {
            l == &label && match (*w, source.window) {
                (Some(a), Some(b)) => a == b,
                (None, None) => name == &source.room,
                _ => false,
            }
        }).map(|n| local.len() + n)
    }

    pub(crate) fn goto_room_number(&mut self, number: usize) {
        self.chrome_dirty = true;
        let local: Vec<_> = (0..self.windows.len()).filter(|&i| self.remote_view_of_window(i).is_none()).collect();
        if let Some(&window) = local.get(number) {
            self.goto_room(window);
        } else if let Some((label, window, room)) = number.checked_sub(local.len())
            .and_then(|n| self.remote_room_navigation().get(n).cloned()) {
            if let Err(error) = self.open_remote_room(&label, window, &room, None) {
                self.set_toast(format!("방을 열 수 없음: {error}"));
            }
        }
    }

    /// 다른 기기의 기기색 표를 받아 더 새로운 항목을 들인다 — 양쪽이 같은 색을 쓰게(5초마다).
    pub(crate) fn sync_device_colors(&mut self) {
        use std::sync::{Mutex, OnceLock};
        static LAST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
        {
            let mut last = LAST.get_or_init(|| Mutex::new(None)).lock().unwrap();
            if last.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(5)) { return; }
            *last = Some(Instant::now());
        }
        let mut changed = false;
        for (_, table) in kasa_mcp::machines::cached_device_colors() {
            changed |= crate::render::pane_identity::merge_device_colors(&table);
        }
        if changed { self.chrome_dirty = true; }
    }

    /// 내부 방은 트리 교체 외의 진입 절차도 필요하므로 각자의 열기 함수로 보낸다.
    pub(crate) fn goto_room(&mut self, idx: usize) {
        if idx >= self.windows.len() {
            return;
        }
        match self.internal_room_kind_at(idx) {
            Some(crate::internal_room::InternalRoomKind::Settings) => {
                self.commit_room_rename();
                self.open_settings_room(None);
            }
            None => {
                self.commit_room_rename();
                self.switch_window(idx);
            }
        }
    }

    pub(crate) fn switch_window(&mut self, idx: usize) {
        let Some(changes_room) =
            Self::room_selection_changes(idx, self.windows.len(), self.active_window)
        else {
            return;
        };
        self.close_inline_web();
        if !changes_room {
            return;
        }
        // 줌은 App 전역 상태인데 pane 은 방(윈도우)마다 다르다 — 줌한 채로 방을
        // 옮기면 그 방에 없는 pane 을 가리킨 유령 줌이 남아, 새 방이 「최대화된
        // 무언가」처럼 보이거나 되돌릴 대상이 없어진다. 방을 옮기는 순간 푼다.
        self.zoomed_pane = None;
        self.windows[self.active_window] = self.pty_layout.take();
        self.pty_layout = self.windows[idx].take();
        self.active_window = idx;
        // 빈 방이면 셸을 하나 띄워 되살린다. 예전엔 여기 오기 전에 `windows[idx]
        // .is_none()` 으로 막았는데, 그러면 그 방은 **활성으로 만들 수 없고 활성이
        // 아니면 닫을 수도 없다** — 사이드바에는 계속 보이는데 눌러도 아무 일이
        // 없었다(사용자 2026-08-25 「방을 닫을수도 pane을 닫을수도 없어 복구도
        // 안되고」). 막는 대신 들여보내고 쓸 수 있는 방으로 만든다.
        if self.pty_layout.is_none() {
            if let Err(e) = self.spawn_session_pane() {
                eprintln!("[window] 빈 방 {idx} 되살리기 실패: {e:#}");
            }
        }
        self.win_tab_reveal(idx);
        // The user is now looking at this window — clear any unseen-notification
        // pulse on its sidebar tab, and stop its panes blinking (states stay shown).
        self.window_alert.remove(&idx);
        self.quiet_room_blinks(idx);
        // Swapping in a stashed window produces no new PTY output, so nothing
        // would flip a pane's `dirty` and the damage-tracked render would skip
        // the frame — the screen stays on the old window. Mark every leaf of
        // the incoming window dirty (plus chrome for the sidebar highlight) so
        // the next redraw actually repaints.
        let leaves: Vec<String> = self
            .pty_layout
            .as_ref()
            .map(|l| l.leaves().iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        if !leaves.is_empty() {
            let mut ws = self.ws.lock().unwrap();
            ws.active_pane = Some(leaves[0].clone());
            for leaf in &leaves {
                if let Some(p) = ws.panes.get_mut(leaf) {
                    p.dirty = true;
                }
            }
        }
        self.handoff_ime_to_active_surface();
        self.chrome_dirty = true;
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        // The sidebar highlight + window body are chrome state. Without
        // flagging chrome_dirty, `about_to_wait` parks on WaitUntil(blink)
        // and the switch only paints on the next blink tick (or not at all
        // if the redraw request is coalesced) — the tab looks unresponsive.
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// 방(윈도우) 탭을 끌어 순서를 바꾼다. `from` 을 뽑아 `to` 자리에 꽂는다.
    /// `to` 는 **뽑기 전** 기준의 삽입 슬롯(0..=len)이라, 드래그 중 그리는 삽입선
    /// 위치를 그대로 넘기면 된다.
    ///
    /// 인덱스가 곧 신원인 상태가 넷이다 — 활성 방, 이름 오버라이드, 알림 마킹,
    /// 라벨 캐시. 벡터만 흔들고 이것들을 두면 방을 옮긴 순간 이름이 남의 방에
    /// 붙고 알림 점이 엉뚱한 탭에서 뛰므로, 같은 remap 을 넷 다 통과시킨다.
    /// 세션 저장(`session_state_json`)과 board 의 `window_idx` 는 이 벡터 순서를
    /// 매번 다시 읽으니 저절로 따라온다.
    pub(crate) fn reorder_window(&mut self, from: usize, to: usize) {
        let n = self.windows.len();
        if from >= n || to > n {
            return;
        }
        // 설정·보드 같은 내부 방도 자리를 옮길 수 있다. 셸이 없을 뿐 사이드바에서는
        // 다른 방과 똑같은 탭이라, 잡히기는 하는데 놓으면 아무 일도 없는 편이 더
        // 이상하다(2026-09-05 지시 「설정방이랑 다른방 위치바꾸는거까지」). 인덱스를
        // 키로 쓰는 필드는 예외 없이 아래 remap 을 지나고, 내부 방은 애초에 저장에서
        // 빠지므로(`should_persist_layout`) 바뀐 순서가 기록에 남아 어긋날 여지도 없다.
        // 뽑고 난 뒤 기준의 착지 인덱스. 제자리면 옮길 것이 없다.
        let dst = if to > from { to - 1 } else { to };
        if dst == from {
            return;
        }
        // 활성 방의 트리는 슬롯이 아니라 pty_layout 에 있다 — 슬롯만 옮기면 활성
        // 방의 내용이 통째로 빠진다. 제자리에 돌려놓고 옮긴 뒤 새 자리에서 꺼낸다.
        self.windows[self.active_window] = self.pty_layout.take();
        let slot = self.windows.remove(from);
        self.windows.insert(dst, slot);
        let remap = move |i: usize| crate::remap_window_index(i, from, dst);
        self.active_window = remap(self.active_window);
        let overrides = std::mem::take(&mut self.window_name_override);
        self.window_name_override = overrides
            .into_iter()
            .map(|(i, name)| (remap(i), name))
            .collect();
        let alerts = std::mem::take(&mut self.window_alert);
        self.window_alert = alerts.into_iter().map(remap).collect();
        let expanded = std::mem::take(&mut self.expanded_windows);
        self.expanded_windows = expanded.into_iter().map(remap).collect();
        let bodies = std::mem::take(&mut self.room_list_body);
        self.room_list_body = bodies.into_iter().map(|(i, v)| (remap(i), v)).collect();
        // 도는 중인 펼침 모션도 방을 인덱스로 가리킨다. 0.16초짜리라 그 안에 방을
        // 끌어 옮기는 일은 드물지만, 인덱스를 키로 쓰는 필드가 **예외 없이** 여기를
        // 지나야 다음 사람이 이 목록을 믿는다.
        self.expand_anim = self
            .expand_anim
            .map(|(i, opening, at)| (remap(i), opening, at));
        // 되살리기 대기 중인 pane 도 자기 방을 인덱스로 가리킨다 — 안 옮기면 ⌘⇧T 가
        // 엉뚱한 방에서 pane 을 꺼낸다.
        for c in self.closed_panes.iter_mut() {
            c.window = remap(c.window);
        }
        // 별도창 터미널도 떠나온 방을 인덱스로 든다 — dock 이 돌아갈 곳.
        for t in self.aux.terminals.iter_mut() {
            t.home_window = remap(t.home_window);
        }
        self.pty_layout = self.windows[self.active_window].take();
        // 라벨은 인덱스 병렬 배열이라 캐시를 버려 다음 paint 에 다시 뽑게 한다.
        self.window_labels_at = None;
        self.win_tab_reveal(self.active_window);
        self.session_touched = true;
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// pane 하나로 포커스를 옮긴다 — 다른 방(윈도우)에 있으면 그 방부터 앞으로
    /// 가져온다. `active_pane` 만 바꾸면 안 보이는 윈도우의 pane 이 선택돼 화면은
    /// 그대로다. `switch_window` 가 leaves[0] 로 `active_pane` 을 덮으므로 순서는
    /// 반드시 **방 전환 → pane 지정**이다.
    ///
    /// 실재하는 leaf 일 때만 옮긴다 — 캐릭터·작업명 같은 집계 id 로 `active_pane`
    /// 을 덮으면 다음 `/layout` 폴에서 그 타일이 빠져 pane 이 닫힌 것처럼 보였다
    /// (사용자: 캐릭터 클릭→학생 선택하면 닫힘).
    pub(crate) fn focus_pane(&mut self, pane: &str) -> bool {
        self.focus_target(pane, false)
    }

    /// `surface.focus`/알림처럼 PTY id를 지목한 전환. 바깥 leaf만 고르면 같은
    /// pane의 다른 탭을 요청해도 화면은 옛 탭에 남으므로 active_tab까지 맞춘다.
    pub(crate) fn focus_surface(&mut self, surface: &str) -> bool {
        self.focus_target(surface, true)
    }

    pub(super) fn focus_target(&mut self, requested: &str, exact_surface: bool) -> bool {
        let outer = {
            let ws = self.ws.lock().unwrap();
            ws.outer_for_pty(requested)
                .unwrap_or_else(|| requested.to_string())
        };
        // 별도창으로 뗀 pane 은 어느 트리에도 없다 — 그 OS 창을 앞으로 보내는 것이
        // 「거기로 가기」다(알림 클릭·소켓 focus 가 조용히 실패하지 않게).
        if let Some(ai) = self.aux_terminal_index(&outer) {
            if exact_surface {
                let mut ws = self.ws.lock().unwrap();
                if let Some(pane) = ws.panes.get_mut(&outer) {
                    if let Some(index) = pane.tabs.iter().position(|tab| tab.pid.as_deref() == Some(requested)) {
                        pane.active_tab = index;
                        pane.dirty = true;
                    }
                }
            }
            let w = &self.aux.terminals[ai].window;
            w.set_minimized(false);
            w.focus_window();
            w.request_redraw();
            return true;
        }
        let Some(wi) = self.window_of_pane(&outer) else {
            return false;
        };
        if wi != self.active_window {
            self.switch_window(wi);
        }
        let applied = {
            let mut ws = self.ws.lock().unwrap();
            apply_workspace_focus(&mut ws, requested, exact_surface).is_some()
        };
        if !applied {
            return false;
        }
        self.handoff_ime_to_active_surface();
        self.chrome_dirty = true;
        true
    }

    /// Close the window at `idx`. The last window can't be closed (a session
    /// always needs one). Every pane in the closed window is torn down — its
    /// PTY Arc dropped (kills the shell) and its render state removed — same
    /// teardown remove_pane uses. Closing the visible window swaps a neighbor
    /// in so the terminal keeps painting.
    pub(crate) fn close_window(&mut self, idx: usize) -> Result<()> {
        if idx >= self.windows.len() {
            anyhow::bail!("no such window: {idx}");
        }
        if let Some(kind) = self.internal_room_kind_at(idx) {
            let closed = match kind {
                crate::internal_room::InternalRoomKind::Settings => self.close_settings_room(),
            };
            return closed
                .then_some(())
                .ok_or_else(|| anyhow::anyhow!("cannot close internal room"));
        }
        // 설정 방이 옆에 있어도 마지막 사용자 방은 마지막이다. `windows.len()`만
        // 보면 사용자 방 하나 + 설정 방 하나를 둘로 세어 작업 공간을 없앨 수 있다.
        if self.user_room_count() <= 1 {
            anyhow::bail!("cannot close the last user window");
        }
        self.session_touched = true;
        // Pull the closing window's layout (active one lives in pty_layout) and
        // kill every pane it owns.
        let layout = if idx == self.active_window {
            self.pty_layout.take()
        } else {
            self.windows[idx].take()
        };
        if let Some(layout) = layout {
            for pane_id in layout.leaves() { self.cancel_restore_pane(pane_id); }
            let mut ws = self.ws.lock().unwrap();
            for pane_id in layout.leaves() {
                self.pty.remove(pane_id);
                ws.panes.remove(pane_id);
            }
        }
        if idx == self.active_window {
            let target = (0..idx)
                .rev()
                .chain(idx + 1..self.windows.len())
                .find(|candidate| self.internal_room_kind_at(*candidate).is_none())
                .ok_or_else(|| anyhow::anyhow!("no user window remains"))?;
            self.pty_layout = self.windows[target].take();
            self.windows.remove(idx);
            self.active_window = if target > idx { target - 1 } else { target };
            if let Some(first) = self
                .pty_layout
                .as_ref()
                .and_then(|l| l.leaves().first().map(|s| s.to_string()))
            {
                self.ws.lock().unwrap().active_pane = Some(first);
                self.handoff_ime_to_active_surface();
            }
        } else {
            self.windows.remove(idx);
            if idx < self.active_window {
                self.active_window -= 1;
            }
        }
        // 뒤쪽 방의 인덱스가 한 칸 당겨지므로 인덱스를 키로 쓰는 chrome 상태도
        // 같이 옮긴다 — 설정·보드 방을 닫을 때와 같은 규칙이다. 안 옮기면 닫은
        // 방의 펼침·알림·이름이 다음 방에 그대로 얹혀, 엉뚱한 방이 펴져 있거나
        // 남의 이름을 달고 있었다.
        self.remap_removed_window_metadata(idx);
        // 픽셀 스크롤이라 인덱스를 당길 것이 없다 — 닫는 동안은 `close_freeze` 가
        // 그 위치를 붙잡고, 범위 밖 값은 `sidebar_layout` 이 클램프한다.
        // 여기서 활성 방을 보이게 끌어오지 않는다 — 방을 하나 닫을 때마다 목록이
        // 그쪽으로 튀어, ×를 한자리에서 연달아 누를 수가 없었다(2026-08-27 지시:
        // "크롬처럼 한곳에서 마우스누르다가 떼면 재정렬되고"). 새 방·방 전환은
        // 그 방이 보여야 하는 동작이라 `win_tab_reveal` 을 그대로 둔다.
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        Ok(())
    }
    /// 방이 사라지면 이름·펼침·복원 위치가 다음 방에 잘못 붙지 않게 함께 당긴다.
    pub(crate) fn remap_removed_window_metadata(&mut self, idx: usize) {
        let remap = |i: usize| crate::internal_room::remap_after_removal(i, idx).unwrap_or(0);
        self.window_name_override = std::mem::take(&mut self.window_name_override)
            .into_iter()
            .filter(|(i, _)| *i != idx)
            .map(|(i, name)| (remap(i), name))
            .collect();
        self.window_alert = std::mem::take(&mut self.window_alert)
            .into_iter()
            .filter(|i| *i != idx)
            .map(remap)
            .collect();
        self.expanded_windows = std::mem::take(&mut self.expanded_windows)
            .into_iter()
            .filter(|i| *i != idx)
            .map(remap)
            .collect();
        self.room_list_body = std::mem::take(&mut self.room_list_body)
            .into_iter()
            .filter(|(i, _)| *i != idx)
            .map(|(i, v)| (remap(i), v))
            .collect();
        self.expand_anim = self
            .expand_anim
            .filter(|(i, _, _)| *i != idx)
            .map(|(i, opening, at)| (remap(i), opening, at));
        for closed in &mut self.closed_panes {
            closed.window = remap(closed.window);
        }
        // 별도창 터미널은 방이 닫혀도 살아남는다(leaf 가 아니라 안 죽는다) — 집이
        // 없어졌으면 활성 방을 새 집으로 삼는다.
        for t in self.aux.terminals.iter_mut() {
            t.home_window = crate::internal_room::remap_after_removal(t.home_window, idx)
                .unwrap_or(self.active_window);
        }
        self.window_labels_at = None;
    }

    pub(crate) fn refresh_window_labels(&mut self) {
        let now = Instant::now();
        let fresh = self.window_labels.len() == self.windows.len()
            && self
                .window_labels_at
                .is_some_and(|t| now.duration_since(t).as_millis() < 1000);
        if fresh {
            self.overlay_room_rename_label();
            return;
        }
        let n = self.windows.len();
        let mut out = Vec::with_capacity(n);
        let ws = self.ws.lock().unwrap();
        for i in 0..n {
            if let Some(kind) = self.internal_room_kind_at(i) {
                out.push((kind.label().to_string(), String::new()));
                continue;
            }
            // Representative pane = first leaf of the window's layout. The
            // active window's tree lives in pty_layout; the rest in windows[i].
            let leaves: Vec<String> = {
                let layout = if i == self.active_window {
                    self.pty_layout.as_ref()
                } else {
                    self.windows.get(i).and_then(|o| o.as_ref())
                };
                let mut v: Vec<String> = layout.map_or(Vec::new(), |l| {
                    l.leaves().iter().map(|s| s.to_string()).collect()
                });
                // 별도창으로 뗀 pane 도 이 방 식구다 — 빠지면 방 이름이 흔들린다.
                v.extend(self.room_undocked(i));
                v
            };
            let repr = leaves.first().cloned();
            // 방을 대표하는 cwd — 첫 leaf 가 아니라 **학생이 앉은 곳의 최빈값**이다.
            // 첫 pane 하나로 이름을 지으면 그 pane 이 뭘 띄웠는지에 따라 방 이름이
            // 흔들려, 사이드바에서 자리로 방을 찾던 눈이 매번 다시 읽어야 했다.
            // 좁히는 규칙은 `room_home_cwd` 에 있다.
            let cwds: Vec<(std::path::PathBuf, bool)> = leaves
                .iter()
                .filter_map(|id| {
                    // 학생 판정은 `pane_record` 가 쓰는 기준과 같다 — 바인딩된 세션
                    // id 도 함께 봐서, claude 가 잠시 내려간 pane 이 셸로 강등돼
                    // 방 이름이 흔들리는 일이 없다.
                    let agent = self.pane_claude_sid.contains_key(id)
                        || self.pty.get(id).and_then(|p| p.active_agent()).is_some();
                    self.pane_current_cwd(id).map(|p| (p, agent))
                })
                .collect();
            let home = room_home_cwd(&cwds);
            // 거울 방 — pane 전부가 같은 남의 기계면 그 기계 이름이 방 이름이다. 거울
            // pane 은 로컬 cwd 도 프로세스도 없어 아래 사슬이 전부 비고 「win 6」으로
            // 떨어졌다(2026-09-07 지적). 둘째 줄은 저쪽 작업 폴더.
            let mirror_room: Option<(String, Option<String>)> = {
                let infos: Vec<kasa_mcp::remote::RemoteInfo> = leaves
                    .iter()
                    .filter_map(|id| kasa_mcp::remote::remote_info(id))
                    .collect();
                (!leaves.is_empty()
                    && infos.len() == leaves.len()
                    && !infos[0].label.is_empty()
                    && infos.iter().all(|x| x.label == infos[0].label))
                .then(|| {
                    (
                        infos[0].label.clone(),
                        infos.iter().find_map(|x| x.remote_cwd.clone()),
                    )
                })
            };
            // 손으로 붙인 이름은 파생을 항상 이긴다 — 지정 pane 이 대표 leaf 가
            // 아니어도, 방을 옮겨도 유지돼야 한다.
            let name = self
                .window_name_override
                .get(&i)
                .cloned()
                .or_else(|| mirror_room.as_ref().map(|(l, _)| l.clone()))
                .or_else(|| {
                    home.as_ref()
                        .and_then(|p| p.file_name())
                        .map(|s| s.to_string_lossy().into_owned())
                        .filter(|s| !s.is_empty())
                })
                .or_else(|| {
                    repr.as_ref().and_then(|id| {
                        ws.panes
                            .get(id)
                            .and_then(|p| p.title.clone())
                            .filter(|t| !t.is_empty())
                            .or_else(|| {
                                self.pty
                                    .get(id)
                                    .and_then(|p| p.active_process_name())
                                    .filter(|t| !t.is_empty())
                            })
                    })
                })
                .or_else(|| {
                    // 방의 pane 을 전부 숨기면 대표 pane 도 cwd 도 사라져 이름이
                    // `win 3` 으로 떨어진다 — 조금 전까지 「momewomo」였던 방이 갑자기
                    // 번호가 되면 사람 눈에는 그 방이 없어지고 빈 방이 새로 생긴
                    // 것처럼 읽힌다(2026-09-05 지적 「pane 하나 있을 때 숨기면 윈도우가
                    // 없어지던데? win4 이렇게 되면서」). 숨겨 둔 pane 이 자기 폴더를
                    // 들고 있으니 그것으로 이름을 잇는다 — 숨김은 정리 루프가 건너뛰어
                    // 되살릴 때까지 남으므로, 이름도 그동안 버틴다.
                    let mine = || self.closed_panes.iter().filter(|c| c.window == i);
                    mine()
                        .find(|c| c.stashed && !c.folder.is_empty())
                        .or_else(|| mine().find(|c| !c.folder.is_empty()))
                        .map(|c| c.folder.clone())
                })
                .unwrap_or_else(|| format!("win {}", i + 1));
            let cwd = home
                .as_ref()
                .map(|p| Self::shorten_cwd(p))
                .or_else(|| {
                    mirror_room
                        .and_then(|(_, c)| c)
                        .map(|c| Self::shorten_cwd(std::path::Path::new(&c)))
                })
                .unwrap_or_default();
            out.push((name, cwd));
        }
        drop(ws);
        self.window_labels = out;
        self.window_labels_at = Some(now);
        self.overlay_room_rename_label();
    }

    /// 편집 중인 방의 라벨을 버퍼(+조합 중인 글자+캐럿)로 덮는다. 별도 입력칸을
    /// 띄우지 않고 라벨 자리를 그대로 쓰는 Finder 식 편집이다.
    ///
    /// **재계산 안이 아니라 밖에서 덮는 게 핵심이다.** 위 캐시는 1초짜리고 cwd 를
    /// `lsof` 로 캐느라 비싸서 매 키마다 깰 수가 없는데, 합성을 그 안에 두면 타이핑이
    /// 1초씩 뭉쳐 나온다(사용자: "이름 바꾸는 게 버벅여").
    pub(super) fn overlay_room_rename_label(&mut self) {
        let Some((idx, buf)) = self.room_rename.editing.as_ref() else {
            return;
        };
        let composing = match self.ime_focus {
            Some(crate::ImeFocus::RoomRename(i)) if i == *idx => self.preedit.as_str(),
            _ => "",
        };
        // 캐럿은 커서 자리다 — 늘 끝에 붙이면 가운데를 고치는 동안 커서가 어디 있는지
        // 화면이 거짓말을 한다. 조합 중인 글자는 커서 바로 앞에 온다.
        let (before, after) = crate::lineedit::split(buf, self.room_rename.cursor);
        let text = format!("{before}{composing}\u{258c}{after}");
        if let Some(slot) = self.window_labels.get_mut(*idx) {
            slot.0 = text;
        }
    }
    /// Compress a cwd for the sidebar: home → `~`, then keep the tail if it
    /// runs past `max` chars so the meaningful (deepest) part stays visible.
    /// 탭/헤더 라벨용. 셸이 idle이면 cwd의 마지막 폴더명, 명령 실행 중이면
    /// 그 프로세스명. zsh 4개로 안 보이고 위치/작업이 드러나게.
    pub(crate) fn smart_pane_label(sess: &kasa_pty::PtySession) -> Option<String> {
        let proc = sess.active_process_name().filter(|t| !t.is_empty());
        let is_shell = proc.as_deref().map_or(false, |p| {
            let base = p.strip_prefix('-').unwrap_or(p);
            matches!(
                base,
                "zsh" | "bash" | "fish" | "sh" | "dash" | "tcsh" | "ksh"
            )
        });
        if is_shell {
            sess.shell_pid()
                .and_then(socket::pid_cwd)
                .map(|p| Self::cwd_basename(&p))
                .or(proc)
        } else {
            proc
        }
    }
    /// cwd의 마지막 폴더명. 홈 디렉토리면 `~`.
    pub(crate) fn cwd_basename(p: &std::path::Path) -> String {
        if kasa_socket::home_dir().as_deref() == Some(p) {
            return "~".to_string();
        }
        p.file_name()
            .and_then(|s| s.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| "/".to_string())
    }
    pub(crate) fn shorten_cwd(p: &std::path::Path) -> String {
        let s = tilde_home(&p.to_string_lossy());
        let max = 26usize;
        let chars: Vec<char> = s.chars().collect();
        if chars.len() > max {
            let tail: String = chars[chars.len() - (max - 1)..].iter().collect();
            format!("…{tail}")
        } else {
            s
        }
    }
}

/// Resolve the cwd for a newly spawned shell, honoring the user's `default_cwd`
/// setting like other terminals' "working directory" option:
///   - `"last"` (default) with a spawning `prev` pane → inherit its cwd. This
///     wins over `KASATERM_CWD` on purpose: that env is a *first-pane* launch
///     override (a launcher saying "start this instance here"), and it leaks
///     into child shells via env inheritance, so letting it beat the split
///     inheritance would pin every sibling to the launch dir instead of the
///     pane the user split off.
///   - `KASATERM_CWD` env (explicit launch override) — first pane / fixed modes.
///   - `"home"` → `$HOME`.
///   - an absolute or `~`-prefixed path → that directory if it exists.
/// 방(윈도우) 하나를 `from` 에서 `dst` 로 옮겼을 때 인덱스 `i` 가 가는 새 자리.
/// `dst` 는 뽑아낸 **뒤** 기준(`Vec::remove` 후 `insert` 자리)이다.
///
/// 인덱스가 곧 방의 신원인 상태 — 활성 방, 이름 오버라이드, 알림 마킹 — 를 전부
/// 이 하나로 통과시킨다. 자리마다 손으로 옮기면 한 군데만 어긋나도 이름이 남의
/// 방에 붙는 식으로 조용히 틀어지므로, 규칙을 한 곳에 두고 테스트로 못박았다.
pub(crate) fn remap_window_index(i: usize, from: usize, dst: usize) -> usize {
    if i == from {
        dst
    } else if from < dst && i > from && i <= dst {
        i - 1
    } else if from > dst && i >= dst && i < from {
        i + 1
    } else {
        i
    }
}

/// 임시 디렉토리 아래인가. 방 이름 후보에서 밀어내는 데 쓴다 — claude 가 pane 마다
/// 파는 `…/scratchpad/<슬러그>` 는 프로젝트가 아니라 세션 부산물이라, 그게 방
/// 이름이 되면 방이 무슨 일을 하는 곳인지 알려주지 못한다. 게다가 여러 방이 같은
/// 슬러그를 쓰면 사이드바에서 방끼리 구별되지 않는다(2026-08-24 지적: 서로 다른 두
/// 방이 나란히 `dogfood8-run3` 로 떴다).
pub(super) fn is_temp_path(p: &std::path::Path) -> bool {
    // macOS 는 `/tmp`·`/var` 가 `/private` 아래 실경로를 갖는다 — cwd 를 `lsof` 로
    // 캐면 그 실경로로 돌아오므로 양쪽을 다 적는다.
    const ROOTS: &[&str] = &[
        "/tmp",
        "/private/tmp",
        "/var/tmp",
        "/private/var/tmp",
        "/var/folders",
        "/private/var/folders",
    ];
    if ROOTS.iter().any(|r| p.starts_with(r)) {
        return true;
    }
    // 위 목록에 없는 플랫폼 임시 루트(Windows 의 `%TEMP%`).
    let t = std::env::temp_dir();
    t.parent().is_some() && p.starts_with(&t)
}

/// 방을 대표하는 cwd — 사이드바 방 이름·부제의 원본. `panes` 는 leaf 순서대로
/// `(cwd, 학생이 앉은 pane 인가)`.
///
/// **방의 정체는 학생이 앉은 프로젝트다.** 곁다리 셸 하나가 다른 폴더에 있다고 방
/// 이름이 그리로 끌려가면, 사이드바에서 자리로 방을 찾던 눈이 매번 다시 읽어야
/// 한다(2026-08-24 지적: recall 방이 곁다리 셸 하나 때문에 임시폴더 이름을
/// 뒤집어썼다). 그래서 최빈값을 세기 전에 후보를 두 번 좁힌다.
///
/// ① 학생이 앉은 pane 이 하나라도 있으면 후보를 그 pane 들로 좁힌다 — 셸은 학생이
/// 하나도 없는 방(사람이 직접 쓰는 터미널 방)에서만 이름을 짓는다.
/// ② 그중 프로젝트 경로가 하나라도 있으면 임시 경로를 뺀다. **전부 임시면 남긴다** —
/// 스크래치패드에서만 도는 방은 그게 유일한 정체다.
///
/// 좁히고 남은 것의 최빈값, 동률이면 leaf 순서가 먼저인 쪽. leaves 순서가 고정이라
/// 같은 방이 늘 같은 이름을 얻는다.
pub(super) fn room_home_cwd(panes: &[(std::path::PathBuf, bool)]) -> Option<std::path::PathBuf> {
    let agents: Vec<&std::path::PathBuf> =
        panes.iter().filter(|(_, a)| *a).map(|(p, _)| p).collect();
    let pool: Vec<&std::path::PathBuf> = if agents.is_empty() {
        panes.iter().map(|(p, _)| p).collect()
    } else {
        agents
    };
    let named: Vec<&std::path::PathBuf> =
        pool.iter().copied().filter(|p| !is_temp_path(p)).collect();
    let pool = if named.is_empty() { pool } else { named };

    pool.into_iter()
        .fold(Vec::<(&std::path::PathBuf, usize)>::new(), |mut acc, p| {
            match acc.iter_mut().find(|(q, _)| *q == p) {
                Some((_, c)) => *c += 1,
                None => acc.push((p, 1)),
            }
            acc
        })
        .into_iter()
        .reduce(|a, b| if b.1 > a.1 { b } else { a })
        .map(|(p, _)| p.clone())
}

#[cfg(test)]
mod room_label_tests {
    /// 방의 pane 을 전부 숨기면 대표 pane 도 cwd 도 없어져 이름이 번호로 떨어졌다.
    /// 조금 전까지 폴더 이름이던 방이 갑자기 `win 4` 가 되면, 사람 눈에는 그 방이
    /// 사라지고 빈 방이 새로 생긴 것으로 읽힌다(2026-09-05 지적). 숨긴 pane 이
    /// 자기 폴더를 들고 있으므로 번호로 가기 전에 그것을 본다.
    #[test]
    fn an_emptied_room_keeps_its_name_before_falling_back_to_a_number() {
        let session = include_str!("rooms.rs");
        let numbered = session
            .find(r#"format!("win {}", i + 1)"#)
            .expect("번호 폴백");
        let stashed = session
            .find("c.stashed && !c.folder.is_empty()")
            .expect("숨긴 pane 의 폴더를 보는 겹");
        assert!(
            stashed < numbered,
            "번호로 떨어지기 전에 숨겨 둔 pane 의 폴더를 먼저 봐야 한다"
        );
    }
}

/// 사이드바 방 이름 판정 — 어떤 cwd 가 방을 대표하는가.
#[cfg(test)]
mod room_name_tests {
    use super::{is_temp_path, room_home_cwd};

    /// 방 이름 판정. `(cwd, 학생이 앉았나)` 를 leaf 순서대로 준다.
    fn room(panes: &[(&str, bool)]) -> Option<String> {
        let v: Vec<(std::path::PathBuf, bool)> = panes
            .iter()
            .map(|(p, a)| (std::path::PathBuf::from(p), *a))
            .collect();
        room_home_cwd(&v).map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
    }
    const SCRATCH: &str =
        "/private/tmp/claude-501/-Users-kasa-repo/0e7ab2f6/scratchpad/dogfood8-run3";

    #[test]
    fn room_name_takes_the_most_common_cwd() {
        assert_eq!(
            room(&[
                ("/a/recall", true),
                ("/a/branding", true),
                ("/a/recall", true)
            ]),
            Some("recall".into())
        );
        assert_eq!(room(&[]), None);
    }

    /// 동률이면 leaf 순서가 먼저인 쪽 — 순서가 고정이라 같은 방이 늘 같은 이름을 얻는다.
    #[test]
    fn room_name_breaks_a_tie_by_leaf_order() {
        assert_eq!(
            room(&[("/a/recall", true), ("/a/branding", true)]),
            Some("recall".into())
        );
        assert_eq!(
            room(&[("/a/branding", true), ("/a/recall", true)]),
            Some("branding".into())
        );
    }

    /// 2026-08-24 지적의 재현. 곁다리 셸이 임시 폴더에 앉아 있어도 방 이름은
    /// 학생이 보는 프로젝트다 — 이게 깨지면 recall 방이 `dogfood8-run3` 가 된다.
    #[test]
    fn room_name_ignores_a_stray_shell_in_a_temp_dir() {
        // leaf 순서상 셸이 **먼저**다 — 옛 규칙은 동률에서 이쪽이 이겼다.
        assert_eq!(
            room(&[(SCRATCH, false), ("/a/recall", true)]),
            Some("recall".into())
        );
    }

    /// 셸이 다수여도 학생 쪽이 이긴다 — 방의 정체는 학생이 앉은 프로젝트다.
    #[test]
    fn room_name_prefers_agent_panes_over_a_shell_majority() {
        assert_eq!(
            room(&[
                ("/a/Downloads", false),
                ("/a/Downloads", false),
                ("/a/recall", true),
            ]),
            Some("recall".into())
        );
    }

    /// 학생이 하나도 없는 방(사람이 직접 쓰는 터미널)은 셸 cwd 로 이름이 지어진다.
    #[test]
    fn room_name_falls_back_to_shells_when_no_agent_sits_there() {
        assert_eq!(
            room(&[
                ("/a/Downloads", false),
                ("/a/Downloads", false),
                ("/a/recall", false)
            ]),
            Some("Downloads".into())
        );
    }

    /// 방 전체가 임시 폴더면 그게 유일한 정체다 — 밀어내면 이름이 없어진다.
    #[test]
    fn room_name_keeps_a_temp_dir_when_thats_all_there_is() {
        assert_eq!(room(&[(SCRATCH, true)]), Some("dogfood8-run3".into()));
        assert_eq!(
            room(&[(SCRATCH, false), ("/tmp/scratch", false)]),
            Some("dogfood8-run3".into())
        );
    }

    /// 학생만 남긴 뒤에도 임시 제외가 걸린다 — 학생 둘이 각각 프로젝트/스크래치면
    /// 프로젝트 쪽이다.
    #[test]
    fn room_name_drops_temp_after_narrowing_to_agents() {
        assert_eq!(
            room(&[(SCRATCH, true), ("/a/recall", true)]),
            Some("recall".into())
        );
    }

    #[test]
    fn temp_paths_cover_both_macos_spellings() {
        use std::path::Path;
        assert!(is_temp_path(Path::new(SCRATCH)));
        assert!(is_temp_path(Path::new("/tmp/x")));
        assert!(is_temp_path(Path::new("/var/folders/ab/cd/T/x")));
        assert!(is_temp_path(Path::new("/private/var/folders/ab/cd/T/x")));
        assert!(!is_temp_path(Path::new("/Users/kasa/Desktop/repo")));
        // 이름이 임시 루트로 시작할 뿐인 경로는 임시가 아니다.
        assert!(!is_temp_path(Path::new("/tmpfs/repo")));
        assert!(!is_temp_path(Path::new("/Users/kasa/tmp/repo")));
    }
}

#[cfg(test)]
mod inline_room_selection_tests {
    use super::App;

    #[test]
    fn 현재_방_재선택도_유효한_닫기_요청이다() {
        assert_eq!(App::room_selection_changes(1, 3, 1), Some(false));
        assert_eq!(App::room_selection_changes(2, 3, 1), Some(true));
        assert_eq!(App::room_selection_changes(3, 3, 1), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_reorder_remap_matches_the_actual_move() {
        // 2번 방을 3번 자리로 끌면 잡은 방이 3으로 가고, 밀려난 3이 2로 당겨온다.
        assert_eq!(remap_window_index(2, 2, 3), 3);
        assert_eq!(remap_window_index(3, 2, 3), 2);
        assert_eq!(remap_window_index(0, 2, 3), 0);
        assert_eq!(remap_window_index(4, 2, 3), 4);
        // 뒤에서 앞으로 끌면 사이에 낀 방들이 한 칸씩 뒤로 밀린다.
        assert_eq!(remap_window_index(3, 3, 1), 1);
        assert_eq!(remap_window_index(1, 3, 1), 2);
        assert_eq!(remap_window_index(2, 3, 1), 3);
        // remap 이 말하는 자리와 벡터를 실제로 옮긴 결과가 어긋나면, 방 이름과
        // 알림이 남의 방에 붙는다 — 눈에 잘 안 띄는 종류라 전수로 못박는다.
        for n in 1..7usize {
            for from in 0..n {
                for dst in 0..n {
                    let mut moved: Vec<usize> = (0..n).collect();
                    let grabbed = moved.remove(from);
                    moved.insert(dst, grabbed);
                    for i in 0..n {
                        assert_eq!(
                            moved[remap_window_index(i, from, dst)],
                            i,
                            "n={n} from={from} dst={dst} i={i}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn surface_focus_selects_the_requested_tab_but_keeps_the_outer_leaf_active() {
        let mut ws = Workspace::default();
        ws.active_pane = Some("%outer".to_string());
        ws.panes.insert(
            "%outer".to_string(),
            PaneState {
                tabs: vec![
                    PaneTab {
                        pid: Some("%outer".to_string()),
                        ..Default::default()
                    },
                    PaneTab {
                        pid: Some("%second".to_string()),
                        ..Default::default()
                    },
                ],
                active_tab: 0,
                ..Default::default()
            },
        );
        ws.rebuild_pid_map();

        assert_eq!(apply_workspace_focus(&mut ws, "%second", true), Some(true));
        assert_eq!(ws.active_pane.as_deref(), Some("%outer"));
        assert_eq!(ws.panes["%outer"].active_tab, 1);

        assert_eq!(apply_workspace_focus(&mut ws, "%outer", false), Some(false));
        assert_eq!(ws.panes["%outer"].active_tab, 1, "pane 클릭은 보던 탭 유지");
        assert_eq!(apply_workspace_focus(&mut ws, "%outer", true), Some(true));
        assert_eq!(ws.panes["%outer"].active_tab, 0, "surface.focus는 첫 탭 선택");
    }

    #[test]
    fn backend_focus_queue_preserves_a_to_b_to_a_order() {
        let (tx, rx) = std::sync::mpsc::channel();
        for pane in ["%a", "%b", "%a"] {
            forward_backend_focus(pane.to_string(), |id| tx.send(id).unwrap());
        }
        drop(tx);
        assert_eq!(rx.into_iter().collect::<Vec<_>>(), ["%a", "%b", "%a"]);
    }
}
