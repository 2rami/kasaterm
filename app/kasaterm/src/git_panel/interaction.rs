use super::*;

impl App {
    pub(crate) fn git_panel_press(&mut self, point: (f32, f32)) -> bool {
        let inside = |rect: &(f32, f32, f32, f32)| info::hit(point, rect);
        self.chrome_dirty = true;
        // Open dropdowns overlay everything — resolve their items
        // (and the header toggles) before the list/buttons under.
        if self.git.path_menu_open {
            if let Some(repo) = self
                .git
                .path_menu_repo_rects
                .iter()
                .find(|(_, r)| inside(r))
                .map(|(repo, _)| repo.clone())
            {
                if let Some(id) = self.git_active_pane() {
                    self.git.col_repo_choice.insert(id, repo);
                }
                self.git.col_pinned_cwd = None;
                self.git.path_menu_open = false;
                self.publish_git_col_cwd();
                return true;
            }
            if let Some(key) = self
                .git
                .path_menu_rects
                .iter()
                .find(|(_, r)| inside(r))
                .map(|(k, _)| k.clone())
            {
                // None = "자동 추적" (unpin, 이 칸의 하위 레포 고르기도 풀기); Some = pin that repo.
                if key.is_none() {
                    if let Some(id) = self.git_active_pane() {
                        self.git.col_repo_choice.remove(&id);
                    }
                }
                self.git.col_pinned_cwd = key;
                self.git.path_menu_open = false;
                self.publish_git_col_cwd();
                return true;
            }
        }
        if self.git.branch_menu_open {
            if let Some(back) = self.git.branch_page_rects.iter()
                .find(|(_, rect)| inside(rect)).map(|(back, _)| *back) {
                if back { self.git.branch_page = self.git.branch_page.saturating_sub(1); }
                else { self.git.branch_page += 1; }
                return true;
            }
        }
        // Header rows toggle their dropdowns (mutually exclusive).
        if self.git.path_hdr_rect.map(|r| inside(&r)).unwrap_or(false) {
            self.git.path_menu_open = !self.git.path_menu_open;
            self.git.branch_menu_open = false;
            return true;
        }
        if self
            .git
            .branch_hdr_rect
            .map(|r| inside(&r))
            .unwrap_or(false)
        {
            self.git.branch_menu_open = !self.git.branch_menu_open;
            self.git.path_menu_open = false;
            return true;
        }
        // A click elsewhere in the column dismisses an open menu
        // (swallowed, so it doesn't also hit the list/buttons).
        if self.git.path_menu_open || self.git.branch_menu_open {
            self.git.path_menu_open = false;
            self.git.branch_menu_open = false;
            return true;
        }
        // Commit-button dropdown items (overlay) first.
        if self.git.commit_menu_open {
            if let Some(act) = self
                .git
                .commit_menu_rects
                .iter()
                .find(|(_, r)| inside(r))
                .map(|(a, _)| *a)
            {
                self.git.commit_menu_open = false;
                match act {
                    crate::GitCommitAction::Commit => self.open_commit_modal(),
                    crate::GitCommitAction::Push => {
                        self.run_git_col_action(crate::GitColBtn::Push)
                    }
                    crate::GitCommitAction::Pull => {
                        self.run_git_col_action(crate::GitColBtn::Pull)
                    }
                    crate::GitCommitAction::CreatePr => self.create_git_pr(),
                }
                return true;
            }
            self.git.commit_menu_open = false;
            return true;
        }
        // Commit split button: main → modal, caret → dropdown.
        if self
            .git
            .commit_btn_rect
            .map(|r| inside(&r))
            .unwrap_or(false)
        {
            // Matches the render: with no changes but commits to
            // sync, the primary button is the sync action — 당길
            // 것이 있으면 Pull 이 먼저다(render 의 pull_mode 와 같은
            // 우선순위여야 「보이는 버튼」과 「눌리는 동작」이 안
            // 갈린다).
            let sync = self.git.col_frame.as_ref()
                .filter(|view| view.staged.is_empty() && view.unstaged.is_empty())
                .map(|view| (view.behind > 0, view.ahead > 0))
                .unwrap_or((false, false));
            match sync {
                (true, _) => self.run_git_col_action(crate::GitColBtn::Pull),
                (false, true) => self.run_git_col_action(crate::GitColBtn::Push),
                _ => self.open_commit_modal(),
            }
            return true;
        }
        if self
            .git
            .commit_caret_rect
            .map(|r| inside(&r))
            .unwrap_or(false)
        {
            self.git.commit_menu_open = !self.git.commit_menu_open;
            return true;
        }
        // Row +/− button → stage / unstage that one file. Checked
        // before the file-preview path since it sits inside the
        // row rect. Off-thread; the poller repaints the lists.
        if let Some((stage, path)) = self
            .git
            .col_stage_rects
            .iter()
            .find(|(_, _, r)| inside(r))
            .map(|(s, p, _)| (*s, p.clone()))
        {
            if self.git.op.is_none() {
                if let Some(cwd) = self.local_git_panel_cwd("스테이지 변경") {
                    let label = if stage { "Staging" } else { "Unstaging" };
                    self.run_git_operation(label, move || {
                        if stage {
                            kasa_git::git::git_add_path(&cwd, &path)
                        } else {
                            kasa_git::git::git_unstage_path(&cwd, &path)
                        }
                    });
                }
            }
            return true;
        }
        // Row ↩ discard → restore the file (or delete if untracked).
        if let Some((path, untracked)) = self
            .git
            .col_discard_rects
            .iter()
            .find(|(_, _, r)| inside(r))
            .map(|(p, u, _)| (p.clone(), *u))
        {
            if self.git.op.is_none() {
                if let Some(cwd) = self.local_git_panel_cwd("변경 되돌리기") {
                    self.run_git_operation("Discarding", move || {
                        kasa_git::git::git_discard_path(&cwd, &path, untracked)
                    });
                }
            }
            return true;
        }
        // Row ⤴ open → preview the file in a pane.
        if let Some(path) = self
            .git
            .col_open_rects
            .iter()
            .find(|(_, r)| inside(r))
            .map(|(p, _)| p.clone())
        {
            self.open_git_file(&path);
            return true;
        }
        // File row → toggle its inline unified diff.
        if let Some((staged, path)) = self
            .git
            .col_file_rects
            .iter()
            .find(|(_, _, r)| inside(r))
            .map(|(s, p, _)| (*s, p.clone()))
        {
            self.toggle_git_diff(staged, path);
            return true;
        }
        // Commit-detail file row (inside an expanded commit) →
        // toggle that file's diff.
        if let Some((hash, path)) = self
            .git
            .col_commit_file_rects
            .iter()
            .find(|(_, _, r)| inside(r))
            .map(|(h, p, _)| (h.clone(), p.clone()))
        {
            self.toggle_git_commit_file(hash, path);
            return true;
        }
        // Recent-commit row → double-click expands its file list
        // (single click is just the preview, no action).
        if let Some(hash) = self
            .git
            .col_commit_rects
            .iter()
            .find(|(_, r)| inside(r))
            .map(|(h, _)| h.clone())
        {
            let now = Instant::now();
            let is_double = matches!(
                self.git.last_commit_click.as_ref(),
                Some((t, h)) if *h == hash && now.duration_since(*t).as_millis() < 400
            );
            if is_double {
                self.git.last_commit_click = None;
                self.toggle_git_commit(hash);
            } else {
                self.git.last_commit_click = Some((now, hash));
            }
            return true;
        }
        false
    }

    pub(crate) fn git_panel_wheel(&mut self, lines: f32) -> bool {
        if self.git.commit_modal_open || self.git.panel_menu_open() {
            return true;
        }
        let point = self.cursor_px;
        let git = &mut self.git;
        let (scroll, (visible, content), step) = if git.graph_rect.as_ref().is_some_and(|r| info::hit(point, r)) {
            (&mut git.graph_scroll, git.graph_extent, 40.0)
        } else if git.list_rect.as_ref().is_some_and(|r| info::hit(point, r)) {
            (&mut git.col_scroll, git.col_list_extent, 22.0)
        } else {
            return false;
        };
        let next = (*scroll - lines * step).clamp(0.0, (content - visible).max(0.0));
        if next == *scroll {
            return false;
        }
        *scroll = next;
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        true
    }

    pub(crate) fn git_commit_modal_press(&mut self) {
        let point = self.cursor_px;
        let hit = self.git.commit_modal_rects.iter()
            .find(|(_, r)| info::hit(point, r))
            .map(|(action, _)| *action);
        match hit {
            Some(GitModalBtn::Close | GitModalBtn::Cancel) => self.close_commit_modal(),
            Some(GitModalBtn::IncludeUnstaged) => {
                self.git.commit_modal_include_unstaged = !self.git.commit_modal_include_unstaged;
            }
            Some(GitModalBtn::Commit) => self.run_commit_modal(false),
            Some(GitModalBtn::CommitAndPush) => self.run_commit_modal(true),
            None => {}
        }
        self.chrome_dirty = true;
    }
}

impl state::GitState {
    pub(crate) fn panel_menu_open(&self) -> bool {
        self.path_menu_open || self.branch_menu_open || self.commit_menu_open
    }

    pub(crate) fn close_panel_menus(&mut self) {
        self.path_menu_open = false;
        self.branch_menu_open = false;
        self.commit_menu_open = false;
    }
}
