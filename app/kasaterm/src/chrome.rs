//! 사이드바·git col·파일트리 토글·패널·줌/폰트·toast 등 chrome UI 메서드.
use super::*;

/// How long a completion notification pulses a pane header / sidebar done-dot.
const NOTIFY_FLASH_MS: u128 = 1800;

impl App {
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
        // busy-grace timer. The completion toast is surfaced here (named by the
        // pane's tab-header label); the glyph working→idle path in
        // `refresh_pane_activity` then sees the pane is already idle and won't
        // double-fire. A pane that wasn't working (idle hook re-fire) skips the
        // toast.
        self.pane_last_busy.remove(surface_id);
        let was_working = self
            .pane_activity
            .get(surface_id)
            .map_or(false, |a| a.status != "idle" && !a.status.is_empty());
        self.pane_activity
            .entry(surface_id.to_string())
            .and_modify(|a| a.status = "idle".to_string())
            .or_insert_with(|| crate::stream::PaneStatusView {
                status: "idle".to_string(),
                ..Default::default()
            });
        // A sticky approval toast (chips waiting on the user) outranks a
        // completion blip — same guard as the grid-scan path in input.rs.
        if was_working && self.collab.toast_action.is_none() {
            let name = self.pane_header_label(surface_id);
            self.collab.toast = Some((format!("✓ {name} 작업 완료"), now));
            self.collab.toast_rect = None;
        }
        self.notify_flash.insert(surface_id.to_string(), now);
        // A pane in a *background* window finished — pulse that window's sidebar
        // tab until the user switches to it (switch_window clears the entry).
        if let Some(wi) = self.window_of_pane(surface_id) {
            if wi != self.active_window {
                self.window_alert.insert(wi);
            }
        }
        self.chrome_dirty = true;
        if !(self.window_focused && is_active_pane) {
            self.unread_panes.insert(surface_id.to_string());
            notify_desktop(title, body);
        }
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
        let name = self.pane_header_label(surface_id);
        let reason = reason.trim();
        let detail = if reason.is_empty() {
            String::new()
        } else {
            format!(" — {reason}")
        };
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
            self.collab.toast = Some((format!("⚠ {name} 권한 대기중{detail}"), now));
            self.collab.toast_rect = None;
        }
        if !(self.window_focused && is_active_pane) {
            self.unread_panes.insert(surface_id.to_string());
            notify_desktop("⚠ 권한 필요", &format!("{name}{detail}"));
        }
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
    /// Width of the session-tab strip alone (0 when collapsed).
    pub(crate) fn tab_strip_w(&self) -> f32 {
        if self.sidebar_visible {
            self.sidebar_w_logical
        } else {
            0.0
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
    /// Settings-screen toggle, parked just left of the git-column toggle at the
    /// right end of the title strip. Always present (unlike the sidebar's
    /// bottom entry, which hides when the sidebar is collapsed) so the screen is
    /// always one click away.
    pub(crate) fn settings_toggle_rect(&self) -> Option<(f32, f32, f32, f32)> {
        let w = 26.0;
        let h = 22.0;
        let win_w = self.window.as_ref().map(|win| {
            let scale = self.effective_scale();
            win.inner_size().width as f32 / scale
        })?;
        let x = win_w - w - 8.0 - (w + 4.0);
        #[cfg(windows)]
        let x = Self::win_control_rects(win_w)[0].0 - 2.0 - w - (w + 4.0);
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
        let q = dir.to_string_lossy().replace('\'', "'\\''");
        let cmd = format!("cd '{q}'\r");
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
            let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
        }
        #[cfg(target_os = "windows")]
        {
            let _ = std::process::Command::new("explorer")
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
            let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
        }
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
        let any_bar = self.pane_cwd_cache.keys().any(|id| self.statusbar_visible(id));
        if !any_bar && !self.git.col_visible {
            // Every bar collapsed and the git column hidden — no badge consumer.
            return;
        }
        let cwds: Vec<std::path::PathBuf> = self.pane_cwd_cache.values().cloned().collect();
        if let Ok(mut guard) = self.git_poll_cwds.lock() {
            *guard = cwds;
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
    /// git/network. StageAll = `git add -A`; Pull/Push sync the branch; Commit
    /// commits the STAGED changes with the panel's message (VSCode model). All
    /// read the column's repo from the poller's snapshot so the action always
    /// targets what the user sees.
    pub(crate) fn run_git_col_action(&mut self, btn: GitColBtn) {
        let cwd = self.git.col_data.lock().ok().and_then(|g| g.cwd.clone());
        let Some(cwd) = cwd else { return };
        match btn {
            GitColBtn::StageAll => {
                // `git add -A` off-thread; the poller's next tick flips the
                // rows to staged.
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    let _ = kasa_mcp::git::git_stage_all(&cwd);
                    let _ = proxy.send_event(UserEvent::Redraw);
                });
            }
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
            let _ = std::process::Command::new("gh")
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
    /// Preview a changed file from the git column (image/code/markdown by
    /// extension), resolved against the column's repo cwd. A native diff view
    /// is phase 2; opening the file is the useful v1. Daemon-only, like the
    /// file-tree's file-click path.
    pub(crate) fn open_git_file(&mut self, rel: &str) {
        let cwd = self.git.col_data.lock().ok().and_then(|g| g.cwd.clone());
        let Some(cwd) = cwd else { return };
        let abs = cwd.join(rel);
        let ext = abs
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let kind = match ext.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "tif" | "ico" => "image",
            "md" | "markdown" | "txt" | "log" | "" => "markdown",
            _ => "code",
        };
        let ps = abs.to_string_lossy().into_owned();
        let _ = (kind, ps);
        // TODO(local-mode): 파일 미리보기를 로컬에서 직접 spawn
        // (split_image_pane / split_markdown_pane). 데몬 open_preview RPC 제거됨.
    }
    /// Title-bar sidebar-toggle button rect (logical px), parked just
    /// right of the macOS traffic lights. Fixed position (doesn't depend
    /// on state) so the renderer and the click handler share one source.
    pub(crate) fn sidebar_toggle_rect() -> (f32, f32, f32, f32) {
        let w = 26.0;
        let h = 22.0;
        #[cfg(not(windows))]
        let x = TRAFFIC_LIGHT_WIDTH + 6.0;
        // Windows is frameless with no traffic-light cluster to clear — start
        // the toggles at the left edge instead of reserving the macOS width.
        #[cfg(windows)]
        let x = 10.0;
        let y = (TITLE_HEIGHT - h) / 2.0;
        (x, y, w, h)
    }
    /// File-tree-toggle button rect, parked just right of the sidebar toggle.
    pub(crate) fn file_tree_toggle_rect() -> (f32, f32, f32, f32) {
        let (sx, sy, sw, sh) = Self::sidebar_toggle_rect();
        (sx + sw + 2.0, sy, sw, sh)
    }
    /// SCHALE OS(아로나) 토글 버튼 — 우측 끝, 설정 토글 왼쪽(git-col·settings 다음).
    /// 좌측에 두면 경로 브레드크럼과 겹쳐서 우측으로(거노). ✨ 아이콘, 클릭 →
    /// toggle_arona_panel. win_w 필요해 첫 페인트 전엔 None.
    pub(crate) fn arona_btn_rect(&self) -> Option<(f32, f32, f32, f32)> {
        let w = 26.0;
        let h = 22.0;
        let win_w = self.window.as_ref().map(|win| {
            let scale = self.effective_scale();
            win.inner_size().width as f32 / scale
        })?;
        let x = win_w - w - 8.0 - 2.0 * (w + 4.0);
        #[cfg(windows)]
        let x = Self::win_control_rects(win_w)[0].0 - 2.0 - w - 2.0 * (w + 4.0);
        let y = (TITLE_HEIGHT - h) / 2.0;
        Some((x, y, w, h))
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
        let html = SESSION_PANEL_HTML.replace("__PORT__", &port);
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
        let html = BOARD_PANEL_HTML.replace("__PORT__", &port);
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
    /// Open the arona (god-mode) full UI in its own OS window. Unlike the
    /// HTML-string panels this loads the arona-ui dist over the MCP HTTP
    /// server (`/arona-ui/`) — same-origin with the API the page fetches, and
    /// the in-window wry embed is off the table anyway (Metal layer conflict).
    pub(crate) fn open_arona_panel(&mut self, event_loop: &ActiveEventLoop) {
        if self.arona_panel_window.is_some() {
            return;
        }
        // 모드 게이트 없음: solo/미설정 방에서도 창은 연다. 모드 안내·전환은
        // 웹 쪽 ModePicker 담당(GET /mode 로 분기) — 네이티브가 차단하면
        // ModePicker 에 도달 자체가 불가한 설계 모순이었다(거노 실측).
        let port = mcp_panel_port();
        // 창을 숨긴 채 만든다 — 보이는 채로 띄우면 webview 콘텐츠가 로드되기 전
        // 빈 창이 깜빡인다. 페이지 로드 완료(PageLoadEvent::Finished) 후에야
        // set_visible(true) 로 드러낸다(아래 핸들러).
        let attrs = WindowAttributes::default()
            .with_title("아로나 — 샬레 교실")
            .with_theme(Some(Theme::Dark))
            .with_visible(false)
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
        // 로드 끝나면 창을 드러내는 핸들러. webview2 가 UI(메인) 스레드에서
        // 콜백하므로 winit set_visible 호출이 안전하다.
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
                    win_show.set_visible(true);
                    win_show.focus_window();
                }
            })
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                size: wry::dpi::LogicalSize::new(1100.0, 720.0).into(),
            })
            .build_as_child(window.as_ref())
        {
            Ok(wv) => wv,
            Err(e) => {
                eprintln!("[arona-panel] webview build failed: {e}");
                return;
            }
        };
        eprintln!("[arona-panel] open; http://127.0.0.1:{port}/arona-ui/");
        self.arona_panel_window = Some(window);
        self.arona_panel_webview = Some(webview);
        // BA GUI 는 세션을 건드리지 않는다(거노 06-17 방향전환: "시각 레이어"). god 통솔
        // 자체가 폐기됐다(솔로 확정 06-18) — 아로나/SCHALE OS 는 관찰·시각 레이어일 뿐
        // 세션을 통솔하지 않는다(활성 pane god 승격 호출 제거).
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
    /// Toggle the arona UI window from the menu: close if open, open if not.
    pub(crate) fn toggle_arona_panel(&mut self, event_loop: &ActiveEventLoop) {
        if self.arona_panel_window.is_some() {
            self.close_arona_panel();
        } else {
            self.open_arona_panel(event_loop);
        }
    }

    /// 거노: 평면도 빈 방 클릭 → 새 윈도우(빈 셸). 방별 collab 격리를 위해 room slug
    /// 를 셸 env(KASATERM_ROOM)로 주입한다(spawn_session_pane 이 pending_room 을 읽음).
    /// claude 자동 스폰·persona 주입 없음 — 솔로(god 폐기 06-18)라 사용자가 직접 친다.
    /// `character` 는 좌측 방 라벨용(SCHALE 시각 구분).
    pub(crate) fn new_room_with_god(&mut self, character: &str) {
        let room = format!("room-{}", self.next_room_seq);
        self.next_room_seq += 1;
        self.pending_room = Some(room);
        self.new_window();
        // 좌측 방 라벨 = 선택 캐릭터 이름(방 구분 시각 라벨).
        self.window_name_override
            .insert(self.active_window, format!("● {character}"));
    }
    /// First-run onboarding: open the arona window (whose ModePicker shows
    /// the 터미널 vs 아로나 choice) once the boot settles, but only when this
    /// room has no collab-mode marker yet — picking a mode writes the marker,
    /// so the next boot skips straight to the terminal. `KASATERM_NO_ONBOARD`
    /// opts out for headless verification and pre-onboarding user setups.
    #[allow(dead_code)] // 온보딩 제거(거노) — 재활성 대비 보존. 호출부 handler.rs 에서 뺌.
    pub(crate) fn arm_first_run_onboarding(&mut self) {
        if std::env::var_os("KASATERM_NO_ONBOARD").is_some() {
            return;
        }
        // 1.5s: 초기 pane 의 shell 이 spawn 되어 lsof cwd 해석이 안정될 여유.
        self.onboard_check_at =
            Some(Instant::now() + std::time::Duration::from_millis(1500));
    }
    pub(crate) fn run_pending_onboarding(&mut self, event_loop: &ActiveEventLoop) {
        let Some(due) = self.onboard_check_at else { return };
        if Instant::now() < due {
            return;
        }
        self.onboard_check_at = None;
        // 온보딩은 앱 전역 1회 사건이다. 옛 구현은 active pane 의 cwd 로 per-cwd
        // collab 마커를 봤는데, 부팅 시 그 cwd 가 임의적이라(데스크탑에서 열면
        // 데스크탑 온보딩 — 2026-06 실측 사고) + 새 방마다 재온보딩이었다.
        // 글로벌 플래그 하나로 판정한다.
        let Some(flag) = kasa_mcp::onboarded_marker_path() else {
            return; // HOME unset → 마커 경로 불명, 온보딩 안 띄움
        };
        if flag.exists() {
            return;
        }
        // 마이그레이션: 글로벌 플래그 도입 전 이미 방 모드를 정한 적 있는
        // 사용자는 첫 실행이 아니다 — 플래그만 세우고 조용히 skip.
        if kasa_mcp::any_collab_mode_marker() {
            kasa_mcp::mark_onboarded();
            return;
        }
        eprintln!("[onboard] first run — opening arona ModePicker");
        self.open_arona_panel(event_loop);
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
    /// Cmd+W / header ×: close tab `idx` of `pane`. A multi-tab pane drops just
    /// that tab; the last tab drops the pane (no-op on a single-pane window, so
    /// we skip it there and leave the OS close button to quit). If the tab is
    /// running a real job, raise the confirm modal instead of closing now.
    pub(crate) fn confirm_or_close_tab(&mut self, pane: &str, idx: usize) {
        let (tabs_len, pid) = {
            let ws = self.ws.lock().unwrap();
            match ws.panes.get(pane) {
                Some(p) => (p.tabs.len(), p.tabs.get(idx).and_then(|t| t.pid.clone())),
                None => return,
            }
        };
        let action = if tabs_len > 1 {
            PendingClose::Tab { pane: pane.to_string(), idx }
        } else {
            let leaves = self.pty_layout.as_ref().map_or(0, |t| t.leaves().len());
            if leaves <= 1 {
                // Last pane of the window: close is a no-op (OS button quits),
                // so don't even confirm.
                return;
            }
            PendingClose::Pane { pane: pane.to_string() }
        };
        match pid.as_deref().and_then(|p| self.pid_busy(p)) {
            Some(proc) => self.open_confirm_close(proc, action),
            None => self.do_close(action),
        }
    }
    /// CloseRequested (red light / Cmd+Q): returns true when a job is running
    /// and the confirm modal was raised — the caller must NOT exit yet. Returns
    /// false when nothing's running, so the caller exits immediately.
    pub(crate) fn confirm_or_close_window(&mut self) -> bool {
        match self.any_pane_busy() {
            Some(proc) => {
                self.open_confirm_close(proc, PendingClose::Window);
                true
            }
            None => false,
        }
    }
    fn open_confirm_close(&mut self, proc: String, action: PendingClose) {
        self.confirm_close = Some(ConfirmClose { proc, action });
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
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
            PendingClose::Window => {}
        }
    }
}

/// Raise a macOS desktop notification. Inside the signed `.app` bundle we use
/// `UNUserNotificationCenter` so the alert carries kasaterm's own app icon (and
/// gets the native sound/click affordances). The bare `cargo run` binary has no
/// bundle identifier and can't obtain notification authorization, so there we
/// fall back to `osascript` — which shows the Script Editor icon (dev-only).
#[cfg(target_os = "macos")]
pub(crate) fn notify_desktop(title: &str, body: &str) {
    if is_bundled() {
        notify_native(title, body);
    } else {
        notify_osascript(title, body);
    }
}

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
        let handler = block2::RcBlock::new(|_granted: objc2::runtime::Bool, _err: *mut objc2_foundation::NSError| {});
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let opts = UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound;
        center.requestAuthorizationWithOptions_completionHandler(opts, &handler);
    });
}

#[cfg(target_os = "macos")]
fn notify_native(title: &str, body: &str) {
    use objc2_foundation::NSString;
    use objc2_user_notifications::{
        UNMutableNotificationContent, UNNotificationRequest, UNUserNotificationCenter,
    };
    ensure_notification_authorization();
    // Unique id per request so rapid completions don't replace each other.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    let ident = NSString::from_str(&format!("kasaterm-notify-{seq}"));
    let request =
        UNNotificationRequest::requestWithIdentifier_content_trigger(&ident, &content, None);
    let center = UNUserNotificationCenter::currentNotificationCenter();
    center.addNotificationRequest_withCompletionHandler(&request, None);
}

#[cfg(target_os = "macos")]
fn notify_osascript(title: &str, body: &str) {
    let script = format!(
        "display notification {} with title {}",
        applescript_quote(body),
        applescript_quote(title),
    );
    let _ = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .spawn();
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn notify_desktop(_title: &str, _body: &str) {}

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

