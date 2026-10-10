//! 백엔드 기동 — 첫 칸(PTY), 옛 tmux 백엔드, 에이전트가 붙는 소켓 서버를 띄운다.
use super::*;

impl App {
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
        Ok(())
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
                mouse_motion,
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
                let resized = tp.cols != cols || tp.rows != rows || tp.cells.len() != rows as usize;
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
                if !alt_screen && !tp.prev_cells.is_empty() && tp.prev_cells.len() == tp.cells.len()
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
                tp.mouse_motion = mouse_motion;
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
                        let display = new_title.unwrap_or_else(|| "kasaterm".into());
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
        let proxy_events = self.proxy.clone();
        std::thread::spawn(move || {
            while let Ok(evt) = events.recv() {
                match evt {
                    TmuxEvent::LayoutChange { layout, .. } => {
                        // tmux's %layout-change emits both the visible
                        // and default layouts in one message,
                        // space-separated, plus a trailing flag.
                        // parse_layout wants exactly one layout
                        // string, so take the first token.
                        let first = layout.split_whitespace().next().unwrap_or(&layout);
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
                        // 백엔드 스레드의 active_pane은 GUI 이벤트 처리보다 늦다.
                        // 여기서 중복 제거하면 빠른 A→B→A의 마지막 A를 옛 상태와
                        // 같다고 버려 GUI가 B에 멈춘다. 순서 보존은 큐에 전부 싣고,
                        // 실제 no-op 판정은 GUI 스레드가 자기 최신 상태로 한다.
                        let _ = forward_backend_focus(pane_id, |id| {
                            proxy_events.send_event(UserEvent::BackendFocus(id))
                        });
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
        if !self.lite && !crate::verification_run() { self.register_account_sync(); }
        // 정본 포트. 이미 물려 있으면 spawn_http_server 가 임시 포트로 떨어진다.
        const CANONICAL_MCP_PORT: u16 = 8765;
        // lite 는 HTTP 서버를 안 띄운다 — 포트 파일이 본판 것과 섞인다.
        // 아래 unix 소켓(CLI)은 그대로.
        let http_port = if self.lite {
            Err(std::io::Error::other("lite: no http server"))
        } else {
            kasa_mcp::spawn_http_server(backend.clone(), CANONICAL_MCP_PORT)
        };
        let http_port = match http_port {
            Ok(port) => {
                eprintln!("[kasaspace-mcp] HTTP on 127.0.0.1:{port}");
                std::env::set_var("KASASPACE_MCP_PORT", port.to_string());
                // 다른 기계가 `/version` 으로 묻는 빌드 표식 — 앱 build.rs 가 박은
                // git 리비전. kasa-mcp 는 자기 것이 없어 여기서 넘긴다.
                kasa_mcp::machines::set_build_id(env!("KASATERM_GIT_REV"));
                // 재시작 작업이 이 부팅을 기다리고 있었으면 도착을 적는다 — 빌드 표식이 선 뒤라야 맞는 값이 실린다.
                crate::app_restart::mark_restart_booted();
                #[cfg(unix)]
                crate::app_update::mark_update_booted();
                if !crate::verification_run() {
                    kasa_mcp::agent_accounts::spawn();
                    kasa_mcp::agent_chains::spawn();
                    kasa_mcp::unregister_clients();
                }
                Some(port)
            }
            Err(e) => {
                if !self.lite {
                    eprintln!("[kasaspace-mcp] HTTP start failed: {e}");
                }
                None
            }
        };
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
        // Publish the port only now: the file is keyed to the *resolved* socket,
        // and until `Server::bind` returns we do not know it. Writing earlier
        // used the inherited env — so an instance launched from a pane stamped
        // its port next to the parent's socket and hijacked the parent's hooks.
        // A failed bind returns above without publishing, which is correct: a
        // port nobody can reach through this socket should not be advertised.
        if let Some(port) = http_port {
            let _ = std::fs::write(mcp_port_file_for(&resolved), port.to_string());
        }
        let _join = server.spawn(backend);
    }
    pub(crate) fn start_socket_tmux(&self, tmux: Arc<kasa_bridge::TmuxSession>) {
        self.start_socket_with(Arc::new(socket::TmuxBackend::new(tmux)));
    }
    /// Local PTY-mode socket server. Same cmux/MCP surface as tmux mode but
    /// backed by the GUI's own panes — pane writes/split/focus delegate to the
    /// GUI thread via the proxy (see socket::PtyBackend).
    pub(crate) fn start_socket_pty(&mut self) {
        let backend = Arc::new(socket::PtyBackend::new(
            self.proxy.clone(),
            self.ws.clone(),
            self.collab.attention.clone(),
            self.collab.hook_activity.clone(),
            self.pane_status_pub.clone(),
            self.bg_agents.clone(),
            self.collab.hub.clone(),
        ));
        backend.start_session_discovery();
        // 칸 안 mod 가 사실을 알리면 다음 판정이 메모를 건너뛰고, 승인·질문이 닫힌 칸에 밀린 tell 을 곧바로 꺼낸다.
        let (hub, proxy) = (self.collab.hub.clone(), self.proxy.clone());
        kasa_mcp::claude_mod::set_listener(move |_| {
            hub.invalidate();
            let _ = proxy.send_event(UserEvent::SafeTellWake);
        });
        let status_proxy = self.proxy.clone();
        kasa_mcp::claude_mod::set_status_listener(move |_| {
            let _ = status_proxy.send_event(UserEvent::Redraw);
        });
        crate::info_focus::start(&self.info.focus, self.info.sites.clone(), backend.clone(), self.proxy.clone());
        // GUI 쪽에도 핸들 보관 — ResumeSession 이 attach/재개 pane 의 transcript 를
        // bind hook 없이 즉석 확정(bind_transcript)할 때 쓴다.
        self.socket_backend = Some(backend.clone());
        if let Ok(mut shared) = self.shared_backend.lock() {
            *shared = Some(backend.clone());
        }
        self.start_socket_with(backend);
    }
}
