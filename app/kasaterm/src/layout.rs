//! pane 레이아웃 조작 — split/move/close/focus/swap/drop/divider/zoom/tab + 좌표·resize. daemon-authoritative.
use super::*;

impl App {
    /// Convert logical-pixel position into a (pane_id, col, row) cell
    /// inside the pane the click landed in. Multi-pane aware: walks the
    /// parsed Layout to find the pane whose rect contains the click,
    /// then translates the pixel into that pane's cell-local coords.
    /// Returns None when the workspace has no panes or the click missed
    /// every pane (gutter between split borders, padding, etc).
    pub(crate) fn px_to_pane_cell(&self, px: f32, py: f32) -> Option<(String, u16, u16)> {
        let sb = self.effective_sidebar_w();
        let ws = self.ws.lock().unwrap();
        if let Some(layout) = ws.layout.as_ref() {
            let split = layout.leaves().len() > 1;
            let header_h = if split { PANE_HEADER_HEIGHT } else { 0.0 };
            // Box hit-test runs in whole-grid cells (header included, no
            // inset) so a click anywhere in the pane box selects it.
            let gcol = ((px - sb - WINDOW_PADDING).max(0.0) / self.cell.w).floor() as i32;
            // Render shifts every split pane down by the header band (origin_y
            // += header_shift, see render_frame_gpu). The box hit-test must
            // subtract the same band, or the lower pane's rows map ~one header
            // above where they're actually drawn — clicks / scroll there miss
            // the pane entirely.
            let grow = ((py - TITLE_HEIGHT - header_h).max(0.0) / self.cell.h).floor() as i32;
            for leaf in layout.leaves() {
                if let Layout::Pane { id, x, y, w, h } = leaf {
                    let (bx, by, bw, bh) = (*x as i32, *y as i32, *w as i32, *h as i32);
                    if gcol >= bx && gcol < bx + bw && grow >= by && grow < by + bh {
                        // Local cell uses the body origin: box edge + header
                        // band + inner inset, matching the render origin.
                        let pid = format!("%{id}");
                        // Per-pane font zoom: glyphs render at cell × fs, so the
                        // pixel→cell divisor must use the same zoomed cell or a
                        // font-bumped pane maps the cursor to the wrong row/col
                        // (selection + mouse-report drift). The box origin stays
                        // on the shared grid — only the in-pane step scales.
                        let fs = self
                            .pane_font_scales
                            .get(&pid)
                            .copied()
                            .unwrap_or(1.0)
                            .max(0.1);
                        let box_left = sb + WINDOW_PADDING + bx as f32 * self.cell.w;
                        let box_top = TITLE_HEIGHT + by as f32 * self.cell.h;
                        let lc = ((px - box_left - PANE_INNER_X).max(0.0) / (self.cell.w * fs))
                            .floor() as u16;
                        let lr = ((py - box_top - header_h - PANE_INNER_Y).max(0.0)
                            / (self.cell.h * fs))
                            .floor() as u16;
                        let (mc, mr) = ws
                            .panes
                            .get(&pid)
                            .and_then(|p| p.term())
                            .map_or((lc, lr), |t| {
                                (
                                    lc.min(t.cols.saturating_sub(1)),
                                    lr.min(t.rows.saturating_sub(1)),
                                )
                            });
                        return Some((pid, mc, mr));
                    }
                }
            }
            return None;
        }
        // No layout yet — single pane fills the window (inset only).
        let id = ws.active_pane.clone().or_else(|| ws.panes.keys().next().cloned())?;
        let pane = ws.panes.get(&id)?;
        let t = pane.term()?;
        if t.cols == 0 || t.rows == 0 {
            return None;
        }
        let fs = self.pane_font_scales.get(&id).copied().unwrap_or(1.0).max(0.1);
        let lc = ((px - sb - WINDOW_PADDING - PANE_INNER_X).max(0.0) / (self.cell.w * fs))
            .floor() as u16;
        let lr =
            ((py - TITLE_HEIGHT - PANE_INNER_Y).max(0.0) / (self.cell.h * fs)).floor() as u16;
        Some((id, lc.min(t.cols - 1), lr.min(t.rows - 1)))
    }
    /// Convenience wrapper that returns only the active pane's local
    /// cell coords. Most callers (wheel, selection drag) only care
    /// about the active pane.
    pub(crate) fn px_to_cell_active(&self, px: f32, py: f32) -> Option<(u16, u16)> {
        let (pane_id, col, row) = self.px_to_pane_cell(px, py)?;
        let ws = self.ws.lock().unwrap();
        let active_match = ws.active_pane.as_deref() == Some(pane_id.as_str());
        active_match.then_some((col, row))
    }
    /// Target pane for outgoing key/text. When the workspace has an
    /// active pane, we name it explicitly so tmux doesn't fall back to
    /// "last-active" semantics that disagree with our UI.
    pub(crate) fn target_pane(&self) -> Option<String> {
        self.ws.lock().unwrap().active_pane.clone()
    }
    /// Surface id that should receive keyboard input — the active pane's
    /// *active tab*'s pid, not the outer pane id. `target_pane()` returns
    /// the layout key (== first tab's pid), so once the user switches tabs
    /// the daemon keeps routing keystrokes to the first tab. The daemon's
    /// PTY map is keyed by tab pid, so input must name the active tab
    /// explicitly. Falls back to the outer id for single-tab / tmux panes
    /// whose tabs carry no explicit pid (same fallback as `active_pty`).
    pub(crate) fn target_surface(&self) -> Option<String> {
        let ws = self.ws.lock().ok()?;
        let outer = ws.active_pane.clone()?;
        let pid = ws
            .panes
            .get(&outer)
            .and_then(|p| p.tabs.get(p.active_tab).and_then(|t| t.pid.clone()))
            .unwrap_or(outer);
        Some(pid)
    }
    /// The PtySession that currently has keyboard focus, if any. Used
    /// by every routing-by-active-pane code path in PTY mode.
    /// PtySession of a pane's currently-active tab. Use this instead of
    /// `self.pty.get(outer_id)` — after a cross-pane tab drag the layout
    /// id and the active tab's pid diverge, and the direct lookup misses.
    /// Drives wheel scroll / mouse-reporting / pane-targeted send_bytes.
    pub(crate) fn pty_for_pane(&self, outer_id: &str) -> Option<&Arc<kasa_pty::PtySession>> {
        let ws = self.ws.lock().ok()?;
        let pid = ws
            .panes
            .get(outer_id)
            .and_then(|p| p.tabs.get(p.active_tab).and_then(|t| t.pid.clone()))
            .unwrap_or_else(|| outer_id.to_string());
        drop(ws);
        self.pty.get(&pid)
    }
    pub(crate) fn active_pty(&self) -> Option<&Arc<kasa_pty::PtySession>> {
        // The active *tab*'s pid drives input/scroll/title — falling back
        // to the outer pane id (== first-tab pid) for single-tab panes
        // whose tabs haven't been initialised with an explicit pid yet
        // (e.g. tmux-mode panes, where the outer key is what `pty` keys on).
        let ws = self.ws.lock().unwrap();
        let outer = ws.active_pane.clone()?;
        let pid = ws
            .panes
            .get(&outer)
            .and_then(|p| p.tabs.get(p.active_tab).and_then(|t| t.pid.clone()))
            .unwrap_or(outer);
        drop(ws);
        self.pty.get(&pid)
    }
    /// Window size in cell coordinates. Source of truth for resize
    /// distribution + new-pane sizing. The grid lives inside
    /// `WINDOW_PADDING` on every side, so subtract 2× padding from the
    /// logical viewport before dividing — otherwise we tell the PTY it
    /// has N rows but only N-1 fit before clipping, and the last row
    /// (where most TUIs paint their statusline) gets cut in half.
    /// Falls back to (80, 24) when the window isn't ready yet.
    pub(crate) fn window_cells(&self) -> (u16, u16) {
        let Some(window) = self.window.as_ref() else {
            return (80, 24);
        };
        let size = window.inner_size();
        let scale = self.effective_scale();
        let raw_lw = size.width as f32 / scale;
        let raw_lh = size.height as f32 / scale;
        let lw = (raw_lw
            - self.effective_sidebar_w()
            - self.effective_right_chrome_w()
            - 2.0 * WINDOW_PADDING)
            .max(0.0);
        // Top: TITLE_HEIGHT (chrome strip). Bottom: WINDOW_PADDING. The
        // asymmetry is intentional — the strip replaces the top padding.
        // Reserve the dock bar from the grid only when it carries chips.
        let dock = if self.docked.is_empty() { 0.0 } else { DOCK_HEIGHT };
        let lh = (raw_lh - TITLE_HEIGHT - WINDOW_PADDING - dock).max(0.0);
        let cols = (lw / self.cell.w).floor().max(40.0) as u16;
        let rows = (lh / self.cell.h).floor().max(10.0) as u16;
        if std::env::var_os("KASATERM_LOG_LAYOUT").is_some() {
            eprintln!(
                "[layout] win=({raw_lw:.0}x{raw_lh:.0}) usable=({lw:.0}x{lh:.0}) cell=({:.1}x{:.1}) cells=({cols}x{rows})",
                self.cell.w, self.cell.h
            );
        }
        (cols, rows)
    }
    /// Push the current PtyLayout into `ws.layout` so the renderer
    /// (which only knows the tmux Layout shape) picks up the splits.
    /// A single-leaf tree leaves `ws.layout` empty — the render path's
    /// single-pane fallback handles that case.
    pub(crate) fn publish_pty_layout(&self) {
        if let Some(tree) = self.pty_layout.as_ref() {
            let (cols, rows) = self.window_cells();
            let mut ws = self.ws.lock().unwrap();
            if tree.leaves().len() <= 1 {
                ws.layout = None;
            } else {
                ws.layout = Some(tree.to_tmux_layout(cols, rows));
            }
        }
        // Keep the socket snapshot in lockstep with the renderer view —
        // every code path that adds/removes panes or moves focus goes
        // through publish_pty_layout, so this is the one spot we have
        // to wire the cmux mirror.
    }
    /// Resize every backend session so its grid matches the new window
    /// size. In tmux mode the daemon redistributes for us. In PTY mode
    /// we walk the BSP tree and SIGWINCH each leaf to its own rect.
    pub(crate) fn resize_backend(&self, cols: u16, rows: u16) {
        if let Some(tmux) = self.tmux.as_ref() {
            let _ = tmux.resize_client(cols, rows);
            return;
        }
        // The window is the single source of truth for size. Derive every
        // leaf's usable grid from the window cell box here, then push those
        // sizes to whoever owns the PTY — the daemon over RPC, or local
        // sessions. Panes themselves only carry BSP ratios, never absolute
        // rows/cols, so this one computation feeds both backends.
        let Some(tree) = self.pty_layout.as_ref() else {
            return;
        };
        // When the workspace is split, every pane wears a per-pane header
        // strip that eats a few cell rows off the top of its box, so the
        // PTY's usable grid shrinks by the same amount — otherwise claude
        // code paints its statusline / `bypass…` row off the bottom edge.
        let leaves = tree.leaves().len();
        let header_px = if leaves > 1 { PANE_HEADER_HEIGHT } else { 0.0 };
        // Per-pane font scale shrinks/grows that pane's usable cells: bigger
        // glyphs ⇒ fewer cols/rows in the same box. 1.0 panes keep the exact
        // integer-cell math; scaled panes divide the base cell span by the
        // factor (the box stays on the base grid, matching the per-slot render
        // which sizes glyphs by the same factor). Keyed by pty/leaf id.
        let scale_of = self.pane_font_scales.clone();
        let cw = self.cell.w.max(1.0);
        let ch = self.cell.h.max(1.0);
        let mut leaf_cells: HashMap<String, (u16, u16)> = HashMap::new();
        for (id, _x, _y, w, h) in self.effective_leaf_rects(cols, rows) {
            let fs = scale_of.get(&id).copied().unwrap_or(1.0).max(0.1);
            // Work in logical px on the BASE grid — exactly the span the
            // renderer fills (origin + w·cell), then subtract the real px
            // insets/header and divide by the ZOOMED cell. The old path
            // rounded the inset to whole base cells and divided by fs, so a
            // shrunk pane (small fs) amplified that ceil error ∝ 1/fs and
            // told the PTY a grid that no longer matched the drawn area —
            // that's the "비율 안 맞음" past a certain zoom-out.
            let box_w_px = w as f32 * cw;
            let box_h_px = h as f32 * ch;
            let scaled_cw = cw * fs;
            let scaled_ch = ch * fs;
            let usable_w = (box_w_px - 2.0 * PANE_INNER_X).max(scaled_cw);
            let usable_h = (box_h_px - header_px - 2.0 * PANE_INNER_Y).max(scaled_ch);
            let pcols = (usable_w / scaled_cw).floor().max(1.0) as u16;
            let prows = (usable_h / scaled_ch).floor().max(1.0) as u16;
            leaf_cells.insert(id, (pcols, prows));
        }
        // Each leaf id IS its primary pane's pid, so resize that PTY directly
        // from leaf_cells — no dependency on ws.panes being populated. A
        // freshly split pane has no PaneState until its first output, so the
        // old ws.panes walk left it at 80×24 spawn size (화면 겹침/하단 잘림).
        for (id, (pc, pr)) in &leaf_cells {
            if let Some(sess) = self.pty.get(id) {
                let _ = sess.resize(*pc, *pr);
            }
        }
        // In-pane secondary tabs (pid != outer) still resolve via ws.panes —
        // they share the outer leaf's rect but have their own PtySession.
        let snapshot: Vec<(String, Vec<String>)> = {
            let ws = self.ws.lock().unwrap();
            ws.panes
                .iter()
                .map(|(outer, p)| {
                    let pids: Vec<String> = p
                        .tabs
                        .iter()
                        .filter_map(|t| t.pid.clone())
                        .filter(|pid| pid != outer)
                        .collect();
                    (outer.clone(), pids)
                })
                .collect()
        };
        for (outer, pids) in snapshot {
            let Some(&(pc, pr)) = leaf_cells.get(&outer) else { continue };
            for pid in pids {
                if let Some(sess) = self.pty.get(&pid) {
                    let _ = sess.resize(pc, pr);
                }
            }
        }
        // Re-publish the layout because rect proportions may have
        // shifted (rounding) and the renderer caches the previous tree.
        self.publish_pty_layout();
    }
    /// If the cursor (logical px) rests on a split seam, return the BSP
    /// tree path of that split plus its axis. A few px of tolerance makes
    /// the thin seam easy to grab. None when not over any divider.
    pub(crate) fn divider_at_px(&self, x: f32, y: f32) -> Option<(Vec<u8>, kasa_pty::SplitDir)> {
        let tree = self.pty_layout.as_ref()?;
        if tree.leaves().len() <= 1 {
            return None;
        }
        let (cols, rows) = self.window_cells();
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        let tol = 6.0_f32;
        for d in tree.dividers(cols, rows) {
            match d.dir {
                kasa_pty::SplitDir::Horizontal => {
                    let seam_x = pad + d.edge as f32 * self.cell.w;
                    let y0 = TITLE_HEIGHT + d.span_start as f32 * self.cell.h;
                    let y1 = y0 + d.span_len as f32 * self.cell.h;
                    if (x - seam_x).abs() <= tol && y >= y0 && y <= y1 {
                        return Some((d.path, d.dir));
                    }
                }
                kasa_pty::SplitDir::Vertical => {
                    let seam_y = TITLE_HEIGHT + d.edge as f32 * self.cell.h;
                    let x0 = pad + d.span_start as f32 * self.cell.w;
                    let x1 = x0 + d.span_len as f32 * self.cell.w;
                    if (y - seam_y).abs() <= tol && x >= x0 && x <= x1 {
                        return Some((d.path, d.dir));
                    }
                }
            }
        }
        None
    }
    /// Split the focused pane in PTY mode. Spawns a new shell into a
    /// fresh PTY, inserts it into the BSP tree on the right (Horizontal)
    /// or bottom (Vertical) of the focused leaf, then resizes every
    /// session so each one matches its new rect. Becomes a no-op in
    /// tmux mode — splits there go through the cmux socket / tmux
    /// `split-window` instead.
    pub(crate) fn split_active_pane(&mut self, dir: kasa_pty::SplitDir) -> Result<()> {
        if self.tmux.is_some() {
            return Ok(());
        }
        let Some(active) = self.ws.lock().unwrap().active_pane.clone() else {
            return Ok(());
        };
        let new_id = format!("%{}", self.next_pane_id);
        self.next_pane_id += 1;

        // Spawn the new session at a placeholder size — the resize
        // pass right after `split_leaf` puts every leaf at its real
        // rect, so the initial cols/rows here only matters for the
        // first bytes the shell prints before SIGWINCH lands.
        let (win_cols, win_rows) = self.window_cells();
        let cwd = resolve_initial_cwd();
        let session = kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            shell: resolve_default_shell(),
            cwd,
            cols: win_cols,
            rows: win_rows,
            env: Vec::new(),
            pane_id: new_id.clone(),
            initial_scrollback: Vec::new(),
        })?;
        self.pump_pty_screens(session.screens.clone(), new_id.clone());
        self.pty.insert(new_id.clone(), Arc::new(session));

        let layout = self.pty_layout.as_mut().expect("pty_layout set in start_pty");
        if !layout.split_leaf(&active, dir, new_id.clone()) {
            // Active pane isn't in the tree — shouldn't happen, but
            // bail without leaking the spawned session entry.
            self.pty.remove(&new_id);
            self.next_pane_id -= 1;
            return Ok(());
        }
        self.ws.lock().unwrap().active_pane = Some(new_id);
        self.resize_backend(win_cols, win_rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        Ok(())
    }
    /// Stage-3 in-pane tab spawn. Creates a fresh PtySession with its own
    /// pid, registers it in `pid_to_pane` so output streams find the right
    /// (outer pane, tab) pair, and appends a `PaneTab` whose `pid` points at
    /// the new shell. The new tab becomes active. Outer pane id and layout
    /// don't change — adding a tab never reshapes the BSP tree.
    pub(crate) fn spawn_new_tab(&mut self, outer: &str) -> Result<()> {
        if self.tmux.is_some() {
            anyhow::bail!("in-pane tabs not supported on tmux backend");
        }
        // Outer pane must already exist in the layout (it's the user's focused
        // pane). Use its size for the initial pty so the shell starts at the
        // right cols/rows — `resize_backend` after re-applies it anyway, but a
        // sane initial size keeps the welcome banner from wrapping weird.
        let (cols, rows) = self.pane_cells(outer).unwrap_or_else(|| self.window_cells());
        let cwd = resolve_initial_cwd();
        let new_pid = format!("%{}", self.next_pane_id);
        self.next_pane_id += 1;
        let session = kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            shell: resolve_default_shell(),
            cwd,
            cols,
            rows,
            env: Vec::new(),
            pane_id: new_pid.clone(),
            initial_scrollback: Vec::new(),
        })?;
        self.pump_pty_screens(session.screens.clone(), new_pid.clone());
        self.pty.insert(new_pid.clone(), Arc::new(session));
        {
            let mut ws = self.ws.lock().unwrap();
            ws.pid_to_pane.insert(new_pid.clone(), outer.to_string());
            if let Some(pane) = ws.panes.get_mut(outer) {
                let mut tab = PaneTab::default();
                tab.pid = Some(new_pid.clone());
                pane.tabs.push(tab);
                pane.active_tab = pane.tabs.len() - 1;
                pane.dirty = true;
            }
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        Ok(())
    }
    /// Cell extent of `outer` inside the current `pty_layout`. Used by
    /// `spawn_new_tab` to size a brand-new shell at the pane's real bounds.
    /// Returns `None` when the layout is in single-pane fallback or the id
    /// isn't a leaf.
    pub(crate) fn pane_cells(&self, outer: &str) -> Option<(u16, u16)> {
        let (cols, rows) = self.window_cells();
        let tree = self.pty_layout.as_ref()?;
        for (id, _x, _y, w, h) in tree.leaf_rects(cols, rows) {
            if id == outer {
                return Some((w.max(1), h.max(1)));
            }
        }
        None
    }
    /// Leaf rects for render / hit-test / resize, honoring a tmux-style zoom.
    /// When a pane is zoomed it fills the whole work area and the others are
    /// hidden; the daemon's layout tree is untouched (zoom is GUI-local render
    /// state). If the zoomed pane is gone (closed or moved out by a broadcast)
    /// this falls back to the real layout, so a stale zoom never paints a
    /// phantom pane.
    pub(crate) fn effective_leaf_rects(&self, cols: u16, rows: u16) -> Vec<(String, u16, u16, u16, u16)> {
        if let Some(z) = self.zoomed_pane.as_ref() {
            if let Some(tree) = self.pty_layout.as_ref() {
                if tree.leaves().iter().any(|l| *l == z.as_str()) {
                    return vec![(z.clone(), 0, 0, cols, rows)];
                }
            }
        }
        self.pty_layout
            .as_ref()
            .map(|t| t.leaf_rects(cols, rows))
            .unwrap_or_default()
    }
    /// Toggle tmux-style zoom on `pane`: zoom fills the work area with just that
    /// pane; toggling again (or the pane already being zoomed) restores the
    /// split. Reflows the backend so the PTY matches its new extent.
    pub(crate) fn toggle_pane_zoom(&mut self, pane: &str) {
        if self.zoomed_pane.as_deref() == Some(pane) {
            self.zoomed_pane = None;
        } else {
            self.zoomed_pane = Some(pane.to_string());
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Drop a non-primary tab: kill its PTY, remove the pid map entry, drop
    /// the slot. The primary tab (index 0, pid == outer pane id) can't be
    /// closed this way — callers fall through to `remove_pane` for that.
    pub(crate) fn close_tab(&mut self, outer: &str, idx: usize) {
        let (pid_opt, preview_opt): (Option<String>, Option<String>) = {
            let ws = self.ws.lock().unwrap();
            let tab = ws.panes.get(outer).and_then(|p| p.tabs.get(idx));
            (
                tab.and_then(|t| t.pid.clone()),
                tab.and_then(|t| t.preview_id.clone()),
            )
        };
        if let Some(pid) = pid_opt.as_deref() {
            if pid != outer {
                // Secondary tab — drop its session entry; reader thread sees
                // the channel close and pushes EOF to `dead_panes`, but with
                // the pid_to_pane entry gone the reap pass routes through
                // remove_pane(pid) which is a no-op (pty already gone). Fine.
                self.pty.remove(pid);
                self.ws.lock().unwrap().pid_to_pane.remove(pid);
            }
        }
        // Preview tab removal is immediate via the ws.panes mutation below;
        // with no daemon there's no broadcast to resurrect it.
        let _ = preview_opt;
        let mut ws = self.ws.lock().unwrap();
        if let Some(pane) = ws.panes.get_mut(outer) {
            if idx < pane.tabs.len() {
                pane.tabs.remove(idx);
            }
            if idx < pane.active_tab {
                pane.active_tab -= 1;
            }
            if pane.active_tab >= pane.tabs.len() {
                pane.active_tab = pane.tabs.len() - 1;
            }
            pane.dirty = true;
        }
    }
    /// Drain `dead_panes` and remove each from the BSP tree + pty map.
    /// Called on the main thread from `about_to_wait` so the mutation
    /// runs without competing with the per-session reader threads.
    /// If removing all panes empties the tree, exit the event loop.
    pub(crate) fn reap_dead_panes(&mut self, event_loop: &ActiveEventLoop) {
        let ids: Vec<String> = std::mem::take(&mut *self.dead_panes.lock().unwrap());
        if ids.is_empty() {
            return;
        }
        for id in ids {
            if !self.pty.contains_key(&id) {
                continue;
            }
            self.remove_pane(&id);
        }
        // Last pane closed (e.g. user typed `exit` in the only shell): shut the
        // window so kasaterm exits cleanly the way users expect from a regular
        // terminal.
        if self.tmux.is_none() && self.pty.is_empty() {
            event_loop.exit();
        }
    }
    /// Drag a single-tab pane onto its own body half. Spawns a fresh shell
    /// next to `source` on the side OPPOSITE the drop, so the original
    /// pane visually "lands" on the side the user threw it to. Distinct
    /// from `drop_tab_into_body` (which lifts a tab into a new pane on the
    /// drop side) — this one keeps the source intact and adds a sibling.
    pub(crate) fn split_pane_opposite(&mut self, source: &str, zone: DropZone) -> Result<()> {
        if self.tmux.is_some() {
            anyhow::bail!("split via drag unsupported on tmux backend");
        }
        let (cols, rows) = self.pane_cells(source).unwrap_or_else(|| self.window_cells());
        let cwd = resolve_initial_cwd();
        let new_id = format!("%{}", self.next_pane_id);
        self.next_pane_id += 1;
        let session = kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            shell: resolve_default_shell(),
            cwd,
            cols,
            rows,
            env: Vec::new(),
            pane_id: new_id.clone(),
            initial_scrollback: Vec::new(),
        })?;
        self.pump_pty_screens(session.screens.clone(), new_id.clone());
        self.pty.insert(new_id.clone(), Arc::new(session));
        // `before=true` means the new leaf becomes the LEFT/TOP child, so
        // the source ends up on the RIGHT/BOTTOM. We want source on the
        // dropped side → new on the opposite side.
        let (dir, before) = match zone {
            DropZone::Right => (kasa_pty::SplitDir::Horizontal, true),
            DropZone::Left => (kasa_pty::SplitDir::Horizontal, false),
            DropZone::Down => (kasa_pty::SplitDir::Vertical, true),
            DropZone::Up => (kasa_pty::SplitDir::Vertical, false),
            // Center is handled by the caller as a tab merge — splitting
            // would lose the "drop into this pane's tabs" intent.
            DropZone::Center => return Ok(()),
        };
        let inserted = self
            .pty_layout
            .as_mut()
            .map(|t| t.insert_beside(source, dir, before, new_id.clone()))
            .unwrap_or(false);
        if !inserted {
            // Source vanished mid-drag — bail and clean up the spawned shell.
            self.pty.remove(&new_id);
            self.next_pane_id -= 1;
            return Ok(());
        }
        let (win_cols, win_rows) = self.window_cells();
        self.resize_backend(win_cols, win_rows);
        self.publish_pty_layout();
        // Focus the freshly-spawned pane so the user is typing into it.
        self.ws.lock().unwrap().active_pane = Some(new_id);
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        Ok(())
    }
    /// Tab drag dropped onto another pane's BODY. Splits the target pane
    /// in the matching quadrant and makes the moved tab the new leaf — the
    /// dragged shell now lives in its own pane next to `target`. Unifies
    /// the old "drag pane header" semantics into the tab drag so there's
    /// one drop UX.
    pub(crate) fn drop_tab_into_body(&mut self, td: &TabDrag, target: &str, zone: DropZone) {
        // 1. Lift the tab out of source.
        let (moved, src_empty): (Option<PaneTab>, bool) = {
            let mut ws = self.ws.lock().unwrap();
            let Some(src) = ws.panes.get_mut(&td.pane) else { return };
            if td.from >= src.tabs.len() { return }
            let t = src.tabs.remove(td.from);
            if td.from < src.active_tab && src.active_tab > 0 {
                src.active_tab -= 1;
            }
            if src.active_tab >= src.tabs.len() && !src.tabs.is_empty() {
                src.active_tab = src.tabs.len() - 1;
            }
            src.dirty = true;
            let empty = src.tabs.is_empty();
            (Some(t), empty)
        };
        let Some(moved) = moved else { return };
        // 2. If source emptied, drop it from layout (PtySession survives —
        //    it's the very shell we're about to re-attach as a new leaf).
        if src_empty {
            self.ws.lock().unwrap().panes.remove(&td.pane);
            self.collapse_layout_only(&td.pane);
        }
        // 3. Allocate a fresh layout id for the new pane. Layout ids and
        //    pty ids decoupled from stage-3 onward, so this avoids any
        //    clash with the moved tab's pid (which may have been the old
        //    source's outer id).
        let new_outer = format!("%{}", self.next_pane_id);
        self.next_pane_id += 1;
        let (dir, before) = match zone {
            DropZone::Left => (kasa_pty::SplitDir::Horizontal, true),
            DropZone::Right => (kasa_pty::SplitDir::Horizontal, false),
            DropZone::Up => (kasa_pty::SplitDir::Vertical, true),
            DropZone::Down => (kasa_pty::SplitDir::Vertical, false),
            // Caller routes Center to the cross-pane tab-merge path; if it
            // slips through here, abort the split so we don't double-spawn.
            DropZone::Center => return,
        };
        if let Some(tree) = self.pty_layout.as_mut() {
            if !tree.insert_beside(target, dir, before, new_outer.clone()) {
                // Target gone — fall back to inserting at the first leaf.
                if let Some(anchor) = tree.leaves().first().map(|s| s.to_string()) {
                    tree.insert_beside(&anchor, dir, before, new_outer.clone());
                }
            }
        } else {
            self.pty_layout = Some(kasa_pty::PtyLayout::single(&new_outer));
        }
        // 4. Build the new PaneState with the moved tab as its only tab.
        let moved_pid = moved.pid.clone();
        {
            let mut ws = self.ws.lock().unwrap();
            let mut ps = PaneState::default();
            ps.tabs.clear();
            ps.tabs.push(moved);
            ps.active_tab = 0;
            ps.dirty = true;
            ws.panes.insert(new_outer.clone(), ps);
            if let Some(pid) = moved_pid {
                // Rebind the pid map so future ScreenUpdates / find_tab_by_pty
                // route to new_outer even when pid != new_outer.
                ws.pid_to_pane.insert(pid, new_outer.clone());
            }
            ws.active_pane = Some(new_outer.clone());
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// Cross-pane drag aftermath. The source pane lost every tab to dest;
    /// we just need its layout slot gone — *not* the PtySession (now owned
    /// by dest under the same pid key) or the image / markdown caches the
    /// moved tabs depend on. Picks a survivor focus exactly like
    /// `remove_pane` so the chrome doesn't blink to "no active".
    pub(crate) fn collapse_layout_only(&mut self, target: &str) {
        let leaves: Vec<String> = match self.pty_layout.as_ref() {
            Some(t) => t.leaves().iter().map(|s| s.to_string()).collect(),
            None => return,
        };
        let was_active = self
            .ws
            .lock()
            .unwrap()
            .active_pane
            .as_deref()
            .map(|a| a == target)
            .unwrap_or(false);
        let next_focus: Option<String> = if was_active && leaves.len() > 1 {
            let cur_idx = leaves.iter().position(|l| l == target).unwrap_or(0);
            Some(if cur_idx + 1 < leaves.len() {
                leaves[cur_idx + 1].clone()
            } else {
                leaves[cur_idx - 1].clone()
            })
        } else {
            None
        };
        if leaves.len() > 1 {
            if let Some(tree) = self.pty_layout.as_mut() {
                tree.remove_leaf(target);
            }
        } else {
            self.pty_layout = None;
        }
        {
            let mut ws = self.ws.lock().unwrap();
            ws.panes.remove(target);
            ws.rebuild_pid_map();
            if was_active && next_focus.is_some() {
                ws.active_pane = next_focus;
            }
            for pane in ws.panes.values_mut() {
                pane.dirty = true;
            }
        }
        self.chrome_dirty = true;
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// Internal: drop a pane regardless of whether it's the active one.
    /// Used by both `close_pane` (Cmd+W / header ×) and `reap_dead_panes`
    /// (shell exit). Picks a survivor focus when removing the focused
    /// pane.
    pub(crate) fn remove_pane(&mut self, target: &str) {
        let was_active = self
            .ws
            .lock()
            .unwrap()
            .active_pane
            .as_deref()
            .map(|a| a == target)
            .unwrap_or(false);
        let leaves: Vec<String> = match self.pty_layout.as_ref() {
            Some(t) => t.leaves().iter().map(|s| s.to_string()).collect(),
            None => return,
        };
        let next_focus: Option<String> = if was_active && leaves.len() > 1 {
            let cur_idx = leaves.iter().position(|l| l == target).unwrap_or(0);
            if cur_idx + 1 < leaves.len() {
                Some(leaves[cur_idx + 1].clone())
            } else {
                Some(leaves[cur_idx - 1].clone())
            }
        } else {
            None
        };
        if leaves.len() > 1 {
            if let Some(tree) = self.pty_layout.as_mut() {
                tree.remove_leaf(target);
            }
        } else {
            // Last leaf — drop the tree entirely so single-pane
            // fallback re-engages if a future split repopulates it.
            self.pty_layout = None;
        }
        self.pty.remove(target);
        // Free the GPU texture if this was an image pane (no-op otherwise).
        if let Some(g) = self.gpu.as_mut() {
            g.drop_image(target);
        }
        self.md_content_h.remove(target);
        // Drop secondary-tab ptys hosted by this pane and prune the reverse
        // map. Without this, an in-pane tab's shell would linger past its
        // container pane and `find_tab_by_pty` would point at a dead outer.
        let secondary_pids: Vec<String> = {
            let ws = self.ws.lock().unwrap();
            ws.pid_to_pane
                .iter()
                .filter_map(|(pid, outer)| (outer == target).then(|| pid.clone()))
                .collect()
        };
        for pid in &secondary_pids {
            self.pty.remove(pid);
        }
        {
            let mut ws = self.ws.lock().unwrap();
            ws.panes.remove(target);
            ws.rebuild_pid_map();
            if was_active {
                ws.active_pane = next_focus;
            }
            // Layout shrank — every survivor needs a repaint, else the render
            // loop sees pane.dirty=false and skips the GPU pass, leaving the
            // closed pane's slot blank until the next dirty signal.
            for pane in ws.panes.values_mut() {
                pane.dirty = true;
            }
        }
        self.chrome_dirty = true;
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// Close `pid`'s pane: remove it from the BSP tree and drop its PTY.
    /// Shared by Cmd+W and the header × button. The window's last pane is a
    /// no-op — the OS close button quits a single-pane window, and a shell
    /// `exit` cascades through reap_dead_panes.
    pub(crate) fn close_pane(&mut self, pid: &str) {
        if self.tmux.is_some() {
            return;
        }
        let leaves = self.pty_layout.as_ref().map_or(0, |t| t.leaves().len());
        if leaves <= 1 {
            return;
        }
        self.remove_pane(pid);
    }
    /// Cmd+W: close the active *tab*. A pane with several tabs drops only the
    /// focused one — the rest stay alive (the "Cmd+W killed every bound tab /
    /// my claude pane" bug). Routes through `confirm_or_close_tab`, which both
    /// decides tab-vs-pane (last tab → pane, no-op on a single-pane window) and
    /// raises the "close while a job is running?" modal when needed.
    pub(crate) fn close_active_tab(&mut self) {
        let (pane, idx) = {
            let ws = self.ws.lock().unwrap();
            let Some(id) = ws.active_pane.clone() else { return };
            let idx = ws.panes.get(&id).map(|p| p.active_tab).unwrap_or(0);
            (id, idx)
        };
        self.confirm_or_close_tab(&pane, idx);
    }
    /// Cycle focus to the previous (delta=-1) or next (delta=+1) pane
    /// in document order. No-op when there's only one pane.
    pub(crate) fn cycle_focus(&self, delta: i32) {
        let Some(tree) = self.pty_layout.as_ref() else { return; };
        let leaves: Vec<String> = tree.leaves().iter().map(|s| s.to_string()).collect();
        if leaves.len() < 2 {
            return;
        }
        let mut ws = self.ws.lock().unwrap();
        let cur_idx = ws
            .active_pane
            .as_deref()
            .and_then(|id| leaves.iter().position(|l| l == id))
            .unwrap_or(0);
        let n = leaves.len() as i32;
        let new_idx = ((cur_idx as i32 + delta).rem_euclid(n)) as usize;
        let new_active = leaves[new_idx].clone();
        ws.active_pane = Some(new_active.clone());
        drop(ws);
        let _ = new_active;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// Pane whose rectangle lies immediately in `dir` of the active pane
    /// and overlaps it on the perpendicular axis. Picks the nearest by
    /// centre distance so a tall neighbour split into several panes still
    /// resolves to the one the user is pointing at. None when there is no
    /// pane on that side.
    pub(crate) fn adjacent_pane(&self, dir: FocusDir) -> Option<String> {
        let tree = self.pty_layout.as_ref()?;
        let (cols, rows) = self.window_cells();
        let rects = tree.leaf_rects(cols, rows);
        if rects.len() < 2 {
            return None;
        }
        let active = self.ws.lock().unwrap().active_pane.clone()?;
        let cur = rects.iter().find(|(id, ..)| id == &active)?;
        let (cx, cy, cw, ch) = (cur.1 as f32, cur.2 as f32, cur.3 as f32, cur.4 as f32);
        let (acx, acy) = (cx + cw / 2.0, cy + ch / 2.0);
        let mut best: Option<(String, f32)> = None;
        for (id, x, y, w, h) in &rects {
            if id == &active {
                continue;
            }
            let (x, y, w, h) = (*x as f32, *y as f32, *w as f32, *h as f32);
            let overlap_y = y < cy + ch && y + h > cy;
            let overlap_x = x < cx + cw && x + w > cx;
            let ok = match dir {
                FocusDir::Left => x + w <= cx + 1.0 && overlap_y,
                FocusDir::Right => x >= cx + cw - 1.0 && overlap_y,
                FocusDir::Up => y + h <= cy + 1.0 && overlap_x,
                FocusDir::Down => y >= cy + ch - 1.0 && overlap_x,
            };
            if !ok {
                continue;
            }
            let dist = (x + w / 2.0 - acx).abs() + (y + h / 2.0 - acy).abs();
            if best.as_ref().is_none_or(|(_, d)| dist < *d) {
                best = Some((id.clone(), dist));
            }
        }
        best.map(|(id, _)| id)
    }
    /// Move keyboard focus to the adjacent pane in `dir`.
    pub(crate) fn focus_dir(&self, dir: FocusDir) {
        if let Some(id) = self.adjacent_pane(dir) {
            self.ws.lock().unwrap().active_pane = Some(id);
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }
    /// Swap the active pane with its neighbour in `dir`. The BSP tree
    /// exchanges the two leaves' ids, so each pane's content moves into
    /// the other's slot while the PTYs stay put; focus rides along with
    /// the active id into its new position.
    pub(crate) fn swap_dir(&mut self, dir: FocusDir) {
        let Some(active) = self.ws.lock().unwrap().active_pane.clone() else {
            return;
        };
        let Some(target) = self.adjacent_pane(dir) else {
            return;
        };
        if let Some(tree) = self.pty_layout.as_mut() {
            tree.swap_leaves(&active, &target);
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// Pane whose header band contains the cursor (logical px), or None.
    /// Headers only exist when the workspace is split.
    pub(crate) fn header_at_px(&self, x: f32, y: f32) -> Option<String> {
        let (cols, rows) = self.window_cells();
        let rects = self.effective_leaf_rects(cols, rows);
        // A zoomed pane is a single rect but still has a header (to un-zoom),
        // so only bail on a lone pane when nothing is zoomed.
        if rects.len() <= 1 && self.zoomed_pane.is_none() {
            return None;
        }
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        for (id, cx, cy, cw, _ch) in rects {
            let bx = pad + cx as f32 * self.cell.w;
            let by = TITLE_HEIGHT + cy as f32 * self.cell.h;
            let bw = cw as f32 * self.cell.w;
            if x >= bx && x <= bx + bw && y >= by && y <= by + PANE_HEADER_HEIGHT {
                return Some(id);
            }
        }
        None
    }
    /// Pane + edge the cursor is over, for header drag-and-drop. The zone
    /// is the dominant axis from the pane box centre, so the cursor always
    /// resolves to one of the four edges. None when off every pane.
    pub(crate) fn drop_target_at(&self, x: f32, y: f32) -> Option<(String, DropZone)> {
        let tree = self.pty_layout.as_ref()?;
        let leaves_count = tree.leaves().len();
        let (cols, rows) = self.window_cells();
        let rects = self.effective_leaf_rects(cols, rows);
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        // Per-pane tab-strip top: where the pill row starts. Combined
        // with the header-band height below, this gives the full
        // pane-header region (even when a single-tab pane has a tiny
        // strip).
        let mut strip_top: HashMap<String, f32> = HashMap::new();
        for (pid, _, (_, ry, _, _)) in &self.pane_tab_rects {
            strip_top
                .entry(pid.clone())
                .and_modify(|t| { if *ry < *t { *t = *ry; } })
                .or_insert(*ry);
        }
        // When the layout has >1 leaf every pane gets a 30 logical-px
        // header band — including single-tab panes — so the box must
        // extend up by at least that amount or a drop onto a single-tab
        // header falls into the body's Up zone (split-up) instead of
        // Center (tab-merge), which was the "drag→merge gives split"
        // bug.
        let header_band = if leaves_count > 1 { PANE_HEADER_HEIGHT } else { 0.0 };
        for (id, cx, cy, cw, ch) in rects {
            let bx = pad + cx as f32 * self.cell.w;
            // pane_top = pane 시작 (헤더 띠 시작, chrome 포함). 기존엔
            // 이걸 본문 시작으로 잘못 가정해 box_top을 한 칸 위로
            // 잡았고 그래서 hit-test가 전부 30px 위로 shift됨 — 헤더 띠
            // 안 마우스가 본문 판정, 헤더 위(title bar 영역)가 헤더 판정.
            let pane_top = TITLE_HEIGHT + cy as f32 * self.cell.h;
            let bw = (cw as f32 * self.cell.w).max(1.0);
            let bh = (ch as f32 * self.cell.h).max(1.0);
            let body_top = pane_top + header_band;
            if x >= bx && x <= bx + bw && y >= pane_top && y <= pane_top + bh {
                // 헤더 띠 (pane_top ~ body_top) = Center (tab merge).
                // 본문 (body_top ~ pane_top+bh) = 4방향 split.
                if y < body_top {
                    return Some((id, DropZone::Center));
                }
                let dist_left = x - bx;
                let dist_right = bx + bw - x;
                let dist_top = y - body_top;
                let dist_bottom = (pane_top + bh) - y;
                let zone = if dist_left.min(dist_right) < dist_top.min(dist_bottom) {
                    if dist_left < dist_right { DropZone::Left } else { DropZone::Right }
                } else if dist_top < dist_bottom {
                    DropZone::Up
                } else {
                    DropZone::Down
                };
                return Some((id, zone));
            }
        }
        None
    }
    /// Window chip in the left sidebar under the cursor, resolved to that
    /// window's anchor leaf — the drop target for a cross-window header drag.
    /// Returns None when off every chip or over the already-active window (its
    /// panes are on screen, so an in-window drop is `drop_target_at`'s job).
    /// The daemon's `move_surface` does the actual cross-window detach/insert.
    pub(crate) fn sidebar_window_drop_target(&self, x: f32, y: f32) -> Option<String> {
        let inside =
            |r: &(f32, f32, f32, f32)| x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3;
        let idx = self
            .window_tab_rects
            .iter()
            .find(|(_, r)| inside(r))
            .map(|(i, _)| *i)?;
        if idx == self.active_window {
            return None;
        }
        self.windows
            .get(idx)
            .and_then(|w| w.as_ref())
            .and_then(|l| l.leaves().first().map(|s| s.to_string()))
    }
    /// Relocate `moving` next to `target` along the edge given by `zone`.
    /// Detaches the moving leaf (its PTY stays alive) and re-attaches it
    /// beside the target, then resizes every pane to its new rect. No-op
    /// when source and target are the same pane.
    pub(crate) fn move_pane(&mut self, moving: &str, target: &str, zone: DropZone) {
        if moving == target {
            return;
        }
        let (dir, before) = match zone {
            DropZone::Left => (kasa_pty::SplitDir::Horizontal, true),
            DropZone::Right => (kasa_pty::SplitDir::Horizontal, false),
            DropZone::Up => (kasa_pty::SplitDir::Vertical, true),
            DropZone::Down => (kasa_pty::SplitDir::Vertical, false),
            // Header drag onto a target's centre = ambiguous for a
            // whole-pane move; ignore rather than picking a random edge.
            DropZone::Center => return,
        };
        if let Some(tree) = self.pty_layout.as_mut() {
            if !tree.remove_leaf(moving) {
                return;
            }
            if !tree.insert_beside(target, dir, before, moving.to_string()) {
                // Target vanished (shouldn't happen) — re-attach beside
                // the first surviving leaf so the pane isn't orphaned.
                if let Some(anchor) = tree.leaves().first().map(|s| s.to_string()) {
                    tree.insert_beside(&anchor, dir, before, moving.to_string());
                }
            }
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.ws.lock().unwrap().active_pane = Some(moving.to_string());
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}
