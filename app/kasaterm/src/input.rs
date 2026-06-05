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
        if let Some(client) = self.daemon_client.as_ref() {
            // Daemon-attached GUI: input goes over the control socket; the
            // daemon owns the PTY writer.
            client.send_raw(surface.as_deref(), bytes);
        } else if let Some(tmux) = self.tmux.as_ref() {
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
        } else if let Some(client) = self.daemon_client.as_ref() {
            client.send_raw(Some(pane_id), payload.as_bytes());
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
                    eprintln!("[tmuxify] clipboard write failed: {e}");
                }
            }
            Err(e) => eprintln!("[tmuxify] clipboard open failed: {e}"),
        }
    }
    pub(crate) fn paste_clipboard(&self) {
        let text = match arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[tmuxify] clipboard read failed: {e}");
                return;
            }
        };
        let mut payload = Vec::with_capacity(text.len() + 12);
        payload.extend_from_slice(b"\x1b[200~");
        payload.extend_from_slice(text.as_bytes());
        payload.extend_from_slice(b"\x1b[201~");
        self.send_bytes(&payload);
    }
    pub(crate) fn handle_wheel(&mut self, delta: MouseScrollDelta) {
        let wdbg = std::env::var_os("KASATERM_WHEEL_DEBUG").is_some();
        let dy_cells = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * 0.3,
            MouseScrollDelta::PixelDelta(p) => (p.y as f32) / self.cell.h.max(1.0) * 0.3,
        };
        if wdbg {
            eprintln!(
                "[wheel] delta={delta:?} dy_cells={dy_cells:.4} accum_before={:.4} cursor_px=({:.1},{:.1})",
                self.wheel_accum_y, self.cursor_px.0, self.cursor_px.1
            );
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
        // File-tree column: the pointer is over the tree, not a terminal, so
        // scroll the rows instead of delegating to a pane (px_to_pane_cell
        // returns None here and would otherwise fall through to the active
        // pane). Clamp so it can't scroll above the top or past the last row.
        if self.file_tree_visible
            && self.cursor_px.1 > TITLE_HEIGHT
            && self.cursor_px.0 >= self.file_tree_col_x()
            && self.cursor_px.0 < self.file_tree_col_x() + self.file_tree_w_logical
        {
            let item_h = 22.0_f32;
            let win_h = self.window.as_ref().map_or(800.0, |w| {
                w.inner_size().height as f32 / self.effective_scale()
            });
            let start_y = TITLE_HEIGHT + 10.0;
            let content_h = self.file_tree_nodes.len() as f32 * item_h;
            let max_scroll = (content_h - (win_h - start_y).max(0.0)).max(0.0);
            // lines>0 = wheel up = toward the top = less scroll.
            let delta_px = lines as f32 * item_h;
            let next = (self.file_tree_scroll - delta_px).clamp(0.0, max_scroll);
            if (next - self.file_tree_scroll).abs() > 0.01 {
                self.file_tree_scroll = next;
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
        if self.git_col_visible
            && self.cursor_px.1 > TITLE_HEIGHT
            && self.cursor_px.0 >= self.git_col_x()
        {
            let item_h = 22.0_f32;
            let n = self
                .git_col_data
                .lock()
                .map(|g| g.files.len())
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
            let next = (self.git_col_scroll - delta_px).clamp(0.0, max_scroll);
            if (next - self.git_col_scroll).abs() > 0.01 {
                self.git_col_scroll = next;
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
        let is_md = {
            let ws = self.ws.lock().unwrap();
            target_pane_id
                .as_deref()
                .and_then(|id| ws.panes.get(id))
                .map_or(false, |p| p.markdown().is_some())
        };
        if is_md {
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
                if let Some(client) = self.daemon_client.as_ref() {
                    client.send_raw(Some(id), &payload);
                } else if let Some(pty) = self.pty_for_pane(id) {
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
                if let Some(client) = self.daemon_client.as_ref() {
                    client.send_raw(Some(id), esc);
                } else if let Some(pty) = self.pty_for_pane(id) {
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
            if let Some(client) = self.daemon_client.as_ref() {
                // Daemon owns the scrollback; it re-snapshots and streams the
                // scrolled grid back to us. Positive `lines` = toward history.
                client.scroll(id, if lines > 0 { step } else { -step });
            } else if self.tmux.is_some() {
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
    pub(crate) fn forward_key(&mut self, event: &KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        // Touch the input timer so the cursor stays solid for a beat and
        // the blink phase re-starts from "on" once it kicks in.
        self.last_input_at = Instant::now();
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
            if let Key::Character(s) = &event.logical_key {
                match s.as_str() {
                    "+" | "=" => {
                        if let Ok(mut ws) = self.ws.lock() {
                            if let Some(pane) = ws.active_mut() {
                                let z = pane.image_view_zoom();
                                pane.image_zoom = (z * 1.25).clamp(1.0, 8.0);
                                changed = true;
                            }
                        }
                    }
                    "-" | "_" => {
                        if let Ok(mut ws) = self.ws.lock() {
                            if let Some(pane) = ws.active_mut() {
                                let z = pane.image_view_zoom();
                                pane.image_zoom = (z / 1.25).max(1.0);
                                changed = true;
                            }
                        }
                    }
                    "0" => {
                        if let Ok(mut ws) = self.ws.lock() {
                            if let Some(pane) = ws.active_mut() {
                                pane.image_zoom = 1.0;
                                pane.image_rot = 0;
                                changed = true;
                            }
                        }
                    }
                    "r" | "R" => {
                        if let Ok(mut ws) = self.ws.lock() {
                            if let Some(pane) = ws.active_mut() {
                                pane.image_rot = (pane.image_rot + 1) % 4;
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
                            eprintln!("[tmuxify] split failed: {e}");
                        }
                        return;
                    }
                    if code == KeyCode::KeyE {
                        if let Err(e) = self.split_active_pane(kasa_pty::SplitDir::Vertical) {
                            eprintln!("[tmuxify] split failed: {e}");
                        }
                        return;
                    }
                    // Close the focused pane. Last-pane close is left
                    // to the OS close button.
                    if code == KeyCode::KeyW {
                        self.close_active_pane();
                        return;
                    }
                    // Cmd+T → new window in the current session (PTY backend
                    // only; tmux owns its own windows). Cmd+1..9 switch to
                    // that window. Digit0 is font-reset above, so windows
                    // start at 1.
                    if code == KeyCode::KeyT && self.tmux.is_none() {
                        if self.modifiers.shift_key() {
                            // Cmd+Shift+T → restore the most recently docked pane
                            // (ghostty reopen-closed-tab). No-op if dock empty.
                            if let (Some(client), Some(d)) =
                                (self.daemon_client.as_ref(), self.docked.last())
                            {
                                client.undock(&d.id);
                            }
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
            Key::Named(NamedKey::Tab) => b"\t".to_vec(),
            Key::Named(NamedKey::Escape) => b"\x1b".to_vec(),
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
