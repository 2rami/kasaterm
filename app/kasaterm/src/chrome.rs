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
        if was_working && self.collab_toast_action.is_none() {
            let name = self.pane_header_label(surface_id);
            self.collab_toast = Some((format!("✓ {name} 작업 완료"), now));
            self.collab_toast_rect = None;
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
        // munder식 라우팅: 사람과 대화하는 건 god 뿐이다. 워커 pane 의 권한/입력
        // 대기는 토스트·데스크탑 알림 없이 위의 flash + board `waiting`(socket
        // attention 맵, CLI 경로가 이미 기록)으로만 남기고 god 이 처리하게 둔다.
        if !self.pane_faces_user(surface_id) {
            return;
        }
        // 이미 sticky 승인 토스트(칩 포함)가 이 pane 으로 떠 있으면 hook 의
        // 중복 알림으로 텍스트를 덮지 않는다.
        if self.collab_toast_action.as_deref() != Some(surface_id) {
            self.collab_toast = Some((format!("⚠ {name} 권한 대기중{detail}"), now));
            self.collab_toast_rect = None;
        }
        if !(self.window_focused && is_active_pane) {
            notify_desktop("⚠ 권한 필요", &format!("{name}{detail}"));
        }
    }

    /// 이 pane 의 막힘(승인 프롬프트)을 사용자에게 직접 띄울 것인가.
    /// 협업방(lead 파일)이 있으면 god pane 만 사용자 직행 — 워커는 god 이 처리
    /// (munder: "only the god agent talks to the human"). 협업방이 없으면(단독
    /// 사용) 모든 pane 이 사용자 직행 — 기존 동작 그대로. lead 파일은 pane cwd 의
    /// slug(`/`·`.` → `-`, god-elect.sh/kasacollab 과 동일 규칙)로 찾는다.
    ///
    /// cwd 는 **캐시(pane_cwd_cache)를 우회하고 라이브 lsof** 로 푼다. 캐시는
    /// split 시점값이 박제될 수 있어(refresh 타이밍·키 불일치) cd 후 옛 방 lead
    /// 를 읽어 god/워커를 오판했다(거노 실측: %4 가 cd /tmp 후 권한 메뉴 떴는데
    /// 레포 방 lead 로 워커 취급→토스트 미발화). claude pane 은 claude 가 cd 를
    /// 못 하므로 shell cwd = claude 시작 cwd = 협업방 slug 가 항상 일치 →
    /// 라이브 pid_cwd 가 정확하다(collab_board god 판정과 같은 결과). 승인 프롬프트
    /// 는 저빈도라 lsof 1회 비용은 무시 가능. 라이브 실패 시에만 캐시 폴백.
    pub(crate) fn pane_faces_user(&self, id: &str) -> bool {
        let cwd = self
            .pty
            .get(id)
            .and_then(|s| s.shell_pid())
            .and_then(socket::pid_cwd)
            .or_else(|| self.pane_cwd_cache.get(id).cloned());
        let Some(cwd) = cwd else {
            return true; // cwd 를 못 풀면 보수적으로 사용자 직행(기존 동작)
        };
        Self::pane_faces_user_for(&cwd, id)
    }

    /// `pane_faces_user` 의 순수 판정부 — cwd + pane id 로 협업방 lead 를 읽어
    /// 이 pane 이 god(또는 협업방 없음)이라 사용자 직행인지 본다. cwd 조회(lsof)와
    /// 분리해 단위테스트가 가능하다.
    pub(crate) fn pane_faces_user_for(cwd: &std::path::Path, id: &str) -> bool {
        let slug: String = cwd
            .to_string_lossy()
            .chars()
            .map(|c| if c == '/' || c == '.' { '-' } else { c })
            .collect();
        match std::fs::read_to_string(format!("/tmp/kasaterm-collab/{slug}/lead")) {
            Ok(lead) => lead.trim() == id,
            Err(_) => true, // 협업방 없음(단독) → 사용자 직행
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
        if self.file_tree_visible {
            self.file_tree_w_logical
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
        if self.git_col_visible {
            self.git_col_w_logical
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
    /// Whether `id`'s per-pane status bar is shown (default true — only the
    /// pane ids the user explicitly collapsed sit in `statusbar_hidden`).
    pub(crate) fn statusbar_visible(&self, id: &str) -> bool {
        !self.statusbar_hidden.contains(id)
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
        self.git_col_visible = !self.git_col_visible;
        if self.git_col_visible {
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
        if self.statusbar_hidden.contains(id) {
            self.statusbar_hidden.remove(id);
        } else {
            self.statusbar_hidden.insert(id.to_string());
            if self.statusbar_menu.as_ref().map(|(p, _)| p == id).unwrap_or(false) {
                self.statusbar_menu = None;
            }
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
    }
    /// Open (or toggle) a status-bar dropdown for pane `id`. The list is
    /// snapshotted now — `read_dir` / `git_branches` block, so they can't run on
    /// the render path. A second click on the same chip closes the menu.
    pub(crate) fn open_statusbar_menu(&mut self, id: &str, kind: StatusbarMenu) {
        if self.statusbar_menu.as_ref() == Some(&(id.to_string(), kind)) {
            self.statusbar_menu = None;
            self.chrome_dirty = true;
            return;
        }
        let cwd = self.pane_cwd_cache.get(id).cloned();
        self.statusbar_menu_dirs.clear();
        self.statusbar_menu_branches.clear();
        self.statusbar_menu_scroll = 0.0;
        self.statusbar_menu_search.clear();
        match kind {
            StatusbarMenu::Path => {
                if let Some(cwd) = cwd.as_ref() {
                    // `..` first, then child entries (folders before files, each
                    // alpha-sorted) — a quick-nav picker, so files show too, not
                    // just directories. Dotfiles (and `.git`) stay hidden here.
                    if let Some(parent) = cwd.parent() {
                        self.statusbar_menu_dirs.push(parent.to_path_buf());
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
                        self.statusbar_menu_dirs.extend(entries.into_iter().map(|(_, p)| p));
                    }
                }
            }
            StatusbarMenu::Branch => {
                if let Some(cwd) = cwd.as_ref() {
                    self.statusbar_menu_branches = kasa_mcp::git::git_branches(cwd);
                }
            }
        }
        self.statusbar_menu = Some((id.to_string(), kind));
        self.chrome_dirty = true;
    }
    /// Indices into `statusbar_menu_dirs` that survive the live search query
    /// (case-insensitive substring on the entry name; the `..` parent row at
    /// index 0 always shows). Drives both the dropdown render and Enter-to-open.
    pub(crate) fn statusbar_menu_filtered(&self) -> Vec<usize> {
        let q = self.statusbar_menu_search.to_lowercase();
        self.statusbar_menu_dirs
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
        let Some((pid, _)) = self.statusbar_menu.clone() else { return };
        let idxs = self.statusbar_menu_filtered();
        let target = if self.statusbar_menu_search.is_empty() {
            idxs.first().copied()
        } else {
            idxs.iter().find(|&&i| i != 0).or_else(|| idxs.first()).copied()
        };
        if let Some(path) = target.and_then(|i| self.statusbar_menu_dirs.get(i).cloned()) {
            if path.is_dir() {
                self.statusbar_cd(&pid, &path);
            } else {
                self.statusbar_menu = None;
                self.open_file_split(path);
            }
        }
    }
    /// `cd` pane `id`'s shell into `dir` (status-bar path picker). Sent straight
    /// to that pane's PTY — single-quoted so spaces survive — and the dropdown
    /// closes. The cwd sniffer repaints the bar once the shell reports the move.
    pub(crate) fn statusbar_cd(&mut self, id: &str, dir: &std::path::Path) {
        self.statusbar_menu = None;
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
        self.statusbar_menu = None;
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
        self.collab_toast = Some((msg, std::time::Instant::now()));
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Surface a transient top-right toast (reuses the collab toast slot).
    pub(crate) fn set_toast(&mut self, msg: String) {
        self.collab_toast = Some((msg, std::time::Instant::now()));
        self.collab_toast_rect = None;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Expand/collapse a file's inline unified diff in the git panel. The diff
    /// is parsed once on first expand and cached; `git diff` for a single file
    /// is cheap but not render-loop cheap, so it must not run per frame.
    pub(crate) fn toggle_git_diff(&mut self, staged: bool, path: String) {
        let key = (staged, path.clone());
        if self.git_col_expanded.remove(&key) {
            self.chrome_dirty = true;
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
            return;
        }
        if !self.git_col_diff_cache.contains_key(&key) {
            if let Some(cwd) = self.git_col_data.lock().ok().and_then(|g| g.cwd.clone()) {
                let rows = kasa_mcp::git::git_file_diff(&cwd, &path, staged);
                self.git_col_diff_cache.insert(key.clone(), rows);
            }
        }
        self.git_col_expanded.insert(key);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Collapse + drop cached diffs after a stage/unstage/commit changes the
    /// tree — the cached rows (and which side a file lives on) are now stale, so
    /// closing them is the no-surprise reset; the user re-expands for fresh diff.
    pub(crate) fn invalidate_git_diffs(&mut self) {
        self.git_col_expanded.clear();
        self.git_col_diff_cache.clear();
    }
    /// Open the git column for pane `id`'s repo (status-bar diff chip click).
    /// Focuses that pane so the column follows it (auto-track), then opens the
    /// column if it's hidden. A second click on an already-open column for the
    /// same pane closes it (toggle).
    pub(crate) fn open_git_panel_for(&mut self, id: &str) {
        let already = self.git_col_visible
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
        self.git_col_pinned_cwd = None;
        if self.git_col_visible {
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
        if self.statusbar_hidden.len() >= self.pane_cwd_cache.len() && !self.git_col_visible {
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
        if !self.git_col_visible {
            return;
        }
        // A user-pinned repo (picked from the path dropdown) overrides the
        // active-pane follow — the column stays on that repo until unpinned.
        if let Some(pinned) = self.git_col_pinned_cwd.clone() {
            if let Ok(mut guard) = self.git_col_cwd.lock() {
                *guard = Some(pinned);
            }
            return;
        }
        let active = self.ws.lock().ok().and_then(|w| w.active_pane.clone());
        let resolved = active
            .as_ref()
            .and_then(|id| self.pane_cwd_cache.get(id).cloned());
        if let Ok(mut guard) = self.git_col_cwd.lock() {
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
        let cwd = self.git_col_data.lock().ok().and_then(|g| g.cwd.clone());
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
                self.git_op = Some("Pulling");
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    let _ = kasa_mcp::git::git_pull(&cwd);
                    // GitOpDone clears the spinner; the poller's next tick
                    // repaints ahead/behind.
                    let _ = proxy.send_event(UserEvent::GitOpDone);
                });
            }
            GitColBtn::Push => {
                self.git_op = Some("Pushing");
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
                let msg = self.git_commit_msg.trim().to_string();
                if msg.is_empty() {
                    self.git_commit_focused = true;
                    self.chrome_dirty = true;
                    return;
                }
                self.git_op = Some("Committing");
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    let _ = kasa_mcp::git::git_commit_staged(&cwd, &msg);
                    let _ = proxy.send_event(UserEvent::GitOpDone);
                });
                self.git_commit_msg.clear();
                self.git_commit_cursor = 0;
                self.git_commit_focused = false;
                self.chrome_dirty = true;
            }
        }
    }
    /// Open the cursor-style Commit modal: pre-fill nothing, focus the message
    /// box, default to including unstaged changes (the toggle in the modal).
    pub(crate) fn open_commit_modal(&mut self) {
        self.git_commit_menu_open = false;
        self.git_commit_modal_open = true;
        self.git_commit_focused = true;
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    pub(crate) fn close_commit_modal(&mut self) {
        self.git_commit_modal_open = false;
        self.git_commit_focused = false;
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Run the modal's commit. `push` also pushes after. Honors the
    /// include-unstaged toggle: when on, stage everything first (`git add -A`),
    /// else commit only what's already staged. Empty message is a no-op.
    pub(crate) fn run_commit_modal(&mut self, push: bool) {
        let msg = self.git_commit_msg.trim().to_string();
        if msg.is_empty() {
            self.git_commit_focused = true;
            self.chrome_dirty = true;
            return;
        }
        let Some(cwd) = self.git_col_data.lock().ok().and_then(|g| g.cwd.clone()) else { return };
        let include = self.git_commit_modal_include_unstaged;
        self.git_op = Some(if push { "Pushing" } else { "Committing" });
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
        self.git_commit_msg.clear();
        self.git_commit_cursor = 0;
        self.git_commit_focused = false;
        self.git_commit_modal_open = false;
        self.invalidate_git_diffs();
        self.chrome_dirty = true;
    }
    /// `gh pr create --web` for the column's repo (Commit-menu → Create PR).
    pub(crate) fn create_git_pr(&mut self) {
        self.git_commit_menu_open = false;
        let Some(cwd) = self.git_col_data.lock().ok().and_then(|g| g.cwd.clone()) else { return };
        std::thread::spawn(move || {
            let _ = std::process::Command::new("gh")
                .args(["pr", "create", "--web"])
                .current_dir(&cwd)
                .spawn();
        });
        self.collab_toast = Some(("gh pr create --web 실행".to_string(), std::time::Instant::now()));
        self.chrome_dirty = true;
    }
    /// Expand/restore the git column width (header ⤢ button). Toggles between a
    /// wide reading width and the normal sidebar width; reshapes the PTYs.
    pub(crate) fn toggle_git_col_expand(&mut self) {
        let wide = 620.0_f32;
        let normal = 340.0_f32;
        self.git_col_w_logical = if self.git_col_w_logical >= wide - 1.0 { normal } else { wide };
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
    }
    /// Check out `branch` in the column's repo (off-thread). A dirty tree makes
    /// git refuse with a clear message — we don't stash/force, just let the
    /// poller repaint whatever git did. Closes the branch dropdown.
    pub(crate) fn run_git_checkout(&mut self, branch: String) {
        self.git_branch_menu_open = false;
        let cwd = self.git_col_data.lock().ok().and_then(|g| g.cwd.clone());
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
        let cwd = self.git_col_data.lock().ok().and_then(|g| g.cwd.clone());
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
        self.file_tree_visible = !self.file_tree_visible;
        if self.file_tree_visible {
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
        let Some((_, at)) = self.collab_toast.as_ref() else { return 0.0 };
        // 승인 토스트(칩 포함)는 사용자가 응답하거나 프롬프트가 풀릴 때까지
        // 고정 — 시간 페이드 없음. (해제는 route_approval_prompts/클릭 핸들러)
        if self.collab_toast_action.is_some() {
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
        // god 모드 방 전용 — solo 방에선 토스트로 안내만 하고 열지 않는다.
        if crate::current_collab_mode() != "god" {
            self.collab_toast =
                Some(("아로나 UI는 god 모드 방에서만 열려요 (kasacollab mode god)".into(), Instant::now()));
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
            return;
        }
        let port = mcp_panel_port();
        let attrs = WindowAttributes::default()
            .with_title("아로나 — 샬레 교실")
            .with_theme(Some(Theme::Dark))
            .with_inner_size(LogicalSize::new(1100.0, 720.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[arona-panel] window create failed: {e}");
                return;
            }
        };
        // build_as_child for the same use-after-free reason as the git panel.
        let webview = match wry::WebViewBuilder::new()
            .with_url(format!("http://127.0.0.1:{port}/arona-ui/"))
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
    }
    /// Toggle the arona UI window from the menu: close if open, open if not.
    pub(crate) fn toggle_arona_panel(&mut self, event_loop: &ActiveEventLoop) {
        if self.arona_panel_window.is_some() {
            // Drop the webview before the window it borrows from.
            self.arona_panel_webview = None;
            self.arona_panel_window = None;
        } else {
            self.open_arona_panel(event_loop);
        }
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

#[cfg(test)]
mod faces_user_tests {
    use super::*;
    use std::path::PathBuf;

    // 실제 협업방 루트(/tmp/kasaterm-collab)를 오염 안 시키게 pid+태그로 유니크한
    // cwd 를 만들어 그 slug 경로에 lead 를 깐다. pane_faces_user_for 는 cwd→slug
    // 변환 후 그 lead 를 읽으므로 cwd 만 유니크하면 격리된다.
    fn room(tag: &str) -> (PathBuf, String) {
        let cwd = PathBuf::from(format!("/tmp/kt-faces-test-{}-{tag}", std::process::id()));
        let slug: String = cwd
            .to_string_lossy()
            .chars()
            .map(|c| if c == '/' || c == '.' { '-' } else { c })
            .collect();
        let dir = format!("/tmp/kasaterm-collab/{slug}");
        std::fs::create_dir_all(&dir).unwrap();
        (cwd, dir)
    }

    #[test]
    fn god_pane_faces_user() {
        let (cwd, dir) = room("god");
        std::fs::write(format!("{dir}/lead"), "%1\n").unwrap();
        assert!(App::pane_faces_user_for(&cwd, "%1")); // god 본인 → 직행
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn worker_pane_does_not_face_user() {
        let (cwd, dir) = room("worker");
        std::fs::write(format!("{dir}/lead"), "%1\n").unwrap();
        assert!(!App::pane_faces_user_for(&cwd, "%2")); // 워커 → god 이 처리
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn no_lead_means_solo_faces_user() {
        let (cwd, dir) = room("solo"); // dir 만들되 lead 안 씀
        assert!(App::pane_faces_user_for(&cwd, "%7")); // 협업방 없음(단독) → 직행
        std::fs::remove_dir_all(&dir).ok();
    }
}
