//! 키/마우스/휠 입력 + 클립보드 + claude 상태 글리프/타이틀.
use super::*;

impl App {
    pub(crate) fn send_bytes(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        // Route to whichever backend owns the *active tab*. In-pane tabs
        // (`spawn_new_tab`) are always GUI-local PtySessions in `self.pty`,
        // even when the GUI is daemon-attached: the daemon owns only the
        // primary tab (pid == outer id). So a GUI-local hit must win over the
        // daemon path — otherwise keystrokes for a secondary tab get sent to
        // the daemon, which has no such surface and routes them to the primary
        // tab instead (the "typing in another tab lands in the first tab" bug).
        let surface = self.target_surface();
        if let Some(pid) = surface.as_deref() {
            if let Some(pty) = self.pty.get(pid) {
                let _ = pty.send_bytes(bytes);
                return;
            }
        }
        // Dispatch by which backend is wired up. The hex encoding is
        // a tmux send-keys quirk (the daemon decodes hex pairs back
        // to bytes itself); for the pty backend we hand the raw bytes
        // straight to the PTY writer.
        let _ = &surface;
        if let Some(tmux) = self.tmux.as_ref() {
            let hex: String = bytes
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            let target = self.target_pane();
            let _ = tmux.send_keys_hex(target.as_deref(), &hex);
        } else if let Some(pty) = self.active_pty() {
            let _ = pty.send_bytes(bytes);
        }
    }
    /// True when the pane has mouse reporting + SGR encoding enabled
    /// (claude code / vim / less in alt-screen). Shift-held overrides
    /// to false so the user has an iTerm-style escape hatch back to
    /// our own selection logic.
    pub(crate) fn pane_takes_mouse(&self, pane_id: &str) -> bool {
        if self.modifiers.shift_key() {
            return false;
        }
        let ws = self.ws.lock().unwrap();
        ws.panes
            .get(pane_id)
            .and_then(|p| p.term())
            .map(|t| t.mouse_enabled && t.mouse_sgr)
            .unwrap_or(false)
    }
    /// True if the pane shows a terminal (vs a markdown / image document
    /// view). Document panes are scrolled with the wheel, not dragged —
    /// terminal cell text-selection must not start on them. Unknown pane
    /// (e.g. a leaf that hasn't produced a ScreenUpdate yet) defaults to
    /// terminal so the normal split flow is never blocked.
    pub(crate) fn pane_is_terminal(&self, pane_id: &str) -> bool {
        let ws = self.ws.lock().unwrap();
        ws.panes
            .get(pane_id)
            .map(|p| matches!(p.content, PaneContent::Terminal(_)))
            .unwrap_or(true)
    }
    /// True if the pane's active tab is an image view — a drag here pans the
    /// zoomed image instead of selecting text / following a markdown link.
    pub(crate) fn pane_is_image(&self, pane_id: &str) -> bool {
        let ws = self.ws.lock().unwrap();
        ws.panes
            .get(pane_id)
            .map(|p| matches!(p.content, PaneContent::Image(_)))
            .unwrap_or(false)
    }
    /// Max pan offset (logical px, per axis) for the image pane's current
    /// zoom/rotation against its last-known body box — the slack between the
    /// zoomed image and the box. Clamping the stored pan to this keeps a drag
    /// past the edge from building dead-zone slack. Returns (0,0) when the
    /// image fits (no room to pan) or the box/image is unknown.
    pub(crate) fn image_pan_bounds(&self, pane_id: &str) -> (f32, f32) {
        // Body box ≈ the pane's leaf cell span in logical px. Close enough for
        // clamping the stored pan; `queue_image` does the pixel-exact crop.
        let (cols, rows) = self.window_cells();
        let Some((bw, bh)) = self
            .effective_leaf_rects(cols, rows)
            .into_iter()
            .find(|(id, _, _, _, _)| id == pane_id)
            .map(|(_, _, _, cw, ch)| (cw as f32 * self.cell.w, ch as f32 * self.cell.h))
        else {
            return (0.0, 0.0);
        };
        let ws = self.ws.lock().unwrap();
        let Some(pane) = ws.panes.get(pane_id) else { return (0.0, 0.0) };
        let Some(img) = pane.image() else { return (0.0, 0.0) };
        // Rotation by an odd quarter swaps the texture's width/height.
        let (iw, ih) = if pane.image_rot % 2 == 1 {
            (img.h as f32, img.w as f32)
        } else {
            (img.w as f32, img.h as f32)
        };
        if iw <= 0.0 || ih <= 0.0 || bw <= 0.0 || bh <= 0.0 {
            return (0.0, 0.0);
        }
        let fit = (bw / iw).min(bh / ih).min(1.0);
        let z = pane.image_view_zoom().max(1.0);
        let raw_w = iw * fit * z;
        let raw_h = ih * fit * z;
        (
            ((raw_w - bw) * 0.5).max(0.0),
            ((raw_h - bh) * 0.5).max(0.0),
        )
    }
    /// 이미지 pane 의 화면 박스(원점+크기, logical px). `image_pan_bounds` 와 같은
    /// `effective_leaf_rects` 기반 — 커서기준 줌의 pane 중심 계산에 쓴다.
    pub(crate) fn image_pane_box(&self, pane_id: &str) -> Option<(f32, f32, f32, f32)> {
        let (cols, rows) = self.window_cells();
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        self.effective_leaf_rects(cols, rows)
            .into_iter()
            .find(|(id, ..)| id == pane_id)
            .map(|(_, cx, cy, cw, ch)| {
                (
                    pad + cx as f32 * self.cell.w,
                    TITLE_HEIGHT + cy as f32 * self.cell.h,
                    cw as f32 * self.cell.w,
                    ch as f32 * self.cell.h,
                )
            })
    }
    /// 이미지 pane 줌을 `factor` 배(버튼·키보드·핀치·휠 공용). 1.0~8.0 클램프,
    /// fit(≤1.0) 복귀 시 pan 리셋, 아니면 새 zoom 의 pan 한계로 재클램프. `anchor`
    /// 가 Some 이면 그 화면 좌표 아래 이미지 점이 고정되도록 pan 을 보정한다(커서
    /// 기준 줌). 대상이 이미지 pane 이 아니거나 변화가 없으면 false.
    pub(crate) fn image_zoom_by(
        &mut self,
        pane_id: &str,
        factor: f32,
        anchor: Option<(f32, f32)>,
    ) -> bool {
        let (z0, px0, py0) = {
            let ws = self.ws.lock().unwrap();
            match ws.panes.get(pane_id) {
                Some(p) if p.image().is_some() => {
                    (p.image_zoom.max(1.0), p.image_pan_x, p.image_pan_y)
                }
                _ => return false,
            }
        };
        let z1 = (z0 * factor).clamp(1.0, 8.0);
        if (z1 - z0).abs() < 1e-4 {
            return false;
        }
        let ratio = z1 / z0;
        // pan 기본은 비례 스케일. anchor 있으면 커서 밑 이미지 점을 고정:
        // 화면점 o(=pane 중심 대비 오프셋)에 대해 pan1 = o*(1-ratio) + pan0*ratio.
        let (mut px1, mut py1) = (px0 * ratio, py0 * ratio);
        if let Some((ax, ay)) = anchor {
            if let Some((bx, by, bw, bh)) = self.image_pane_box(pane_id) {
                let ox = ax - (bx + bw * 0.5);
                let oy = ay - (by + bh * 0.5);
                px1 = ox * (1.0 - ratio) + px0 * ratio;
                py1 = oy * (1.0 - ratio) + py0 * ratio;
            }
        }
        // zoom 을 먼저 반영해야 image_pan_bounds 가 z1 기준으로 한계를 낸다.
        if let Ok(mut ws) = self.ws.lock() {
            if let Some(p) = ws.panes.get_mut(pane_id) {
                p.image_zoom = z1;
                p.dirty = true;
            }
        }
        if z1 <= 1.0 {
            px1 = 0.0;
            py1 = 0.0;
        } else {
            let (mx, my) = self.image_pan_bounds(pane_id);
            px1 = px1.clamp(-mx, mx);
            py1 = py1.clamp(-my, my);
        }
        if let Ok(mut ws) = self.ws.lock() {
            if let Some(p) = ws.panes.get_mut(pane_id) {
                p.image_pan_x = px1;
                p.image_pan_y = py1;
            }
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        true
    }
    /// 트랙패드 핀치(magnification) → 커서 아래 이미지 pane 을 커서기준 줌. `delta`
    /// 는 winit 의 배율 증분(누적 아님, 양수=확대). 이미지 pane 이 아니면 no-op.
    pub(crate) fn handle_pinch(&mut self, delta: f64) {
        let pid = self
            .px_to_pane_cell(self.cursor_px.0, self.cursor_px.1)
            .map(|(id, _, _)| id)
            .or_else(|| self.target_pane());
        if let Some(pid) = pid {
            self.image_zoom_by(&pid, (1.0 + delta as f32).max(0.1), Some(self.cursor_px));
        }
    }
    /// Arm an image-pane pan drag from the current cursor, snapshotting the
    /// pane's current pan so CursorMoved can apply `base + delta`.
    pub(crate) fn begin_image_pan(&mut self, pane_id: &str) {
        let base = self
            .ws
            .lock()
            .unwrap()
            .panes
            .get(pane_id)
            .map(|p| (p.image_pan_x, p.image_pan_y))
            .unwrap_or((0.0, 0.0));
        self.image_pan_drag = Some((pane_id.to_string(), self.cursor_px, base));
    }
    /// Encode an SGR mouse event and ship it to the pane. `button` is
    /// the SGR button code (0 = left press/motion/release, +32 for
    /// motion-with-button-held). `press` toggles the final byte
    /// between `M` (press / motion) and `m` (release).
    pub(crate) fn send_mouse_sgr(&self, pane_id: &str, button: u8, col: u16, row: u16, press: bool) {
        let final_byte = if press { 'M' } else { 'm' };
        let payload = format!("\x1b[<{button};{};{}{final_byte}", col + 1, row + 1);
        if let Some(tmux) = self.tmux.as_ref() {
            let hex: String = payload
                .as_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            let _ = tmux.send_keys_hex(Some(pane_id), &hex);
        } else if let Some(pty) = self.pty_for_pane(pane_id) {
            let _ = pty.send_bytes(payload.as_bytes());
        }
    }
    /// Resolve the user-visible label for a pane: OSC 0/2 title set
    /// by the shell or a TUI → foreground process comm → cwd. Used by
    /// both the per-pane header strip and the single-pane native
    /// window title so the two stay consistent.
    /// Scan the bottom of the active pane's grid for a Braille
    /// spinner glyph (U+2800..U+28FF). Tools like Claude Code, oh-my-
    /// zsh's pure-prompt, npm, etc. paint these one cell at a time
    /// to animate progress — picking the glyph straight from the
    /// grid lets us mirror their phase in the window title without
    /// any extra timing math. Returns None when no spinner is
    /// currently visible.
    /// Pull Claude Code's progress line ("✻ Brewed for 5s",
    /// "✶ Thinking…", etc.) straight out of the cell grid. We scan
    /// the bottom of the active pane for a row that starts with a
    /// star/asterisk glyph and trim that row to its text. The
    /// rendered grid is the only signal Claude Code gives us — it
    /// doesn't push these as OSC titles — so this is how we mirror
    /// the live status into the macOS titlebar.
    #[allow(dead_code)]
    pub(crate) fn active_claude_status(&self) -> Option<String> {
        let ws = self.ws.lock().unwrap();
        let id = ws.active_pane.as_deref()?;
        let pane = ws.panes.get(id)?;
        let t = pane.term()?;
        let rows = t.cells.len();
        let start = rows.saturating_sub(10);
        for row in t.cells[start..].iter() {
            let mut text = String::new();
            let mut has_marker = false;
            for cell in row {
                if cell.ch == '\0' {
                    text.push(' ');
                } else {
                    text.push(cell.ch);
                    let cp = cell.ch as u32;
                    if (0x2731..=0x274F).contains(&cp) {
                        has_marker = true;
                    }
                }
            }
            if has_marker {
                let trimmed = text.trim();
                if trimmed.len() > 4 && trimmed.len() < 80 {
                    return Some(trimmed.to_string());
                }
            }
        }
        None
    }
    #[allow(dead_code)]
    pub(crate) fn active_spinner_glyph(&self) -> Option<char> {
        let ws = self.ws.lock().unwrap();
        let id = ws.active_pane.as_deref()?;
        let pane = ws.panes.get(id)?;
        let t = pane.term()?;
        let rows = t.cells.len();
        let start = rows.saturating_sub(8);
        for row in &t.cells[start..] {
            for cell in row {
                let c = cell.ch;
                let cp = c as u32;
                // Braille spinners (npm, pure-prompt, etc.) +
                // Dingbats asterisks/stars (Claude Code uses
                // ✻/✶/✷/✸/✹/✺ as its "thinking" indicator).
                if (0x2800..=0x28FF).contains(&cp) || (0x2731..=0x274F).contains(&cp) {
                    return Some(c);
                }
            }
        }
        None
    }
    /// Refresh per-pane busy state by scanning each pane's grid for Claude
    /// Code's "working" marker, then drive the header working bar + completion
    /// toast off it. This is the local replacement for the removed daemon's
    /// transcript watcher: Claude doesn't push status over OSC, so the rendered
    /// grid is the only live signal. A `working → idle` flip fires the
    /// top-right completion toast + a header pulse, same as the old daemon path.
    /// Throttled (the scan walks every pane), so safe to call each frame.
    /// The claude spinner blanks/scrolls between frames, so the raw glyph scan
    /// flickers working↔idle — hold `busy` this long past the last sighting.
    /// Shared by the busy loop and the approval-prompt router below.
    const BUSY_GRACE: std::time::Duration = std::time::Duration::from_millis(1200);

    pub(crate) fn refresh_pane_activity(&mut self) {
        let now = Instant::now();
        if let Some(t) = self.pane_busy_check {
            if now.duration_since(t).as_millis() < 300 {
                return;
            }
        }
        self.pane_busy_check = Some(now);

        // Scan under the lock, then mutate `pane_activity` after dropping it —
        // the completion-toast path takes no further workspace lock. The same
        // pass also looks for a pending approval prompt (munder BLOCK_HINTS):
        // only meaningful when the spinner is gone, so busy panes skip it.
        let busy_now: Vec<(String, bool, Option<ApprovalPrompt>)> = {
            let ws = self.ws.lock().unwrap();
            ws.panes
                .iter()
                .map(|(id, pane)| match pane.term() {
                    Some(t) => {
                        let busy = term_is_working(t);
                        let prompt = if busy {
                            None
                        } else {
                            rows_show_approval_prompt(&t.cells)
                        };
                        (id.clone(), busy, prompt)
                    }
                    None => (id.clone(), false, None),
                })
                .collect()
        };

        // The claude spinner blanks/scrolls between frames, so the raw glyph
        // scan flickers working↔idle. Hold `busy` for BUSY_GRACE past the last
        // spinner sighting; only a real stop (grace elapsed with no spinner)
        // counts as completion — otherwise every blink fired a bogus toast.
        let mut completed: Vec<String> = Vec::new();
        for (id, raw_busy, _) in &busy_now {
            if *raw_busy {
                self.pane_last_busy.insert(id.clone(), now);
            }
            let busy = *raw_busy
                || self
                    .pane_last_busy
                    .get(id)
                    .map_or(false, |t| now.duration_since(*t) < Self::BUSY_GRACE);
            // Only a real "working" run counts toward the completion toast —
            // a pane leaving `blocked`/`waiting` (prompt answered) didn't
            // finish anything, it just got unstuck.
            let was_busy = self
                .pane_activity
                .get(id)
                .map_or(false, |a| a.status == "working");
            if was_busy && !busy {
                completed.push(id.clone());
                self.pane_last_busy.remove(id);
            }
            let status = if busy { "working" } else { "idle" };
            self.pane_activity
                .entry(id.clone())
                .and_modify(|a| a.status = status.to_string())
                .or_insert_with(|| crate::stream::PaneStatusView {
                    status: status.to_string(),
                    ..Default::default()
                });
        }
        // Drop entries for panes that no longer exist (closed/undocked).
        self.pane_activity
            .retain(|k, _| busy_now.iter().any(|(id, _, _)| id == k));
        self.pane_last_busy
            .retain(|k, _| busy_now.iter().any(|(id, _, _)| id == k));
        self.route_approval_prompts(&busy_now, now);

        if completed.is_empty() {
            return;
        }
        // A sibling finished: top-right toast + header pulse. Label the toast
        // with the pane's tab-header name (custom title / foreground process),
        // matching what the user reads in the tab strip — not the cwd basename.
        let name = completed.first().map(|id| self.pane_header_label(id));
        let msg = match name {
            Some(name) => format!("✓ {name} 작업 완료"),
            None => "✓ 작업 완료".to_string(),
        };
        // A sticky approval toast (a pane waiting on the user) outranks a sibling
        // completion blip — don't swap its text out from under the chips.
        if self.collab.toast_action.is_none() {
            self.collab.toast = Some((msg, now));
        }
        for id in completed {
            self.notify_flash.insert(id, now);
        }
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// munder식 승인 프롬프트 라우팅: 오케스트레이터(또는 협업방 없는 단독 pane)의 프롬프트만
    /// 사용자를 부르고 — sticky 토스트 + 승인/거부 칩 + 데스크탑 알림 — 워커의
    /// 프롬프트는 board `waiting` 으로만 흘려 오케스트레이터가 처리하게 둔다. 프롬프트가
    /// 사라지거나(답함) pane 이 다시 일하면 플래그·토스트를 걷는다.
    fn route_approval_prompts(
        &mut self,
        scan: &[(String, bool, Option<ApprovalPrompt>)],
        now: Instant,
    ) {
        let mut changed = false;
        for (id, raw_busy, prompt) in scan {
            let busy = *raw_busy
                || self
                    .pane_last_busy
                    .get(id)
                    .map_or(false, |t| now.duration_since(*t) < Self::BUSY_GRACE);
            let flagged = self.pane_prompt_wait.contains_key(id);
            if !busy && prompt.is_some() {
                if !flagged {
                    // 솔로(자동통솔 폐기 06-18) — 모든 pane 이 사용자 직행.
                    let faces_user = true;
                    self.pane_prompt_wait.insert(id.clone(), faces_user);
                    self.notify_flash.insert(id.clone(), now);
                    // board 에 waiting 으로 노출 — 오케스트레이터가 board 로 본다.
                    self.collab.attention
                        .lock()
                        .unwrap()
                        .insert(id.clone(), "승인 대기 (화면 감지)".to_string());
                    if faces_user {
                        let name = self.pane_header_label(id);
                        self.collab.toast = Some((format!("⚠ {name} 승인 대기"), now));
                        self.collab.toast_action = Some(id.clone());
                        self.collab.toast_rect = None;
                        let looking = self.window_focused
                            && self.ws.lock().unwrap().active_pane.as_deref()
                                == Some(id.as_str());
                        if !looking {
                            crate::chrome::notify_desktop("⚠ 승인 필요", &name);
                        }
                    }
                    changed = true;
                }
                let st = if self.pane_prompt_wait.get(id).copied().unwrap_or(false) {
                    "blocked"
                } else {
                    "waiting"
                };
                if let Some(a) = self.pane_activity.get_mut(id) {
                    if a.status != st {
                        a.status = st.to_string();
                        changed = true;
                    }
                }
            } else if flagged {
                self.pane_prompt_wait.remove(id);
                self.collab.attention.lock().unwrap().remove(id);
                if self.collab.toast_action.as_deref() == Some(id.as_str()) {
                    self.clear_approval_toast();
                }
                changed = true;
            }
        }
        self.pane_prompt_wait
            .retain(|k, _| scan.iter().any(|(id, _, _)| id == k));
        if changed {
            self.chrome_dirty = true;
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
    }

    /// Drop the sticky approval toast and its chip hit-rects in one place —
    /// called when the prompt resolves, a chip is clicked, or it's dismissed.
    pub(crate) fn clear_approval_toast(&mut self) {
        self.collab.toast = None;
        self.collab.toast_action = None;
        self.collab.toast_rect = None;
        self.collab.toast_approve_rect = None;
        self.collab.toast_deny_rect = None;
    }

    /// 승인 토스트 칩 클릭 → 대상 pane PTY 에 응답 키 직주입 (forward_key 의
    /// IME/모디파이어 경로를 타지 않는다). 클릭 시점에 그리드를 재스캔해 종류를
    /// 확정한다 — 메뉴에 'n' 을 보내면 글자는 무시되고 \r 만 남아 Yes 를 골라버리는
    /// 오발이 있어서, munder처럼 y\r 맹발사하지 않는다.
    pub(crate) fn respond_approval(&mut self, pane_id: &str, approve: bool) {
        let kind = {
            let ws = self.ws.lock().unwrap();
            ws.panes
                .get(pane_id)
                .and_then(|p| p.term())
                .and_then(|t| rows_show_approval_prompt(&t.cells))
        };
        let bytes: &[u8] = match (kind, approve) {
            (Some(ApprovalPrompt::YesNo), true) => b"y\r",
            (Some(ApprovalPrompt::YesNo), false) => b"n\r",
            // 메뉴(또는 프롬프트가 막 사라진 경우): Enter=하이라이트(기본 Yes) / Esc=거부.
            (_, true) => b"\r",
            (_, false) => b"\x1b",
        };
        if let Some(pty) = self.pty_for_pane(pane_id) {
            let _ = pty.send_bytes(bytes);
        }
    }

    /// Sync the macOS window title (Dock label, ⌘-Tab switcher) with
    /// the active pane's resolved label when only one pane is open.
    /// Skipped for multi-pane workspaces — there the per-pane header
    /// strip carries the same information and the OS title gets
    /// noisy as the user shuffles focus between splits.
    pub(crate) fn maybe_update_window_title(&mut self) {
        // Throttle: scroll bursts can fire RedrawRequested 60+ times
        // per second, and every call here takes a workspace lock and
        // may shell out to `ps` for the process name. 200ms is fast
        // enough for "title follows focus" but cheap enough that a
        // wheel sweep stays smooth.
        let now = Instant::now();
        if let Some(t) = self.last_window_title_check {
            if now.duration_since(t).as_millis() < 200 {
                return;
            }
        }
        self.last_window_title_check = Some(now);
        // Native window title always tracks the focused pane. In a
        // split workspace this means macOS's Dock / ⌘-Tab label
        // updates when the user clicks a different split — matching
        // iTerm / Terminal.app.
        let active = {
            let ws = self.ws.lock().unwrap();
            let id = ws
                .active_pane
                .clone()
                .or_else(|| ws.panes.keys().next().cloned());
            let osc = id.as_ref().and_then(|i| ws.panes.get(i)).and_then(|p| p.title.clone());
            id.map(|i| (i, osc))
        };
        let Some((id, osc)) = active else { return };
        let _ = osc;
        let label = self
            .pty
            .get(&id)
            .and_then(|p| p.shell_pid())
            .and_then(socket::pid_cwd)
            .map(|p| {
                let s = p.to_string_lossy().into_owned();
                match std::env::var("HOME").ok() {
                    Some(home) if s.starts_with(&home) => s.replacen(&home, "~", 1),
                    _ => s,
                }
            })
            .unwrap_or_else(|| Self::resolve_pane_label(&self.pty, &id, None));
        // Claude Code response indicator. Priority:
        //   1. Lift Claude Code's own status line straight from the
        //      cell grid ("✻ Brewed for 5s") so the user sees the
        //      same words they would on the prompt — including the
        //      live elapsed time.
        //   2. Fallback when only a spinner glyph is detected but no
        //      status text: cycle our own asterisk sequence
        //      ✶ ✳ ✶ · ✽ next to the "claude" label.
        // Note: we intentionally do NOT scrape the grid for the
        // "✻ Brewed for Ns" status line here. iTerm-style behavior is
        // to let the inner program drive the title via OSC 0/2 only
        // — Claude Code sends the conversation summary that way, and
        // the per-question status is meant to stay inside the pane.
        // Scraping the grid would clobber the conversation title
        // every few hundred ms with whatever Claude was rendering.
        if self.last_window_title.as_deref() == Some(&label) {
            return;
        }
        if let Some(w) = self.window.as_ref() {
            w.set_title(&label);
        }
        self.last_window_title = Some(label);
    }
    pub(crate) fn resolve_pane_label(
        pty: &HashMap<String, Arc<kasa_pty::PtySession>>,
        pane_id: &str,
        osc_title: Option<&str>,
    ) -> String {
        if let Some(t) = osc_title.filter(|s| !s.is_empty()) {
            return t.to_string();
        }
        if let Some(name) = pty.get(pane_id).and_then(|p| p.active_process_name()) {
            return decorate_process_name(&name);
        }
        std::env::current_dir()
            .ok()
            .map(|p| {
                let s = p.to_string_lossy().into_owned();
                match std::env::var("HOME").ok() {
                    Some(home) if s.starts_with(&home) => s.replacen(&home, "~", 1),
                    _ => s,
                }
            })
            .unwrap_or_else(|| "shell".to_string())
    }
    pub(crate) fn copy_selection(&self) {
        let Some(sel) = self.selection else { return; };
        let rows = {
            let ws = self.ws.lock().unwrap();
            match ws.active().and_then(|p| p.term()) {
                Some(t) => t.cells.clone(),
                None => return,
            }
        };
        let text = extract_selection(&rows, sel);
        if text.is_empty() {
            return;
        }
        match arboard::Clipboard::new() {
            Ok(mut cb) => {
                if let Err(e) = cb.set_text(text) {
                    eprintln!("[kasaterm] clipboard write failed: {e}");
                }
            }
            Err(e) => eprintln!("[kasaterm] clipboard open failed: {e}"),
        }
    }
    pub(crate) fn paste_clipboard(&self) {
        let mut cb = match arboard::Clipboard::new() {
            Ok(cb) => cb,
            Err(e) => {
                eprintln!("[kasaterm] clipboard open failed: {e}");
                return;
            }
        };
        // 텍스트가 있으면 무조건 텍스트 우선(bracketed paste). 일부 앱은 텍스트를
        // 복사해도 TIFF 표현을 같이 올려 get_image()가 Ok를 뱉는데, 이미지를 먼저
        // 검사하면 멀쩡한 텍스트 paste가 0x16으로 새버린다(거노: 붙여넣기 먹통).
        // 텍스트가 *없고* 이미지만 있을 때만 0x16을 흘려 claude code가 osascript로
        // 클립보드 PNG를 [Image] 칩으로 읽게 한다.
        if let Ok(text) = cb.get_text() {
            if !text.is_empty() {
                let mut payload = Vec::with_capacity(text.len() + 12);
                payload.extend_from_slice(b"\x1b[200~");
                payload.extend_from_slice(text.as_bytes());
                payload.extend_from_slice(b"\x1b[201~");
                self.send_bytes(&payload);
                return;
            }
        }
        if cb.get_image().is_ok() {
            self.send_bytes(&[0x16]);
            return;
        }
        eprintln!("[kasaterm] paste: clipboard has neither text nor image");
    }
    pub(crate) fn handle_wheel(&mut self, delta: MouseScrollDelta) {
        let wdbg = std::env::var_os("KASATERM_WHEEL_DEBUG").is_some();
        let dy_cells = match delta {
            // Mouse wheel: winit normalises one notch to y=±1.0. At 0.3 cells a
            // notch it took ~4 notches to move a single row (wheel_step only
            // emits once |accum| ≥ 1). 3 cells/notch matches the 3-line default
            // most GUI terminals use, so a notch scrolls immediately.
            MouseScrollDelta::LineDelta(_, y) => y * 3.0,
            // Trackpad: pixel-precise, already smooth — keep the gentle factor.
            MouseScrollDelta::PixelDelta(p) => (p.y as f32) / self.cell.h.max(1.0) * 0.3,
        };
        if wdbg {
            eprintln!(
                "[wheel] delta={delta:?} dy_cells={dy_cells:.4} accum_before={:.4} cursor_px=({:.1},{:.1})",
                self.wheel_accum_y, self.cursor_px.0, self.cursor_px.1
            );
        }
        // Image pane: 트랙패드 두손가락(PixelDelta)=pan, 마우스 휠(LineDelta)=
        // Preview 식 zoom. wheel_step 양자화 *전* raw delta 로 처리해야 pan 이 안
        // 끊긴다. 커서가 이미지 pane 위일 때만 가로채고, 아니면 아래로 흘려보낸다.
        let hover_image = self
            .px_to_pane_cell(self.cursor_px.0, self.cursor_px.1)
            .map(|(id, _, _)| id)
            .filter(|id| {
                self.ws
                    .lock()
                    .ok()
                    .and_then(|w| w.panes.get(id).map(|p| p.image().is_some()))
                    .unwrap_or(false)
            });
        if let Some(pid) = hover_image {
            match delta {
                MouseScrollDelta::PixelDelta(p) => {
                    // 두손가락 grab-pan — zoom>1 일 때만 의미(bounds 0 이면 no-op).
                    // 자연 스크롤 부호가 grab 과 반대라 빼서 이미지가 손가락을 따라온다.
                    let (mx, my) = self.image_pan_bounds(&pid);
                    if mx > 0.0 || my > 0.0 {
                        if let Ok(mut ws) = self.ws.lock() {
                            if let Some(pane) = ws.panes.get_mut(&pid) {
                                pane.image_pan_x =
                                    (pane.image_pan_x - p.x as f32).clamp(-mx, mx);
                                pane.image_pan_y =
                                    (pane.image_pan_y - p.y as f32).clamp(-my, my);
                                pane.dirty = true;
                            }
                        }
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                }
                MouseScrollDelta::LineDelta(_, y) => {
                    // 마우스 휠 = Preview 식 줌(위로 굴리면 확대), 커서 기준.
                    let factor = if y > 0.0 { 1.25 } else { 1.0 / 1.25 };
                    self.image_zoom_by(&pid, factor, Some(self.cursor_px));
                }
            }
            return;
        }
        let lines = match wheel_step(
            &mut self.wheel_accum_y,
            dy_cells,
            &mut self.last_wheel_emit,
            Instant::now(),
        ) {
            Some(l) => l,
            None => {
                if wdbg {
                    eprintln!(
                        "[wheel]   -> None (accum_after={:.4}, no emit)",
                        self.wheel_accum_y
                    );
                }
                return;
            }
        };
        // Open status-bar dropdown overlays everything, so the wheel scrolls
        // its list first when the pointer is inside it (a cwd with many subdirs
        // overflows the capped viewport). Whole-row steps to match the render.
        if let Some((mx, my, mw, mh)) = self.statusbar.menu_rect {
            let (cx, cy) = self.cursor_px;
            if cx >= mx && cx <= mx + mw && cy >= my && cy <= my + mh {
                let item_h = 24.0_f32;
                // lines>0 = wheel up = toward the top = less scroll.
                self.statusbar.menu_scroll =
                    (self.statusbar.menu_scroll - lines as f32 * item_h).max(0.0);
                self.chrome_dirty = true;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
                return;
            }
        }
        // File-tree column: the pointer is over the tree, not a terminal, so
        // scroll the rows instead of delegating to a pane (px_to_pane_cell
        // returns None here and would otherwise fall through to the active
        // pane). Clamp so it can't scroll above the top or past the last row.
        if self.file_tree.visible
            && self.cursor_px.1 > TITLE_HEIGHT
            && self.cursor_px.0 >= self.file_tree_col_x()
            && self.cursor_px.0 < self.file_tree_col_x() + self.file_tree.w_logical
        {
            let item_h = 26.0_f32;
            let win_h = self.window.as_ref().map_or(800.0, |w| {
                w.inner_size().height as f32 / self.effective_scale()
            });
            let start_y = TITLE_HEIGHT + 10.0;
            let content_h = self.file_tree.nodes.len() as f32 * item_h;
            let max_scroll = (content_h - (win_h - start_y).max(0.0)).max(0.0);
            // lines>0 = wheel up = toward the top = less scroll.
            let delta_px = lines as f32 * item_h;
            let next = (self.file_tree.scroll - delta_px).clamp(0.0, max_scroll);
            if (next - self.file_tree.scroll).abs() > 0.01 {
                self.file_tree.scroll = next;
                self.chrome_dirty = true;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            return;
        }
        // Git column: scroll the change list when the pointer is over it. Same
        // clamp idea as the file tree; the visible height is the band between
        // the header and the bottom button zone.
        if self.git.col_visible
            && self.cursor_px.1 > TITLE_HEIGHT
            && self.cursor_px.0 >= self.git_col_x()
        {
            let item_h = 22.0_f32;
            let n = self
                .git.col_data
                .lock()
                .map(|g| g.staged.len() + g.unstaged.len())
                .unwrap_or(0);
            let win_h = self.window.as_ref().map_or(800.0, |w| {
                w.inner_size().height as f32 / self.effective_scale()
            });
            let dock_h = if self.docked.is_empty() { 0.0 } else { DOCK_HEIGHT };
            // Header (branch + summary + rule) ≈ 68px; button zone ≈ 44px.
            let list_top = TITLE_HEIGHT + 68.0;
            let visible_h = (win_h - dock_h - list_top - 44.0).max(0.0);
            let content_h = n as f32 * item_h;
            let max_scroll = (content_h - visible_h).max(0.0);
            let delta_px = lines as f32 * item_h;
            let next = (self.git.col_scroll - delta_px).clamp(0.0, max_scroll);
            if (next - self.git.col_scroll).abs() > 0.01 {
                self.git.col_scroll = next;
                self.chrome_dirty = true;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            return;
        }
        // Decide which pane handles this wheel: the pane the pointer is
        // hovering over. Falls back to the active pane if the pointer
        // is in a gutter. Multi-pane lets the user scroll inside any
        // pane regardless of which one currently has keyboard focus.
        let target_pane_id = self
            .px_to_pane_cell(self.cursor_px.0, self.cursor_px.1)
            .map(|(id, _, _)| id)
            .or_else(|| self.target_pane());
        if wdbg {
            eprintln!(
                "[wheel]   lines={lines} target_pane={:?} active={:?}",
                target_pane_id,
                self.ws.lock().unwrap().active_pane
            );
        }
        // Markdown pane: scroll the laid-out document by pixels (it has no PTY
        // history to delegate to). Clamp to the content height the renderer
        // last published so it can't scroll past the end.
        let (is_md, is_raw) = {
            let ws = self.ws.lock().unwrap();
            target_pane_id
                .as_deref()
                .and_then(|id| ws.panes.get(id))
                .and_then(|p| p.markdown())
                .map_or((false, false), |m| (true, m.raw_mode))
        };
        if is_md {
            // Raw editor: a horizontal wheel/trackpad component pans long code
            // lines under the fixed gutter. Clamp to the longest line so it
            // can't scroll into empty space past the end of the text.
            if is_raw {
                let (dx_px, dy_cmp) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x * self.cell.w * 3.0, y),
                    MouseScrollDelta::PixelDelta(p) => (p.x as f32, p.y as f32),
                };
                // Trackpad vertical swipes carry a stray horizontal component
                // (e.g. x:36, y:-98). Pan only when the gesture is decisively
                // horizontal (dx > 2×dy) — otherwise a plain up/down scroll
                // yanks long code lines sideways and the short ones vanish.
                if dx_px.abs() > dy_cmp.abs() * 2.0 && dx_px.abs() > 1.0 {
                    if let Some(id) = target_pane_id.as_deref() {
                        if let Ok(mut ws) = self.ws.lock() {
                            if let Some(pane) = ws.panes.get_mut(id) {
                                let cw = self.cell.w;
                                if let Some(m) = pane.markdown_mut() {
                                    let longest = m
                                        .edit_lines
                                        .iter()
                                        .map(|l| l.chars().count())
                                        .max()
                                        .unwrap_or(0);
                                    let max_h = (longest as f32 * cw - cw * 4.0).max(0.0);
                                    m.h_scroll = (m.h_scroll - dx_px).clamp(0.0, max_h);
                                }
                                pane.dirty = true;
                            }
                        }
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                }
            }
            if let Some(id) = target_pane_id.as_deref() {
                let visible_h = self.window.as_ref().map_or(400.0, |w| {
                    w.inner_size().height as f32 / (w.scale_factor() as f32 * self.ui_zoom)
                }) - TITLE_HEIGHT
                    - PANE_HEADER_HEIGHT
                    - 2.0 * PANE_INNER_Y;
                let content_h = self.md_content_h.get(id).copied().unwrap_or(0.0);
                let max_scroll = (content_h - visible_h).max(0.0);
                // lines>0 = wheel up = toward the top of the doc = less scroll.
                let delta_px = lines as f32 * self.cell.h;
                if let Ok(mut ws) = self.ws.lock() {
                    if let Some(pane) = ws.panes.get_mut(id) {
                        pane.dirty = true;
                        if let Some(m) = pane.markdown_mut() {
                            let cur = m.scroll as f32;
                            let next = (cur - delta_px).clamp(0.0, max_scroll);
                            m.scroll = next.round() as usize;
                        }
                    }
                }
            }
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }
        let (alt, hist_len, mouse_on, mouse_sgr) = {
            let ws = self.ws.lock().unwrap();
            let pane = target_pane_id
                .as_deref()
                .and_then(|id| ws.panes.get(id));
            match pane.and_then(|p| p.term()) {
                Some(t) => (t.alt_screen, t.history.len(), t.mouse_enabled, t.mouse_sgr),
                None => return,
            }
        };
        if mouse_on && mouse_sgr {
            let (col, row) = self
                .px_to_cell_active(self.cursor_px.0, self.cursor_px.1)
                .unwrap_or((1, 1));
            let button = if lines > 0 { 64 } else { 65 };
            let count = lines.unsigned_abs().min(8) as usize;
            let single = format!("\x1b[<{button};{};{}M", col + 1, row + 1);
            let payload: Vec<u8> = single.as_bytes().repeat(count.max(1));
            // For the tmux backend we name the pane explicitly so an
            // inactive-but-hovered pane scrolls instead of the focused
            // one. The pty backend is single-pane: the pane id is
            // already implicit.
            if let Some(tmux) = self.tmux.as_ref() {
                if let Some(target) = target_pane_id.as_deref() {
                    let hex: String = payload
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    let _ = tmux.send_keys_hex(Some(target), &hex);
                }
            } else if let Some(id) = target_pane_id.as_deref() {
                if let Some(pty) = self.pty_for_pane(id) {
                    let _ = pty.send_bytes(&payload);
                }
            }
            return;
        }
        if alt {
            let esc: &[u8] = if lines > 0 { b"\x1b[5~" } else { b"\x1b[6~" };
            if let Some(tmux) = self.tmux.as_ref() {
                if let Some(target) = target_pane_id.as_deref() {
                    let hex: String = esc
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    let _ = tmux.send_keys_hex(Some(target), &hex);
                }
            } else if let Some(id) = target_pane_id.as_deref() {
                if let Some(pty) = self.pty_for_pane(id) {
                    let _ = pty.send_bytes(esc);
                }
            }
            return;
        }
        // Normal screen scrollback. PTY backend delegates to
        // alacritty's own scrollback (display_offset) — it tracks
        // scroll-region TUIs (claude code's pinned input) correctly,
        // unlike the old frame-diff shift heuristic. tmux backend
        // keeps the local history composition.
        let step = lines.unsigned_abs().min(8) as i32;
        let _ = hist_len;
        if let Some(id) = target_pane_id.as_deref() {
            if self.tmux.is_some() {
                if let Ok(mut ws) = self.ws.lock() {
                    if let Some(pane) = ws.panes.get_mut(id) {
                        pane.dirty = true;
                        if let Some(t) = pane.term_mut() {
                            let s = step as usize;
                            if lines > 0 {
                                t.scroll_offset = (t.scroll_offset + s).min(hist_len);
                            } else {
                                t.scroll_offset = t.scroll_offset.saturating_sub(s);
                            }
                        }
                    }
                }
            } else if let Some(pty) = self.pty_for_pane(id) {
                // Positive `lines` = scroll up = toward older history.
                let off = pty.scroll(if lines > 0 { step } else { -step });
                if wdbg {
                    eprintln!("[wheel]   pty.scroll step={step} -> display_offset={off}");
                }
            } else if wdbg {
                eprintln!("[wheel]   no pty_for_pane({id}) -> NO-OP");
            }
        } else if wdbg {
            eprintln!("[wheel]   target_pane_id=None -> NO-OP");
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// Insert text at the commit-message cursor (committed Hangul or a typed
    /// char). Char-indexed; advances the cursor by the inserted char count.
    pub(crate) fn git_commit_insert(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let col = self.git.commit_cursor.min(self.git.commit_msg.chars().count());
        let b = char_byte(&self.git.commit_msg, col);
        self.git.commit_msg.insert_str(b, text);
        self.git.commit_cursor = col + text.chars().count();
    }
    /// Commit-input key entry with Hangul composition, mirroring
    /// `md_editor_input` for the single-line git commit field. macOS hands jamo
    /// through `event.text`; feed the shared composer, insert committed
    /// syllables, keep the preedit in `self.preedit` for the overlay. Non-jamo
    /// flushes the pending syllable first, then edits.
    pub(crate) fn git_commit_input(&mut self, event: &KeyEvent) {
        use winit::keyboard::{Key, NamedKey};
        #[cfg(target_os = "macos")]
        if let Some(t) = &event.text {
            if t.chars().count() == 1 {
                if let Some(c) = t.chars().next() {
                    if (0x3130..=0x318F).contains(&(c as u32)) {
                        if let Some(commit) = self.hangul.feed(c) {
                            self.git_commit_insert(&commit);
                        }
                        self.preedit = self.hangul.preedit().unwrap_or_default();
                        self.in_preedit = !self.preedit.is_empty();
                        self.chrome_dirty = true;
                        return;
                    }
                }
            }
        }
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) && self.hangul.backspace() {
            self.preedit = self.hangul.preedit().unwrap_or_default();
            self.in_preedit = !self.preedit.is_empty();
            self.chrome_dirty = true;
            return;
        }
        if let Some(flushed) = self.hangul.flush() {
            self.git_commit_insert(&flushed);
        }
        self.preedit.clear();
        self.in_preedit = false;
        self.git_commit_key(event);
    }
    /// Single-line editing for the commit field: char insert, backspace/delete,
    /// left/right/home/end. Enter submits the commit, Escape blurs. Hangul is
    /// composed in `git_commit_input` before this runs.
    pub(crate) fn git_commit_key(&mut self, event: &KeyEvent) {
        use winit::keyboard::{Key, NamedKey};
        let len = self.git.commit_msg.chars().count();
        let mut col = self.git.commit_cursor.min(len);
        match &event.logical_key {
            Key::Named(NamedKey::Backspace) => {
                if col > 0 {
                    let b0 = char_byte(&self.git.commit_msg, col - 1);
                    let b1 = char_byte(&self.git.commit_msg, col);
                    self.git.commit_msg.replace_range(b0..b1, "");
                    col -= 1;
                }
            }
            Key::Named(NamedKey::Delete) => {
                if col < len {
                    let b0 = char_byte(&self.git.commit_msg, col);
                    let b1 = char_byte(&self.git.commit_msg, col + 1);
                    self.git.commit_msg.replace_range(b0..b1, "");
                }
            }
            Key::Named(NamedKey::ArrowLeft) => col = col.saturating_sub(1),
            Key::Named(NamedKey::ArrowRight) => {
                if col < len {
                    col += 1;
                }
            }
            Key::Named(NamedKey::Home) => col = 0,
            Key::Named(NamedKey::End) => col = len,
            Key::Named(NamedKey::Enter) => {
                self.git.commit_cursor = col;
                self.run_git_col_action(GitColBtn::Commit);
                return;
            }
            Key::Named(NamedKey::Escape) => {
                self.git.commit_focused = false;
                self.preedit.clear();
                self.in_preedit = false;
                let _ = self.hangul.flush();
                self.chrome_dirty = true;
                return;
            }
            Key::Named(NamedKey::Space) => {
                let b = char_byte(&self.git.commit_msg, col);
                self.git.commit_msg.insert(b, ' ');
                col += 1;
            }
            Key::Character(txt) => {
                let b = char_byte(&self.git.commit_msg, col);
                self.git.commit_msg.insert_str(b, txt);
                col += txt.chars().count();
            }
            _ => {}
        }
        self.git.commit_cursor = col;
        self.chrome_dirty = true;
    }
    /// Type-to-search for the open path dropdown. Append-only (no mid-string
    /// cursor — a search box doesn't need one), with the shared Hangul composer
    /// so Korean filters compose. Esc closes, Enter opens the first match,
    /// Backspace deletes a jamo then a char. Every edit resets the scroll.
    pub(crate) fn statusbar_menu_search_key(&mut self, event: &KeyEvent) {
        use winit::keyboard::{Key, NamedKey};
        #[cfg(target_os = "macos")]
        if let Some(t) = &event.text {
            if t.chars().count() == 1 {
                if let Some(c) = t.chars().next() {
                    if (0x3130..=0x318F).contains(&(c as u32)) {
                        if let Some(commit) = self.hangul.feed(c) {
                            self.statusbar.menu_search.push_str(&commit);
                        }
                        self.preedit = self.hangul.preedit().unwrap_or_default();
                        self.in_preedit = !self.preedit.is_empty();
                        self.statusbar.menu_scroll = 0.0;
                        self.chrome_dirty = true;
                        return;
                    }
                }
            }
        }
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.statusbar.menu = None;
                self.preedit.clear();
                self.in_preedit = false;
                let _ = self.hangul.flush();
                self.chrome_dirty = true;
                return;
            }
            Key::Named(NamedKey::Enter) => {
                let _ = self.hangul.flush();
                self.preedit.clear();
                self.in_preedit = false;
                self.statusbar_menu_activate_first();
                self.chrome_dirty = true;
                return;
            }
            Key::Named(NamedKey::Backspace) => {
                if self.hangul.backspace() {
                    self.preedit = self.hangul.preedit().unwrap_or_default();
                    self.in_preedit = !self.preedit.is_empty();
                } else {
                    self.statusbar.menu_search.pop();
                }
                self.statusbar.menu_scroll = 0.0;
                self.chrome_dirty = true;
                return;
            }
            _ => {}
        }
        if let Some(flushed) = self.hangul.flush() {
            self.statusbar.menu_search.push_str(&flushed);
        }
        self.preedit.clear();
        self.in_preedit = false;
        match &event.logical_key {
            Key::Named(NamedKey::Space) => self.statusbar.menu_search.push(' '),
            Key::Character(txt) => self.statusbar.menu_search.push_str(txt),
            _ => {}
        }
        self.statusbar.menu_scroll = 0.0;
        self.chrome_dirty = true;
    }
    /// Same append-only search entry for the file-tree column's search box.
    /// Esc closes the box, Backspace deletes a jamo then a char. The filtered
    /// node list is recomputed by `rebuild_file_tree_nodes` on each edit.
    pub(crate) fn file_tree_search_key(&mut self, event: &KeyEvent) {
        use winit::keyboard::{Key, NamedKey};
        #[cfg(target_os = "macos")]
        if let Some(t) = &event.text {
            if t.chars().count() == 1 {
                if let Some(c) = t.chars().next() {
                    if (0x3130..=0x318F).contains(&(c as u32)) {
                        if let Some(commit) = self.hangul.feed(c) {
                            self.file_tree.search_query.push_str(&commit);
                            self.file_tree_search_collect();
                        }
                        self.preedit = self.hangul.preedit().unwrap_or_default();
                        self.in_preedit = !self.preedit.is_empty();
                        self.file_tree.scroll = 0.0;
                        self.chrome_dirty = true;
                        return;
                    }
                }
            }
        }
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.file_tree.search_active = false;
                self.file_tree.search_query.clear();
                self.preedit.clear();
                self.in_preedit = false;
                let _ = self.hangul.flush();
                self.file_tree_search_collect(); // empty query → restore tree
                self.file_tree.scroll = 0.0;
                self.chrome_dirty = true;
                return;
            }
            Key::Named(NamedKey::Backspace) => {
                if self.hangul.backspace() {
                    self.preedit = self.hangul.preedit().unwrap_or_default();
                    self.in_preedit = !self.preedit.is_empty();
                } else {
                    self.file_tree.search_query.pop();
                }
                self.file_tree_search_collect();
                self.file_tree.scroll = 0.0;
                self.chrome_dirty = true;
                return;
            }
            _ => {}
        }
        if let Some(flushed) = self.hangul.flush() {
            self.file_tree.search_query.push_str(&flushed);
        }
        self.preedit.clear();
        self.in_preedit = false;
        match &event.logical_key {
            Key::Named(NamedKey::Space) => self.file_tree.search_query.push(' '),
            Key::Character(txt) => self.file_tree.search_query.push_str(txt),
            _ => {}
        }
        self.file_tree_search_collect();
        self.file_tree.scroll = 0.0;
        self.chrome_dirty = true;
    }
    /// Name entry for the inline new-file/folder row. Enter creates the entry,
    /// Esc cancels; Hangul composes like the search box.
    /// 인라인 입력행(새 파일/폴더 또는 이름변경) 한 키. 두 모드의 편집 버퍼는
    /// `ft_edit_buf` 로 통일 — rename 이 있으면 그쪽, 없으면 new. Enter 는 모드에
    /// 맞는 commit, Esc 는 둘 다 취소. 한글 조합은 search 행과 동일 경로.
    pub(crate) fn file_tree_new_key(&mut self, event: &KeyEvent) {
        use winit::keyboard::{Key, NamedKey};
        #[cfg(target_os = "macos")]
        if let Some(t) = &event.text {
            if t.chars().count() == 1 {
                if let Some(c) = t.chars().next() {
                    if (0x3130..=0x318F).contains(&(c as u32)) {
                        if let Some(commit) = self.hangul.feed(c) {
                            if let Some(buf) = self.ft_edit_buf() {
                                buf.push_str(&commit);
                            }
                        }
                        self.preedit = self.hangul.preedit().unwrap_or_default();
                        self.in_preedit = !self.preedit.is_empty();
                        self.chrome_dirty = true;
                        return;
                    }
                }
            }
        }
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.file_tree.new = None;
                self.file_tree.new_parent = None;
                self.file_tree.rename = None;
                self.preedit.clear();
                self.in_preedit = false;
                let _ = self.hangul.flush();
                self.chrome_dirty = true;
                return;
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(f) = self.hangul.flush() {
                    if let Some(buf) = self.ft_edit_buf() {
                        buf.push_str(&f);
                    }
                }
                self.preedit.clear();
                self.in_preedit = false;
                if self.file_tree.rename.is_some() {
                    self.commit_rename();
                } else {
                    self.commit_new_entry();
                }
                self.chrome_dirty = true;
                return;
            }
            Key::Named(NamedKey::Backspace) => {
                if self.hangul.backspace() {
                    self.preedit = self.hangul.preedit().unwrap_or_default();
                    self.in_preedit = !self.preedit.is_empty();
                } else if let Some(buf) = self.ft_edit_buf() {
                    buf.pop();
                }
                self.chrome_dirty = true;
                return;
            }
            _ => {}
        }
        if let Some(flushed) = self.hangul.flush() {
            if let Some(buf) = self.ft_edit_buf() {
                buf.push_str(&flushed);
            }
        }
        self.preedit.clear();
        self.in_preedit = false;
        match &event.logical_key {
            Key::Named(NamedKey::Space) => {
                if let Some(buf) = self.ft_edit_buf() {
                    buf.push(' ');
                }
            }
            Key::Character(txt) => {
                if let Some(buf) = self.ft_edit_buf() {
                    buf.push_str(txt);
                }
            }
            _ => {}
        }
        self.chrome_dirty = true;
    }
    /// 인라인 입력행의 편집 버퍼 — rename 우선, 없으면 new. 둘 다 마지막 필드가
    /// 편집 중 텍스트라 키 입력을 한 곳에서 받는다.
    fn ft_edit_buf(&mut self) -> Option<&mut String> {
        if let Some((_, b)) = self.file_tree.rename.as_mut() {
            return Some(b);
        }
        if let Some((_, b)) = self.file_tree.new.as_mut() {
            return Some(b);
        }
        None
    }
    pub(crate) fn forward_key(&mut self, event: &KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        // Touch the input timer so the cursor stays solid for a beat and
        // the blink phase re-starts from "on" once it kicks in.
        self.last_input_at = Instant::now();
        // Settings screen swallows all keys (never leaks to the PTY). A focused
        // text field consumes them; Esc with no field open closes the screen.
        if self.settings_open {
            use winit::keyboard::{Key, NamedKey};
            if self.settings_key(event) {
                return;
            }
            if matches!(&event.logical_key, Key::Named(NamedKey::Escape)) {
                self.close_settings();
            }
            return;
        }
        // Git commit field has focus: keystrokes edit the message, not the PTY.
        // (Click elsewhere blurs it — see the column's mouse handler.)
        if self.git.commit_focused {
            self.git_commit_input(event);
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }
        // Open path dropdown is a modal search: keystrokes filter it, not the
        // PTY. (Branch menu has no search — its lists are short — so it falls
        // through.)
        if self
            .statusbar.menu
            .as_ref()
            .map(|(_, k)| matches!(k, StatusbarMenu::Path))
            .unwrap_or(false)
        {
            self.statusbar_menu_search_key(event);
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }
        // Cmd+Delete (= Cmd+Backspace) on a selected tree row → trash it.
        // Gated on no text input having focus so it never eats shell editing
        // keys; the selection clears when a terminal pane is clicked.
        if !self.git.commit_focused
            && self.file_tree.new.is_none()
            && self.file_tree.rename.is_none()
            && !self.file_tree.search_active
        {
            let has_sel = self.file_tree.selected.is_some()
                || !self.file_tree.selected_more.is_empty();
            if has_sel {
                use winit::keyboard::{Key, NamedKey};
                let del = matches!(&event.logical_key, Key::Named(NamedKey::Delete))
                    || (self.modifiers.super_key()
                        && matches!(&event.logical_key, Key::Named(NamedKey::Backspace)));
                if std::env::var_os("KASATERM_KEY_DEBUG").is_some() {
                    eprintln!(
                        "[ftdel] has_sel={} del={} super={} key={:?}",
                        has_sel, del, self.modifiers.super_key(), event.logical_key
                    );
                }
                if del {
                    self.delete_tree_selection();
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }
            }
        }
        // Inline new-file/folder or rename row is open: keystrokes name it.
        if self.file_tree.new.is_some() || self.file_tree.rename.is_some() {
            self.file_tree_new_key(event);
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }
        // File-tree search box has focus: keystrokes filter the tree.
        if self.file_tree.search_active {
            self.file_tree_search_key(event);
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }
        // Past here the key is bound for the shell (or an image/raw pane), so
        // drop any tree selection — a later Cmd+Backspace then edits the shell
        // line, not the previously-clicked file.
        self.file_tree.selected = None;
        // Image panes have no PTY — repurpose keys for view control.
        //   +/=      zoom in     -    zoom out
        //   0        reset       r/R  rotate 90° CW
        // Every other key is swallowed (no shell to receive them).
        let is_image = {
            let ws = self.ws.lock().unwrap();
            ws.active().map(|p| p.image().is_some()).unwrap_or(false)
        };
        if is_image {
            let mut changed = false;
            // 키보드 줌은 active pane 중심(anchor 없음). id 를 먼저 떠서 헬퍼에 넘긴다.
            let active_id = self.ws.lock().ok().and_then(|w| w.active_pane.clone());
            if let Key::Character(s) = &event.logical_key {
                match s.as_str() {
                    "+" | "=" => {
                        if let Some(id) = &active_id {
                            changed = self.image_zoom_by(id, 1.25, None);
                        }
                    }
                    "-" | "_" => {
                        if let Some(id) = &active_id {
                            changed = self.image_zoom_by(id, 1.0 / 1.25, None);
                        }
                    }
                    "0" => {
                        if let Ok(mut ws) = self.ws.lock() {
                            if let Some(pane) = ws.active_mut() {
                                pane.image_zoom = 1.0;
                                pane.image_rot = 0;
                                pane.image_pan_x = 0.0;
                                pane.image_pan_y = 0.0;
                                changed = true;
                            }
                        }
                    }
                    "r" | "R" => {
                        if let Ok(mut ws) = self.ws.lock() {
                            if let Some(pane) = ws.active_mut() {
                                pane.image_rot = (pane.image_rot + 1) % 4;
                                pane.image_pan_x = 0.0;
                                pane.image_pan_y = 0.0;
                                changed = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
            if changed {
                self.chrome_dirty = true;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            return;
        }
        // Markdown panes have no PTY. In Raw mode keys edit the buffer; in
        // Render mode they're swallowed (scrolling is wheel-driven).
        let (is_md, is_raw) = {
            let ws = self.ws.lock().unwrap();
            ws.active().map_or((false, false), |p| match p.markdown() {
                Some(m) => (true, m.raw_mode),
                None => (false, false),
            })
        };
        if is_md {
            if is_raw {
                self.md_editor_input(event);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            return;
        }
        // Typing snaps the active pane back to live tail. Other panes'
        // scroll offsets are left alone — switching focus by clicking
        // doesn't disturb where the user was reading.
        if let Ok(mut ws) = self.ws.lock() {
            if let Some(t) = ws.active_mut().and_then(|p| p.term_mut()) {
                if t.scroll_offset != 0 {
                    t.scroll_offset = 0;
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
        }
        // Inline-autosuggestion accept. When a ghost suggestion is on
        // screen the cursor is necessarily at the end of the typed line
        // (we clear the suggestion on any left/up/down motion), so it's
        // safe to repurpose → / End / Ctrl-E to accept it and Alt-F to
        // accept one word — matching zsh-autosuggestions / fish. Tab is
        // deliberately left to the shell's own completion. We send the
        // remainder to the PTY and grow input_buf so the next frame keeps
        // suggesting from the extended prefix.
        if let Some(sugg) = self.current_suggestion.clone() {
            if !sugg.is_empty() {
                use winit::keyboard::{KeyCode, PhysicalKey};
                let plain = !self.modifiers.alt_key() && !self.modifiers.super_key();
                let phys = match event.physical_key {
                    PhysicalKey::Code(c) => Some(c),
                    _ => None,
                };
                let accept_full = (matches!(event.logical_key, Key::Named(NamedKey::ArrowRight))
                    && plain)
                    || (matches!(event.logical_key, Key::Named(NamedKey::End)) && plain)
                    || (self.modifiers.control_key() && phys == Some(KeyCode::KeyE));
                let accept_word =
                    self.modifiers.alt_key() && phys == Some(KeyCode::KeyF) && !sugg.is_empty();
                if accept_full {
                    self.send_bytes(sugg.as_bytes());
                    self.input_buf.push_str(&sugg);
                    self.current_suggestion = None;
                    return;
                }
                if accept_word {
                    // One word = leading spaces + the run up to the next
                    // space boundary, so repeated Alt-F walks the line.
                    let mut end = 0usize;
                    let bytes = sugg.as_bytes();
                    while end < bytes.len() && bytes[end] == b' ' {
                        end += 1;
                    }
                    while end < bytes.len() && bytes[end] != b' ' {
                        end += 1;
                    }
                    let word = &sugg[..end];
                    self.send_bytes(word.as_bytes());
                    self.input_buf.push_str(word);
                    self.current_suggestion = None;
                    return;
                }
            }
        }
        // Modifier-bearing keys must NEVER reach the Hangul composer.
        // In Korean keyboard layout the C key still produces 'ㅊ' as
        // text, but Ctrl+C is meant for SIGINT / "copy" and Cmd+V for
        // paste — we look at the *physical* key for these, not the
        // IME-resolved logical key. While we're here, also forward the
        // standard control-letter byte for any Ctrl+<letter> combo
        // (Ctrl+L clears, Ctrl+D = EOF, etc), so shells and TUI apps
        // behave as users expect regardless of which keyboard layout
        // happens to be active.
        let host = self.host_mod();
        let ctrl = self.modifiers.control_key();
        if host || ctrl {
            use winit::keyboard::{KeyCode, PhysicalKey};
            if let PhysicalKey::Code(code) = event.physical_key {
                // Host-modifier shortcuts. macOS uses Cmd, Windows/Linux
                // use Ctrl+Shift — see `host_mod()`.
                if host {
                    // OS 키 자동반복(키를 누르고 있을 때 반복 발사되는 Pressed
                    // 이벤트, repeat=true)은 무시한다. Cmd 단축키는 전부 단발성
                    // 동작이라, 안 거르면 Cmd+D를 살짝 길게 누르는 것만으로
                    // split이 우르르 나가 pane이 증식하고 Cmd+W는 여러 pane을
                    // 한꺼번에 닫는다. 글자 타이핑 반복은 이 블록 밖이라 무관.
                    if event.repeat {
                        return;
                    }
                    if code == KeyCode::KeyC && self.selection.is_some() {
                        self.copy_selection();
                        return;
                    }
                    if code == KeyCode::KeyV {
                        // Pasted text bypasses our key path, so we can't
                        // mirror it into input_buf — drop the suggestion
                        // prefix so we never suggest off a stale line.
                        self.input_buf.clear();
                        self.current_suggestion = None;
                        self.paste_clipboard();
                        return;
                    }
                    // Terminal.app-style split shortcuts. PTY mode only
                    // — tmux mode lets the daemon handle its own keys.
                    //   D       → horizontal (stacked, default)
                    //   Shift+D → vertical (side-by-side, macOS chord)
                    //   E       → vertical (Windows-friendly chord that
                    //              avoids the Shift-on-Shift conflict)
                    // On macOS host_mod_alt resolves to Shift so
                    // Cmd+Shift+D still flips to vertical. On
                    // Windows/Linux host_mod already owns Shift, so the
                    // dedicated KeyE binding is the practical one.
                    if code == KeyCode::KeyD {
                        let dir = if self.host_mod_alt() {
                            kasa_pty::SplitDir::Vertical
                        } else {
                            kasa_pty::SplitDir::Horizontal
                        };
                        if let Err(e) = self.split_active_pane(dir) {
                            eprintln!("[kasaterm] split failed: {e}");
                        }
                        return;
                    }
                    if code == KeyCode::KeyE {
                        if let Err(e) = self.split_active_pane(kasa_pty::SplitDir::Vertical) {
                            eprintln!("[kasaterm] split failed: {e}");
                        }
                        return;
                    }
                    // Close the focused tab (a multi-tab pane keeps its other
                    // tabs). Last-tab/last-pane close is left to the OS close
                    // button.
                    if code == KeyCode::KeyW {
                        self.close_active_tab();
                        return;
                    }
                    // Cmd+T → new window in the current session (PTY backend
                    // only; tmux owns its own windows). Cmd+1..9 switch to
                    // that window. Digit0 is font-reset above, so windows
                    // start at 1.
                    if code == KeyCode::KeyT && self.tmux.is_none() {
                        if self.modifiers.shift_key() {
                            // Cmd+Shift+T → reopen closed pane (ghostty). dock이
                            // 제거됐으므로 후속에서 claude --resume 식 로컬 복원으로
                            // 재구현 예정 — 현재는 no-op.
                        } else {
                            self.new_window();
                        }
                        return;
                    }
                    let win_digit = match code {
                        KeyCode::Digit1 | KeyCode::Numpad1 => Some(0),
                        KeyCode::Digit2 | KeyCode::Numpad2 => Some(1),
                        KeyCode::Digit3 | KeyCode::Numpad3 => Some(2),
                        KeyCode::Digit4 | KeyCode::Numpad4 => Some(3),
                        KeyCode::Digit5 | KeyCode::Numpad5 => Some(4),
                        KeyCode::Digit6 | KeyCode::Numpad6 => Some(5),
                        KeyCode::Digit7 | KeyCode::Numpad7 => Some(6),
                        KeyCode::Digit8 | KeyCode::Numpad8 => Some(7),
                        KeyCode::Digit9 | KeyCode::Numpad9 => Some(8),
                        _ => None,
                    };
                    if let Some(idx) = win_digit {
                        if self.tmux.is_none() {
                            self.switch_window(idx);
                            return;
                        }
                    }
                    // `[` / `]` cycle focus through panes in document
                    // order.
                    if code == KeyCode::BracketLeft {
                        self.cycle_focus(-1);
                        return;
                    }
                    if code == KeyCode::BracketRight {
                        self.cycle_focus(1);
                        return;
                    }
                    // Cmd+Option+Arrow → move focus to the spatially
                    // adjacent pane; add Shift to swap the two panes.
                    // iTerm uses the same chord. Gated on Option so plain
                    // Cmd+Arrow still reaches the shell (line start/end).
                    if self.modifiers.alt_key() {
                        let fdir = match code {
                            KeyCode::ArrowLeft => Some(FocusDir::Left),
                            KeyCode::ArrowRight => Some(FocusDir::Right),
                            KeyCode::ArrowUp => Some(FocusDir::Up),
                            KeyCode::ArrowDown => Some(FocusDir::Down),
                            _ => None,
                        };
                        if let Some(d) = fdir {
                            if self.modifiers.shift_key() {
                                self.swap_dir(d);
                            } else {
                                self.focus_dir(d);
                            }
                            return;
                        }
                    }
                }
                // Font zoom. macOS gates on Cmd (= host_mod); Windows/Linux
                // on plain Ctrl (Ctrl+= / Ctrl+- / Ctrl+0, matching Windows
                // Terminal, VS Code, and browsers). Ctrl+Shift+= also lands
                // here since `+` is Shift+`=`. macOS deliberately stays on
                // Cmd so plain Ctrl+letter still reaches the shell as a
                // control byte. Match BOTH the physical key (US layout
                // assumption) AND the logical key text — Korean / European
                // layouts may emit the same character from a different
                // physical position.
                let zoom_mod = if cfg!(target_os = "macos") { host } else { ctrl };
                if zoom_mod {
                    use winit::keyboard::Key;
                    let logical_str = match &event.logical_key {
                        Key::Character(s) => Some(s.as_str()),
                        _ => None,
                    };
                    let is_plus = code == KeyCode::Equal
                        || code == KeyCode::NumpadAdd
                        || logical_str == Some("=")
                        || logical_str == Some("+");
                    let is_minus = code == KeyCode::Minus
                        || code == KeyCode::NumpadSubtract
                        || logical_str == Some("-")
                        || logical_str == Some("_");
                    let is_zero = code == KeyCode::Digit0
                        || code == KeyCode::Numpad0
                        || logical_str == Some("0");
                    // host_mod_alt (Win: Alt, mac: Shift) narrows the zoom to
                    // just the focused pane; without it, the whole UI zooms.
                    let pane_only = self.host_mod_alt();
                    if is_plus {
                        if pane_only { self.change_pane_font(0.1); } else { self.change_ui_zoom(0.1); }
                        return;
                    }
                    if is_minus {
                        if pane_only { self.change_pane_font(-0.1); } else { self.change_ui_zoom(-0.1); }
                        return;
                    }
                    if is_zero {
                        if pane_only { self.reset_pane_font(); } else { self.reset_ui_zoom(); }
                        return;
                    }
                }
                // Ctrl+letter → the corresponding ASCII control byte.
                // This covers Ctrl+C → 0x03 (SIGINT), Ctrl+D → 0x04 (EOF),
                // Ctrl+L → 0x0c (clear), Ctrl+R → 0x12 (reverse search), etc.
                // Suppressed when host is engaged so Ctrl+Shift+letter
                // shortcuts on Windows/Linux don't double-fire as both a
                // shortcut and a control byte.
                if ctrl && !host {
                    let letter = match code {
                        KeyCode::KeyA => Some(b'\x01'),
                        KeyCode::KeyB => Some(b'\x02'),
                        KeyCode::KeyC => Some(b'\x03'),
                        KeyCode::KeyD => Some(b'\x04'),
                        KeyCode::KeyE => Some(b'\x05'),
                        KeyCode::KeyF => Some(b'\x06'),
                        KeyCode::KeyG => Some(b'\x07'),
                        KeyCode::KeyH => Some(b'\x08'),
                        KeyCode::KeyI => Some(b'\x09'),
                        KeyCode::KeyJ => Some(b'\x0a'),
                        KeyCode::KeyK => Some(b'\x0b'),
                        KeyCode::KeyL => Some(b'\x0c'),
                        KeyCode::KeyM => Some(b'\x0d'),
                        KeyCode::KeyN => Some(b'\x0e'),
                        KeyCode::KeyO => Some(b'\x0f'),
                        KeyCode::KeyP => Some(b'\x10'),
                        KeyCode::KeyQ => Some(b'\x11'),
                        KeyCode::KeyR => Some(b'\x12'),
                        KeyCode::KeyS => Some(b'\x13'),
                        KeyCode::KeyT => Some(b'\x14'),
                        KeyCode::KeyU => Some(b'\x15'),
                        KeyCode::KeyV => Some(b'\x16'),
                        KeyCode::KeyW => Some(b'\x17'),
                        KeyCode::KeyX => Some(b'\x18'),
                        KeyCode::KeyY => Some(b'\x19'),
                        KeyCode::KeyZ => Some(b'\x1a'),
                        _ => None,
                    };
                    if let Some(b) = letter {
                        // Flush any pending Hangul syllable before
                        // sending the control byte — typing Enter
                        // mid-syllable already does this; control
                        // letters should too.
                        if let Some(flushed) = self.hangul.flush() {
                            self.send_bytes(flushed.as_bytes());
                            self.preedit.clear();
                            self.in_preedit = false;
                        }
                        // Keep the autosuggest line buffer in sync with
                        // the control byte the shell is about to act on.
                        match b {
                            0x15 | 0x03 | 0x01 => self.input_buf.clear(), // Ctrl-U / Ctrl-C / Ctrl-A
                            0x17 => self.buf_pop_word(),                  // Ctrl-W
                            _ => {}
                        }
                        self.send_bytes(&[b]);
                        return;
                    }
                }
            }
        }
        // Backspace special: when the in-process Hangul composer is
        // mid-syllable, eat the backspace to chip a jamo off the
        // preedit rather than forwarding `\x7f` to the shell (which
        // would erase already-committed text instead).
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) {
            if self.hangul.backspace() {
                self.preedit = self.hangul.preedit().unwrap_or_default();
                self.in_preedit = !self.preedit.is_empty();
                self.chrome_dirty = true;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
                return;
            }
        }
        // Any non-character control flushes the composer first so a
        // pending 자모 doesn't get stranded when the user hits Enter /
        // arrow / escape mid-syllable.
        let is_control_key = matches!(
            event.logical_key,
            Key::Named(NamedKey::Enter)
                | Key::Named(NamedKey::Tab)
                | Key::Named(NamedKey::Escape)
                | Key::Named(NamedKey::ArrowUp)
                | Key::Named(NamedKey::ArrowDown)
                | Key::Named(NamedKey::ArrowLeft)
                | Key::Named(NamedKey::ArrowRight)
        );
        // Stash the committed syllable instead of writing it now, so it
        // ships in the SAME write as the key's own bytes below. Two
        // separate writes let an async TUI (claude code / Ink) submit
        // on the trailing \r before it has applied the multibyte
        // syllable — the last char gets dropped. One atomic write keeps
        // "녕" and "\r" in the same stdin chunk.
        let mut commit_prefix: Vec<u8> = Vec::new();
        if is_control_key {
            if let Some(flushed) = self.hangul.flush() {
                commit_prefix.extend_from_slice(flushed.as_bytes());
            }
            self.preedit.clear();
            self.in_preedit = false;
        }
        // Readline-style delete shortcuts. Defaults match iTerm2 /
        // Terminal.app on macOS and Windows Terminal on Windows:
        //   Option/Alt+Backspace → `\e\x7f`  (backward-kill-word)
        //   host_mod+Backspace   → `\x15`    (unix-line-discard, Ctrl+U)
        // host_mod resolves to Cmd on macOS, Ctrl+Shift on Windows/Linux.
        // We match physical key so the Korean layout's mapped char
        // ('ㅣ' etc.) doesn't interfere.
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) {
            if self.host_mod() {
                self.input_buf.clear();
                self.send_bytes(b"\x15");
                return;
            }
            // Ctrl+Backspace → backward-kill-word (Ctrl+W, 0x17). Windows'
            // standard word-erase chord; on macOS host_mod is Cmd so plain
            // Ctrl still lands here. Alt+Backspace does the same via `\e\x7f`.
            if self.modifiers.control_key() {
                self.buf_pop_word();
                self.send_bytes(b"\x17");
                return;
            }
            if self.modifiers.alt_key() {
                self.buf_pop_word();
                self.send_bytes(b"\x1b\x7f");
                return;
            }
        }
        let bytes: Vec<u8> = match &event.logical_key {
            // Shift+Enter / Option(Alt)+Enter insert a newline instead of
            // submitting. claude code reads a bare LF (0x0a, the byte
            // Ctrl+J sends) as a newline; plain Enter stays CR (0x0d),
            // which submits. We used to send ESC+CR here, but current
            // claude code / Ink doesn't treat that as a newline — so
            // multiline never engaged and the up-arrow fell through to
            // command history instead of moving between lines. claude
            // never negotiates the kitty keyboard protocol (no `CSI ? u`
            // in its startup modes), so CSI 13;2u wouldn't reach it
            // either; a raw LF is the portable answer.
            Key::Named(NamedKey::Enter) => {
                if self.modifiers.shift_key() || self.modifiers.alt_key() {
                    b"\n".to_vec()
                } else {
                    // Plain Enter submits the line: remember it for
                    // instant suggestions and reset the buffer.
                    if !self.input_buf.is_empty() {
                        self.autosuggest.record(&self.input_buf);
                    }
                    self.input_buf.clear();
                    self.current_suggestion = None;
                    b"\r".to_vec()
                }
            }
            Key::Named(NamedKey::Backspace) => {
                self.input_buf.pop();
                b"\x7f".to_vec()
            }
            Key::Named(NamedKey::Tab) => {
                // Shift+Tab = CSI Z (backtab). 표준 시퀀스라 claude code 의 permission
                // 모드 순환·역방향 포커스 등이 이걸 기대한다. Shift 무시하고 \t 만
                // 보내면 backtab 이 영영 안 닿음.
                if self.modifiers.shift_key() {
                    b"\x1b[Z".to_vec()
                } else {
                    b"\t".to_vec()
                }
            }
            Key::Named(NamedKey::Escape) => b"\x1b".to_vec(),
            // Editing / navigation keys that carry no `event.text`, so without
            // an explicit arm they fall through and send nothing (the "Delete /
            // Home / End don't work" report). Delete carries a Ctrl modifier
            // (CSI mod 5) for forward-word-delete; Home/End use the CSI form
            // every shell understands.
            Key::Named(NamedKey::Delete) => {
                if self.modifiers.control_key() {
                    b"\x1b[3;5~".to_vec()
                } else {
                    b"\x1b[3~".to_vec()
                }
            }
            Key::Named(NamedKey::Home) => b"\x1b[H".to_vec(),
            Key::Named(NamedKey::End) => b"\x1b[F".to_vec(),
            Key::Named(NamedKey::PageUp) => b"\x1b[5~".to_vec(),
            Key::Named(NamedKey::PageDown) => b"\x1b[6~".to_vec(),
            Key::Named(
                nk @ (NamedKey::ArrowUp
                | NamedKey::ArrowDown
                | NamedKey::ArrowRight
                | NamedKey::ArrowLeft),
            ) => {
                // Any cursor motion ends a suggestion: it only makes
                // sense while the cursor sits at the end of the typed
                // line. (→ at end-of-line is intercepted earlier as
                // accept when a suggestion is showing.)
                self.input_buf.clear();
                self.current_suggestion = None;
                let letter = match nk {
                    NamedKey::ArrowUp => 'A',
                    NamedKey::ArrowDown => 'B',
                    NamedKey::ArrowRight => 'C',
                    _ => 'D', // ArrowLeft
                };
                // Carry modifiers so claude code (Ink) / zsh see word-wise
                // and line-wise motion instead of a bare one-cell arrow.
                //   Option(Alt)+←/→ → CSI modifier 3 = backward/forward-word
                //   Cmd(super)+←/→  → Home / End  = line start/end
                // Cmd+Option+arrow never reaches here — it's consumed above
                // as the pane-focus shortcut.
                if self.modifiers.super_key() {
                    match letter {
                        'D' => b"\x1b[H".to_vec(),
                        'C' => b"\x1b[F".to_vec(),
                        _ => format!("\x1b[{letter}").into_bytes(),
                    }
                } else if self.modifiers.control_key() {
                    // Windows / readline word-wise motion: Ctrl+←/→.
                    // CSI modifier 5 = Ctrl (1 + ctrl-bit 4). zsh, bash
                    // readline, and Ink all read this as forward/backward-word.
                    format!("\x1b[1;5{letter}").into_bytes()
                } else if self.modifiers.alt_key() {
                    format!("\x1b[1;3{letter}").into_bytes()
                } else {
                    // Plain arrow: honor the active pane's DECCKM. When the
                    // inner app (claude code / vim / readline) set
                    // application-cursor mode it expects SS3 (`ESC O A`);
                    // sending CSI (`ESC [ A`) there silently fails, which
                    // is why up/down line-navigation in the prompt did
                    // nothing while modified arrows still worked.
                    let app_cursor = self
                        .ws
                        .lock()
                        .unwrap()
                        .active()
                        .and_then(|p| p.term())
                        .map(|t| t.app_cursor)
                        .unwrap_or(false);
                    if app_cursor {
                        format!("\x1bO{letter}").into_bytes()
                    } else {
                        format!("\x1b[{letter}").into_bytes()
                    }
                }
            }
            _ => match event.text.as_ref() {
                Some(t) => {
                    if std::env::var_os("KASATERM_IME_DEBUG").is_some() {
                        eprintln!(
                            "[key] text={t:?} logical_key={:?} ime_active={} in_preedit={}",
                            event.logical_key, self.ime_active, self.in_preedit
                        );
                    }
                    // Hangul branch (macOS only). On macOS our
                    // set_ime_allowed(false) means the Korean keyboard
                    // layout hands jamo codepoints straight through on
                    // KeyboardInput.text; we feed them into the local
                    // Composer to dodge the NSTextInputContext first-
                    // key drop. Windows / Linux let the OS IME handle
                    // the whole composition (Ime::Preedit/Commit), so
                    // we skip this branch and forward whatever text the
                    // keyboard layer produced as-is.
                    #[cfg(target_os = "macos")]
                    if t.chars().count() == 1 {
                        if let Some(c) = t.chars().next() {
                            if (0x3130..=0x318F).contains(&(c as u32)) {
                                if let Some(commit) = self.hangul.feed(c) {
                                    // Remember the committed text + cursor
                                    // so the overlay can show it until the
                                    // shell echo catches up (cursor moves).
                                    let before = self.ws.lock().ok().and_then(|ws| {
                                        ws.active_pane.clone().and_then(|id| {
                                            ws.panes
                                                .get(&id)
                                                .and_then(|p| p.term())
                                                .map(|t| (t.cursor_row, t.cursor_col))
                                        })
                                    });
                                    self.commit_overlay =
                                        before.map(|b| (commit.clone(), b));
                                    self.input_buf.push_str(&commit);
                                    self.send_bytes(commit.as_bytes());
                                }
                                self.preedit = self.hangul.preedit().unwrap_or_default();
                                self.in_preedit = !self.preedit.is_empty();
                                // Preedit lives in the chrome overlay, not the
                                // PTY grid — without flagging chrome_dirty the
                                // damage gate skips the frame and the composing
                                // syllable only flickers in on blink ticks.
                                self.chrome_dirty = true;
                                if let Some(w) = &self.window {
                                    w.request_redraw();
                                }
                                return;
                            }
                        }
                    }
                    // Non-Hangul ASCII / control characters: flush any
                    // pending Hangul syllable to the shell first, then
                    // forward the new character verbatim.
                    if !t.chars().all(|c| c.is_ascii() && !c.is_control()) {
                        if (self.ime_active || self.in_preedit)
                            && t.chars().any(is_hangul_codepoint)
                        {
                            return;
                        }
                    }
                    if let Some(flushed) = self.hangul.flush() {
                        commit_prefix.extend_from_slice(flushed.as_bytes());
                        self.input_buf.push_str(&flushed);
                        self.preedit.clear();
                        self.in_preedit = false;
                    }
                    // Mirror printable text into the autosuggest buffer.
                    // Control chars (e.g. a lone ESC sequence) don't grow
                    // the visible line, so they don't belong in the prefix.
                    if t.chars().all(|c| !c.is_control()) {
                        self.input_buf.push_str(t);
                    }
                    t.as_bytes().to_vec()
                }
                None => return,
            },
        };
        if std::env::var_os("KASATERM_IME_DEBUG").is_some() {
            eprintln!(
                "[send] prefix={:?} bytes={:?} preedit={:?} in_preedit={} ime_active={}",
                String::from_utf8_lossy(&commit_prefix),
                String::from_utf8_lossy(&bytes),
                self.preedit,
                self.in_preedit,
                self.ime_active
            );
        }
        if commit_prefix.is_empty() {
            self.send_bytes(&bytes);
        } else if is_control_key {
            // claude code (Ink) reads stdin asynchronously and can act on
            // a trailing control byte (\r submit, arrow nav) before it has
            // applied the multibyte syllable in front of it — even when
            // both arrive in one write. Send the committed syllable, let
            // it land, then the control byte. The gap is below human
            // perception and only happens on the (rare) control keypress.
            self.send_bytes(&commit_prefix);
            std::thread::sleep(std::time::Duration::from_millis(12));
            self.send_bytes(&bytes);
        } else {
            // Plain text after a flush has no submit race — ship it in one
            // write so the syllable and the next char stay together.
            commit_prefix.extend_from_slice(&bytes);
            self.send_bytes(&commit_prefix);
        }
    }
}

/// True when a pane's grid shows a live "working" animation in its bottom rows:
/// Braille spinners (U+2800..U+28FF — npm, pure-prompt) or Dingbat stars
/// (U+2731..U+274F — Claude Code's ✻/✶/✷ thinking indicator). Claude clears
/// this line the moment it goes idle, so the absence of a marker is a reliable
/// idle signal. Only the last ~10 rows are scanned (the live status sits at the
/// bottom); scrollback above is ignored.
fn term_is_working(t: &TerminalPane) -> bool {
    rows_show_working(&t.cells)
}

/// Whether the bottom of `cells` shows a live "agent working" indicator.
///
/// Scans the last 10 *non-blank* rows, not the last 10 physical rows. Claude
/// (and other TUIs) leave blank padding rows at the bottom of the grid, so
/// `cells[rows-10..]` can be all whitespace with the live status line
/// ("✢ Gitifying…") sitting just above the window — the bar then never shows
/// even while the spinner is clearly on screen. Anchor on the last non-blank
/// row instead (the same content range `visible_text`/peek report), which is
/// why peek saw the spinner but the working bar didn't.
///
/// Claude's live footer reads "✳ Verbing… (12s · esc to interrupt)". Three
/// signals, in order of reliability:
///   - "esc to interrupt" — exact, but TRUNCATED on narrow panes, so it can't
///     be the only cue.
///   - star dingbat (✢✳✻… U+2720–274F) + "…" ellipsis on the SAME line —
///     present at any width. The completion summary ("✻ Churned for 42s") keeps
///     the star but DROPS the ellipsis, so requiring both rejects it. (The old
///     bare-star check pinned the bar on forever after a turn.)
///   - braille spinner (other CLIs) — animation-only, safe on its own.
pub(crate) fn rows_show_working(cells: &[Vec<GridCell>]) -> bool {
    let Some(last) = cells
        .iter()
        .rposition(|row| row.iter().any(|cell| !matches!(cell.ch, ' ' | '\0')))
    else {
        return false;
    };
    let start = (last + 1).saturating_sub(10);
    cells[start..=last].iter().any(|row| {
        let line: String = row.iter().map(|cell| cell.ch).collect();
        if line.contains("esc to interrupt") {
            return true;
        }
        let has_star = row.iter().any(|cell| (0x2720..=0x274F).contains(&(cell.ch as u32)));
        let has_braille = row.iter().any(|cell| (0x2800..=0x28FF).contains(&(cell.ch as u32)));
        // 스피너는 가운뎃점(·) 프레임도 순환하는데, 최근 claude code 는 스피너
        // 행에 "esc to interrupt" 힌트를 안 넣는다("· Verbing… (3m · ↓ 9k tokens)")
        // — 점 프레임에서 working 판정이 프레임마다 풀리지 않게 점도 잡는다.
        // 점은 본문에 흔해 행 앞머리(col<8)로만 제한한다.
        let has_dot = row.iter().take(8).any(|cell| cell.ch == '·');
        ((has_star || has_dot) && line.contains('…')) || has_braille
    })
}

/// 승인/질문 프롬프트의 종류 — 응답 키 주입이 다르다 (munder-difflin BLOCK_HINTS 이식).
///   Menu:  claude 의 "❯ 1. Yes" 번호 메뉴(permission/AskUserQuestion). Enter=하이라이트
///          선택(기본 Yes), Esc=거부. 'y'/'n' 글자는 메뉴에서 무시되므로 못 쓴다.
///   YesNo: 셸 스크립트의 인라인 "(y/n)" 질문. y\r / n\r.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ApprovalPrompt {
    Menu,
    YesNo,
}

/// Bottom-anchored scan for a *pending* approval/question prompt. Mirrors
/// munder's BLOCK_HINTS with three hard-won exclusions baked in:
///   - never match the bare word "permission" — claude's footer permanently
///     shows "bypass permissions on", which would flag every busy pane;
///   - "(y/n)" counts only on the LAST non-blank row (the cursor line). An
///     answered shell prompt scrolls up but stays on screen ("proceed? (y/n) y"),
///     so matching it anywhere re-flags long after it was answered;
///   - a Menu match is REJECTED if a bare chevron row ("❯" with nothing after
///     it) sits BELOW it. That bare "❯ " is claude's idle input line; a *live*
///     approval menu replaces the input line with its options, so a real menu
///     never has a bare chevron under it. When one does, the "menu" text is
///     just quoted history in the transcript (e.g. a `peek` dump of another
///     pane's prompt) — matching it made an idle orchestrator pane toast itself and,
///     worse, a chip click injected Enter into its own input line. (거노
///     실클릭으로 확인된 false-positive.)
/// Callers must check `rows_show_working` first — a spinner means the prompt
/// text still on screen is history, not a question.
pub(crate) fn rows_show_approval_prompt(cells: &[Vec<GridCell>]) -> Option<ApprovalPrompt> {
    let last = cells
        .iter()
        .rposition(|row| row.iter().any(|cell| !matches!(cell.ch, ' ' | '\0')))?;
    // 메뉴는 옵션 + 안내문으로 working 스캔(10행)보다 길 수 있어 14행까지 본다.
    let start = (last + 1).saturating_sub(14);
    let mut menu_found = false;
    let mut bare_chevron_below_menu = false;
    for (i, row) in cells[start..=last].iter().enumerate() {
        let line: String = row
            .iter()
            .map(|c| if c.ch == '\0' { ' ' } else { c.ch })
            .collect();
        if let Some(pos) = line.find('❯') {
            let rest = line[pos + '❯'.len_utf8()..].trim();
            if rest.is_empty() {
                // bare "❯ " = claude idle 입력행. 이미 찾은 메뉴 후보 아래에
                // 있으면 그 메뉴는 인용된 가짜 → 뒤에서 reject.
                if menu_found {
                    bare_chevron_below_menu = true;
                }
                continue;
            }
            // "❯ 12. …" — 커서가 올라간 번호 옵션.
            let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
            if digits > 0 && rest[digits..].starts_with('.') {
                menu_found = true;
                continue;
            }
        }
        let lower = line.to_lowercase();
        // claude 승인 메뉴의 헤더 — 메뉴 행이 잘려도(좁은 pane) 이 문구로 Menu 판정.
        if lower.contains("do you want to proceed") {
            menu_found = true;
            continue;
        }
        // YesNo 는 last 행에서만(인라인 셸 질문). 메뉴와 독립이라 즉시 반환.
        if start + i == last && (lower.contains("(y/n)") || lower.contains("[y/n]")) {
            return Some(ApprovalPrompt::YesNo);
        }
    }
    if menu_found && !bare_chevron_below_menu {
        return Some(ApprovalPrompt::Menu);
    }
    None
}

#[cfg(test)]
mod working_scan_tests {
    use super::*;
    use kasa_bridge::screen::Cell;

    fn row(s: &str) -> Vec<GridCell> {
        s.chars().map(|ch| Cell { ch, ..Cell::blank() }).collect()
    }
    fn blank() -> Vec<GridCell> {
        vec![Cell::blank(); 8]
    }

    #[test]
    fn spinner_above_trailing_blank_padding_is_working() {
        // The exact 거노 case: claude's status line, then a wall of blank
        // padding rows filling out the grid. The old `cells[rows-10..]` scan
        // saw only blanks here and missed it.
        let mut cells = vec![row("✢ Gitifying… (1m 35s · ↑ 4.9k tokens)")];
        cells.extend(std::iter::repeat_with(blank).take(12));
        assert!(rows_show_working(&cells));
    }

    #[test]
    fn all_blank_is_idle() {
        assert!(!rows_show_working(&vec![blank(); 20]));
    }

    #[test]
    fn dot_frame_without_esc_hint_is_working() {
        // 라이브 실측(claude code 2.1.207): 점 프레임 스피너 행에 "esc to
        // interrupt" 힌트가 없다 — 점+… 문맥만으로 working 이어야 한다.
        assert!(rows_show_working(&[row("· Caramelizing… (3m 39s · ↓ 9.7k tokens)")]));
    }

    #[test]
    fn completion_summary_without_ellipsis_is_idle() {
        // "✻ Churned for 42s" keeps the star but drops the ellipsis.
        assert!(!rows_show_working(&[row("✻ Churned for 42s")]));
    }

    #[test]
    fn esc_to_interrupt_is_working() {
        assert!(rows_show_working(&[row("Working (esc to interrupt)")]));
    }

    #[test]
    fn braille_spinner_is_working() {
        assert!(rows_show_working(&[row("⠋ installing")]));
    }

    #[test]
    fn numbered_menu_is_menu_prompt() {
        let cells = vec![
            row("Do you want to proceed?"),
            row("❯ 1. Yes"),
            row("  2. Yes, and don't ask again"),
            row("  3. No, and tell Claude what to do differently"),
        ];
        assert_eq!(rows_show_approval_prompt(&cells), Some(ApprovalPrompt::Menu));
    }

    #[test]
    fn ask_user_question_menu_without_yes_is_menu_prompt() {
        // AskUserQuestion 옵션은 Yes/No 가 아닐 수 있다 — 번호+점이면 메뉴.
        let cells = vec![row("❯ 1. worktree로 격리"), row("  2. 그냥 main에서")];
        assert_eq!(rows_show_approval_prompt(&cells), Some(ApprovalPrompt::Menu));
    }

    #[test]
    fn bare_input_chevron_is_not_a_prompt() {
        // claude 입력창의 맨 "❯ " — 번호가 없으면 메뉴가 아니다.
        assert_eq!(rows_show_approval_prompt(&[row("❯ ")]), None);
    }

    #[test]
    fn permission_footer_alone_is_not_a_prompt() {
        // 푸터의 "bypass permissions on" 은 항상 떠 있다 — 매칭 금지 (munder 함정).
        let cells = vec![row("❯ "), row("  bypass permissions on (shift+tab to cycle)")];
        assert_eq!(rows_show_approval_prompt(&cells), None);
    }

    #[test]
    fn yn_on_last_row_is_yesno_prompt() {
        let cells = vec![row("Overwrite existing file? (y/n)")];
        assert_eq!(rows_show_approval_prompt(&cells), Some(ApprovalPrompt::YesNo));
    }

    #[test]
    fn answered_yn_above_prompt_line_is_ignored() {
        // 이미 답한 (y/n) 가 위로 스크롤돼 남아 있어도 마지막 행이 아니면 무시.
        let cells = vec![row("Overwrite? (y/n) y"), row("done."), row("$ ")];
        assert_eq!(rows_show_approval_prompt(&cells), None);
    }

    #[test]
    fn quoted_menu_with_bare_chevron_below_is_rejected() {
        // 오케스트레이터 pane 이 `peek %2` 결과를 자기 대화창에 인용 → transcript 에 박제된
        // 가짜 메뉴. 그 아래에 claude idle 입력행(bare "❯ ")이 있으면 reject.
        // (거노 실클릭으로 확인: 안 잡으면 idle pane 이 자기한테 토스트 쏘고
        //  칩 클릭 시 자기 입력행에 Enter 가 주입됨.)
        let cells = vec![
            row("> peek %2 결과:"),
            row("  Do you want to proceed?"),
            row("  ❯ 1. Yes"),
            row("    2. No"),
            row("알겠어, 확인했어."),
            row("❯ "),
        ];
        assert_eq!(rows_show_approval_prompt(&cells), None);
    }

    #[test]
    fn live_menu_without_bare_chevron_below_is_menu() {
        // 진짜 활성 메뉴: 입력행이 메뉴 옵션으로 대체돼 아래에 bare "❯ " 가 없다.
        let cells = vec![row("Do you want to proceed?"), row("❯ 1. Yes"), row("  2. No")];
        assert_eq!(rows_show_approval_prompt(&cells), Some(ApprovalPrompt::Menu));
    }

    #[test]
    fn menu_then_blank_padding_then_bare_chevron_is_rejected() {
        // 인용 메뉴와 입력행 사이에 빈 줄이 끼어도 reject (아래 어디든 bare-❯면).
        let cells = vec![row("❯ 1. Yes"), row("  2. No"), blank(), row("❯ ")];
        assert_eq!(rows_show_approval_prompt(&cells), None);
    }

    #[test]
    fn multi_digit_menu_option_is_menu_prompt() {
        assert_eq!(
            rows_show_approval_prompt(&[row("❯ 12. 마지막 옵션")]),
            Some(ApprovalPrompt::Menu)
        );
    }
}
