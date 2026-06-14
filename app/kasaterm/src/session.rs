//! 세션·윈도우·cwd/label·daemon·pty·tmux/socket·스크린 펌프·상태 저장.
use super::*;

impl App {
    /// Drain a PtySession's screen-update channel into shared workspace
    /// state. Used both by `start_pty` (initial pane) and by
    /// `split_active_pane` (every additional pane), so the per-pane
    /// state arrives through the same path no matter when the session
    /// was spawned.
    /// Apply one decoded ScreenUpdate to the workspace: route to the right
    /// tab, reflow on size change, blit dirty rows, carry cursor/mode/title.
    /// Shared by the in-process channel pump (`pump_pty_screens`) and the
    /// daemon stream pump (`pump_daemon_stream`). The caller holds the ws lock
    /// and fires the redraw; this only mutates ws.
    pub(crate) fn apply_screen_update(ws: &mut Workspace, update: kasa_bridge::screen::ScreenUpdate) {
        if ws.active_pane.is_none() {
            ws.active_pane = Some(update.pane_id.clone());
        }
        // Route the update to the *tab* whose pid matches this stream.
        // Single-tab panes round-trip through the outer id; secondary
        // tabs spawned via the in-pane + button route through
        // `pid_to_pane`. Falls back to creating an outer pane entry
        // when the first update from a freshly-spawned shell arrives.
        let (pane, tab_idx) = match ws.find_tab_by_pty(&update.pane_id) {
            Some(p) => p,
            None => {
                // Brand-new pty id → create the outer PaneState with a
                // single tab that owns this pid. Seed pid_to_pane so
                // subsequent updates hit the O(1) path.
                let pane = ws.pane_mut(&update.pane_id);
                pane.tabs[0].pid = Some(update.pane_id.clone());
                ws.pid_to_pane
                    .insert(update.pane_id.clone(), update.pane_id.clone());
                let pane = ws.panes.get_mut(&update.pane_id).expect("just inserted");
                (pane, 0usize)
            }
        };
        let tab = &mut pane.tabs[tab_idx];
        let tp = tab.term_mut().expect("pty pane must be terminal");
        let resized = tp.cols != update.cols
            || tp.rows != update.rows
            || tp.cells.len() != update.rows as usize;
        if resized {
            // Preserve existing rows / columns through a resize so
            // the user sees their old content during the brief gap
            // between SIGWINCH and the shell's reflowed repaint —
            // otherwise the grid blanks for one frame and the
            // divider drag flickers visibly on every cell crossing.
            // Truncate / extend in place; the shell's subsequent
            // `update.dirty` overwrites the affected rows.
            tp.cols = update.cols;
            tp.rows = update.rows;
            let nr = update.rows as usize;
            let nc = update.cols as usize;
            tp.cells.truncate(nr);
            while tp.cells.len() < nr {
                tp.cells.push(vec![GridCell::blank(); nc]);
            }
            for row in &mut tp.cells {
                row.truncate(nc);
                while row.len() < nc {
                    row.push(GridCell::blank());
                }
            }
            tp.prev_cells.clear();
        }
        for (r, row) in update.dirty {
            if let Some(dst) = tp.cells.get_mut(r as usize) {
                *dst = row;
            }
        }
        // Shift detection on the pty side is retired — alacritty handles
        // scrollback natively via display_offset. Hand-rolled detection
        // breaks scroll-region TUIs (like Claude Code) when they write to sync.
        tp.cursor_row = update.cursor_row;
        tp.cursor_col = update.cursor_col;
        tp.cursor_visible = update.cursor_visible;
        tp.alt_screen = update.alt_screen;
        tp.mouse_enabled = update.mouse_enabled;
        tp.mouse_sgr = update.mouse_sgr;
        tp.app_cursor = update.app_cursor;
        // Carry the OSC 133 prompt-end mark only on frames that
        // actually emitted one; keep the last otherwise so a
        // mid-typing frame doesn't erase it.
        if let Some(pe) = update.prompt_end {
            tp.prompt_end = Some(pe);
        }
        // OSC 0/2 title from the inner program (Claude Code's
        // conversation summary, vim filename, etc.). Pinned panes
        // (renamed via surface.rename / run_job) keep their agent-set
        // label; only unpinned panes track OSC.
        if let Some(t) = update.title.clone() {
            if !tab.title_pinned {
                tab.title = Some(t);
            }
        }
        let _ = tab;
        pane.dirty = true;
    }
    pub(crate) fn pump_pty_screens(
        &self,
        screens: kasa_pty::ScreenReceiver<kasa_bridge::screen::ScreenUpdate>,
        pane_id: String,
    ) {
        let ws_screens = self.ws.clone();
        let win_screens = self.window.clone();
        let dead = self.dead_panes.clone();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            // winit's `request_redraw` is itself idempotent — repeated
            // calls within one frame coalesce into a single
            // RedrawRequested. The previous code added a 16ms throttle
            // on top of that, which had a sharp edge: a *single*
            // ScreenUpdate (the user hitting space, echoed once by the
            // PTY) that landed inside the 16ms window would be
            // dropped, and nothing would fire the next redraw until
            // the *next* update arrived — which for a space character
            // could be ~never. Result was a ~1s perceived cursor lag
            // after spacebar. Letting winit own the coalescing keeps
            // streaming-burst CPU bounded while making every dirty
            // frame visible.
            while let Ok(mut update) = screens.recv() {
                // EOF sentinel: the PTY reader died (shell/claude exited).
                // The PtySession keeps a Sender alive for scroll/resize, so
                // the channel never closes on its own — without this signal
                // the pane would linger as a zombie. Flag it dead and wake
                // the loop so reap_dead_panes drops it on the next turn.
                if update.eof {
                    dead.lock().unwrap().push(update.pane_id.clone());
                    if let Some(w) = win_screens.as_ref() {
                        w.request_redraw();
                    }
                    let _ = proxy.send_event(UserEvent::Redraw);
                    return;
                }
                // Coalesce: drain every other ScreenUpdate currently sitting
                // in the channel and merge them into one. Scroll inertia /
                // bursty Claude Code output can stuff hundreds of frames in
                // the queue between render cycles; processing each
                // separately means N ws-locks + N redraws + N renders. With
                // the merge we do ONE lock per burst, so direction reversals
                // and other late inputs aren't stuck behind a queue.
                loop {
                    match screens.try_recv() {
                        Ok(next) if !next.eof => {
                            let mut row_map: std::collections::HashMap<u16, Row> =
                                update.dirty.into_iter().collect();
                            for (r, row) in next.dirty {
                                row_map.insert(r, row);
                            }
                            let merged_dirty: Vec<(u16, Row)> =
                                row_map.into_iter().collect();
                            update = kasa_bridge::screen::ScreenUpdate {
                                dirty: merged_dirty,
                                ..next
                            };
                        }
                        Ok(next) => {
                            // EOF mid-burst: handle the current merge then
                            // signal death so reap fires next turn.
                            dead.lock().unwrap().push(next.pane_id.clone());
                            break;
                        }
                        Err(_) => break,
                    }
                }
                let mut ws = ws_screens.lock().unwrap();
                Self::apply_screen_update(&mut ws, update);
                drop(ws);
                if let Some(w) = win_screens.as_ref() {
                    w.request_redraw();
                }
                // Wake the loop even if it's parked on a WaitUntil —
                // request_redraw alone doesn't do that reliably on macOS.
                let _ = proxy.send_event(UserEvent::Redraw);
            }
            // Channel disconnected — the reader thread exited because
            // the PTY hit EOF (shell quit) or errored. Flag this pane
            // for the main thread to remove on its next tick.
            dead.lock().unwrap().push(pane_id);
            if let Some(w) = win_screens.as_ref() {
                w.request_redraw();
            }
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }
    /// Phase C path. Spawns the shell into a direct PTY (no tmux),
    /// hooks the screens channel into the same per-pane state the
    /// renderer expects. Single-pane MVP — the workspace holds one
    /// PaneState keyed "%0" and the layout is `None` (the render path
    /// falls back to single-pane when no layout has arrived).
    /// Spawn the first shell pane for the *current* (already-cleared) session.
    /// Mirrors start_pty's pane bring-up with a fresh pane id and no socket
    /// (re)init — used by new_session.
    pub(crate) fn spawn_session_pane(&mut self) -> Result<()> {
        let (cols, rows) = self.window_cells();
        let cwd = resolve_initial_cwd();
        let id = format!("%{}", self.next_pane_id);
        self.next_pane_id += 1;
        let session = kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            shell: self.pending_shell.take().or_else(resolve_default_shell),
            cwd,
            cols,
            rows,
            env: Vec::new(),
            pane_id: id.clone(),
            initial_scrollback: Vec::new(),
        })?;
        self.pump_pty_screens(session.screens.clone(), id.clone());
        self.pty.insert(id.clone(), Arc::new(session));
        self.pty_layout = Some(kasa_pty::PtyLayout::single(&id));
        self.ws.lock().unwrap().active_pane = Some(id);
        Ok(())
    }
    /// Create a new window inside the *current* session: stash the visible
    /// window's layout, then bring up a fresh window with a single new pane.
    /// The new pane's PTY joins the session's shared `pty` map and runs in the
    /// same `ws`, so it's a sibling of the existing windows — switching between
    /// them never tears a pane down. Windows are this session's tmux-style
    /// "windows"; the session list one level up is tmux "sessions".
    pub(crate) fn new_window(&mut self) {
        // Active window's slot is None — its layout lives in pty_layout. Park
        // it back into the slot before opening a new window.
        self.windows[self.active_window] = self.pty_layout.take();
        self.windows.push(None);
        self.active_window = self.windows.len() - 1;
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
        (0..self.windows.len()).find(|&i| {
            let layout = if i == self.active_window {
                self.pty_layout.as_ref()
            } else {
                self.windows[i].as_ref()
            };
            layout.is_some_and(|l| l.leaves().contains(&pane))
        })
    }
    pub(crate) fn switch_window(&mut self, idx: usize) {
        if idx == self.active_window || idx >= self.windows.len() {
            return;
        }
        if self.windows[idx].is_none() {
            return;
        }
        self.windows[self.active_window] = self.pty_layout.take();
        self.pty_layout = self.windows[idx].take();
        self.active_window = idx;
        // The user is now looking at this window — clear any unseen-notification
        // pulse on its sidebar tab.
        self.window_alert.remove(&idx);
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
    /// Close the window at `idx`. The last window can't be closed (a session
    /// always needs one). Every pane in the closed window is torn down — its
    /// PTY Arc dropped (kills the shell) and its render state removed — same
    /// teardown remove_pane uses. Closing the visible window swaps a neighbor
    /// in so the terminal keeps painting.
    pub(crate) fn close_window(&mut self, idx: usize) -> Result<()> {
        if self.windows.len() <= 1 {
            anyhow::bail!("cannot close the last window");
        }
        if idx >= self.windows.len() {
            anyhow::bail!("no such window: {idx}");
        }
        // Pull the closing window's layout (active one lives in pty_layout) and
        // kill every pane it owns.
        let layout = if idx == self.active_window {
            self.pty_layout.take()
        } else {
            self.windows[idx].take()
        };
        if let Some(layout) = layout {
            let mut ws = self.ws.lock().unwrap();
            for pane_id in layout.leaves() {
                self.pty.remove(pane_id);
                ws.panes.remove(pane_id);
            }
        }
        if idx == self.active_window {
            let target = if idx == 0 { 1 } else { idx - 1 };
            self.pty_layout = self.windows[target].take();
            self.windows.remove(idx);
            self.active_window = if target > idx { target - 1 } else { target };
            if let Some(first) = self
                .pty_layout
                .as_ref()
                .and_then(|l| l.leaves().first().map(|s| s.to_string()))
            {
                self.ws.lock().unwrap().active_pane = Some(first);
            }
        } else {
            self.windows.remove(idx);
            if idx < self.active_window {
                self.active_window -= 1;
            }
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        Ok(())
    }
    /// Refresh the per-window tab labels (window name + cwd). cwd resolution
    /// shells out to `lsof`, so this is throttled to ~1s and also re-runs
    /// whenever the window count changes (new/switch/close). The render path
    /// calls this each frame; the throttle keeps it cheap.
    pub(crate) fn refresh_window_labels(&mut self) {
        let now = Instant::now();
        let fresh = self.window_labels.len() == self.windows.len()
            && self
                .window_labels_at
                .is_some_and(|t| now.duration_since(t).as_millis() < 1000);
        if fresh {
            return;
        }
        let n = self.windows.len();
        let mut out = Vec::with_capacity(n);
        let ws = self.ws.lock().unwrap();
        for i in 0..n {
            // Representative pane = first leaf of the window's layout. The
            // active window's tree lives in pty_layout; the rest in windows[i].
            let repr = {
                let layout = if i == self.active_window {
                    self.pty_layout.as_ref()
                } else {
                    self.windows.get(i).and_then(|o| o.as_ref())
                };
                layout.and_then(|l| l.leaves().first().map(|s| s.to_string()))
            };
            // window.rename override (god marker) wins over the derived name —
            // it must hold even when the god pane isn't the representative leaf.
            let name = self
                .window_name_override
                .get(&i)
                .cloned()
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
                .unwrap_or_else(|| format!("win {}", i + 1));
            let cwd = repr
                .as_ref()
                .and_then(|id| self.pty.get(id))
                .and_then(|p| p.shell_pid())
                .and_then(socket::pid_cwd)
                .map(|p| Self::shorten_cwd(&p))
                .unwrap_or_default();
            out.push((name, cwd));
        }
        drop(ws);
        self.window_labels = out;
        self.window_labels_at = Some(now);
    }
    /// Compress a cwd for the sidebar: home → `~`, then keep the tail if it
    /// runs past `max` chars so the meaningful (deepest) part stays visible.
    /// 탭/헤더 라벨용. 셸이 idle이면 cwd의 마지막 폴더명, 명령 실행 중이면
    /// 그 프로세스명. zsh 4개로 안 보이고 위치/작업이 드러나게.
    pub(crate) fn smart_pane_label(sess: &kasa_pty::PtySession) -> Option<String> {
        let proc = sess.active_process_name().filter(|t| !t.is_empty());
        let is_shell = proc.as_deref().map_or(false, |p| {
            let base = p.strip_prefix('-').unwrap_or(p);
            matches!(base, "zsh" | "bash" | "fish" | "sh" | "dash" | "tcsh" | "ksh")
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
    /// The pane's tab-header label — custom title (rename/OSC) wins, else the
    /// live foreground process, else the raw `%N` id. Mirrors the header render
    /// path (render.rs) so the completion toast names a pane the same way the
    /// user sees it in the tab strip.
    pub(crate) fn pane_header_label(&self, id: &str) -> String {
        let title = {
            let ws = self.ws.lock().unwrap();
            ws.panes
                .get(id)
                .and_then(|p| p.title.clone())
                .filter(|t| !t.is_empty())
        };
        title
            .or_else(|| self.pty.get(id).and_then(|p| Self::smart_pane_label(p)))
            .unwrap_or_else(|| id.to_string())
    }
    /// cwd의 마지막 폴더명. 홈 디렉토리면 `~`.
    pub(crate) fn cwd_basename(p: &std::path::Path) -> String {
        if let Ok(h) = std::env::var("HOME") {
            if !h.is_empty() && p == std::path::Path::new(&h) {
                return "~".to_string();
            }
        }
        p.file_name()
            .and_then(|s| s.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| "/".to_string())
    }
    pub(crate) fn shorten_cwd(p: &std::path::Path) -> String {
        let raw = p.to_string_lossy().to_string();
        let s = match std::env::var("HOME") {
            Ok(h) if !h.is_empty() && raw.starts_with(&h) => format!("~{}", &raw[h.len()..]),
            _ => raw,
        };
        let max = 26usize;
        let chars: Vec<char> = s.chars().collect();
        if chars.len() > max {
            let tail: String = chars[chars.len() - (max - 1)..].iter().collect();
            format!("…{tail}")
        } else {
            s
        }
    }
    /// Refresh the per-pane shell cwd cache that feeds the header breadcrumb.
    /// `pid_cwd` shells out to `lsof`, so resolving it per pane on every frame
    /// would spawn a burst during a scroll/hover storm. Rate-limited to
    /// ~700ms — a breadcrumb only moves on `cd`, so the lag is imperceptible.
    pub(crate) fn refresh_pane_cwds(&mut self) {
        // Daemon-attached mode keeps self.pty empty — the breadcrumb cache is
        // filled from the daemon's StateView instead (see UserEvent::DaemonState).
        // Bail so we never wipe that; only the in-process PTY backend fills
        // self.pty and needs this lsof sweep.
        if self.pty.is_empty() {
            return;
        }
        if let Some(t) = self.pane_cwd_check {
            if t.elapsed() < std::time::Duration::from_millis(700) {
                return;
            }
        }
        self.pane_cwd_check = Some(Instant::now());
        let mut cache = HashMap::new();
        for (id, sess) in &self.pty {
            if let Some(cwd) = sess.shell_pid().and_then(socket::pid_cwd) {
                cache.insert(id.clone(), cwd);
            }
        }
        self.pane_cwd_cache = cache;
    }
    /// A pane's current shell cwd — cache first (refreshed ~700ms), else a live
    /// `lsof` on its shell pid so a just-spawned pane (not yet in the cache)
    /// still resolves. Used to inherit the cwd into a sibling on split/tab.
    pub(crate) fn pane_current_cwd(&self, id: &str) -> Option<std::path::PathBuf> {
        if let Some(p) = self.pane_cwd_cache.get(id) {
            return Some(p.clone());
        }
        self.pty
            .get(id)
            .and_then(|s| s.shell_pid())
            .and_then(socket::pid_cwd)
    }
    /// cwd for a shell about to be spawned off `prev_pane` (the pane being split
    /// or tabbed). Threads the spawning pane's live cwd into `resolve_spawn_cwd`
    /// so the `"last"` setting behaves like other terminals' "reuse previous
    /// directory" mode.
    pub(crate) fn spawn_cwd_from(&self, prev_pane: Option<&str>) -> Option<String> {
        let prev = prev_pane.and_then(|id| self.pane_current_cwd(id));
        resolve_spawn_cwd(prev)
    }
    /// Recompute the sidebar file tree when its root (the active pane's cwd)
    /// changes — pane switch or `cd`. Cheap string compare per frame; the
    /// read_dir walk only runs on a real change (or after expand/collapse,
    /// which calls `rebuild_file_tree_nodes` directly).
    pub(crate) fn refresh_file_tree(&mut self) {
        self.ensure_file_tree_watcher();
        // A background watcher flagged an on-disk change (file added / removed /
        // renamed / modified) — rebuild even if the root is unchanged.
        if self
            .file_tree.fs_dirty
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            self.rebuild_file_tree_nodes();
        }
        let active = self.ws.lock().ok().and_then(|w| w.active_pane.clone());
        let root = active
            .as_ref()
            .and_then(|id| self.pane_cwd_cache.get(id).cloned())
            // Preview panes (markdown/image splits) have no cwd in the cache —
            // keep the current tree root rather than snapping to the process
            // cwd, so opening a file doesn't reshuffle the sidebar root.
            .or_else(|| self.file_tree.root.clone())
            .or_else(|| std::env::current_dir().ok());
        if root == self.file_tree.root {
            return;
        }
        // Open the new root by default so the sidebar shows its contents
        // immediately rather than a single collapsed folder row.
        if let Some(r) = &root {
            self.file_tree.expanded.insert(r.clone());
        }
        self.file_tree.root = root;
        self.file_tree.scroll = 0.0;
        self.rebuild_file_tree_nodes();
    }
    /// Spawn the file-tree FS poller once. It watches the dirs in
    /// `file_tree_watch` (root + expanded folders, kept current by
    /// `rebuild_file_tree_nodes`), hashing each entry's name/mtime/kind every
    /// ~800ms; on any change it sets `file_tree_fs_dirty` and wakes the loop so
    /// `refresh_file_tree` rebuilds. Polling lives off the GUI thread, so the
    /// event-driven loop stays parked until the disk actually changes.
    pub(crate) fn ensure_file_tree_watcher(&mut self) {
        if self.file_tree.watch_started {
            return;
        }
        self.file_tree.watch_started = true;
        let watch = self.file_tree.watch.clone();
        let dirty = self.file_tree.fs_dirty.clone();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let mut last: u64 = 0;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(800));
                let dirs = watch.lock().map(|d| d.clone()).unwrap_or_default();
                if dirs.is_empty() {
                    continue;
                }
                let mut sig: u64 = 1469598103934665603; // FNV offset basis
                let mut mix = |bytes: &[u8]| {
                    for &b in bytes {
                        sig ^= b as u64;
                        sig = sig.wrapping_mul(1099511628211);
                    }
                };
                for dir in &dirs {
                    let Ok(rd) = std::fs::read_dir(dir) else { continue };
                    for ent in rd.flatten() {
                        mix(ent.file_name().as_encoded_bytes());
                        if let Ok(md) = ent.metadata() {
                            mix(&[md.is_dir() as u8]);
                            if let Ok(mt) = md.modified() {
                                if let Ok(d) = mt.duration_since(std::time::UNIX_EPOCH) {
                                    mix(&d.as_secs().to_le_bytes());
                                }
                            }
                        }
                    }
                }
                if sig != last {
                    last = sig;
                    dirty.store(true, std::sync::atomic::Ordering::Relaxed);
                    let _ = proxy.send_event(UserEvent::Redraw);
                }
            }
        });
    }
    /// Spawn the `git check-ignore` worker once. It drains `git_ignore_req`
    /// (set by `rebuild_file_tree_nodes`), runs the batched ignore check off
    /// the GUI thread — so Defender's ~5s scan of the spawned git never
    /// freezes the file-tree toggle — and on a changed result fills
    /// `file_tree_ignored` + sets `file_tree_fs_dirty` so the next refresh
    /// re-dims rows. Skips a request identical to the last one it ran, so a
    /// repeated rebuild can't loop git forever.
    pub(crate) fn ensure_git_ignore_worker(&mut self) {
        if self.git_ignore_started {
            return;
        }
        self.git_ignore_started = true;
        let req = self.git_ignore_req.clone();
        let cache = self.file_tree.ignored.clone();
        let dirty = self.file_tree.fs_dirty.clone();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let mut last: Option<(std::path::PathBuf, Vec<String>)> = None;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(150));
                let job = req.lock().ok().and_then(|mut r| r.take());
                let Some((root, paths)) = job else { continue };
                if last.as_ref() == Some(&(root.clone(), paths.clone())) {
                    continue;
                }
                last = Some((root.clone(), paths.clone()));
                let result = kasa_mcp::git::git_ignored(&root, &paths);
                let mut guard = match cache.lock() {
                    Ok(g) => g,
                    Err(_) => break,
                };
                if *guard != result {
                    *guard = result;
                    drop(guard);
                    dirty.store(true, std::sync::atomic::Ordering::Relaxed);
                    if proxy.send_event(UserEvent::Redraw).is_err() {
                        break;
                    }
                }
            }
        });
    }
    /// Open a sidebar file in a fresh split pane (right of the active pane).
    /// Images decode into an `Image` pane; real markdown renders as a laid-out
    /// doc; any other text loads as a fenced code block so the highlighter
    /// colors it. Re-opening a file already on screen just focuses its pane
    /// instead of stacking duplicate splits. PTY-less — `resize_backend` skips
    /// leaves with no `self.pty` entry, so the new pane never spawns a shell.
    pub(crate) fn open_file_split(&mut self, path: std::path::PathBuf) {
        if self.tmux.is_some() {
            return;
        }
        // Already open? Focus that pane rather than spawning a duplicate.
        let existing = {
            let ws = self.ws.lock().unwrap();
            ws.panes.iter().find_map(|(id, p)| {
                p.tabs
                    .iter()
                    .any(|t| t.preview_path.as_deref() == Some(path.as_path()))
                    .then(|| id.clone())
            })
        };
        if let Some(id) = existing {
            self.ws.lock().unwrap().active_pane = Some(id);
            self.chrome_dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }

        let new_id = format!("%{}", self.next_pane_id);
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let is_image = matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "tif" | "ico"
        );
        let content = if is_image {
            match decode_image_rgba(&path) {
                Ok(img) => PaneContent::Image(Arc::new(img)),
                Err(e) => {
                    eprintln!("[open] 이미지 디코드 실패 {}: {e}", path.display());
                    return;
                }
            }
        } else {
            let raw = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("[open] 파일 읽기 실패 {}: {e}", path.display());
                    return;
                }
            };
            let is_md = matches!(ext.as_str(), "md" | "markdown");
            let doc = Arc::new(build_markdown_doc(&new_id, &path, &raw));
            // Markdown renders as a laid-out doc; code/text opens straight into
            // the raw editor (line-number gutter + syntax highlight + editable)
            // — the fenced-code-block render path mangled long lines and was
            // read-only, which is wrong for source files.
            let edit_lines: Vec<String> = if is_md {
                Vec::new()
            } else {
                raw.split('\n').map(|s| s.to_string()).collect()
            };
            PaneContent::Markdown(MarkdownPane {
                doc,
                is_md_doc: is_md,
                raw_mode: !is_md,
                edit_lines,
                cur_line: 0,
                cur_col: 0,
                scroll: 0,
                h_scroll: 0.0,
            })
        };

        let active = self.ws.lock().unwrap().active_pane.clone();
        let Some(active) = active else {
            return;
        };
        self.next_pane_id += 1;
        let title = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string());
        let mut tab = PaneTab::default();
        tab.content = content;
        tab.title = title;
        tab.title_pinned = true;
        tab.preview_path = Some(path.clone());
        // Headless verification of the zoom + pan crop (mouse drags aren't
        // injectable in a background run). KASATERM_TEST_IMG_ZOOM sets the
        // initial zoom; KASATERM_TEST_IMG_PAN="x,y" the initial pan (logical
        // px). Only meaningful for image panes.
        if is_image {
            if let Some(z) = std::env::var("KASATERM_TEST_IMG_ZOOM")
                .ok()
                .and_then(|s| s.parse::<f32>().ok())
            {
                tab.image_zoom = z;
            }
            if let Some((px, py)) = std::env::var("KASATERM_TEST_IMG_PAN").ok().and_then(|s| {
                let (a, b) = s.split_once(',')?;
                Some((a.trim().parse::<f32>().ok()?, b.trim().parse::<f32>().ok()?))
            }) {
                tab.image_pan_x = px;
                tab.image_pan_y = py;
            }
        }
        let ps = PaneState { tabs: vec![tab], active_tab: 0, color: None, dirty: true };
        self.ws.lock().unwrap().panes.insert(new_id.clone(), ps);

        let layout = self.pty_layout.as_mut().expect("pty_layout set in start_pty");
        if !layout.split_leaf(&active, kasa_pty::SplitDir::Horizontal, new_id.clone()) {
            // Active pane isn't in the tree — undo the orphan insert.
            self.ws.lock().unwrap().panes.remove(&new_id);
            self.next_pane_id -= 1;
            return;
        }
        self.ws.lock().unwrap().active_pane = Some(new_id);
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// Walk the root + every expanded folder into the flat `file_tree_nodes`.
    pub(crate) fn rebuild_file_tree_nodes(&mut self) {
        self.file_tree.nodes.clear();
        if let Some(root) = self.file_tree.root.clone() {
            // Show the project root itself as the first row (depth 0) so the
            // sidebar is anchored on the folder you're in, not a rootless list
            // of its children. Its contents nest under it at depth 1+.
            let root_name = root
                .file_name()
                .map(|n| nfc_hangul(&n.to_string_lossy()))
                .unwrap_or_else(|| root.to_string_lossy().into_owned());
            self.file_tree.nodes.push(FileNode {
                path: root.clone(),
                name: root_name,
                is_dir: true,
                depth: 0,
                ignored: false,
            });
            if self.file_tree.expanded.contains(&root) {
                Self::walk_dir(&root, 1, &self.file_tree.expanded, &mut self.file_tree.nodes);
            }
            // Second pass: one batched `git check-ignore` over every visible
            // path marks the gitignored rows italic+dim. Dotfiles get the same
            // treatment regardless (check-ignore won't flag a tracked dotfile).
            let paths: Vec<String> = self
                .file_tree.nodes
                .iter()
                .map(|n| n.path.to_string_lossy().into_owned())
                .collect();
            // Dim dotfiles + whatever the background worker last resolved.
            // `git check-ignore` is NOT run inline — spawning git from the
            // unsigned exe stalls ~5s under Defender, which would freeze the
            // toggle. We hand the worker this (root, paths) and apply its
            // cached result; the worker wakes us when fresh ignores land.
            let ignored = self
                .file_tree.ignored
                .lock()
                .map(|g| g.clone())
                .unwrap_or_default();
            for n in &mut self.file_tree.nodes {
                n.ignored = n.name.starts_with('.')
                    || ignored.contains(n.path.to_string_lossy().as_ref());
            }
            if let Ok(mut req) = self.git_ignore_req.lock() {
                *req = Some((root.clone(), paths));
            }
            self.ensure_git_ignore_worker();
        }
        // Hand the FS watcher the dirs currently on screen (root + each expanded
        // folder) so it polls exactly what the user can see change.
        if let Ok(mut watch) = self.file_tree.watch.lock() {
            watch.clear();
            if let Some(root) = &self.file_tree.root {
                watch.push(root.clone());
            }
            watch.extend(
                self.file_tree.nodes
                    .iter()
                    .filter(|n| n.is_dir && self.file_tree.expanded.contains(&n.path))
                    .map(|n| n.path.clone()),
            );
        }
    }
    /// Rebuild `file_tree_nodes` as flat whole-tree search hits for the current
    /// query (empty → restore the normal expanded tree). Recurses every folder
    /// (not just expanded ones) so a collapsed branch is still searchable, but
    /// skips heavy/ignored dirs and caps results so a huge repo can't stall the
    /// GUI. Matches are flattened to depth 0 — a hit list, not a tree.
    pub(crate) fn file_tree_search_collect(&mut self) {
        let q = self.file_tree.search_query.to_lowercase();
        if q.is_empty() {
            self.rebuild_file_tree_nodes();
            return;
        }
        self.file_tree.nodes.clear();
        if let Some(root) = self.file_tree.root.clone() {
            Self::search_walk(&root, &q, 0, &mut self.file_tree.nodes);
        }
    }
    /// Depth-bounded recursive name search. `.git`, deep nests, and the usual
    /// build/dep dirs are skipped (they're huge and gitignored anyway); the hit
    /// list is capped at 300 so the worst case stays bounded.
    fn search_walk(dir: &std::path::Path, q: &str, depth: usize, out: &mut Vec<FileNode>) {
        if out.len() >= 300 || depth > 7 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let heavy = ["node_modules", "target", "dist", ".git", "build", ".next"];
        let mut subdirs: Vec<std::path::PathBuf> = Vec::new();
        for e in rd.filter_map(|e| e.ok()) {
            let name = nfc_hangul(&e.file_name().to_string_lossy());
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if name.to_lowercase().contains(q) {
                out.push(FileNode { path: e.path(), name: name.clone(), is_dir, depth: 0, ignored: false });
                if out.len() >= 300 {
                    return;
                }
            }
            if is_dir && !heavy.contains(&name.as_str()) {
                subdirs.push(e.path());
            }
        }
        for sub in subdirs {
            Self::search_walk(&sub, q, depth + 1, out);
            if out.len() >= 300 {
                return;
            }
        }
    }
    /// Move a tree entry into `dst_dir` (drag-and-drop in the sidebar). No-ops
    /// when the move is meaningless or unsafe: already in that dir, dropping a
    /// folder onto itself or a descendant, or a name clash at the target.
    pub(crate) fn move_tree_entry(&mut self, src: &std::path::Path, dst_dir: &std::path::Path) {
        if !dst_dir.is_dir() {
            return;
        }
        let Some(name) = src.file_name() else { return };
        if src.parent() == Some(dst_dir) {
            return; // already here
        }
        if dst_dir == src || dst_dir.starts_with(src) {
            return; // would move a folder inside itself
        }
        let target = dst_dir.join(name);
        if target.exists() {
            self.set_toast(format!("이미 있음: {}", name.to_string_lossy()));
            return;
        }
        if let Err(e) = std::fs::rename(src, &target) {
            self.set_toast(format!("이동 실패: {e}"));
            return;
        }
        // Carry the expanded state across the move and reveal the drop target.
        if self.file_tree.expanded.remove(src) {
            self.file_tree.expanded.insert(target.clone());
        }
        self.file_tree.expanded.insert(dst_dir.to_path_buf());
        self.rebuild_file_tree_nodes();
    }
    /// Move every selected entry (primary + Cmd/Shift multi-select) to the OS
    /// trash, clear the selection, refresh. One toast covers the whole batch.
    pub(crate) fn delete_tree_selection(&mut self) {
        let mut targets: Vec<std::path::PathBuf> =
            self.file_tree.selected_more.iter().cloned().collect();
        if let Some(p) = self.file_tree.selected.clone() {
            targets.push(p);
        }
        targets.sort();
        targets.dedup();
        if targets.is_empty() {
            return;
        }
        let total = targets.len();
        let mut ok = 0usize;
        let mut last_name = String::new();
        for path in &targets {
            if trash::delete(path).is_ok() {
                self.file_tree.expanded.remove(path);
                ok += 1;
                last_name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
            }
        }
        self.file_tree.selected = None;
        self.file_tree.selected_more.clear();
        if ok == 0 {
            self.set_toast("삭제 실패".to_string());
        } else if total == 1 {
            self.set_toast(format!("휴지통으로 이동: {last_name}"));
        } else if ok == total {
            self.set_toast(format!("휴지통으로 이동: {total}개"));
        } else {
            self.set_toast(format!("휴지통으로 이동: {ok}/{total}개"));
        }
        self.rebuild_file_tree_nodes();
    }
    /// Create the entry the inline "new file/folder" row is naming, under the
    /// current tree root, then clear the entry and refresh the tree.
    pub(crate) fn commit_new_entry(&mut self) {
        let Some((is_dir, name)) = self.file_tree.new.take() else { return };
        // Right-click menu pins a parent folder; the toolbar buttons leave it
        // None and fall back to the tree root.
        let parent = self
            .file_tree
            .new_parent
            .take()
            .or_else(|| self.file_tree.root.clone());
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        if let Some(parent) = parent {
            let path = parent.join(&name);
            if path.exists() {
                self.set_toast(format!("이미 있음: {name}"));
                return;
            }
            let res = if is_dir {
                std::fs::create_dir(&path)
            } else {
                std::fs::File::create(&path).map(|_| ())
            };
            match res {
                Ok(()) => {
                    self.file_tree.expanded.insert(parent.clone());
                    if is_dir {
                        self.file_tree.expanded.insert(path.clone());
                    }
                }
                Err(e) => self.set_toast(format!("생성 실패: {e}")),
            }
        }
        self.rebuild_file_tree_nodes();
    }
    /// Apply the inline rename: `fs::rename` the target to the edited name in its
    /// own parent. Carries expanded/selected state across; no-ops on empty /
    /// unchanged / name clash.
    pub(crate) fn commit_rename(&mut self) {
        let Some((path, name)) = self.file_tree.rename.take() else { return };
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        let Some(parent) = path.parent() else { return };
        let target = parent.join(&name);
        if target == path {
            return;
        }
        if target.exists() {
            self.set_toast(format!("이미 있음: {name}"));
            return;
        }
        match std::fs::rename(&path, &target) {
            Ok(()) => {
                if self.file_tree.expanded.remove(&path) {
                    self.file_tree.expanded.insert(target.clone());
                }
                if self.file_tree.selected.as_deref() == Some(path.as_path()) {
                    self.file_tree.selected = Some(target.clone());
                }
                self.file_tree.selected_more.remove(&path);
                self.rebuild_file_tree_nodes();
            }
            Err(e) => self.set_toast(format!("이름변경 실패: {e}")),
        }
    }
    /// Recursive read_dir: folders first then files (case-insensitive), dotfiles
    /// skipped, descending only into expanded folders.
    pub(crate) fn walk_dir(
        dir: &std::path::Path,
        depth: usize,
        expanded: &std::collections::HashSet<std::path::PathBuf>,
        out: &mut Vec<FileNode>,
    ) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<FileNode> = rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = nfc_hangul(&e.file_name().to_string_lossy());
                // `.git` is the one dotfile we hide: expanding it floods the
                // tree with thousands of object files. Everything else (.claude,
                // .gitignore …) shows, just italic + dim (set in rebuild).
                if name == ".git" {
                    return None;
                }
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                Some(FileNode { path: e.path(), name, is_dir, depth, ignored: false })
            })
            .collect();
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        for node in entries {
            let (is_dir, path) = (node.is_dir, node.path.clone());
            out.push(node);
            if is_dir && expanded.contains(&path) {
                Self::walk_dir(&path, depth + 1, expanded, out);
            }
        }
    }
    /// Geometry of the left window-tab sidebar, in logical px. Returns
    /// `(tab_rects, close_rects, plus_rect)`:
    ///   - one `(window_idx, rect)` tab per window, stacked under the title
    ///     strip,
    ///   - one `(window_idx, ×-rect)` per window *only* when more than one
    ///     window exists (the last window can't be closed),
    ///   - the "+" new-window button rect under the last tab.
    /// Pure read of `windows.len()` so the render path and the mouse
    /// hit-test agree on every rect. `win_h` is the logical window height
    /// (unused today but kept so a future scroll/overflow clamp has it).
    pub(crate) fn sidebar_layout(
        &self,
        _win_h: f32,
    ) -> (
        Vec<(usize, (f32, f32, f32, f32))>,
        Vec<(usize, (f32, f32, f32, f32))>,
        (f32, f32, f32, f32),
    ) {
        let n = self.windows.len();
        let tab_x = SIDEBAR_TAB_INSET;
        let tab_w = (self.sidebar_w_logical - 2.0 * SIDEBAR_TAB_INSET).max(0.0);
        let top = TITLE_HEIGHT + 8.0;
        let stride = SIDEBAR_TAB_H + SIDEBAR_TAB_GAP;
        let mut tabs = Vec::with_capacity(n);
        let mut closes = Vec::new();
        for i in 0..n {
            let y = top + i as f32 * stride;
            tabs.push((i, (tab_x, y, tab_w, SIDEBAR_TAB_H)));
            if n > 1 {
                let cs = 14.0;
                closes.push((i, (tab_x + tab_w - cs - 3.0, y + 3.0, cs, cs)));
            }
        }
        let plus_y = top + n as f32 * stride;
        let plus = (tab_x, plus_y, tab_w, 28.0);
        (tabs, closes, plus)
    }
    pub(crate) fn start_pty(&mut self) -> Result<()> {
        let _window = self.window.as_ref().expect("window before pty");
        // Local PTY mode: spawn one pane in *this* process and bring up the
        // cmux socket server (claude tmux shim, kasaterm-cli, pane collab)
        // backed by our own panes. No daemon — split/focus are immediate
        // local ops; session continuity comes from claude --resume on relaunch
        // (load_local_session, follow-up).
        // Socket server FIRST so KASATERM_SOCKET_PATH is exported into the
        // process env *before* the first pane's shell is spawned — otherwise
        // pane %1 (and only %1) inherits an empty socket path and can't reach
        // the board/bind-transcript, while later split panes get it fine.
        self.start_socket_pty();
        self.spawn_session_pane()?;
        // 터미널 부팅 시 claude 자동 실행은 폐기(거노 06-14: "터미널 켜자마자 claude
        // 가 켜지는 게 이상"). god 은 GUI(아로나 패널) 켤 때 promote_active_pane_to_god
        // 가 쓰던 활성 세션을 승격한다. pending_autoleader 는 항상 None.
        self.pending_autoleader = None;
        self.pending_autoleader_at = None;
        Ok(())
    }
    /// Serialize every session (active + stashed) as a layout tree so the next
    /// launch can restore the full multi-pane, multi-session workspace. Written
    /// on exit by save_session_state.
    pub(crate) fn save_session_state(&self) {
        let mut sessions_json = Vec::new();
        for i in 0..self.sessions.len() {
            // Each session contributes all its windows. The active session's
            // live state is in self.{pty,pty_layout,windows,active_window};
            // stashed sessions carry the same fields on their Session.
            let (pty, active_layout, windows, active_window, ws_arc) = if i == self.active_session {
                (
                    &self.pty,
                    self.pty_layout.as_ref(),
                    &self.windows,
                    self.active_window,
                    &self.ws,
                )
            } else {
                match self.sessions[i].as_ref() {
                    Some(s) => (
                        &s.pty,
                        s.pty_layout.as_ref(),
                        &s.windows,
                        s.active_window,
                        &s.ws,
                    ),
                    None => continue,
                }
            };
            // Lock this session's workspace once so each leaf can read its
            // pane scrollback while serializing the window trees.
            let ws_guard = ws_arc.lock().unwrap();
            // Serialize every window. The active window's tree lives in
            // active_layout; the rest sit in `windows[j]` (active slot None).
            let mut windows_json = Vec::new();
            let mut new_active = 0usize;
            for (j, slot) in windows.iter().enumerate() {
                let layout = if j == active_window {
                    active_layout
                } else {
                    slot.as_ref()
                };
                let Some(layout) = layout else { continue };
                if j == active_window {
                    new_active = windows_json.len();
                }
                windows_json.push(Self::layout_to_json(layout, pty, &ws_guard));
            }
            if windows_json.is_empty() {
                continue;
            }
            sessions_json.push(serde_json::json!({
                "windows": windows_json,
                "active_window": new_active,
            }));
        }
        if sessions_json.is_empty() {
            return;
        }
        let state = serde_json::json!({
            "active_session": self.active_session,
            "sessions": sessions_json,
        });
        socket::write_session_state(&state);
    }
    /// Walk a live PtyLayout into the nested JSON the restore loader reads,
    /// resolving each leaf's pane id to its cwd/claude record.
    pub(crate) fn layout_to_json(
        layout: &kasa_pty::PtyLayout,
        pty: &HashMap<String, Arc<kasa_pty::PtySession>>,
        ws: &Workspace,
    ) -> serde_json::Value {
        match layout {
            kasa_pty::PtyLayout::Leaf { pane_id } => {
                let mut rec = pty
                    .get(pane_id)
                    .map(|s| socket::pane_record(s))
                    .unwrap_or(serde_json::Value::Null);
                // Attach the pane's scrollback (text lines) so restore can
                // repaint what was on screen. Only when we have a real record.
                if let Some(obj) = rec.as_object_mut() {
                    let sb = ws
                        .panes
                        .get(pane_id)
                        .map(scrollback_lines)
                        .unwrap_or_default();
                    obj.insert("scrollback".to_string(), serde_json::json!(sb));
                }
                serde_json::json!({ "leaf": rec })
            }
            kasa_pty::PtyLayout::Split { dir, ratio, a, b } => {
                let dir = match dir {
                    kasa_pty::SplitDir::Horizontal => "h",
                    kasa_pty::SplitDir::Vertical => "v",
                };
                serde_json::json!({ "split": {
                    "dir": dir,
                    "ratio": ratio,
                    "a": Self::layout_to_json(a, pty, ws),
                    "b": Self::layout_to_json(b, pty, ws),
                }})
            }
        }
    }
    pub(crate) fn start_tmux(&mut self) -> Result<()> {
        let _window = self.window.as_ref().expect("window before tmux");
        let (cols, rows) = self.window_cells();
        let cwd = resolve_initial_cwd();
        let tmux = TmuxSession::start(StartOptions {
            cwd: cwd.as_deref(),
            socket_name: Some("kasaterm"),
            cols,
            rows,
            ..Default::default()
        })?;
        // Screens thread: each ScreenUpdate carries a pane_id; routes to
        // the matching PaneState in the workspace. New pane ids appear
        // automatically when tmux split-window creates them.
        let screens = tmux.screens.clone();
        let ws_screens = self.ws.clone();
        let win_screens = self.window.clone();
        std::thread::spawn(move || {
            while let Ok(ScreenUpdate {
                pane_id,
                rows,
                cols,
                dirty,
                cursor_row,
                cursor_col,
                cursor_visible,
                alt_screen,
                mouse_enabled,
                mouse_sgr,
                title,
                ..
            }) = screens.recv()
            {
                let mut ws = ws_screens.lock().unwrap();
                // First-seen pane becomes the active one so the user
                // doesn't open into a workspace with no focus.
                if ws.active_pane.is_none() {
                    ws.active_pane = Some(pane_id.clone());
                }
                let is_active = ws.active_pane.as_deref() == Some(pane_id.as_str());
                let pane = ws.pane_mut(&pane_id);
                let tp = pane.term_mut().expect("tmux pane must be terminal");
                let resized = tp.cols != cols
                    || tp.rows != rows
                    || tp.cells.len() != rows as usize;
                if resized {
                    // Preserve content across resize — see the PTY-path
                    // copy of this branch for the rationale.
                    tp.cols = cols;
                    tp.rows = rows;
                    let nr = rows as usize;
                    let nc = cols as usize;
                    tp.cells.truncate(nr);
                    while tp.cells.len() < nr {
                        tp.cells.push(vec![GridCell::blank(); nc]);
                    }
                    for row in &mut tp.cells {
                        row.truncate(nc);
                        while row.len() < nc {
                            row.push(GridCell::blank());
                        }
                    }
                    tp.prev_cells.clear();
                }
                for (r, row) in dirty {
                    if let Some(dst) = tp.cells.get_mut(r as usize) {
                        *dst = row;
                    }
                }
                // Shift detection per pane — alt-screen apps manage their
                // own scrollback so we skip there.
                if !alt_screen
                    && !tp.prev_cells.is_empty()
                    && tp.prev_cells.len() == tp.cells.len()
                {
                    let n = tp.prev_cells.len();
                    let mut shifted = 0usize;
                    for k in 1..n {
                        if tp.prev_cells[k..] == tp.cells[..n - k] {
                            shifted = k;
                            break;
                        }
                    }
                    if shifted > 0 {
                        for row in &tp.prev_cells[..shifted] {
                            tp.history.push_back(row.clone());
                        }
                        while tp.history.len() > SCROLLBACK_MAX {
                            tp.history.pop_front();
                        }
                    }
                }
                tp.prev_cells = tp.cells.clone();
                tp.cursor_row = cursor_row;
                tp.cursor_col = cursor_col;
                tp.cursor_visible = cursor_visible;
                tp.alt_screen = alt_screen;
                tp.mouse_enabled = mouse_enabled;
                tp.mouse_sgr = mouse_sgr;
                let new_title = title.filter(|t| !t.is_empty());
                // Pinned panes (renamed via surface.rename / run_job) ignore
                // OSC titles so the agent-set label stays put.
                let title_changed = !pane.title_pinned && pane.title != new_title;
                if title_changed {
                    pane.title = new_title.clone();
                }
                drop(ws);
                if let Some(w) = win_screens.as_ref() {
                    // Only the active pane's title shows in the window
                    // chrome — background panes change silently.
                    if title_changed && is_active {
                        let display =
                            new_title.unwrap_or_else(|| "kasaterm".into());
                        w.set_title(&display);
                    }
                    w.request_redraw();
                }
            }
        });
        // Events thread: parses %layout-change messages so render_frame
        // can lay panes out. Without this, splits would create panes
        // we have screen state for but no rect to draw them at.
        let events = tmux.events.clone();
        let ws_events = self.ws.clone();
        let win_events = self.window.clone();
        std::thread::spawn(move || {
            while let Ok(evt) = events.recv() {
                match evt {
                    TmuxEvent::LayoutChange { layout, .. } => {
                        // tmux's %layout-change emits both the visible
                        // and default layouts in one message,
                        // space-separated, plus a trailing flag.
                        // parse_layout wants exactly one layout
                        // string, so take the first token.
                        let first = layout
                            .split_whitespace()
                            .next()
                            .unwrap_or(&layout);
                        match parse_layout(first) {
                            Ok(parsed) => {
                                let mut ws = ws_events.lock().unwrap();
                                ws.layout = Some(parsed);
                                drop(ws);
                                if let Some(w) = win_events.as_ref() {
                                    w.request_redraw();
                                }
                            }
                            Err(e) => {
                                eprintln!("[layout] parse failed: {e} ({first:?})");
                            }
                        }
                    }
                    TmuxEvent::WindowPaneChanged { pane_id, .. } => {
                        // tmux flipped the active pane (most commonly:
                        // a split-window just landed and the new pane
                        // grabbed focus). Mirror that into our state
                        // so the cursor + active border + outgoing key
                        // target all move together.
                        let mut ws = ws_events.lock().unwrap();
                        if ws.active_pane.as_deref() != Some(pane_id.as_str()) {
                            ws.active_pane = Some(pane_id);
                            drop(ws);
                            if let Some(w) = win_events.as_ref() {
                                w.request_redraw();
                            }
                        }
                    }
                    _ => {}
                }
            }
        });
        let tmux_arc = Arc::new(tmux);
        self.tmux = Some(tmux_arc.clone());
        self.start_socket_tmux(tmux_arc);
        Ok(())
    }
    /// Bring up the cmux-compatible JSON-RPC server so external agents
    /// (Claude Code teammateMode, ad-hoc CLI scripts) can drive this
    /// pane. The server is best-effort — a bind failure logs and the
    /// rest of the binary keeps working without it. Two env names are
    /// exported on the spawned shell:
    ///   - KASATERM_SOCKET_PATH (our brand)
    ///   - CMUX_SOCKET_PATH (so cmux-aware clients auto-detect us)
    /// Both point at the same socket; the second is the cmux-protocol
    /// convention from issue anthropics/claude-code#36926.
    /// Bind the unix socket + export env vars. Common to both backend
    /// modes — the caller decides which concrete `Backend` impl to plug
    /// in (TmuxBackend in tmux mode, PtyBackend in PTY mode).
    pub(crate) fn start_socket_with(&self, backend: Arc<dyn kasa_socket::Backend>) {
        // Model-invoked tools for the claude running inside a pane: the
        // same Backend, exposed over MCP-on-HTTP. Replaces the external
        // python bridge (mcp/kasa_mcp.py).
        match kasa_mcp::spawn_http_server(backend.clone(), 8765) {
            Ok(port) => {
                eprintln!("[kasaspace-mcp] HTTP MCP on 127.0.0.1:{port}/mcp");
                std::env::set_var("KASASPACE_MCP_PORT", port.to_string());
                let _ = std::fs::write(mcp_port_file_path(), port.to_string());
                // No MCP auto-discovery: write our address into each AI
                // client's config so any agent on this machine finds us.
                kasa_mcp::register_clients(port);
            }
            Err(e) => eprintln!("[kasaspace-mcp] HTTP MCP start failed: {e}"),
        }
        let path = resolve_kasaterm_socket_path();
        let server = match kasa_socket::Server::bind(&path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[agent-socket] bind {path:?} failed: {e:#}");
                return;
            }
        };
        let resolved = server.socket_path().to_string_lossy().to_string();
        eprintln!("[agent-socket] listening on {resolved}");
        std::env::set_var("KASATERM_SOCKET_PATH", &resolved);
        std::env::set_var("CMUX_SOCKET_PATH", &resolved);
        let _join = server.spawn(backend);
    }
    pub(crate) fn start_socket_tmux(&self, tmux: Arc<kasa_bridge::TmuxSession>) {
        self.start_socket_with(Arc::new(socket::TmuxBackend::new(tmux)));
    }
    /// Local PTY-mode socket server. Same cmux/MCP surface as tmux mode but
    /// backed by the GUI's own panes — pane writes/split/focus delegate to the
    /// GUI thread via the proxy (see socket::PtyBackend).
    pub(crate) fn start_socket_pty(&self) {
        let backend = Arc::new(socket::PtyBackend::new(
            self.proxy.clone(),
            self.ws.clone(),
            self.collab.attention.clone(),
        ));
        self.start_socket_with(backend);
    }
}
