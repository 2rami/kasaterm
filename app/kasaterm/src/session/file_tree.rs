//! 칸 cwd 와 파일 트리 — cwd 갱신, 트리 읽기·감시·검색·편집, 파일을 알맞은 자리(앱·편집기 칸·
//! 마크다운 창)에서 열기.
use super::*;

impl App {
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
            // OSC 9;9 shell-integration report wins — it's accurate under
            // PowerShell, whose process cwd stays frozen at launch. Shells that
            // don't emit it (zsh/bash) fall back to the pid's real cwd (lsof /
            // PEB), which for them is correct because `cd` moves it.
            let cwd = sess
                .reported_cwd()
                .or_else(|| sess.shell_pid().and_then(socket::pid_cwd));
            if let Some(cwd) = cwd {
                cache.insert(id.clone(), cwd);
            }
        }
        // 셸이 실제로 cd 로 움직였으면 그 pane 의 view-cwd 오버라이드를 버린다 —
        // attach 종료 후 셸 조작 시 파일트리가 옛 프로젝트에 고착되는 것 방지
        // (claude 가 살아 있으면 statusline 이 곧 재보고해 오버라이드가 돌아온다).
        for (id, cwd) in &cache {
            if self.pane_cwd_cache.get(id).is_some_and(|old| old != cwd) {
                self.pane_view_cwd.remove(id);
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
        let sess = self.pty.get(id)?;
        sess.reported_cwd()
            .or_else(|| sess.shell_pid().and_then(socket::pid_cwd))
    }
    /// cwd for a shell about to be spawned off `prev_pane` (the pane being split
    /// or tabbed). Threads the spawning pane's live cwd into `resolve_spawn_cwd`
    /// so the `"last"` setting behaves like other terminals' "reuse previous
    /// directory" mode.
    pub(crate) fn spawn_cwd_from(&self, prev_pane: Option<&str>) -> Option<String> {
        if let Some(c) = self.pending_spawn_cwd.clone() {
            return Some(c);
        }
        let pid = prev_pane.map(|id| self.ws.lock().unwrap().active_tab_pid(id));
        let prev = pid.as_deref().and_then(|id| {
            let cwd = self.pane_current_cwd(id);
            let Some(info) = kasa_mcp::remote::remote_info(id).filter(|info| info.view) else { return cwd };
            // New siblings of a mirror are local shells. Never start one in an
            // absent /Users/<remote-account>/... directory on the other Mac.
            let remote = cwd.as_ref().map(|p| p.to_string_lossy().into_owned()).or(info.remote_cwd);
            let mapped = remote.as_deref().and_then(|path| {
                let machine = kasa_mcp::machines::find(&info.label)?;
                kasa_mcp::machines::map_remote_to_local(&machine, path)
            });
            info.origin_cwd.into_iter().chain(mapped).chain(remote)
                .map(std::path::PathBuf::from).find(|path| path.is_dir())
        });
        resolve_spawn_cwd(prev)
    }
    /// Recompute the sidebar file tree when its root (the active pane's cwd)
    /// changes — pane switch or `cd`. Cheap string compare per frame; the
    /// read_dir walk only runs on a real change (or after expand/collapse,
    /// which calls `rebuild_file_tree_nodes` directly).
    /// `cwd` 를 감싸는 가장 가까운 git 레포 루트(1-엔트리 캐시 경유).
    /// 레포 밖이면 None — 호출부가 cwd 를 그대로 쓴다.
    pub(crate) fn anchored_tree_root(
        &mut self,
        cwd: &std::path::Path,
    ) -> Option<std::path::PathBuf> {
        if let Some((cached_cwd, root)) = &self.file_tree.anchor_cache {
            if cached_cwd == cwd {
                return root.clone();
            }
        }
        let found = git_repo_root(cwd);
        self.file_tree.anchor_cache = Some((cwd.to_path_buf(), found.clone()));
        found
    }
    pub(crate) fn refresh_file_tree(&mut self) {
        self.ensure_file_tree_watcher();
        // A background watcher flagged an on-disk change (file added / removed /
        // renamed / modified) — rebuild even if the root is unchanged.
        if self
            .file_tree
            .fs_dirty
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            self.rebuild_file_tree_nodes();
        }
        let active = self.ws.lock().ok().and_then(|w| w.active_pane.clone());
        // 다른 기기의 거울이면 **그 기계의** 폴더를 본다 — 목록은 저쪽 창구로 받는다
        // (2026-09-17 지시 「파일트리나 깃 패널 기기 달라도 뜨게」).
        let remote = active
            .as_ref()
            .and_then(|id| kasa_mcp::remote::remote_info(id))
            .filter(|info| info.view)
            .and_then(|info| {
                kasa_mcp::machines::label_for_base(&info.base)
                    .map(|label| (label, info.base.clone(), info.remote_cwd.clone()))
            });
        if let Some((label, base, remote_cwd)) = remote {
            let cwd = active
                .as_ref()
                .and_then(|id| {
                    self.pane_view_cwd.get(id).cloned().or_else(|| self.pane_cwd_cache.get(id).cloned())
                })
                .or_else(|| remote_cwd.map(std::path::PathBuf::from));
            let Some(cwd) = cwd else { return };
            let same_machine = self.file_tree.remote.as_ref()
                .is_some_and(|(l, b)| *l == label && *b == base);
            if !same_machine {
                if let Ok(mut cache) = self.file_tree.remote_cache.lock() { cache.clear(); }
                if let Ok(mut pending) = self.file_tree.remote_pending.lock() { pending.clear(); }
            }
            if !same_machine || self.file_tree.root.as_ref() != Some(&cwd) {
                self.file_tree.remote = Some((label, base));
                self.file_tree.expanded.insert(cwd.clone());
                self.file_tree.root = Some(cwd);
                self.file_tree.scroll = 0.0;
                self.rebuild_file_tree_nodes();
            }
            return;
        }
        if self.file_tree.remote.take().is_some() {
            // 로컬로 돌아왔다 — 아래 비교가 「바뀜」으로 보게 뿌리를 비운다.
            self.file_tree.root = None;
        }
        let root = active
            .as_ref()
            // "pane 이 보는 경로"(statusline report / transcript bind)가 셸 cwd 보다
            // 우선 — bg-attach 뷰 pane 은 셸이 spawn 디렉토리(~/Desktop)에 머물러
            // 파일트리가 pane 내용과 다른 프로젝트를 보여줬다(사용자).
            .and_then(|id| self.pane_view_cwd.get(id).cloned())
            .or_else(|| {
                active
                    .as_ref()
                    .and_then(|id| self.pane_cwd_cache.get(id).cloned())
            })
            // Preview panes (markdown/image splits) have no cwd in the cache —
            // keep the current tree root rather than snapping to the process
            // cwd, so opening a file doesn't reshuffle the sidebar root.
            .or_else(|| self.file_tree.root.clone())
            .or_else(|| std::env::current_dir().ok());
        // git 레포 앵커: cwd 가 레포 안이면 레포 루트를 트리 루트로 삼는다.
        // 없으면 cwd 그대로. 이게 없으면 `cd src/` 한 번에 사이드바가 그
        // 하위로 좁아져, 레포를 오가며 작업할 때마다 트리가 다시 접힌다.
        let root = root.map(|c| self.anchored_tree_root(&c).unwrap_or(c));
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
                    let Ok(rd) = std::fs::read_dir(dir) else {
                        continue;
                    };
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
    /// "파일 열기" 설정이 내장 편집기가 아니면 그쪽으로 보내고 `true`. `false` 면
    /// 호출자가 내장 경로를 그대로 이어 간다 — 편집기를 못 찾았을 때도 여기로
    /// 떨어져, 설정이 어긋나 있어도 파일은 늘 열린다.
    pub(super) fn open_file_elsewhere(&mut self, path: &std::path::Path) -> bool {
        // `"system"` 은 `"app"` 의 옛 저장값(앱 미지정 = OS 기본이라 뜻이 같다).
        match socket::read_file_open_mode().as_str() {
            "app" | "system" => self.open_file_in_app(path),
            "terminal" => self.open_file_in_editor_pane(path),
            _ => false,
        }
    }

    /// GUI 편집기로 넘긴다. 지정 앱을 **설치 목록에서 되찾아** 번들 경로로 여는
    /// 게 핵심 — 이 기기의 VS Code 는 `/Applications` 밖에 있어 이름만으로는
    /// LaunchServices 가 못 찾을 수 있다. 앱이 사라졌으면 OS 기본으로 넘기지 않고
    /// 내장 편집기로 되돌린다: 이 맥의 기본 연결 프로그램은 사용자가 목록에서
    /// 일부러 뺀 앱이라, 폴백이 그쪽으로 가면 고친 게 도로 나타난다.
    pub(super) fn open_file_in_app(&mut self, path: &std::path::Path) -> bool {
        let want = socket::read_file_open_app();
        if want.trim().is_empty() {
            crate::proc::open_path_default(path);
            return true;
        }
        match crate::proc::open_with_apps()
            .iter()
            .find(|(name, _)| *name == want)
        {
            Some((_, target)) => {
                crate::proc::open_path_with(target, path);
                true
            }
            None => {
                self.set_toast(format!("{want} 를 못 찾았어요 — 내장 편집기로 엽니다"));
                false
            }
        }
    }

    /// 새 split pane 을 열고 그 셸에 편집기 명령을 친다. helix·vim 처럼 터미널을
    /// 통째로 쓰는 편집기는 이렇게 띄우는 게 정공법이다 — kasaterm 이 터미널이니
    /// 멀티커서·LSP·코드접기가 우리 구현 없이 그대로 딸려 온다.
    pub(super) fn open_file_in_editor_pane(&mut self, path: &std::path::Path) -> bool {
        let cmd = socket::read_file_open_cmd();
        let cmd = if cmd.trim().is_empty() {
            socket::resolve_terminal_editor().unwrap_or_default()
        } else {
            cmd
        };
        if cmd.trim().is_empty() {
            self.set_toast("터미널 편집기를 못 찾았어요 — 내장 편집기로 엽니다".to_string());
            return false;
        }
        let Ok(pane) = self.split_active_pane_focused(kasa_pty::SplitDir::Horizontal) else {
            self.set_toast("pane 을 열지 못했어요 — 내장 편집기로 엽니다".to_string());
            return false;
        };
        let Some(sess) = self.pty.get(&pane).cloned() else {
            self.set_toast("pane 을 열지 못했어요 — 내장 편집기로 엽니다".to_string());
            return false;
        };
        // 900ms = 계정 추가·swap_character 와 같은 "셸 프롬프트가 뜰 즈음" 대기.
        // 더 일찍 보내면 셸이 아직 안 읽어 명령이 통째로 유실된다.
        let at = std::time::Instant::now() + std::time::Duration::from_millis(900);
        self.pending_restores
            .push((sess, format!("{}\r", editor_command_line(&cmd, path)), at));
        true
    }

    /// Open a sidebar file in a fresh split pane (right of the active pane).
    /// Images decode into an `Image` pane; real markdown renders as a laid-out
    /// doc; any other text loads as a fenced code block so the highlighter
    /// colors it. Re-opening a file already on screen just focuses its pane
    /// instead of stacking duplicate splits. PTY-less — `resize_backend` skips
    /// leaves with no `self.pty` entry, so the new pane never spawns a shell.
    pub(crate) fn open_file_split(&mut self, path: std::path::PathBuf) {
        self.open_file_routed(path, None, false, false);
    }

    /// 파일 미리보기를 연다. `as_tab`이면 `target`(=요청자 pid, `$KASATERM_PANE_ID`)
    /// 이 가리키는 pane 의 보조 탭으로 붙인다 — BSP 트리를 안 바꿔(크롬 탭) arona
    /// 멀티뷰가 빈 pane 으로 미러하던 문제를 피한다. `as_tab=false`(파일트리 더블클릭·
    /// 드롭)면 종전처럼 active pane 옆으로 split. target pane 을 못 찾으면 split 폴백.
    pub(crate) fn open_file(
        &mut self,
        path: std::path::PathBuf,
        target: Option<String>,
        as_tab: bool,
    ) {
        // 다른 기기의 파일이면 받아다 임시 자리에 두고 그 사본을 연다.
        let path = match self.file_tree.remote.clone() {
            Some((label, base)) if self.file_tree.root.as_ref().is_some_and(|r| path.starts_with(r)) => {
                match Self::fetch_remote_file(&label, &base, &path) {
                    Ok(local) => local,
                    Err(e) => {
                        self.set_toast(format!("{label} 의 파일을 못 받았어요 — {e:#}"));
                        return;
                    }
                }
            }
            _ => path,
        };
        self.open_file_routed(path, target, as_tab, !as_tab);
    }

    pub(super) fn open_file_routed(
        &mut self,
        path: std::path::PathBuf,
        target: Option<String>,
        as_tab: bool,
        detach_default: bool,
    ) {
        if self.tmux.is_some() {
            return;
        }
        let requested = if as_tab { target.as_deref() } else { None };
        let Ok(active) = self.resolve_user_mutation_anchor(
            requested,
            crate::settings_room::SettingsMutation::FilePreview,
        ) else {
            return;
        };
        // "파일 열기" 설정은 **사람이 연 것**에만 적용한다. `as_tab` 은 에이전트·
        // 소켓의 미리보기 요청이라, 그것까지 pane 을 새로 열면 파일을 보여 달랄
        // 때마다 화면이 쪼개진다.
        if !as_tab && !crate::is_image_path(&path) && self.open_file_elsewhere(&path) {
            return;
        }
        if detach_default && !crate::is_image_path(&path) {
            self.queue_aux_file(path, true);
            return;
        }
        // Already open? Focus that pane + tab rather than spawning a duplicate.
        let existing = {
            let ws = self.ws.lock().unwrap();
            ws.panes.iter().find_map(|(id, p)| {
                p.tabs
                    .iter()
                    .position(|t| t.preview_path.as_deref() == Some(path.as_path()))
                    .map(|tab_idx| (id.clone(), tab_idx))
            })
        };
        if let Some((id, tab_idx)) = existing {
            {
                let mut ws = self.ws.lock().unwrap();
                if let Some(p) = ws.panes.get_mut(&id) {
                    // 에이전트가 부른 것(`as_tab`)이면 이미 있는 탭도 앞으로 끌어내지
                    // 않는다 — 사람이 파일트리에서 누른 것만 「보여 달라」는 뜻이다.
                    if !as_tab {
                        p.active_tab = tab_idx.min(p.tabs.len().saturating_sub(1));
                    }
                    p.dirty = true;
                }
                if !as_tab {
                    ws.active_pane = Some(id);
                }
            }
            if !as_tab {
                self.handoff_ime_to_active_surface();
            }
            self.chrome_dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }

        let new_id = self.alloc_pane_id();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let content = if crate::is_image_path(&path) {
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
            let doc = Arc::new(build_markdown_doc(&path, &raw));
            // Markdown renders as a laid-out doc; code/text opens straight into
            // the raw editor (line-number gutter + syntax highlight + editable)
            // — the fenced-code-block render path mangled long lines and was
            // read-only, which is wrong for source files.
            let edit_lines: Arc<Vec<String>> = Arc::new(if is_md {
                Vec::new()
            } else {
                raw.split('\n').map(|s| s.to_string()).collect()
            });
            PaneContent::Markdown(MarkdownPane {
                saved_text: raw.clone(),
                doc,
                is_md_doc: is_md,
                raw_mode: !is_md,
                edit_lines,
                cur_line: 0,
                cur_col: 0,
                scroll: 0.0,
                h_scroll: 0.0,
                modified: false,
                sel_anchor: None,
                undo_stack: Vec::new(),
                redo_stack: Vec::new(),
                last_edit: EditKind::Break,
                find: None,
                complete: None,
                longest_cache: None,
                edit_gen: 0,
                diff: None,
                diff_peek: None,
                diff_head: None,
                wrap: false,
                extra: Vec::new(),
                undo_locked: false,
                folds: Vec::new(),
                folds_gen: 0,
                edited_at: None,
            })
        };

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
        if crate::is_image_path(&path) {
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
        // 탭 모드: 요청 pane(target=pid → outer_for_pty, 없으면 active)의 보조 탭으로
        // push. 트리를 안 바꾸므로 split 과 달리 resize_backend/publish 가 필요 없다
        // (image/markdown 은 PTY-less, pane_cells 기반 렌더라 redraw 면 충분). 대상 pane
        // 이 panes 에 실재할 때만(contains_key) 탭 경로; 아니면 아래 split 폴백(tab 재사용).
        // 사람이 연 것도 탭이다 — 터미널이 아닌 것은 칸을 차지하지 않는다(칸은 벤토 격자의 몫, docs/design.md 5).
        // 사람이 연 것만 앞 탭으로 세운다.
        let tab_outer = if self.ws.lock().unwrap().panes.contains_key(&active) {
            Some(active.clone())
        } else {
            None
        };
        if let Some(outer) = tab_outer {
            let mut ws = self.ws.lock().unwrap();
            if let Some(pane) = ws.panes.get_mut(&outer) {
                pane.tabs.push(tab);
                if !as_tab {
                    pane.active_tab = pane.tabs.len() - 1;
                }
                // **백그라운드 탭이다** — 활성 탭도 활성 pane 도 안 건드린다(사용자
                // 2026-08-13). 학생이 이미지를 보내면 그 pane 의 대화가 통째로 이미지에
                // 덮이고, 키보드 포커스까지 그 pane 으로 끌려가 다른 데서 타이핑 중이면
                // 뺏긴다. 그림 자체는 OSC 1337 인라인으로 대화 흐름 안에 이미 뜨므로
                // (ad6c04d), 이 탭은 「크게 볼 때 누르는 자리」로 족하다.
                pane.dirty = true;
            }
            if !as_tab {
                ws.active_pane = Some(outer);
            }
            drop(ws);
            if !as_tab {
                self.handoff_ime_to_active_surface();
            }
            self.chrome_dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }

        let ps = PaneState {
            tabs: vec![tab],
            dirty: true,
            ..Default::default()
        };
        self.ws.lock().unwrap().panes.insert(new_id.clone(), ps);

        let layout = self
            .pty_layout
            .as_mut()
            .expect("pty_layout set in start_pty");
        if !layout.split_leaf(&active, kasa_pty::SplitDir::Horizontal, new_id.clone()) {
            // Active pane isn't in the tree — undo the orphan insert. 번호는 따로
            // 되돌릴 게 없다: 등록을 지우면 alloc_pane_id 가 다시 빈 번호로 본다.
            self.ws.lock().unwrap().panes.remove(&new_id);
            return;
        }
        self.ws.lock().unwrap().active_pane = Some(new_id);
        self.handoff_ime_to_active_surface();
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    /// macOS odoc/argv uses the same default detached-document route as a
    /// person opening a text file from the tree. Window creation is deferred
    /// until winit supplies an ActiveEventLoop.
    pub(crate) fn open_markdown_window(&mut self, path: std::path::PathBuf) {
        self.queue_aux_file(path, true);
    }
    /// Walk the root + every expanded folder into the flat `file_tree_nodes`.
    /// 원격 폴더의 자식들 — 캐시에 있으면 세우고, 없으면 `missing` 에 적어 받아 오게 한다.
    pub(super) fn walk_remote(
        dir: &std::path::Path,
        depth: usize,
        expanded: &std::collections::HashSet<std::path::PathBuf>,
        cache: &HashMap<std::path::PathBuf, Vec<(String, bool, bool)>>,
        out: &mut Vec<FileNode>,
        missing: &mut Vec<std::path::PathBuf>,
    ) {
        let Some(entries) = cache.get(dir) else {
            missing.push(dir.to_path_buf());
            return;
        };
        for (name, is_dir, is_repo) in entries {
            let path = dir.join(name);
            out.push(FileNode {
                path: path.clone(),
                name: nfc_hangul(name),
                is_dir: *is_dir,
                depth,
                ignored: name.starts_with('.'),
                is_repo: *is_repo,
            });
            if *is_dir && expanded.contains(&path) {
                Self::walk_remote(&path, depth + 1, expanded, cache, out, missing);
            }
        }
    }

    /// 원격 폴더 한 층을 받아 온다. 오면 `fs_dirty` 로 다시 짓게 한다.
    pub(super) fn request_remote_dir(&self, base: &str, dir: std::path::PathBuf) {
        if let Ok(mut pending) = self.file_tree.remote_pending.lock() {
            if !pending.insert(dir.clone()) { return; }
        }
        let cache = self.file_tree.remote_cache.clone();
        let pending = self.file_tree.remote_pending.clone();
        let dirty = self.file_tree.fs_dirty.clone();
        let proxy = self.proxy.clone();
        let base = base.to_string();
        std::thread::spawn(move || {
            let query = format!("/term/tree?path={}", kasa_mcp::remote::urlencode(&dir.to_string_lossy()));
            let entries: Vec<(String, bool, bool)> = match kasa_mcp::remote::remote_get_json(&base, &query) {
                Ok(v) => v.get("entries").and_then(|e| e.as_array()).map(|arr| arr.iter().filter_map(|e| Some((
                    e.get("name")?.as_str()?.to_string(),
                    e.get("is_dir")?.as_bool()?,
                    e.get("is_repo").and_then(|v| v.as_bool()).unwrap_or(false),
                ))).collect()).unwrap_or_default(),
                // 빈 트리는 「왜 비었나」를 못 말한다 — 옛 판(창구 없음)이면 한 줄로 알린다.
                Err(e) if e.to_string().contains("404") => vec![
                    ("(그 기기가 옛 판이라 목록을 못 받아요 — 새 판으로 띄우면 보여요)".to_string(), false, false),
                ],
                Err(_) => vec![("(목록을 못 받았어요 — 연결을 확인해 주세요)".to_string(), false, false)],
            };
            if let Ok(mut c) = cache.lock() { c.insert(dir.clone(), entries); }
            if let Ok(mut p) = pending.lock() { p.remove(&dir); }
            dirty.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }

    /// 다른 기기의 파일을 받아 임시 자리에 둔다 — 열기는 그 사본으로(읽기 전용).
    pub(super) fn fetch_remote_file(label: &str, base: &str, path: &std::path::Path) -> Result<std::path::PathBuf> {
        let query = format!("/term/file?path={}", kasa_mcp::remote::urlencode(&path.to_string_lossy()));
        let bytes = kasa_mcp::remote::remote_get_bytes(base, &query)?;
        let rel = path.strip_prefix("/").unwrap_or(path);
        let local = std::env::temp_dir().join("kasaterm-remote").join(label).join(rel);
        if let Some(parent) = local.parent() { std::fs::create_dir_all(parent)?; }
        std::fs::write(&local, bytes)?;
        Ok(local)
    }

    pub(crate) fn rebuild_file_tree_nodes(&mut self) {
        self.file_tree.nodes.clear();
        if let Some((label, base)) = self.file_tree.remote.clone() {
            let Some(root) = self.file_tree.root.clone() else { return };
            let name = root.file_name().map(|n| nfc_hangul(&n.to_string_lossy()))
                .unwrap_or_else(|| root.to_string_lossy().into_owned());
            // 기기 이름은 안 붙인다 — 배경 기기색이 이미 말한다(2026-09-17 지적).
            let _ = &label;
            self.file_tree.nodes.push(FileNode {
                path: root.clone(),
                name,
                is_dir: true,
                depth: 0,
                ignored: false,
                is_repo: false,
            });
            let cache = self.file_tree.remote_cache.lock().map(|c| c.clone()).unwrap_or_default();
            let mut missing = Vec::new();
            if self.file_tree.expanded.contains(&root) {
                Self::walk_remote(&root, 1, &self.file_tree.expanded, &cache, &mut self.file_tree.nodes, &mut missing);
            }
            for dir in missing { self.request_remote_dir(&base, dir); }
            // 이 기계의 감시는 쉰다 — 저쪽 폴더는 여기 파일시스템에 없다.
            if let Ok(mut watch) = self.file_tree.watch.lock() { watch.clear(); }
            return;
        }
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
                is_repo: is_git_repo(&root),
            });
            if self.file_tree.expanded.contains(&root) {
                Self::walk_dir(
                    &root,
                    1,
                    &self.file_tree.expanded,
                    &mut self.file_tree.nodes,
                );
            }
            // Second pass: one batched `git check-ignore` over every visible
            // path marks the gitignored rows italic+dim. Dotfiles get the same
            // treatment regardless (check-ignore won't flag a tracked dotfile).
            let paths: Vec<String> = self
                .file_tree
                .nodes
                .iter()
                .map(|n| n.path.to_string_lossy().into_owned())
                .collect();
            // Dim dotfiles + whatever the background worker last resolved.
            // `git check-ignore` is NOT run inline — spawning git from the
            // unsigned exe stalls ~5s under Defender, which would freeze the
            // toggle. We hand the worker this (root, paths) and apply its
            // cached result; the worker wakes us when fresh ignores land.
            let ignored = self
                .file_tree
                .ignored
                .lock()
                .map(|g| g.clone())
                .unwrap_or_default();
            for n in &mut self.file_tree.nodes {
                n.ignored =
                    n.name.starts_with('.') || ignored.contains(n.path.to_string_lossy().as_ref());
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
                self.file_tree
                    .nodes
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
    pub(super) fn search_walk(dir: &std::path::Path, q: &str, depth: usize, out: &mut Vec<FileNode>) {
        if out.len() >= 300 || depth > 7 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let heavy = ["node_modules", "target", "dist", ".git", "build", ".next"];
        let mut subdirs: Vec<std::path::PathBuf> = Vec::new();
        for e in rd.filter_map(|e| e.ok()) {
            let name = nfc_hangul(&e.file_name().to_string_lossy());
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if name.to_lowercase().contains(q) {
                let p = e.path();
                let is_repo = is_dir && is_git_repo(&p);
                out.push(FileNode {
                    path: p,
                    name: name.clone(),
                    is_dir,
                    depth: 0,
                    ignored: false,
                    is_repo,
                });
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
        let Some((is_dir, name)) = self.file_tree.new.take() else {
            return;
        };
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
        let Some((path, name)) = self.file_tree.rename.take() else {
            return;
        };
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
                let p = e.path();
                let is_repo = is_dir && is_git_repo(&p);
                Some(FileNode {
                    path: p,
                    name,
                    is_dir,
                    depth,
                    ignored: false,
                    is_repo,
                })
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
}

pub(crate) fn tilde_home(s: &str) -> String {
    match kasa_socket::home_dir() {
        Some(h) => match s.strip_prefix(h.to_string_lossy().as_ref()) {
            Some(rest) => format!("~{rest}"),
            None => s.to_string(),
        },
        None => s.to_string(),
    }
}

/// `start` 부터 위로 올라가며 첫 git 레포 루트를 찾는다.
///
/// `.git` 은 일반 체크아웃이면 디렉토리, worktree·submodule 이면 **파일**이므로
/// `is_dir` 이 아니라 `exists` 로 봐야 둘 다 잡힌다.
///
/// 홈과 파일시스템 루트는 레포로 인정하지 않는다 — dotfiles 를 git 으로 관리하면
/// 홈 자체가 레포라, 앵커가 어느 프로젝트에서든 홈 전체로 튀어 사이드바가
/// 쓸모없어진다.
pub(crate) fn git_repo_root(start: &std::path::Path) -> Option<std::path::PathBuf> {
    let home = kasa_socket::home_dir();
    let mut cur = Some(start);
    while let Some(dir) = cur {
        if dir.parent().is_none() || home.as_deref() == Some(dir) {
            break;
        }
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

/// 끊어 붙인다 — 공백·한글·따옴표가 든 파일명이 명령을 쪼개지 못하게.
pub(super) fn editor_command_line(cmd: &str, path: &std::path::Path) -> String {
    let q = format!("'{}'", path.display().to_string().replace('\'', r"'\''"));
    if cmd.contains("{}") {
        cmd.replace("{}", &q)
    } else {
        format!("{} {q}", cmd.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_line_quotes_the_path_and_honors_the_placeholder() {
        let p = std::path::Path::new("/tmp/a b/main.rs");
        assert_eq!(editor_command_line("hx", p), "hx '/tmp/a b/main.rs'");
        assert_eq!(
            editor_command_line("code -w {} --goto 1", p),
            "code -w '/tmp/a b/main.rs' --goto 1"
        );
        // 따옴표가 든 이름이 인용을 깨고 나오면 뒤가 명령으로 실행된다.
        assert_eq!(
            editor_command_line("hx", std::path::Path::new("/tmp/it's.rs")),
            r"hx '/tmp/it'\''s.rs'"
        );
    }

    #[test]
    fn anchors_to_repo_root_from_a_subdirectory() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("workspace root");
        let sub = repo.join("app/kasaterm/src");
        assert_eq!(git_repo_root(&sub).as_deref(), Some(repo));
    }

    #[test]
    fn returns_none_outside_any_repo() {
        // /tmp 는 레포가 아니고 홈 아래도 아니라 위로 훑어도 `.git` 이 없다.
        assert_eq!(git_repo_root(std::path::Path::new("/tmp")), None);
    }
}
