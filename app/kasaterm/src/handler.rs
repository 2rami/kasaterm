//! winit ApplicationHandler — App 의 이벤트 루프(window_event/new_events/user_event/resumed/about_to_wait).
//! main.rs 에서 분리. impl App 메서드·타입은 crate root 그대로 참조.
use super::*;

impl ApplicationHandler<UserEvent> for App {
    /// A background thread (PTY snapshot, socket) asked us to repaint.
    /// Delivered even while a WaitUntil is parked, so this is what makes
    /// committed-Hangul echo / backspace / space show up without lag.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        // Local cmux socket backend delegated a pane write / split / focus to
        // this GUI thread (the socket server can't touch self.pty directly).
        match &event {
            UserEvent::SocketBytes(sid, bytes) => {
                {
                    let target = match sid.as_deref() {
                        Some(id) => self.pty_for_pane(id),
                        None => self.active_pty(),
                    };
                    if let Some(p) = target {
                        // Ship the trailing CR/LF as its own *delayed* PTY
                        // write. Splitting the write alone isn't enough: the PTY
                        // is a byte stream, so a CR written right after the body
                        // lands in the *same* read() on the claude side (verified:
                        // one chunk b'\x15msg\r'), and Ink treats a CR fused to
                        // text as a newline insert, not a submit — the message
                        // types in but never fires (the "tell doesn't press
                        // enter" bug, even on an idle pane). A short delay makes
                        // the CR arrive as its own read (verified: b'msg' then a
                        // separate b'\r'), which Ink reads as Enter. 50ms is below
                        // human-perceptible latency.
                        let (body, submit) = crate::socket::split_trailing_submit(bytes);
                        if !body.is_empty() {
                            let _ = p.send_bytes(body);
                        }
                        if !submit.is_empty() {
                            let p2 = Arc::clone(p);
                            let submit = submit.to_vec();
                            std::thread::spawn(move || {
                                // 140ms: bracketed paste needs this gap so Ink
                                // finishes processing \x1b[200~…\x1b[201~ before
                                // the CR arrives (munder pattern). 50ms was enough
                                // for idle panes but too tight for menu state.
                                std::thread::sleep(std::time::Duration::from_millis(140));
                                let _ = p2.send_bytes(&submit);
                            });
                        }
                    }
                }
                self.render_frame();
                return;
            }
            UserEvent::SocketSplit(dir, focus, reply) => {
                // `split_active_pane` always sets the new pane active (correct
                // for the GUI's keyboard split). The socket path defaults to
                // no-focus so a scripted split doesn't yank the user's focus
                // (like `tell`) — restore the prior active pane unless the
                // caller opted in with `--focus`.
                let prev = self.ws.lock().unwrap().active_pane.clone();
                let new_id = self.split_active_pane(*dir).unwrap_or_default();
                if !*focus {
                    if let Some(prev) = prev {
                        self.ws.lock().unwrap().active_pane = Some(prev);
                    }
                }
                let _ = reply.send(new_id);
                self.chrome_dirty = true;
                self.render_frame();
                return;
            }
            UserEvent::SocketFocus(id) => {
                self.ws.lock().unwrap().active_pane = Some(id.clone());
                self.chrome_dirty = true;
                self.render_frame();
                return;
            }
            UserEvent::SocketAronaClose => {
                self.close_arona_panel();
                return;
            }
            UserEvent::SocketSwap(a, b) => {
                // swap_dir 와 같은 시퀀스: leaf id 교환 → 자리가 바뀐 두 PTY
                // 의 그리드 크기가 다를 수 있으니 resize 로 SIGWINCH.
                let swapped = self
                    .pty_layout
                    .as_mut()
                    .is_some_and(|tree| tree.swap_leaves(a, b));
                if swapped {
                    let (cols, rows) = self.window_cells();
                    self.resize_backend(cols, rows);
                    self.chrome_dirty = true;
                    self.render_frame();
                }
                return;
            }
            UserEvent::SocketSetRatio(id, ratio) => {
                let changed = self
                    .pty_layout
                    .as_mut()
                    .is_some_and(|tree| tree.set_leaf_ratio(id, *ratio));
                if changed {
                    let (cols, rows) = self.window_cells();
                    self.resize_backend(cols, rows);
                    self.chrome_dirty = true;
                    self.render_frame();
                }
                return;
            }
            UserEvent::SocketRevealTerminal(show, focus_pane) => {
                if let Some(w) = &self.window {
                    w.set_visible(*show);
                    if *show {
                        // 숨김 동안 OS 가 redraw 를 안 줬으니 복귀 프레임을
                        // 직접 청구해야 마지막 화면 그대로 멈춰 보이지 않는다.
                        w.focus_window();
                        w.request_redraw();
                    }
                }
                if *show {
                    if let Some(id) = focus_pane {
                        self.ws.lock().unwrap().active_pane = Some(id.clone());
                        self.chrome_dirty = true;
                        self.render_frame();
                    }
                }
                return;
            }
            UserEvent::SocketQueryActivePid(reply) => {
                let pid = self
                    .ws
                    .lock()
                    .unwrap()
                    .active_pane
                    .clone()
                    .and_then(|id| self.pty.get(&id).and_then(|s| s.shell_pid()));
                let _ = reply.send(pid);
                return;
            }
            UserEvent::SocketQueryPanePids(reply) => {
                // 메모리 조회만(즉답) — 느린 lsof/ps 발견은 backend 스레드가 한다.
                let pids: Vec<(String, u32)> = self
                    .pty
                    .iter()
                    .filter_map(|(id, s)| s.shell_pid().map(|p| (id.clone(), p)))
                    .collect();
                let _ = reply.send(pids);
                return;
            }
            UserEvent::SocketClose(id) => {
                self.close_pane(id);
                self.chrome_dirty = true;
                self.render_frame();
                return;
            }
            UserEvent::SocketOpenPreview(path) => {
                // imgopen/mdopen·SendUserFile 훅 → 미리보기 pane split(이미지/마크다운).
                self.open_file_split(std::path::PathBuf::from(path));
                self.chrome_dirty = true;
                self.render_frame();
                return;
            }
            UserEvent::SocketRename(id, title) => {
                if let Some(p) = self.ws.lock().unwrap().panes.get_mut(id) {
                    let at = p.active_tab.min(p.tabs.len() - 1);
                    // Pin so the inner program's OSC 0/2 titles stop overriding
                    // the name the user just set (matches surface.rename intent).
                    p.tabs[at].title = Some(title.clone());
                    p.tabs[at].title_pinned = true;
                }
                self.chrome_dirty = true;
                self.render_frame();
                return;
            }
            UserEvent::SocketRenameWindow(id, title) => {
                // Mark the window/session the pane belongs to. window_of_pane
                // resolves the index; the override wins in refresh_window_labels
                // so the sidebar session reads the god marker even when this
                // pane isn't the window's representative leaf.
                if let Some(wi) = self.window_of_pane(id) {
                    self.window_name_override.insert(wi, title.clone());
                    self.window_labels_at = None; // force a relabel next paint
                }
                self.chrome_dirty = true;
                self.render_frame();
                return;
            }
            UserEvent::SocketColor(id, color) => {
                if let Some(p) = self.ws.lock().unwrap().panes.get_mut(id) {
                    p.color = Some(*color);
                }
                self.chrome_dirty = true;
                self.render_frame();
                return;
            }
            UserEvent::Notify { surface_id, title, body } => {
                self.handle_notify(surface_id, title, body);
                self.render_frame();
                return;
            }
            UserEvent::Attention { surface_id, reason } => {
                self.handle_attention(surface_id, reason);
                self.render_frame();
                return;
            }
            UserEvent::GitOpDone => {
                self.git.op = None;
                self.chrome_dirty = true;
                self.render_frame();
                return;
            }
            _ => {}
        }
        // Render directly here instead of request_redraw → (next loop)
        // RedrawRequested. The PTY echo already paid a thread hop +
        // channel to reach us; bouncing through request_redraw adds
        // another winit cycle of latency. Painting inline gets the echo
        // on screen this turn.
        self.render_frame();
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Persist every session's layout + pane cwds + claude sessions so the
        // next launch restores the full workspace (A3).
        self.save_session_state();
        // Persist the window size so the next launch restores it instead of the
        // hardcoded default (껐던 크기 복원).
        if let Some(win) = self.window.as_ref() {
            let scale = win.scale_factor().max(0.5);
            let sz = win.inner_size();
            crate::socket::write_window_size(sz.width as f64 / scale, sz.height as f64 / scale);
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        // Ask for desktop-notification permission up front so the prompt
        // appears at launch rather than mid-work on the first completion.
        crate::chrome::ensure_notification_authorization();
        // macOS menu bar: app submenu (About/Quit) + a "보기" submenu with
        // the "Git 패널" toggle. Built once (NSApp exists by resumed). Clicks
        // arrive on muda's global channel, drained in about_to_wait. Stored
        // on self so the menu outlives this function.
        #[cfg(target_os = "macos")]
        if self.menu.is_none() {
            use muda::{Menu, MenuItem, PredefinedMenuItem, Submenu};
            let menu = Menu::new();
            let app_m = Submenu::new("kasaterm", true);
            let _ = app_m.append_items(&[
                &PredefinedMenuItem::about(None, None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::quit(None),
            ]);
            let view_m = Submenu::new("보기", true);
            let git_item = MenuItem::new("Git 패널 켜기/끄기", true, None);
            let session_item = MenuItem::new("세션 패널 켜기/끄기", true, None);
            let board_item = MenuItem::new("board 패널 켜기/끄기", true, None);
            let arona_item = MenuItem::new("아로나 켜기/끄기", true, None);
            let _ = view_m.append(&git_item);
            let _ = view_m.append(&session_item);
            let _ = view_m.append(&board_item);
            let _ = view_m.append(&arona_item);
            let _ = menu.append(&app_m);
            let _ = menu.append(&view_m);
            menu.init_for_nsapp();
            self.git_menu_item = Some(git_item);
            self.session_menu_item = Some(session_item);
            self.board_menu_item = Some(board_item);
            self.arona_menu_item = Some(arona_item);
            self.menu = Some(menu);
        }
        // WaitUntil so the cursor blink ticks even when no terminal output
        // is arriving — the redraw inside RedrawRequested re-arms the
        // schedule. Pure Wait would freeze the blink mid-phase, Poll would
        // burn CPU on idle.
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + std::time::Duration::from_millis(BLINK_HALF_PERIOD_MS),
        ));
        // Restore the last window size; fall back to the default on first run.
        let (init_w, init_h) =
            crate::socket::read_window_size().unwrap_or((1100.0, 860.0));
        let attrs = WindowAttributes::default()
            .with_title("kasaterm")
            // Force dark appearance so the system titlebar paints its
            // text in light gray. Default is "follow OS", which would
            // give black text on our dark content view and make the
            // process-name label nearly invisible in light mode.
            .with_theme(Some(Theme::Dark))
            .with_inner_size(LogicalSize::new(init_w, init_h));
        // Custom chrome: traffic-light row sits inside the content view
        // so we can paint tabs and drag handles right next to the
        // native buttons. OS still owns the traffic lights themselves
        // and the resize edges — we only paint and route drag from the
        // strip above the cell grid.
        #[cfg(target_os = "macos")]
        let attrs = attrs
            .with_titlebar_transparent(true)
            // Hide the OS-drawn window title (the centered OSC/process
            // label) — the title strip stays clean, just traffic lights +
            // our sidebar-toggle button.
            .with_title_hidden(true)
            .with_fullsize_content_view(true);
        // Windows: drop the native title bar entirely so our chrome strip is
        // the only top bar (no double titlebar). We then paint our own
        // min/max/close, route window drag from the strip, and handle resize
        // from the window edges (see window_event mouse handling).
        #[cfg(windows)]
        let attrs = attrs.with_decorations(false);
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("create window"),
        );
        // Start the launch banner clock when the window actually appears,
        // not at struct construction (which can precede the first frame).
        self.version_anim_start = Instant::now();
        // Without IME enabled, Hangul / kana would arrive as raw key
        // events instead of composing into 안 / 한 / 글.
        // We compose Hangul ourselves via the in-process hangul-ime
        // Composer, so the OS IME stays out of the way. Leaving the
        // platform IME on means macOS fires its own Preedit one key
        // late (the very first jamo after a script switch comes only
        // through KeyboardInput), which produced the "조합이 첫 글자만
        // 안 돼" symptom. With the platform IME disabled we still
        // receive the Hangul jamo on KeyboardInput.text because the
        // selected keyboard layout produces them — we just take the
        // composition into our own hands from there.
        // IME ownership splits per-platform:
        //   macOS: NSTextInputContext drops the first jamo after a
        //     script switch (only KeyboardInput.text fires), so we
        //     refuse OS IME and run hangul-ime/Composer ourselves.
        //   Windows / Linux: the OS IME is the only path that gets us
        //     completed Hangul syllables — set_ime_allowed(true) so
        //     Ime::Preedit / Ime::Commit drive composition.
        #[cfg(target_os = "macos")]
        window.set_ime_allowed(false);
        #[cfg(not(target_os = "macos"))]
        window.set_ime_allowed(true);
        // Cursor-blink timer thread. Ticks every blink half-period and
        // wakes the loop through the proxy, so about_to_wait can sit on
        // ControlFlow::Wait — no WaitUntil timer in the hot path for
        // macOS to coalesce. sleep() drift is irrelevant; the actual
        // phase is computed from last_input_at in cursor_blink_on.
        {
            let blink_proxy = self.proxy.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(BLINK_HALF_PERIOD_MS));
                if blink_proxy.send_event(UserEvent::Redraw).is_err() {
                    break;
                }
            });
        }
        // Sidebar git-badge poller. The sidebar paint publishes each window's
        // repr cwd into `git_poll_cwds`; this thread shells out to `git_badge`
        // off the main thread and wakes the loop only when a badge actually
        // changed — an idle repo costs one cheap git call per distinct cwd
        // every interval, with no repaint. Dedups cwds so N windows in one
        // repo run git once.
        {
            let git_proxy = self.proxy.clone();
            let poll_cwds = self.git_poll_cwds.clone();
            let git_cache = self.window_git.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(1500));
                let targets: Vec<std::path::PathBuf> = poll_cwds.lock().unwrap().clone();
                let mut next: HashMap<std::path::PathBuf, kasa_mcp::git::GitBadge> =
                    HashMap::new();
                for cwd in targets {
                    if next.contains_key(&cwd) {
                        continue;
                    }
                    if let Some(b) = kasa_mcp::git::git_badge(&cwd) {
                        next.insert(cwd, b);
                    }
                }
                let mut guard = match git_cache.lock() {
                    Ok(g) => g,
                    Err(_) => break,
                };
                if *guard != next {
                    *guard = next;
                    drop(guard);
                    if git_proxy.send_event(UserEvent::Redraw).is_err() {
                        break;
                    }
                }
            });
        }
        // Right-hand git-column poller. The render publishes the active pane's
        // cwd into `git_col_cwd`; this thread runs the full `git_status`
        // (porcelain v2 + shortstat) off the main thread and wakes the loop
        // only when the snapshot actually changes — so an unchanged repo costs
        // one git call per interval with no repaint. Separate from the badge
        // poller above because this one needs the file list, not just the
        // branch/+/- summary.
        {
            let git_proxy = self.proxy.clone();
            let panel_cwd = self.git.col_cwd.clone();
            let panel_data = self.git.col_data.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(1200));
                let cwd = panel_cwd.lock().ok().and_then(|g| g.clone());
                let Some(cwd) = cwd else { continue };
                // A transient git failure ('index.lock' contention while another
                // pane commits, a half-written index, …) returns None — skip
                // this tick and keep the last good snapshot so the column never
                // flashes the notice mid-operation.
                let Some(view) = fetch_git_col_view(&cwd) else { continue };
                let mut guard = match panel_data.lock() {
                    Ok(g) => g,
                    Err(_) => break,
                };
                if *guard != view {
                    *guard = view;
                    drop(guard);
                    if git_proxy.send_event(UserEvent::Redraw).is_err() {
                        break;
                    }
                }
            });
        }
        // cell-renderer GPU path is the only path. The old sugarloaf
        // opt-in branch (KASATERM_RENDERER=sugarloaf) was removed once
        // cell-renderer absorbed P3 colour reproduction (shader
        // sRGB→DisplayP3 + root metal layer install). sugarloaf never
        // had the chrome UI ported across; keeping the branch in was
        // bloating the binary for no user-facing benefit.
        let renderer = gpu::GpuRenderer::new(window.clone(), FONT_SIZE)
            .expect("GpuRenderer init");
        self.cell = CellGeom {
            w: renderer.cell_w,
            h: renderer.cell_h,
            baseline: 0.0,
        };
        let scale = window.scale_factor() as f32 * self.ui_zoom;
        eprintln!(
            "[startup] gpu renderer; cell_geom w={:.2} h={:.2} (scale={scale})",
            self.cell.w, self.cell.h,
        );
        self.gpu = Some(renderer);
        self.window = Some(window);
        // Backend selection. Defaults to the Phase C direct-PTY path —
        // no tmux daemon, no `set -g focus-events` warnings inside
        // Claude Code, no kasaterm-cli's tmux quirks. KASATERM_BACKEND=tmux
        // opts back into the tmux-bridge multiplexer when the user wants
        // the multi-pane layout features that the in-process pty
        // multiplexer doesn't have yet.
        let want_tmux = std::env::var("KASATERM_BACKEND")
            .map(|v| v.eq_ignore_ascii_case("tmux"))
            .unwrap_or(false);
        let backend_result = if want_tmux {
            self.start_tmux()
        } else {
            self.start_pty()
        };
        if let Err(e) = backend_result {
            eprintln!("[kasaterm] backend start failed: {e}");
        }
        self.schedule_autosend();
        self.schedule_autocapture();
        self.arm_autosplit();
        self.arm_autowindows();
        self.arm_autotoggle();
        self.arm_autoarona();
        // 온보딩 제거(거노) — 강제 ModePicker 자동오픈 안 함. 터미널이 기본,
        // SCHALE OS 는 타이틀바 ✨ 버튼/단축키(Cmd+Shift+A)로 켠다(progressive disclosure).
        self.arm_autotabs();
        self.arm_autodrag();
        self.arm_autopanemove();
        self.arm_autoopen();
        self.arm_autoconfirm();
        self.schedule_autoquit();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: WindowId,
        event: WindowEvent,
    ) {
        // Child panel windows (session/board) drive their own wry webviews.
        // Their events must never reach the terminal logic below: without this
        // guard a panel's Resized/ScaleFactorChanged falls through and calls
        // gpu.resize() with the panel's tiny size, shrinking the main wgpu
        // viewport uniform → everything renders ~2x zoomed; a CloseRequested
        // would exit the whole app instead of just closing the panel.
        if self.session_panel_window.as_ref().map(|w| w.id()) == Some(id) {
            match &event {
                WindowEvent::CloseRequested => {
                    self.session_panel_webview = None;
                    self.session_panel_window = None;
                }
                WindowEvent::Resized(size) => {
                    if let Some(wv) = self.session_panel_webview.as_ref() {
                        let _ = wv.set_bounds(wry::Rect {
                            position: wry::dpi::PhysicalPosition::new(0, 0).into(),
                            size: wry::dpi::PhysicalSize::new(size.width, size.height).into(),
                        });
                    }
                }
                _ => {}
            }
            return;
        }
        if self.board_panel_window.as_ref().map(|w| w.id()) == Some(id) {
            match &event {
                WindowEvent::CloseRequested => {
                    self.board_panel_webview = None;
                    self.board_panel_window = None;
                }
                WindowEvent::Resized(size) => {
                    if let Some(wv) = self.board_panel_webview.as_ref() {
                        let _ = wv.set_bounds(wry::Rect {
                            position: wry::dpi::PhysicalPosition::new(0, 0).into(),
                            size: wry::dpi::PhysicalSize::new(size.width, size.height).into(),
                        });
                    }
                }
                _ => {}
            }
            return;
        }
        if self.arona_panel_window.as_ref().map(|w| w.id()) == Some(id) {
            match &event {
                WindowEvent::CloseRequested => {
                    // 메인 창 복귀까지 포함한 단일 닫기 경로 — 여기서 직접
                    // 필드를 비우면 reveal 을 빼먹어 터미널이 영영 숨는다.
                    self.close_arona_panel();
                }
                WindowEvent::Resized(size) => {
                    if let Some(wv) = self.arona_panel_webview.as_ref() {
                        let _ = wv.set_bounds(wry::Rect {
                            position: wry::dpi::PhysicalPosition::new(0, 0).into(),
                            size: wry::dpi::PhysicalSize::new(size.width, size.height).into(),
                        });
                    }
                }
                _ => {}
            }
            return;
        }
        // Preview windows (image viewer / markdown editor): same isolation as
        // the panels above. A CloseRequested drops just that one entry
        // (window + its webview together); everything else is swallowed so a
        // preview window's resize never touches the terminal's wgpu surface.
        if let Some(pos) = self
            .preview_windows
            .iter()
            .position(|(w, _)| w.id() == id)
        {
            if matches!(event, WindowEvent::CloseRequested) {
                self.preview_windows.remove(pos);
            }
            return;
        }
        let Some(window) = self.window.clone() else { return; };
        // gpu path uses our own wgpu surface, sugarloaf path keeps
        // its renderer. Only resize / rescale touch the surface
        // owner — everything else (keyboard, mouse, IME, wheel,
        // redraw) is renderer-agnostic.
        let gpu_mode = self.gpu.is_some();
        // Any winit event that *isn't* RedrawRequested counts as a
        // chrome change for the damage gate. RedrawRequested itself
        // never sets the flag — otherwise the early-return at the
        // top of render_frame could never short-circuit a pure-PTY
        // burst.
        if !matches!(event, WindowEvent::RedrawRequested) {
            self.chrome_dirty = true;
        }
        match event {
            WindowEvent::CloseRequested => {
                // A running job (claude / build / editor) gets a confirm modal
                // first; an idle window quits straight away.
                if !self.confirm_or_close_window() {
                    event_loop.exit();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor: _, .. } => {
                let size = window.inner_size();
                if gpu_mode {
                    if let Some(g) = self.gpu.as_mut() {
                        g.resize(size.width, size.height);
                    }
                }
                // DPI changed (monitor move / display-scale change). The
                // renderer's internal scale must follow the window's new
                // scale_factor — otherwise logical→physical mapping is off and
                // the frame compresses into a corner. apply_effective_scale
                // pushes set_scale + font metrics + cell geom + PTY resize.
                // (apply_effective_scale's doc names this exact case as its
                // intended-but-unwired "(future)" caller.)
                self.apply_effective_scale();
                // macOS live-resize coalesces queued RedrawRequested, so paint
                // synchronously here — otherwise the window frame leads and the
                // grid catches up a frame later (ghostty parity). Wrap in a
                // CATransaction with implicit animations off so AppKit doesn't
                // interpolate stale contents to the new bounds on zoom.
                self.chrome_dirty = true;
                gpu::with_disabled_layer_actions(|| {
                    self.render_frame();
                });
            }
            WindowEvent::Resized(size) => {
                if std::env::var_os("KASATERM_RESIZE_DEBUG").is_some() {
                    eprintln!(
                        "[rsz {}ms] Resized {}x{} live={}",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_millis()
                            % 100000,
                        size.width,
                        size.height,
                        gpu::is_in_live_resize(&window)
                    );
                }
                // Beats-ghostty live-resize: chrome + cells reflow EVERY
                // Resized event. wgpu surface.configure + render_frame
                // happen every frame; PTY reshape (SIGWINCH + alacritty +
                // cell reflow) only fires when the integer cell count
                // actually shifted past a boundary — typically 5-10 times
                // per drag, cheap enough that the shell stays current
                // without spamming itself between cell-edge crossings.
                if gpu::is_in_live_resize(&window) {
                    self.pending_resize = Some(size);
                    gpu::with_disabled_layer_actions(|| {
                        if gpu_mode {
                            if let Some(g) = self.gpu.as_mut() {
                                g.resize(size.width, size.height);
                            }
                        }
                        let (cols, rows) = self.window_cells();
                        if (cols, rows) != self.last_resized_cells {
                            self.last_resized_cells = (cols, rows);
                            // Reshape the PTY on every cell-boundary crossing
                            // during a live drag. The (cols,rows) guard above
                            // already coalesces sub-cell pixel moves, so the
                            // shell reflows the instant the integer grid grows
                            // — no throttle, the divider path does the same.
                            self.resize_backend(cols, rows);
                        }
                        self.chrome_dirty = true;
                        self.render_frame();
                    });
                    return;
                }
                self.pending_resize = None;
                gpu::with_disabled_layer_actions(|| {
                    if gpu_mode {
                        if let Some(g) = self.gpu.as_mut() {
                            g.resize(size.width, size.height);
                        }
                    }
                    let (cols, rows) = self.window_cells();
                    if (cols, rows) != self.last_resized_cells {
                        self.last_resized_cells = (cols, rows);
                        self.resize_backend(cols, rows);
                    }
                    self.chrome_dirty = true;
                    self.render_frame();
                });
            }
            WindowEvent::ModifiersChanged(mods) => {
                let new = mods.state();
                // Alt/Option held → show pane numbers (tmux display-panes).
                let alt = new.alt_key();
                if alt != self.show_pane_numbers {
                    self.show_pane_numbers = alt;
                    self.chrome_dirty = true;
                    window.request_redraw();
                }
                self.modifiers = new;
            }
            // 포커스/가림 복귀 시 즉시 다시 그린다. idle은 ControlFlow::Wait라
            // 이 이벤트가 redraw를 안 걸면 다음 blink 타이머(530ms)가 깨울
            // 때까지 화면이 stale — "다른 앱 보다가 돌아오면 0.5초 늦음"의 원인.
            WindowEvent::Focused(focused) => {
                self.window_focused = focused;
                self.chrome_dirty = true;
                window.request_redraw();
            }
            WindowEvent::Occluded(false) => {
                self.chrome_dirty = true;
                window.request_redraw();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_wheel(delta);
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = self.effective_scale();
                if self.autohover.is_none() {
                    self.cursor_px = (position.x as f32 / scale, position.y as f32 / scale);
                }
                // A deferred titlebar press turns into a window move once the
                // pointer travels past the threshold (so a stationary press
                // stays a click and the double-click path keeps working).
                if let Some((px, py)) = self.titlebar_drag_pending {
                    let (cx, cy) = self.cursor_px;
                    if (cx - px).abs() > 4.0 || (cy - py).abs() > 4.0 {
                        self.titlebar_drag_pending = None;
                        let _ = window.drag_window();
                        return;
                    }
                }
                // Commit modal / settings screen are full-window overlays over
                // the pane grid — drive their cursor here (I-beam over a text
                // field, default elsewhere) and skip the pane/column hover below
                // so it can't override the cursor.
                if self.git.commit_modal_open || self.settings_open {
                    let (cx, cy) = self.cursor_px;
                    let hit = |r: (f32, f32, f32, f32)| {
                        cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                    };
                    let want_text = self.git.commit_input_rect.map(hit).unwrap_or(false)
                        || (self.settings_open
                            && self.settings_rects.iter().any(|(a, r)| {
                                matches!(a, SettingsAction::FocusCwdPath | SettingsAction::FocusShell)
                                    && hit(*r)
                            }));
                    if want_text != self.text_cursor_shown {
                        self.text_cursor_shown = want_text;
                        window.set_cursor(if want_text { CursorIcon::Text } else { CursorIcon::Default });
                    }
                    self.chrome_dirty = true;
                    window.request_redraw();
                    return;
                }
                // In-pane tab hover tracking — drives the hover-only × +
                // brightened text on inactive tabs. Updated on every move but
                // only redraws when the hovered tab actually changes.
                {
                    let (cx, cy) = self.cursor_px;
                    let new_hover = self
                        .pane_tab_rects
                        .iter()
                        .find(|(_, _, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                        .map(|(id, idx, _)| (id.clone(), *idx));
                    if new_hover != self.pane_tab_hover {
                        self.pane_tab_hover = new_hover;
                        self.chrome_dirty = true;
                        window.request_redraw();
                    }
                }
                // File-tree row hover — drives the row highlight.
                {
                    let (cx, cy) = self.cursor_px;
                    let new_hover = self
                        .file_tree.rects
                        .iter()
                        .find(|(_, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                        .map(|(p, _)| p.clone());
                    if new_hover != self.file_tree.hover {
                        self.file_tree.hover = new_hover;
                        self.chrome_dirty = true;
                        window.request_redraw();
                    }
                }
                // File-tree path drag: once the cursor leaves the press point,
                // a held row becomes a drag. Releasing over a terminal pane
                // types its path into that shell (handled on release).
                let dragging_tree = if let Some(drag) = self.file_tree.drag.as_mut() {
                    if !drag.active {
                        let (cx, cy) = self.cursor_px;
                        if (cx - drag.start.0).abs() > 4.0 || (cy - drag.start.1).abs() > 4.0 {
                            drag.active = true;
                            window.set_cursor(CursorIcon::Grabbing);
                        }
                    }
                    drag.active
                } else {
                    false
                };
                // While dragging, repaint every move so the ghost pill tracks
                // the cursor.
                if dragging_tree {
                    self.chrome_dirty = true;
                    window.request_redraw();
                }
                // I-beam mouse cursor over the search box / new-entry naming
                // row, restored to default on the way out. Only flipped on the
                // transition so it doesn't fight other cursor setters.
                {
                    let (cx, cy) = self.cursor_px;
                    let hit = |r: (f32, f32, f32, f32)| {
                        cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                    };
                    // I-beam over the file-tree text inputs (search box / inline
                    // new-entry name). The commit modal + settings screen are
                    // full-window overlays, handled by the earlier branch.
                    let want_text = (self.file_tree.visible && hit(self.file_tree.search_rect))
                        || (self.file_tree.new.is_some() && hit(self.file_tree.new_row_rect));
                    if want_text != self.text_cursor_shown {
                        self.text_cursor_shown = want_text;
                        window.set_cursor(if want_text {
                            CursorIcon::Text
                        } else {
                            CursorIcon::Default
                        });
                    }
                }
                // File-tree column hover — repaint while the cursor is over the
                // column so the hover-only scrollbar thumb appears (and clears
                // on the way out). The render reads cursor_px live.
                if self.file_tree.visible {
                    let (cx, cy) = self.cursor_px;
                    let tx = self.file_tree_col_x();
                    if cy > TITLE_HEIGHT && cx >= tx && cx < tx + self.file_tree_col_w() {
                        self.chrome_dirty = true;
                        window.request_redraw();
                    }
                }
                // Git column hover — repaint so the row + button highlights
                // track the cursor (the render reads cursor_px live). Only
                // while the cursor is actually over the column, so it costs
                // nothing elsewhere.
                if self.git.col_visible {
                    let (cx, cy) = self.cursor_px;
                    if cy > TITLE_HEIGHT && cx >= self.git_col_x() {
                        self.chrome_dirty = true;
                        window.request_redraw();
                    }
                }
                // Sidebar resize drag in progress: update width and reflow.
                if let Some((start_x, start_w)) = self.sidebar_resize {
                    let new_w = (start_w + (self.cursor_px.0 - start_x)).clamp(140.0, 520.0);
                    if (new_w - self.sidebar_w_logical).abs() > 0.5 {
                        self.sidebar_w_logical = new_w;
                        let (cols, rows) = self.window_cells();
                        self.resize_backend(cols, rows);
                        self.chrome_dirty = true;
                        window.set_cursor(CursorIcon::ColResize);
                        window.request_redraw();
                    }
                    return;
                }
                // File-tree column resize drag in progress.
                if let Some((start_x, start_w)) = self.file_tree.resize {
                    let new_w = (start_w + (self.cursor_px.0 - start_x))
                        .clamp(FILE_TREE_W_MIN, FILE_TREE_W_MAX);
                    if (new_w - self.file_tree.w_logical).abs() > 0.5 {
                        self.file_tree.w_logical = new_w;
                        let (cols, rows) = self.window_cells();
                        self.resize_backend(cols, rows);
                        self.chrome_dirty = true;
                        window.set_cursor(CursorIcon::ColResize);
                        window.request_redraw();
                    }
                    return;
                }
                // Git column resize drag in progress. Its grip is the LEFT edge
                // (flush-right column), so dragging left widens it — hence the
                // negated delta versus the left-hand columns.
                if let Some((start_x, start_w)) = self.git.col_resize {
                    let new_w = (start_w - (self.cursor_px.0 - start_x))
                        .clamp(GIT_COL_W_MIN, GIT_COL_W_MAX);
                    if (new_w - self.git.col_w_logical).abs() > 0.5 {
                        self.git.col_w_logical = new_w;
                        let (cols, rows) = self.window_cells();
                        self.resize_backend(cols, rows);
                        self.chrome_dirty = true;
                        window.set_cursor(CursorIcon::ColResize);
                        window.request_redraw();
                    }
                    return;
                }
                // Divider drag in progress: ghostty parity — visually
                // update on every cursor move (so the seam tracks the
                // cursor pixel-by-pixel), AND fire `resize_backend` on
                // every cell-boundary crossing so the shells reflow live.
                // The flicker that used to come with this is gone because:
                //   1. pump_pty_screens preserves cell content across a
                //      resize (no blank-then-fill gap)
                //   2. the render path clips cells to the layout pane
                //      rect, so any stale dims that bleed past the seam
                //      get truncated before the user sees them
                if let Some((path, dir)) = self.resize_drag.clone() {
                    let (cols, rows) = self.window_cells();
                    let pad = WINDOW_PADDING + self.effective_sidebar_w();
                    let pos = match dir {
                        kasa_pty::SplitDir::Horizontal => (((self.cursor_px.0 - pad)
                            / self.cell.w.max(1.0))
                        .round() as i32)
                            .clamp(0, cols as i32) as u16,
                        kasa_pty::SplitDir::Vertical => (((self.cursor_px.1 - TITLE_HEIGHT)
                            / self.cell.h.max(1.0))
                        .round() as i32)
                            .clamp(0, rows as i32) as u16,
                    };
                    if Some(pos) != self.last_divider_pos {
                        if let Some(tree) = self.pty_layout.as_mut() {
                            tree.resize_divider(&path, pos, cols, rows);
                        }
                        self.last_divider_pos = Some(pos);
                        self.publish_pty_layout();
                        // PTY reshape is the expensive bit (Claude Code does
                        // a full TUI repaint on every SIGWINCH). Layout
                        // updates every cursor move for the live seam, but
                        // SIGWINCH only fires at ~10 Hz so the shells don't
                        // melt down. The render-time clip hides the
                        // mismatch between layout dims and PTY dims.
                        let now = std::time::Instant::now();
                        let pty_throttle = self
                            .last_divider_pty_resize
                            .map(|t| now.duration_since(t)
                                >= std::time::Duration::from_millis(100))
                            .unwrap_or(true);
                        if pty_throttle {
                            self.resize_backend(cols, rows);
                            self.last_divider_pty_resize = Some(now);
                        }
                    }
                    window.request_redraw();
                    return;
                }
                // Image pan drag: slide the zoomed image by the cursor delta,
                // clamped to the slack so it can't be dragged off the texture.
                if let Some((pane_id, start, base)) = self.image_pan_drag.clone() {
                    let (mx, my) = self.image_pan_bounds(&pane_id);
                    let nx = (base.0 + (self.cursor_px.0 - start.0)).clamp(-mx, mx);
                    let ny = (base.1 + (self.cursor_px.1 - start.1)).clamp(-my, my);
                    if let Ok(mut ws) = self.ws.lock() {
                        if let Some(pane) = ws.panes.get_mut(&pane_id) {
                            pane.image_pan_x = nx;
                            pane.image_pan_y = ny;
                            pane.dirty = true;
                        }
                    }
                    window.set_cursor(CursorIcon::Grabbing);
                    window.request_redraw();
                    return;
                }
                // Tab reorder drag: flip to active past the threshold, then
                // re-derive the drop index from the cursor's x over this
                // pane's tab pills. The insertion bar is painted from
                // `tab_drag.target`.
                if self.tab_drag.is_some() {
                    let (px, py) = self.cursor_px;
                    let (start, src_pane) = {
                        let d = self.tab_drag.as_ref().unwrap();
                        (d.start, d.pane.clone())
                    };
                    let dx = self.cursor_px.0 - start.0;
                    let dy = self.cursor_px.1 - start.1;
                    // Per-pane horizontal extent of the tab strip, derived
                    // from each pane's tab pills (min(x) .. max(x+w)). The
                    // cursor counts as "over pane P" when its y is inside
                    // any of P's pills *and* its x is inside that x-range —
                    // crucially this still holds while the cursor sits over
                    // the + button or the action cluster (which interrupt
                    // the pill row), so the drop_pane doesn't flicker back
                    // to source mid-flight.
                    let mut drop_pane = src_pane.clone();
                    let mut strip_y: HashMap<String, (f32, f32)> = HashMap::new();
                    let mut strip_x: HashMap<String, (f32, f32)> = HashMap::new();
                    for (pid, _i, (rx, ry, rw, rh)) in &self.pane_tab_rects {
                        let y = strip_y
                            .entry(pid.clone())
                            .or_insert((*ry, ry + rh));
                        y.0 = y.0.min(*ry);
                        y.1 = y.1.max(ry + rh);
                        let x = strip_x
                            .entry(pid.clone())
                            .or_insert((*rx, rx + rw));
                        x.0 = x.0.min(*rx);
                        x.1 = x.1.max(rx + rw);
                    }
                    // Body-hit first — drop_target_at extends the hit box
                    // to include the strip, so the same pane stays the
                    // drop target when the cursor slides between body and
                    // strip. Strip y-range scan is a fallback for cursors
                    // that drop_target_at can't catch (e.g. between
                    // panes' gap).
                    if let Some((target_pane, _)) =
                        self.drop_target_at(px, py)
                    {
                        drop_pane = target_pane;
                    } else {
                        for (pid, (y0, y1)) in &strip_y {
                            if py >= *y0 && py <= *y1 {
                                drop_pane = pid.clone();
                                break;
                            }
                        }
                    }
                    // Insertion index = #pills of drop_pane whose midpoint sits
                    // left of cursor. Resets to 0 when the cursor enters a new
                    // pane's strip so the bar starts at that pane's left edge.
                    let mut target = 0usize;
                    for (pid, idx, (rx, _, rw, _)) in &self.pane_tab_rects {
                        if pid == &drop_pane && px > rx + rw / 2.0 {
                            target = idx + 1;
                        }
                    }
                    if let Some(d) = self.tab_drag.as_mut() {
                        if !d.active && dx * dx + dy * dy > 9.0 {
                            d.active = true;
                        }
                        d.target = target;
                        d.drop_pane = drop_pane;
                    }
                    if self.tab_drag.as_ref().map(|d| d.active).unwrap_or(false) {
                        window.set_cursor(CursorIcon::Grabbing);
                        self.chrome_dirty = true;
                        window.request_redraw();
                    }
                    return;
                }
                // Header drag in progress: flip to active once past the
                // threshold, then keep redrawing so the drop-zone overlay
                // tracks the cursor.
                if let Some(hd) = self.header_drag.as_mut() {
                    let dx = self.cursor_px.0 - hd.start.0;
                    let dy = self.cursor_px.1 - hd.start.1;
                    if !hd.active && dx * dx + dy * dy > 25.0 {
                        hd.active = true;
                    }
                    if hd.active {
                        window.set_cursor(CursorIcon::Grabbing);
                        window.request_redraw();
                    }
                    return;
                }
                // Drag inside a mouse-reporting TUI: relay motion as
                // SGR button-32 (left button held) into the same pane
                // we sent the press to, so Claude Code / vim / less
                // sees a continuous drag.
                if let Some(pane_id) = self.mouse_forward_pane.clone() {
                    if let Some((col, row)) =
                        self.px_to_cell_active(self.cursor_px.0, self.cursor_px.1)
                    {
                        self.send_mouse_sgr(&pane_id, 32, col, row, true);
                    }
                } else if let (Some(anchor), Some(cell)) = (
                    self.drag_anchor,
                    self.px_to_cell_active(self.cursor_px.0, self.cursor_px.1),
                ) {
                    self.selection = Some(Selection { anchor, end: cell });
                    window.request_redraw();
                } else {
                    // Hover feedback: show a resize cursor over a seam or the
                    // sidebar's right edge so they read as draggable.
                    let (cx, cy) = self.cursor_px;
                    let on_sidebar_edge = self.sidebar_visible
                        && cy > TITLE_HEIGHT
                        && (cx - self.sidebar_w_logical).abs() <= 3.0;
                    let on_tree_edge = self.file_tree.visible
                        && cy > TITLE_HEIGHT
                        && (cx - (self.file_tree_col_x() + self.file_tree.w_logical)).abs() <= 3.0;
                    let on_git_edge = self.git.col_visible
                        && cy > TITLE_HEIGHT
                        && (cx - self.git_col_x()).abs() <= 3.0;
                    let icon = if on_sidebar_edge || on_tree_edge || on_git_edge {
                        CursorIcon::ColResize
                    } else {
                        match self
                            .divider_at_px(self.cursor_px.0, self.cursor_px.1)
                            .map(|(_, d)| d)
                        {
                            Some(kasa_pty::SplitDir::Horizontal) => CursorIcon::ColResize,
                            Some(kasa_pty::SplitDir::Vertical) => CursorIcon::RowResize,
                            None => CursorIcon::Default,
                        }
                    };
                    // Windows frameless: edge hover shows a resize cursor so the
                    // 8px resize border reads as draggable.
                    #[cfg(windows)]
                    let icon = {
                        let sf = self.effective_scale();
                        let w = window.inner_size().width as f32 / sf;
                        let h = window.inner_size().height as f32 / sf;
                        const B: f32 = 8.0;
                        let (l, r, t, b) = (cx <= B, cx >= w - B, cy <= B, cy >= h - B);
                        match (t, b, l, r) {
                            (true, _, true, _) | (_, true, _, true) => CursorIcon::NwseResize,
                            (true, _, _, true) | (_, true, true, _) => CursorIcon::NeswResize,
                            (true, _, _, _) | (_, true, _, _) => CursorIcon::NsResize,
                            (_, _, true, _) | (_, _, _, true) => CursorIcon::EwResize,
                            _ => icon,
                        }
                    };
                    // Over a zoomed image pane's body → grab cursor, so the
                    // drag-to-pan affordance reads. Only when there's slack to
                    // pan (image overflows its box).
                    let icon = if matches!(icon, CursorIcon::Default) {
                        match self.px_to_pane_cell(cx, cy) {
                            Some((pid, _, _))
                                if self.pane_is_image(&pid)
                                    && self.image_pan_bounds(&pid) != (0.0, 0.0) =>
                            {
                                CursorIcon::Grab
                            }
                            _ => icon,
                        }
                    } else {
                        icon
                    };
                    // Raw editor body → I-beam, so the text reads as editable.
                    let icon = if matches!(icon, CursorIcon::Default)
                        && self.md_body_rects.values().any(|&(bx, by, bw, bh)| {
                            cx >= bx && cx <= bx + bw && cy >= by && cy <= by + bh
                        }) {
                        CursorIcon::Text
                    } else {
                        icon
                    };
                    // Over a detected URL → pointer (hand) cursor + the blue
                    // hover underline (drawn in draw_cells from cursor_px).
                    // Only when nothing more specific already claimed the cursor.
                    let icon = if matches!(icon, CursorIcon::Default)
                        && self.link_hit(cx, cy).is_some()
                    {
                        CursorIcon::Pointer
                    } else {
                        icon
                    };
                    window.set_cursor(icon);
                    // Hover glow on chrome buttons (+ / action cluster) needs
                    // a redraw on every move — paint reads self.cursor_px to
                    // decide which button is under the cursor.
                    self.chrome_dirty = true;
                    window.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                // Resolve a file-tree → terminal path drag first, before any
                // other hit-test, so a release anywhere disarms it. A real drag
                // (cursor left the row) released over a pane types that path
                // into the shell; otherwise it just disarms — the row's
                // expand/preview click already fired on press.
                if matches!(state, ElementState::Released) {
                    if let Some(drag) = self.file_tree.drag.take() {
                        window.set_cursor(CursorIcon::Default);
                        if drag.active {
                            let (cx, cy) = self.cursor_px;
                            // Drop inside the file-tree column → move the entry
                            // into the folder under the cursor (or a file's
                            // parent, or the root if the drop missed a row).
                            let tree_x = self.file_tree_col_x();
                            let tree_w = self.file_tree_col_w();
                            let in_tree = self.file_tree.visible
                                && cy > TITLE_HEIGHT
                                && cx >= tree_x
                                && cx < tree_x + tree_w;
                            if in_tree {
                                let hit = self
                                    .file_tree.rects
                                    .iter()
                                    .find(|(_, r)| {
                                        cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                                    })
                                    .map(|(p, _)| p.clone());
                                let dst_dir = hit
                                    .and_then(|p| {
                                        let is_dir = self
                                            .file_tree.nodes
                                            .iter()
                                            .find(|n| n.path == p)
                                            .map(|n| n.is_dir)
                                            .unwrap_or(false);
                                        if is_dir {
                                            Some(p)
                                        } else {
                                            p.parent().map(|x| x.to_path_buf())
                                        }
                                    })
                                    .or_else(|| self.file_tree.root.clone());
                                if let Some(dst_dir) = dst_dir {
                                    self.move_tree_entry(&drag.path, &dst_dir);
                                }
                                self.chrome_dirty = true;
                                window.request_redraw();
                                return;
                            }
                            if let Some((pid, _, _)) = self.px_to_pane_cell(cx, cy) {
                                if let Ok(mut w) = self.ws.lock() {
                                    w.active_pane = Some(pid);
                                }
                                let mut text =
                                    shell_quote_path(&drag.path.to_string_lossy());
                                text.push(' ');
                                self.send_bytes(text.as_bytes());
                                self.chrome_dirty = true;
                                window.request_redraw();
                                return;
                            }
                        }
                        self.chrome_dirty = true;
                        window.request_redraw();
                        return;
                    }
                }
                // Confirm-close modal swallows every click while it's up. A hit
                // on a button acts; a click on the scrim is ignored (Esc/취소
                // dismiss). Checked before any other hit-test so nothing behind
                // the dim leaks a click.
                if self.confirm_close.is_some() {
                    if matches!(state, ElementState::Pressed) {
                        let (cx, cy) = self.cursor_px;
                        if let Some(btn) = self
                            .confirm_btn_rects
                            .iter()
                            .find(|(_, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                            .map(|(b, _)| *b)
                        {
                            self.confirm_dialog_pick(btn, event_loop);
                            window.request_redraw();
                        }
                    }
                    return;
                }
                // Commit modal is a full-window dialog — handled before the git
                // column (and everything else) so clicks outside the column
                // still hit its buttons, and the scrim swallows the rest.
                if self.git.commit_modal_open {
                    if matches!(state, ElementState::Pressed) {
                        let (cx, cy) = self.cursor_px;
                        let inside = |r: &(f32, f32, f32, f32)| {
                            cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                        };
                        if let Some(btn) = self
                            .git.commit_modal_rects
                            .iter()
                            .find(|(_, r)| inside(r))
                            .map(|(b, _)| *b)
                        {
                            match btn {
                                crate::GitModalBtn::Close | crate::GitModalBtn::Cancel => self.close_commit_modal(),
                                crate::GitModalBtn::IncludeUnstaged => {
                                    self.git.commit_modal_include_unstaged = !self.git.commit_modal_include_unstaged;
                                    window.request_redraw();
                                }
                                crate::GitModalBtn::Commit | crate::GitModalBtn::Confirm => self.run_commit_modal(false),
                                crate::GitModalBtn::CommitAndPush => self.run_commit_modal(true),
                            }
                            return;
                        }
                        if self.git.commit_input_rect.map(|r| inside(&r)).unwrap_or(false) {
                            self.git.commit_focused = true;
                            window.request_redraw();
                            return;
                        }
                    }
                    return;
                }
                // Settings: the sidebar entry toggles the screen. While it's
                // open, clicks in the view area (right of the sidebar) route to
                // the form; a click on the session sidebar closes settings and
                // falls through to normal tab handling below.
                if matches!(state, ElementState::Pressed) {
                    let (cx, cy) = self.cursor_px;
                    let hit = |r: (f32, f32, f32, f32)| {
                        cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                    };
                    if hit(self.settings_btn_rect) {
                        if self.settings_open {
                            self.close_settings();
                        } else {
                            self.open_settings();
                        }
                        window.request_redraw();
                        return;
                    }
                    // Dock chip click. While a pane is zoomed the dock shows the
                    // hidden siblings — clicking one switches the zoom to it
                    // (toggle off the current, on the clicked, in one call since
                    // the clicked id isn't the zoomed one).
                    if let Some(id) = self
                        .dock_chip_rects
                        .iter()
                        .find(|(_, r)| hit(*r))
                        .map(|(i, _)| i.clone())
                    {
                        if self.zoomed_pane.is_some() {
                            self.toggle_pane_zoom(&id);
                        }
                        window.request_redraw();
                        return;
                    }
                    // Settings open: only the main content area (below the title
                    // strip, right of the sidebar) routes to the form. The title
                    // strip toggles and the sidebar stay live so you're never
                    // trapped in the screen.
                    if self.settings_open
                        && cy > TITLE_HEIGHT
                        && cx >= self.tab_strip_w()
                    {
                        self.settings_click(cx, cy);
                        window.request_redraw();
                        return;
                    }
                }
                // Pane header × close button. Catches clicks anywhere
                // in the multi-pane workspace before we drop into the
                // cell-grid click path.
                if matches!(state, ElementState::Pressed) {
                    let cx = self.cursor_px.0;
                    let cy = self.cursor_px.1;
                    // A press outside the inline new-entry row + its buttons
                    // cancels the pending creation. Falls through so the click
                    // still does its normal job (focus a pane, etc.).
                    if self.file_tree.new.is_some() {
                        let hit = |r: (f32, f32, f32, f32)| {
                            cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                        };
                        if !hit(self.file_tree.new_row_rect)
                            && !hit(self.file_tree.new_folder_rect)
                            && !hit(self.file_tree.new_file_rect)
                        {
                            self.file_tree.new = None;
                            self.chrome_dirty = true;
                        }
                    }
                    // 승인 토스트 칩 — 일반 토스트 dismiss 보다 먼저 검사해야
                    // 칩 클릭이 pane/dismiss 로 새지 않는다 (hit-test 순서 의존).
                    if let Some(target) = self.collab.toast_action.clone() {
                        let hit = |r: Option<(f32, f32, f32, f32)>| {
                            r.map_or(false, |(x, y, w, h)| {
                                cx >= x && cx <= x + w && cy >= y && cy <= y + h
                            })
                        };
                        let ok = hit(self.collab.toast_approve_rect);
                        let no = hit(self.collab.toast_deny_rect);
                        if ok || no {
                            self.respond_approval(&target, ok);
                            // pane_prompt_wait/attention 은 여기서 걷지 않는다 —
                            // 주입한 키로 프롬프트가 실제로 사라질 때
                            // route_approval_prompts 가 board 까지 함께 정리.
                            // (flag 가 남아 있는 동안은 토스트 재무장도 없다.)
                            self.clear_approval_toast();
                            self.chrome_dirty = true;
                            window.request_redraw();
                            return;
                        }
                    }
                    // Completion toast: a click anywhere on it dismisses it
                    // immediately (top-right, tested before the cell grid).
                    // 승인 토스트(칩 비적중)의 본문 클릭은 해당 pane 으로 점프 —
                    // 프롬프트 원문을 읽고 직접 답하라는 의미. 플래그는 유지해
                    // 그리드 스캔이 프롬프트 해소 시점에 board까지 정리한다.
                    if let Some((tx, ty, tw, th)) = self.collab.toast_rect {
                        if cx >= tx && cx <= tx + tw && cy >= ty && cy <= ty + th {
                            if let Some(target) = self.collab.toast_action.take() {
                                self.ws.lock().unwrap().active_pane = Some(target);
                            }
                            self.clear_approval_toast();
                            self.chrome_dirty = true;
                            window.request_redraw();
                            return;
                        }
                    }
                    // Any press outside the commit input blurs it, so typing
                    // goes back to the PTY. A press on the input keeps focus
                    // (the git-column handler re-asserts it below).
                    if self.git.commit_focused {
                        let on_input = self
                            .git.commit_input_rect
                            .map(|(x, y, w, h)| cx >= x && cx <= x + w && cy >= y && cy <= y + h)
                            .unwrap_or(false);
                        if !on_input {
                            self.git.commit_focused = false;
                            self.chrome_dirty = true;
                        }
                    }
                    // Windows frameless: resize from the window edges. An 8px
                    // hot border drives drag_resize_window in the matching
                    // direction. Checked first so an edge press resizes instead
                    // of starting a window drag or hitting a button.
                    #[cfg(windows)]
                    {
                        let sf = self.effective_scale();
                        let w = window.inner_size().width as f32 / sf;
                        let h = window.inner_size().height as f32 / sf;
                        const B: f32 = 8.0;
                        let (l, r, t, b) = (cx <= B, cx >= w - B, cy <= B, cy >= h - B);
                        let dir = match (t, b, l, r) {
                            (true, _, true, _) => Some(winit::window::ResizeDirection::NorthWest),
                            (true, _, _, true) => Some(winit::window::ResizeDirection::NorthEast),
                            (_, true, true, _) => Some(winit::window::ResizeDirection::SouthWest),
                            (_, true, _, true) => Some(winit::window::ResizeDirection::SouthEast),
                            (true, _, _, _) => Some(winit::window::ResizeDirection::North),
                            (_, true, _, _) => Some(winit::window::ResizeDirection::South),
                            (_, _, true, _) => Some(winit::window::ResizeDirection::West),
                            (_, _, _, true) => Some(winit::window::ResizeDirection::East),
                            _ => None,
                        };
                        if let Some(dir) = dir {
                            let _ = window.drag_resize_window(dir);
                            return;
                        }
                    }
                    // Shell picker popup. While open it owns the next click:
                    // hit an item → spawn that shell in a new window; click
                    // anywhere else → dismiss. Checked first so it captures
                    // clicks before the sidebar / cell grid underneath.
                    if self.shell_menu_open {
                        let pick = self
                            .shell_menu_hits
                            .iter()
                            .find(|(_, r)| {
                                cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                            })
                            .map(|(s, _)| s.clone());
                        self.shell_menu_open = false;
                        self.chrome_dirty = true;
                        if let Some(shell) = pick {
                            self.pending_shell = Some(shell);
                            self.new_window();
                        }
                        return;
                    }
                    // Sidebar-toggle button in the title strip (right of the
                    // traffic lights). Caught before the title-bar drag path
                    // so the click toggles instead of moving the window.
                    {
                        let (bx, by, bw, bh) = Self::sidebar_toggle_rect();
                        if cx >= bx && cx <= bx + bw && cy >= by && cy <= by + bh {
                            self.toggle_sidebar();
                            return;
                        }
                    }
                    // File-tree toggle, just right of the sidebar toggle.
                    {
                        let (bx, by, bw, bh) = Self::file_tree_toggle_rect();
                        if cx >= bx && cx <= bx + bw && cy >= by && cy <= by + bh {
                            self.toggle_file_tree();
                            return;
                        }
                    }
                    // SCHALE OS(아로나) ✨ 버튼 — 터미널↔SCHALE OS 토글(메뉴 대신).
                    if let Some((bx, by, bw, bh)) = self.arona_btn_rect() {
                        if cx >= bx && cx <= bx + bw && cy >= by && cy <= by + bh {
                            self.toggle_arona_panel(event_loop);
                            window.request_redraw();
                            return;
                        }
                    }
                    // Settings toggle, left of the git-column toggle.
                    if let Some((bx, by, bw, bh)) = self.settings_toggle_rect() {
                        if cx >= bx && cx <= bx + bw && cy >= by && cy <= by + bh {
                            if self.settings_open {
                                self.close_settings();
                            } else {
                                self.open_settings();
                            }
                            return;
                        }
                    }
                    // Git-column toggle, parked at the right end of the strip.
                    if let Some((bx, by, bw, bh)) = self.git_col_toggle_rect() {
                        if cx >= bx && cx <= bx + bw && cy >= by && cy <= by + bh {
                            self.toggle_git_col();
                            return;
                        }
                    }
                    // Windows frameless window controls (min / max / close) at
                    // the strip's right edge. Pressed-time hit-test, before the
                    // titlebar-drag path, so a button click isn't a window move.
                    #[cfg(windows)]
                    {
                        // cursor_px is logical px at effective_scale (= dpi *
                        // ui_zoom); match it or the hit-test misses when zoomed.
                        let win_w = window.inner_size().width as f32 / self.effective_scale();
                        let ctrls = Self::win_control_rects(win_w);
                        for (i, &(bx, by, bw, bh)) in ctrls.iter().enumerate() {
                            if cx >= bx && cx <= bx + bw && cy >= by && cy <= by + bh {
                                match i {
                                    0 => window.set_minimized(true),
                                    1 => gpu::toggle_maximize_no_anim(
                                        &window,
                                        &mut self.saved_window_frame,
                                    ),
                                    _ => event_loop.exit(),
                                }
                                return;
                            }
                        }
                    }
                    // Sidebar resize grip — a 6px hot zone straddling the
                    // sidebar's right edge below the title strip. Caught
                    // before the sidebar click path so dragging the edge
                    // resizes instead of clicking the last sidebar column.
                    if self.sidebar_visible && cy > TITLE_HEIGHT {
                        let edge = self.sidebar_w_logical;
                        if cx >= edge - 3.0 && cx <= edge + 3.0 {
                            self.sidebar_resize = Some((cx, self.sidebar_w_logical));
                            return;
                        }
                    }
                    // File-tree column resize grip — straddles the tree's right
                    // edge. Caught before the tree click path so dragging the
                    // seam resizes instead of selecting the last row.
                    if self.file_tree.visible && cy > TITLE_HEIGHT {
                        let edge = self.file_tree_col_x() + self.file_tree.w_logical;
                        if cx >= edge - 3.0 && cx <= edge + 3.0 {
                            self.file_tree.resize = Some((cx, self.file_tree.w_logical));
                            return;
                        }
                    }
                    // Git column resize grip — its LEFT edge (the column is
                    // flush-right, so dragging the seam left widens it). Caught
                    // before the column click path.
                    if self.git.col_visible && cy > TITLE_HEIGHT {
                        let edge = self.git_col_x();
                        if cx >= edge - 3.0 && cx <= edge + 3.0 {
                            self.git.col_resize = Some((cx, self.git.col_w_logical));
                            return;
                        }
                    }
                    // Left window-tab sidebar. Caught first — it owns the whole
                    // left strip, so a click there never falls through to the
                    // cell grid. Order: close-× (sits on top of a tab) → tab →
                    // "+" new-window button.
                    if self.sidebar_visible && cx < self.sidebar_w_logical {
                        let inside =
                            |r: &(f32, f32, f32, f32)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3;
                        if let Some(idx) = self
                            .window_tab_close_rects
                            .iter()
                            .find(|(_, r)| inside(r))
                            .map(|(i, _)| *i)
                        {
                            if let Err(e) = self.close_window(idx) {
                                eprintln!("[window] close failed: {e:#}");
                            }
                            return;
                        }
                        if let Some(idx) = self
                            .window_tab_rects
                            .iter()
                            .find(|(_, r)| inside(r))
                            .map(|(i, _)| *i)
                        {
                            // Picking a session tab means you want to see it, so
                            // leave the settings screen first.
                            if self.settings_open {
                                self.close_settings();
                            }
                            self.switch_window(idx);
                            return;
                        }
                        if self.new_window_btn_rect.map(|r| inside(&r)).unwrap_or(false) {
                            if self.settings_open {
                                self.close_settings();
                            }
                            // The shell picker only has entries on Windows
                            // (PowerShell/CMD/Git Bash/WSL). On macOS/Linux
                            // `available_shells()` is empty, so toggling the
                            // menu would just swallow the click and never open
                            // a tab — spawn a default window directly instead.
                            if available_shells().is_empty() {
                                self.new_window();
                            } else {
                                self.shell_menu_open = !self.shell_menu_open;
                            }
                            self.chrome_dirty = true;
                            return;
                        }
                        // Empty sidebar space — swallow the click.
                        return;
                    }
                    // Click outside the tree column drops search focus — else
                    // keystrokes meant for the clicked terminal pane keep
                    // landing in the filter box.
                    if self.file_tree.search_active {
                        let in_col = self.file_tree.visible
                            && cy > TITLE_HEIGHT
                            && cx >= self.file_tree_col_x()
                            && cx < self.file_tree_col_x() + self.file_tree.w_logical;
                        if !in_col {
                            self.file_tree.search_active = false;
                            self.chrome_dirty = true;
                        }
                    }
                    // File-tree column — its own band, right of the tab strip.
                    // Caught before the cell grid so a row click never falls
                    // through to the terminal underneath.
                    if self.file_tree.visible
                        && cy > TITLE_HEIGHT
                        && cx >= self.file_tree_col_x()
                        && cx < self.file_tree_col_x() + self.file_tree.w_logical
                    {
                        let inside = |r: &(f32, f32, f32, f32)| {
                            cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                        };
                        // New-folder / new-file buttons beside the search box:
                        // open an inline naming row (keystrokes route to it).
                        if inside(&self.file_tree.new_folder_rect) {
                            self.file_tree.new = Some((true, String::new()));
                            self.file_tree.search_active = false;
                            self.file_tree.scroll = 0.0;
                            self.chrome_dirty = true;
                            window.request_redraw();
                            return;
                        }
                        if inside(&self.file_tree.new_file_rect) {
                            self.file_tree.new = Some((false, String::new()));
                            self.file_tree.search_active = false;
                            self.file_tree.scroll = 0.0;
                            self.chrome_dirty = true;
                            window.request_redraw();
                            return;
                        }
                        // Search box click → focus it (keystrokes now filter the
                        // tree). Clicking it again keeps focus; Esc clears.
                        if inside(&self.file_tree.search_rect) {
                            self.file_tree.search_active = true;
                            self.chrome_dirty = true;
                            window.request_redraw();
                            return;
                        }
                        // Row: folder → toggle expand, file → preview.
                        if let Some(path) = self
                            .file_tree.rects
                            .iter()
                            .find(|(_, r)| inside(r))
                            .map(|(p, _)| p.clone())
                        {
                            // Mark it the Cmd+Delete target.
                            self.file_tree.selected = Some(path.clone());
                            let is_dir = self
                                .file_tree.nodes
                                .iter()
                                .find(|n| n.path == path)
                                .map(|n| n.is_dir)
                                .unwrap_or(false);
                            // Arm a drag from this row. The expand/preview
                            // click action still fires below; only if the
                            // cursor then travels off the sidebar does this
                            // turn into a path drop (handled on release).
                            self.file_tree.drag = Some(crate::FileTreeDrag {
                                path: path.clone(),
                                start: self.cursor_px,
                                active: false,
                            });
                            if is_dir {
                                if !self.file_tree.expanded.remove(&path) {
                                    self.file_tree.expanded.insert(path.clone());
                                }
                                self.rebuild_file_tree_nodes();
                                self.chrome_dirty = true;
                                window.request_redraw();
                            } else {
                                // File row: a second click on the SAME file
                                // within the double-click window opens it in a
                                // split (folders keep single-click expand, so
                                // files get their own gate to avoid opening on
                                // every stray click). Image/markdown/code is
                                // routed by extension inside open_file_split.
                                let now = Instant::now();
                                let is_double = matches!(
                                    self.last_tree_click.as_ref(),
                                    Some((t, p))
                                        if *p == path
                                            && now.duration_since(*t).as_millis() < 400
                                );
                                if is_double {
                                    self.last_tree_click = None;
                                    self.open_file_split(path.clone());
                                } else {
                                    self.last_tree_click = Some((now, path.clone()));
                                }
                            }
                            return;
                        }
                        // Empty tree space — swallow the click.
                        return;
                    }
                    // Git column — right-hand chrome. Caught before the cell
                    // grid so a click never falls through to the terminal.
                    if self.git.col_visible && cy > TITLE_HEIGHT && cx >= self.git_col_x() {
                        let inside = |r: &(f32, f32, f32, f32)| {
                            cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                        };
                        // Open dropdowns overlay everything — resolve their items
                        // (and the header toggles) before the list/buttons under.
                        if self.git.path_menu_open {
                            if let Some(key) = self
                                .git.path_menu_rects
                                .iter()
                                .find(|(_, r)| inside(r))
                                .map(|(k, _)| k.clone())
                            {
                                // None = "자동 추적" (unpin); Some = pin that repo.
                                self.git.col_pinned_cwd = key;
                                self.git.path_menu_open = false;
                                self.publish_git_col_cwd();
                                window.request_redraw();
                                return;
                            }
                        }
                        if self.git.branch_menu_open {
                            if let Some(b) = self
                                .git.branch_menu_rects
                                .iter()
                                .find(|(_, r)| inside(r))
                                .map(|(b, _)| b.clone())
                            {
                                self.run_git_checkout(b);
                                window.request_redraw();
                                return;
                            }
                        }
                        // Header rows toggle their dropdowns (mutually exclusive).
                        if self.git.path_hdr_rect.map(|r| inside(&r)).unwrap_or(false) {
                            self.git.path_menu_open = !self.git.path_menu_open;
                            self.git.branch_menu_open = false;
                            window.request_redraw();
                            return;
                        }
                        if self.git.branch_hdr_rect.map(|r| inside(&r)).unwrap_or(false) {
                            self.git.branch_menu_open = !self.git.branch_menu_open;
                            self.git.path_menu_open = false;
                            window.request_redraw();
                            return;
                        }
                        // A click elsewhere in the column dismisses an open menu
                        // (swallowed, so it doesn't also hit the list/buttons).
                        if self.git.path_menu_open || self.git.branch_menu_open {
                            self.git.path_menu_open = false;
                            self.git.branch_menu_open = false;
                            window.request_redraw();
                            return;
                        }
                        // Commit-button dropdown items (overlay) first.
                        if self.git.commit_menu_open {
                            if let Some(act) = self
                                .git.commit_menu_rects
                                .iter()
                                .find(|(_, r)| inside(r))
                                .map(|(a, _)| *a)
                            {
                                self.git.commit_menu_open = false;
                                match act {
                                    crate::GitCommitAction::Commit => self.open_commit_modal(),
                                    crate::GitCommitAction::Push => self.run_git_col_action(crate::GitColBtn::Push),
                                    crate::GitCommitAction::Pull => self.run_git_col_action(crate::GitColBtn::Pull),
                                    crate::GitCommitAction::CreatePr => self.create_git_pr(),
                                }
                                window.request_redraw();
                                return;
                            }
                            self.git.commit_menu_open = false;
                            window.request_redraw();
                            return;
                        }
                        // Panel header: close / expand.
                        if self.git.col_close_rect.map(|r| inside(&r)).unwrap_or(false) {
                            self.toggle_git_col();
                            return;
                        }
                        if self.git.col_expand_rect.map(|r| inside(&r)).unwrap_or(false) {
                            self.toggle_git_col_expand();
                            window.request_redraw();
                            return;
                        }
                        // Commit split button: main → modal, caret → dropdown.
                        if self.git.commit_btn_rect.map(|r| inside(&r)).unwrap_or(false) {
                            // Matches the render: with no changes but commits to
                            // push, the primary button is Push, not Commit.
                            let push_mode = self
                                .git.col_data
                                .lock()
                                .ok()
                                .map(|g| g.staged.is_empty() && g.unstaged.is_empty() && g.ahead > 0)
                                .unwrap_or(false);
                            if push_mode {
                                self.run_git_col_action(crate::GitColBtn::Push);
                            } else {
                                self.open_commit_modal();
                            }
                            window.request_redraw();
                            return;
                        }
                        if self.git.commit_caret_rect.map(|r| inside(&r)).unwrap_or(false) {
                            self.git.commit_menu_open = !self.git.commit_menu_open;
                            window.request_redraw();
                            return;
                        }
                        // Row +/− button → stage / unstage that one file. Checked
                        // before the file-preview path since it sits inside the
                        // row rect. Off-thread; the poller repaints the lists.
                        if let Some((stage, path)) = self
                            .git.col_stage_rects
                            .iter()
                            .find(|(_, _, r)| inside(r))
                            .map(|(s, p, _)| (*s, p.clone()))
                        {
                            if let Some(cwd) = self.git.col_data.lock().ok().and_then(|g| g.cwd.clone()) {
                                let proxy = self.proxy.clone();
                                let data = self.git.col_data.clone();
                                std::thread::spawn(move || {
                                    if stage {
                                        let _ = kasa_mcp::git::git_add_path(&cwd, &path);
                                    } else {
                                        let _ = kasa_mcp::git::git_unstage_path(&cwd, &path);
                                    }
                                    // Re-read status right away so the row jumps
                                    // sections immediately instead of waiting for
                                    // the 1.2s poller tick.
                                    if let Some(view) = fetch_git_col_view(&cwd) {
                                        if let Ok(mut g) = data.lock() {
                                            *g = view;
                                        }
                                    }
                                    let _ = proxy.send_event(UserEvent::Redraw);
                                });
                            }
                            // The file jumps sections (staged↔changes); cached
                            // diffs keyed by (staged, path) are now stale.
                            self.invalidate_git_diffs();
                            window.request_redraw();
                            return;
                        }
                        // Row ↩ discard → restore the file (or delete if untracked).
                        if let Some((path, untracked)) = self
                            .git.col_discard_rects
                            .iter()
                            .find(|(_, _, r)| inside(r))
                            .map(|(p, u, _)| (p.clone(), *u))
                        {
                            if let Some(cwd) = self.git.col_data.lock().ok().and_then(|g| g.cwd.clone()) {
                                let proxy = self.proxy.clone();
                                let data = self.git.col_data.clone();
                                std::thread::spawn(move || {
                                    let _ = kasa_mcp::git::git_discard_path(&cwd, &path, untracked);
                                    if let Some(view) = fetch_git_col_view(&cwd) {
                                        if let Ok(mut g) = data.lock() {
                                            *g = view;
                                        }
                                    }
                                    let _ = proxy.send_event(UserEvent::Redraw);
                                });
                            }
                            self.invalidate_git_diffs();
                            window.request_redraw();
                            return;
                        }
                        // Row ⤴ open → preview the file in a pane.
                        if let Some(path) = self
                            .git.col_open_rects
                            .iter()
                            .find(|(_, r)| inside(r))
                            .map(|(p, _)| p.clone())
                        {
                            self.open_git_file(&path);
                            return;
                        }
                        // File row → toggle its inline unified diff.
                        if let Some((staged, path)) = self
                            .git.col_file_rects
                            .iter()
                            .find(|(_, _, r)| inside(r))
                            .map(|(s, p, _)| (*s, p.clone()))
                        {
                            self.toggle_git_diff(staged, path);
                            return;
                        }
                        // Empty column space — swallow the click.
                        return;
                    }
                    // Code-block copy button. Checked before the cell-grid /
                    // mouse-forward path so a click lands on the button even
                    // inside a mouse-reporting TUI (Claude Code), the same
                    // way the Shift escape hatch steals selection.
                    if let Some(text) = self
                        .copy_btn_rects
                        .iter()
                        .find(|(_, r)| {
                            cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                        })
                        .map(|(t, _)| t.clone())
                    {
                        self.copy_block_text(&text);
                        window.request_redraw();
                        return;
                    }
                    // Terminal-pane right-action cluster (new-terminal /
                    // web / split-v / split-h). Web spawns a separate OS
                    // window with a wry browser; the other variants are
                    // wired by the main pane-model.
                    if let Some((pid, action)) = self
                        .pane_action_hits
                        .iter()
                        .find(|(_, _, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                        .map(|(id, a, _)| (id.clone(), *a))
                    {
                        // Focus the clicked pane so splits/new-tabs target it.
                        self.ws.lock().unwrap().active_pane = Some(pid.clone());
                        match action {
                            ActionKind::SplitV => {
                                if let Err(e) = self
                                    .split_active_pane(kasa_pty::SplitDir::Vertical)
                                {
                                    eprintln!("[split-v] {e}");
                                }
                            }
                            ActionKind::SplitH => {
                                if let Err(e) = self
                                    .split_active_pane(kasa_pty::SplitDir::Horizontal)
                                {
                                    eprintln!("[split-h] {e}");
                                }
                            }
                            ActionKind::ToggleStatusbar => {
                                self.toggle_statusbar(&pid);
                            }
                            ActionKind::MdRender => {
                                self.set_md_mode(&pid, false);
                            }
                            ActionKind::MdRaw => {
                                self.set_md_mode(&pid, true);
                            }
                        }
                        window.request_redraw();
                        return;
                    }
                    // Per-pane status bar (footer). Open dropdown items overlay
                    // everything, so resolve them first; then the collapse
                    // handle, then the cwd / branch chips. All return so a click
                    // in the footer band never falls through to the cell grid.
                    let sb_hit = |r: &(f32, f32, f32, f32)| {
                        cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                    };
                    if let Some((pid, kind)) = self.statusbar.menu.clone() {
                        match kind {
                            StatusbarMenu::Path => {
                                if let Some(path) = self
                                    .statusbar.menu_dir_rects
                                    .iter()
                                    .find(|(_, r)| sb_hit(r))
                                    .map(|(d, _)| d.clone())
                                {
                                    // Folder → cd the pane; file → open it in a
                                    // preview pane (the picker doubles as a file
                                    // opener now that it lists files too).
                                    if path.is_dir() {
                                        self.statusbar_cd(&pid, &path);
                                    } else {
                                        self.statusbar.menu = None;
                                        self.open_file_split(path);
                                    }
                                    window.request_redraw();
                                    return;
                                }
                            }
                            StatusbarMenu::Branch => {
                                if let Some(b) = self
                                    .statusbar.menu_branch_rects
                                    .iter()
                                    .find(|(_, r)| sb_hit(r))
                                    .map(|(b, _)| b.clone())
                                {
                                    self.statusbar_checkout(&pid, b);
                                    window.request_redraw();
                                    return;
                                }
                            }
                        }
                        // Click outside the open menu dismisses it (swallowed).
                        self.statusbar.menu = None;
                        window.request_redraw();
                        return;
                    }
                    if let Some(pid) = self
                        .statusbar.toggle_rects
                        .iter()
                        .find(|(_, r)| sb_hit(r))
                        .map(|(p, _)| p.clone())
                    {
                        self.toggle_statusbar(&pid);
                        window.request_redraw();
                        return;
                    }
                    if let Some(pid) = self
                        .statusbar.path_rects
                        .iter()
                        .find(|(_, r)| sb_hit(r))
                        .map(|(p, _)| p.clone())
                    {
                        self.open_statusbar_menu(&pid, StatusbarMenu::Path);
                        window.request_redraw();
                        return;
                    }
                    if let Some(pid) = self
                        .statusbar.branch_rects
                        .iter()
                        .find(|(_, r)| sb_hit(r))
                        .map(|(p, _)| p.clone())
                    {
                        self.open_statusbar_menu(&pid, StatusbarMenu::Branch);
                        window.request_redraw();
                        return;
                    }
                    if let Some(pid) = self
                        .statusbar.diff_rects
                        .iter()
                        .find(|(_, r)| sb_hit(r))
                        .map(|(p, _)| p.clone())
                    {
                        self.open_git_panel_for(&pid);
                        window.request_redraw();
                        return;
                    }
                    // Image-pane action buttons (zoom-out/in, rotate, reset).
                    // Checked before the tab/plus path so the image-only
                    // chrome cluster is never swallowed by tab hit-tests.
                    if let Some((pid, kind)) = self
                        .image_btn_rects
                        .iter()
                        .find(|(_, _, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                        .map(|(id, k, _)| (id.clone(), *k))
                    {
                        if let Ok(mut ws) = self.ws.lock() {
                            if let Some(pane) = ws.panes.get_mut(&pid) {
                                let z = pane.image_view_zoom();
                                match kind {
                                    ImageBtn::ZoomIn => {
                                        pane.image_zoom = (z * 1.25).clamp(1.0, 8.0);
                                    }
                                    ImageBtn::ZoomOut => {
                                        pane.image_zoom = (z / 1.25).max(1.0);
                                        // Back at fit → no pan room; recenter.
                                        if pane.image_zoom <= 1.0 {
                                            pane.image_pan_x = 0.0;
                                            pane.image_pan_y = 0.0;
                                        }
                                    }
                                    ImageBtn::Rotate => {
                                        pane.image_rot = (pane.image_rot + 1) % 4;
                                        // Pan is in screen space; rotating the
                                        // texture invalidates it.
                                        pane.image_pan_x = 0.0;
                                        pane.image_pan_y = 0.0;
                                    }
                                    ImageBtn::Reset => {
                                        pane.image_zoom = 1.0;
                                        pane.image_rot = 0;
                                        pane.image_pan_x = 0.0;
                                        pane.image_pan_y = 0.0;
                                    }
                                }
                                pane.dirty = true;
                            }
                            ws.active_pane = Some(pid.clone());
                        }
                        self.chrome_dirty = true;
                        window.request_redraw();
                        return;
                    }
                    // In-pane tab bar: + new-tab, per-tab × close, tab switch.
                    // Checked before the cell grid so a header click never
                    // selects text. (Stage 2: tabs are visual labels; each
                    // tab's real PTY/content lands in stage 3.)
                    if let Some(pid) = self
                        .pane_plus_rects
                        .iter()
                        .find(|(_, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                        .map(|(id, _)| id.clone())
                    {
                        // Stage 3: spawn a real PTY-backed tab. spawn_new_tab
                        // pushes a PaneTab with its own pid and sets active.
                        if let Err(e) = self.spawn_new_tab(&pid) {
                            eprintln!("[spawn_new_tab] {e}");
                        }
                        window.request_redraw();
                        return;
                    }
                    if let Some((pid, idx)) = self
                        .pane_tab_close_rects
                        .iter()
                        .find(|(_, _, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                        .map(|(id, i, _)| (id.clone(), *i))
                    {
                        // Same tab-vs-pane + "job running?" logic as Cmd+W.
                        self.confirm_or_close_tab(&pid, idx);
                        window.request_redraw();
                        return;
                    }
                    // Split-seam drag wins over the tab/header hits below.
                    // The hover cursor already flips to a resize arrow through
                    // this same `divider_at_px`, so a press on the seam MUST
                    // resize too — otherwise a tab pill sitting on the seam
                    // (the lower pane's header butts right up against it) grabs
                    // a tab/pane move while the cursor is saying "resize".
                    if let Some((path, dir)) =
                        self.divider_at_px(self.cursor_px.0, self.cursor_px.1)
                    {
                        self.resize_drag = Some((path, dir));
                        return;
                    }
                    if let Some((pid, idx)) = self
                        .pane_tab_rects
                        .iter()
                        .find(|(_, _, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                        .map(|(id, i, _)| (id.clone(), *i))
                    {
                        // A double-click anywhere on the header — including the
                        // tab pill itself — toggles tmux-style zoom. Users aim
                        // at the tab label when they "double-click the header",
                        // so the pill must share the header band's zoom gesture
                        // (otherwise only the empty strip right of the tabs
                        // zoomed, which felt broken).
                        let (dx, dy) = self.cursor_px;
                        let now = Instant::now();
                        let is_double = matches!(
                            self.last_left_click,
                            Some((t, (x, y)))
                                if now.duration_since(t).as_millis() < 400
                                    && (x - dx).abs() < 5.0
                                    && (y - dy).abs() < 5.0
                        );
                        self.last_left_click = Some((now, (dx, dy)));
                        if is_double {
                            // pane_tab_rects keys off the outer pane id (same
                            // value we push into active_pane below), which is
                            // exactly what toggle_pane_zoom wants.
                            self.toggle_pane_zoom(&pid);
                            self.last_left_click = None;
                            return;
                        }
                        // Focus the pane now; arm a tab drag. A plain press
                        // (no movement) switches to this tab on release; a
                        // drag past the threshold reorders instead.
                        if let Ok(mut ws) = self.ws.lock() {
                            ws.active_pane = Some(pid.clone());
                        }
                        self.tab_drag = Some(TabDrag {
                            pane: pid.clone(),
                            from: idx,
                            start: self.cursor_px,
                            active: false,
                            target: idx,
                            drop_pane: pid,
                        });
                        window.request_redraw();
                        return;
                    }
                    // Press on a pane header (not the × button) → focus it
                    // and arm a drag-and-drop relocation. It only becomes
                    // a real drag once the cursor passes the threshold, so
                    // a plain header click just focuses.
                    if let Some(pane) =
                        self.header_at_px(self.cursor_px.0, self.cursor_px.1)
                    {
                        // A double-click on a pane header toggles tmux-style
                        // zoom (that pane alone fills the work area). Reuse the
                        // same last_left_click window as the titlebar maximize.
                        let (dx, dy) = self.cursor_px;
                        let now = Instant::now();
                        let is_double = matches!(
                            self.last_left_click,
                            Some((t, (x, y)))
                                if now.duration_since(t).as_millis() < 400
                                    && (x - dx).abs() < 5.0
                                    && (y - dy).abs() < 5.0
                        );
                        self.last_left_click = Some((now, (dx, dy)));
                        if is_double {
                            self.toggle_pane_zoom(&pane);
                            self.last_left_click = None;
                            return;
                        }
                        self.ws.lock().unwrap().active_pane = Some(pane.clone());
                        self.header_drag = Some(HeaderDrag {
                            pane,
                            start: self.cursor_px,
                            active: false,
                        });
                        window.request_redraw();
                        return;
                    }
                }
                // Title bar (above the cell grid, right of the traffic
                // lights) → double-click toggles maximize, a single
                // drag moves the window — the macOS native chrome we
                // lost when we turned on fullsize_content_view. macOS
                // owns the traffic-light cluster, so we only act past
                // its width.
                #[cfg(not(windows))]
                let titlebar_press = matches!(state, ElementState::Pressed)
                    && self.cursor_px.1 < TITLE_HEIGHT
                    && self.cursor_px.0 > TRAFFIC_LIGHT_WIDTH;
                // Windows has no traffic-light cluster to dodge; the whole strip
                // is draggable. Toggle + window-control buttons already returned
                // above, and the top resize border is handled before this.
                #[cfg(windows)]
                let titlebar_press =
                    matches!(state, ElementState::Pressed) && self.cursor_px.1 < TITLE_HEIGHT;
                if titlebar_press {
                    let (cx, cy) = self.cursor_px;
                    let now = Instant::now();
                    let is_double = match self.last_left_click {
                        Some((t, (x, y)))
                            if now.duration_since(t).as_millis() < 400
                                && (x - cx).abs() < 5.0
                                && (y - cy).abs() < 5.0 =>
                        {
                            true
                        }
                        _ => false,
                    };
                    self.last_left_click = Some((now, (cx, cy)));
                    if is_double {
                        // Drive the frame swap ourselves with animate:NO —
                        // winit's set_maximized routes through AppKit zoom,
                        // which animates the frame slowly ("늦게 커짐").
                        gpu::toggle_maximize_no_anim(&window, &mut self.saved_window_frame);
                        if std::env::var_os("KASATERM_RESIZE_DEBUG").is_some() {
                            eprintln!(
                                "[rsz {}ms] set_maximized -> {}",
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap()
                                    .as_millis()
                                    % 100000,
                                window.is_maximized()
                            );
                        }
                        self.last_left_click = None;
                        self.titlebar_drag_pending = None;
                        return;
                    }
                    // Defer the actual window-move until the pointer moves —
                    // calling drag_window() here would enter AppKit's modal
                    // loop and swallow the second click of a double-click.
                    self.titlebar_drag_pending = Some((cx, cy));
                    return;
                }
                match state {
                    ElementState::Pressed => {
                        // URL under the press → arm it and bail out before any
                        // text-selection / mouse-forwarding starts. A release
                        // that stays put (a click, not a drag) opens it. We
                        // still move focus to the pane it landed in.
                        if let Some((pid, _, url)) =
                            self.link_hit(self.cursor_px.0, self.cursor_px.1)
                        {
                            self.link_armed = Some((url, self.cursor_px));
                            self.ws.lock().unwrap().active_pane = Some(pid);
                            return;
                        }
                        if let Some((pane_id, col, row)) =
                            self.px_to_pane_cell(self.cursor_px.0, self.cursor_px.1)
                        {
                            let switched = {
                                let mut ws = self.ws.lock().unwrap();
                                let switched =
                                    ws.active_pane.as_deref() != Some(pane_id.as_str());
                                ws.active_pane = Some(pane_id.clone());
                                switched
                            };
                            if switched {
                                // Daemon owns the active pointer: its cwd poll
                                self.selection = None;
                                self.drag_anchor = None;
                                self.mouse_forward_pane = None;
                                // A press that also focuses an image pane still
                                // arms a pan — dragging works on the first grab,
                                // no need to click twice.
                                if self.pane_is_image(&pane_id) {
                                    self.begin_image_pan(&pane_id);
                                }
                            } else if self.pane_takes_mouse(&pane_id) {
                                // Hand the press to the TUI. Its own
                                // selection / copy-on-select kicks in
                                // (Claude Code spawns `pbcopy`).
                                self.selection = None;
                                self.drag_anchor = None;
                                self.send_mouse_sgr(&pane_id, 0, col, row, true);
                                self.mouse_forward_pane = Some(pane_id.clone());
                            } else if self.pane_is_image(&pane_id) {
                                // Image pane: a drag pans the zoomed image
                                // instead of selecting text.
                                self.selection = None;
                                self.drag_anchor = None;
                                self.begin_image_pan(&pane_id);
                            } else if !self.pane_is_terminal(&pane_id) {
                                // Markdown panes are document views, not
                                // terminals — a drag here must not start a cell
                                // text-selection. A click on a code-block copy
                                // button copies it; otherwise (raw editor) the
                                // click places the edit caret, and in rendered
                                // mode it opens a link.
                                self.selection = None;
                                self.drag_anchor = None;
                                if !self.try_copy_md_block() {
                                    if self.md_body_rects.contains_key(&pane_id) {
                                        self.md_click_caret(
                                            &pane_id,
                                            self.cursor_px.0,
                                            self.cursor_px.1,
                                        );
                                    } else {
                                        self.try_open_md_link();
                                    }
                                }
                            } else {
                                self.drag_anchor = Some((col, row));
                                self.selection = Some(Selection {
                                    anchor: (col, row),
                                    end: (col, row),
                                });
                            }
                            self.last_input_at = Instant::now();
                            if let Some(tmux) = self.tmux.as_ref() {
                                let _ =
                                    tmux.send_cmd(&format!("select-pane -t '{pane_id}'"));
                            }
                        }
                    }
                    ElementState::Released => {
                        // A titlebar press that never moved past the drag
                        // threshold: just a click, drop the deferred move.
                        self.titlebar_drag_pending = None;
                        // Armed URL: a click (cursor barely moved) opens it; a
                        // drag past the threshold just disarms (text selection
                        // never started since the press returned early).
                        if let Some((url, (px, py))) = self.link_armed.take() {
                            let (cx, cy) = self.cursor_px;
                            if (cx - px).abs() < 4.0 && (cy - py).abs() < 4.0 {
                                let _ = std::process::Command::new("open")
                                    .arg(&url)
                                    .spawn();
                                window.request_redraw();
                                return;
                            }
                        }
                        // End an image pan drag.
                        if self.image_pan_drag.take().is_some() {
                            window.set_cursor(CursorIcon::Default);
                            window.request_redraw();
                            return;
                        }
                        // End a tab drag: a real drag reorders the pane's tab
                        // list; a plain press just switches to that tab.
                        if let Some(mut td) = self.tab_drag.take() {
                            window.set_cursor(CursorIcon::Default);
                            // Tab → pane BODY drop: split the target pane in
                            // the quadrant the cursor landed in and place the
                            // moved tab as the new leaf. Eats the old
                            // header-drag UX (drop in body = relocate) but
                            // unified into the tab drag so the user never has
                            // to find non-tab space on the header.
                            // drop_target_at already covers the strip area
                            // (box extends up to the pane's tab strip), so
                            // we no longer need the over_strip fallback —
                            // it was the source of body↔strip flicker.
                            let body_drop: Option<(String, DropZone)> = if td.active {
                                self.drop_target_at(self.cursor_px.0, self.cursor_px.1)
                            } else {
                                None
                            };
                            if let Some((target, zone)) = body_drop {
                                // Center on header = tab merge — route
                                // through the cross-pane path below by
                                // rewriting drop_pane; Center on self
                                // cancels (drop on own header is a no-op).
                                if zone == DropZone::Center {
                                    if target != td.pane {
                                        let dst_len = self
                                            .ws
                                            .lock()
                                            .unwrap()
                                            .panes
                                            .get(&target)
                                            .map(|p| p.tabs.len())
                                            .unwrap_or(0);
                                        td.drop_pane = target.clone();
                                        td.target = dst_len;
                                    } else {
                                        self.chrome_dirty = true;
                                        window.request_redraw();
                                        return;
                                    }
                                    // Fall through to cross_pane merge.
                                } else {
                                let src_tab_count = self
                                    .ws
                                    .lock()
                                    .unwrap()
                                    .panes
                                    .get(&td.pane)
                                    .map(|p| p.tabs.len())
                                    .unwrap_or(0);
                                if target == td.pane && src_tab_count == 1 {
                                    // Single-tab pane dropped on its own body
                                    // half: the user "threw" the pane to that
                                    // side. Spawn a fresh shell on the
                                    // OPPOSITE side so the original sits where
                                    // it was dropped.
                                    if let Err(e) =
                                        self.split_pane_opposite(&td.pane, zone)
                                    {
                                        eprintln!("[split-opposite] {e}");
                                    }
                                    self.chrome_dirty = true;
                                    window.request_redraw();
                                    return;
                                }
                                if target != td.pane || src_tab_count > 1 {
                                    // Daemon mode + single-tab cross-pane = move
                                    // the whole pane beside target → surface.move
                                    // RPC (daemon authority). A local
                                    // drop_tab_into_body wouldn't reach the daemon,
                                    // so the next State overwrites it and the pane
                                    // goes dead (drag먹통).
                                    if target != td.pane && src_tab_count == 1 {
                                        self.move_pane(&td.pane, &target, zone);
                                        self.chrome_dirty = true;
                                        window.request_redraw();
                                        return;
                                    }
                                    // Multi-tab same-pane → lift dragged tab into
                                    // a new pane. Cross-pane (non-daemon) → moved
                                    // tab in a new pane on target's drop side.
                                    // (Daemon multi-tab lift = GUI-local 보조탭;
                                    // 데몬 동기화는 후속.)
                                    self.drop_tab_into_body(&td, &target, zone);
                                    self.chrome_dirty = true;
                                    window.request_redraw();
                                    return;
                                }
                                }
                            }
                            let cross_pane = td.active && td.drop_pane != td.pane;
                            if cross_pane {
                                // Move the tab to another pane. We do this in
                                // 3 steps:
                                //   1. lift the PaneTab out of source.tabs
                                //   2. update pid_to_pane so future PTY output
                                //      routes to the destination pane
                                //   3. insert at the target index in dest.tabs;
                                //      if source ends up empty, collapse the
                                //      source pane out of the layout entirely
                                let mut moved_pid: Option<String> = None;
                                let mut moved: Option<PaneTab> = None;
                                let mut src_empty = false;
                                {
                                    let mut ws = self.ws.lock().unwrap();
                                    if let Some(src) = ws.panes.get_mut(&td.pane) {
                                        let n = src.tabs.len();
                                        if td.from < n {
                                            let tab = src.tabs.remove(td.from);
                                            moved_pid = tab.pid.clone();
                                            moved = Some(tab);
                                            if td.from < src.active_tab && src.active_tab > 0 {
                                                src.active_tab -= 1;
                                            }
                                            if src.active_tab >= src.tabs.len()
                                                && !src.tabs.is_empty()
                                            {
                                                src.active_tab = src.tabs.len() - 1;
                                            }
                                            src.dirty = true;
                                            src_empty = src.tabs.is_empty();
                                        }
                                    }
                                    if let (Some(tab), Some(pid)) =
                                        (moved.take(), moved_pid.clone())
                                    {
                                        // Re-bind the pid to the new outer.
                                        ws.pid_to_pane.insert(pid, td.drop_pane.clone());
                                        if let Some(dst) = ws.panes.get_mut(&td.drop_pane) {
                                            let to = td.target.min(dst.tabs.len());
                                            dst.tabs.insert(to, tab);
                                            dst.active_tab = to;
                                            dst.dirty = true;
                                        }
                                    }
                                    if src_empty {
                                        // Source has no tabs left — drop the
                                        // outer entry so remove_pane below can
                                        // collapse the layout cleanly.
                                        ws.panes.remove(&td.pane);
                                    }
                                }
                                if src_empty {
                                    // Source is empty because every tab — INCLUDING the
                                    // primary whose pid equalled the outer id — went to
                                    // dest. `remove_pane` would kill self.pty[outer]
                                    // here, which is the very PtySession we just handed
                                    // to dest. Use a layout-only collapse that leaves
                                    // self.pty / image textures / markdown untouched
                                    // since those resources now belong to dest.
                                    self.collapse_layout_only(&td.pane);
                                }
                                // Focus the destination pane so the moved
                                // tab is immediately interactive.
                                self.ws.lock().unwrap().active_pane =
                                    Some(td.drop_pane.clone());
                            } else if let Ok(mut ws) = self.ws.lock() {
                                if let Some(pane) = ws.panes.get_mut(&td.pane) {
                                    let n = pane.tabs.len();
                                    if td.active && n > 1 {
                                        let from = td.from.min(n - 1);
                                        let mut to = td.target.min(n);
                                        if to > from {
                                            to -= 1;
                                        }
                                        let item = pane.tabs.remove(from);
                                        let to = to.min(pane.tabs.len());
                                        pane.tabs.insert(to, item);
                                        // Dragging a tab selects it at its new spot.
                                        pane.active_tab = to;
                                    } else {
                                        // Plain click → switch to the pressed tab.
                                        pane.active_tab = td.from.min(n.saturating_sub(1));
                                    }
                                    pane.dirty = true;
                                }
                            }
                            self.chrome_dirty = true;
                            window.request_redraw();
                            return;
                        }
                        // End a sidebar resize drag (no other commit needed —
                        // the live width is already in self.sidebar_w_logical).
                        if self.sidebar_resize.take().is_some() {
                            window.set_cursor(CursorIcon::Default);
                            return;
                        }
                        // End a file-tree column resize drag.
                        if self.file_tree.resize.take().is_some() {
                            window.set_cursor(CursorIcon::Default);
                            return;
                        }
                        // End a git-column resize drag.
                        if self.git.col_resize.take().is_some() {
                            window.set_cursor(CursorIcon::Default);
                            return;
                        }
                        // End a divider drag without falling through to the
                        // selection-release path under it.
                        if let Some((path, dir)) = self.resize_drag.take() {
                            // Final flush — the throttle may have suppressed
                            // the cursor's last cell-crossing, leaving the
                            // divider at a stale pos. Re-derive from the
                            // current cursor and apply once authoritatively.
                            let (cols, rows) = self.window_cells();
                            let pad = WINDOW_PADDING + self.effective_sidebar_w();
                            let pos = match dir {
                                kasa_pty::SplitDir::Horizontal => (((self.cursor_px.0
                                    - pad)
                                    / self.cell.w.max(1.0))
                                .round() as i32)
                                    .clamp(0, cols as i32)
                                    as u16,
                                kasa_pty::SplitDir::Vertical => (((self.cursor_px.1
                                    - TITLE_HEIGHT)
                                    / self.cell.h.max(1.0))
                                .round() as i32)
                                    .clamp(0, rows as i32)
                                    as u16,
                            };
                            if let Some(tree) = self.pty_layout.as_mut() {
                                tree.resize_divider(&path, pos, cols, rows);
                            }
                            self.resize_backend(cols, rows);
                            self.last_divider_pos = None;
                            self.last_divider_pty_resize = None;
                            window.request_redraw();
                            return;
                        }
                        // Drop a header drag: relocate onto the target
                        // pane's edge. A non-active drag was just a click
                        // (focus already happened on press), so we only
                        // reset the cursor.
                        if let Some(hd) = self.header_drag.take() {
                            window.set_cursor(CursorIcon::Default);
                            if hd.active {
                                let dt = self.drop_target_at(self.cursor_px.0, self.cursor_px.1);
                                let sw = if dt.is_none() {
                                    self.sidebar_window_drop_target(self.cursor_px.0, self.cursor_px.1)
                                } else {
                                    None
                                };
                                // [임시 진단] cross-window 드래그가 안 되는 원인 파악용.
                                // .app은 stderr가 안 보여 파일에 남긴다. 검증 후 제거.
                                if let Ok(mut f) = std::fs::OpenOptions::new()
                                    .create(true)
                                    .append(true)
                                    .open("/tmp/kt-drag.log")
                                {
                                    use std::io::Write;
                                    let _ = writeln!(
                                        f,
                                        "drop pane={} cursor=({:.0},{:.0}) drop_target={:?} sidebar_win={:?} win_tab_rects={} windows={}",
                                        hd.pane,
                                        self.cursor_px.0,
                                        self.cursor_px.1,
                                        dt.as_ref().map(|(t, _)| t.as_str()),
                                        sw.as_deref(),
                                        self.window_tab_rects.len(),
                                        self.windows.len(),
                                    );
                                }
                                if let Some((target, zone)) = dt {
                                    self.move_pane(&hd.pane, &target, zone);
                                } else if let Some(target) = sw {
                                    // Dropped onto a sidebar window chip: relocate the
                                    // pane into that window. The daemon's move_surface
                                    // does the cross-window detach/insert; the zone is
                                    // arbitrary (it lands beside that window's anchor).
                                    self.move_pane(&hd.pane, &target, DropZone::Right);
                                }
                            }
                            return;
                        }
                        // Mouse-reporting drag end: forward the release
                        // so the TUI can finalize its selection /
                        // copy-on-select.
                        if let Some(pane_id) = self.mouse_forward_pane.take() {
                            if let Some((col, row)) =
                                self.px_to_cell_active(self.cursor_px.0, self.cursor_px.1)
                            {
                                self.send_mouse_sgr(&pane_id, 0, col, row, false);
                            }
                        } else {
                            self.drag_anchor = None;
                            if let Some(sel) = self.selection {
                                if sel.anchor == sel.end {
                                    self.selection = None;
                                } else {
                                    self.copy_selection();
                                }
                            }
                        }
                    }
                }
                window.request_redraw();
            }
            WindowEvent::Ime(ime) => {
                if std::env::var_os("KASATERM_IME_DEBUG").is_some() {
                    eprintln!("[ime] event={ime:?}");
                }
                match ime {
                    Ime::Enabled => {
                        // OS IME just took ownership of the keyboard
                        // (script switch / app focus). Mark active so
                        // the KeyboardInput branch drops any echo of
                        // text the IME will deliver via Preedit/Commit.
                        self.ime_active = true;
                        self.in_preedit = false;
                        self.preedit.clear();
                    }
                    Ime::Disabled => {
                        self.ime_active = false;
                        self.in_preedit = false;
                        self.preedit.clear();
                    }
                    Ime::Preedit(text, _range) => {
                        // Receiving a Preedit implies the IME is
                        // active — winit doesn't always emit Enabled
                        // first on macOS, so we set both flags here.
                        self.ime_active = true;
                        self.in_preedit = !text.is_empty();
                        self.preedit = text;
                    }
                    Ime::Commit(text) => {
                        // Remember the committed text at the current cursor so
                        // the overlay keeps it visible until the PTY echo lands
                        // and moves the cursor (render_frame retires it then).
                        // Without this the next syllable's preedit is drawn over
                        // the not-yet-echoed commit — fast typing looked like
                        // everything composing in one spot, then appearing at
                        // once. Consecutive commits at the same (un-echoed) spot
                        // accumulate so a burst keeps its order.
                        let before = self.ws.lock().ok().and_then(|ws| {
                            ws.active_pane.clone().and_then(|id| {
                                ws.panes
                                    .get(&id)
                                    .and_then(|p| p.term())
                                    .map(|t| (t.cursor_row, t.cursor_col))
                            })
                        });
                        self.commit_overlay = match self.commit_overlay.take() {
                            Some((prev, pos)) if Some(pos) == before => {
                                Some((format!("{prev}{text}"), pos))
                            }
                            _ => before.map(|b| (text.clone(), b)),
                        };
                        self.in_preedit = false;
                        self.preedit.clear();
                        self.send_bytes(text.as_bytes());
                    }
                }
                // Preedit is chrome, not PTY grid — flag it so the damage
                // gate actually paints the composing text this frame.
                self.chrome_dirty = true;
                window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                // Confirm-close modal: Enter = 닫기, Esc = 취소. Swallow all
                // other keys so nothing reaches the PTY behind the dim.
                if self.confirm_close.is_some() {
                    if matches!(event.state, ElementState::Pressed) {
                        use winit::keyboard::{Key, NamedKey};
                        match event.logical_key {
                            Key::Named(NamedKey::Enter) => {
                                self.confirm_dialog_pick(ConfirmBtn::Close, event_loop);
                            }
                            Key::Named(NamedKey::Escape) => {
                                self.confirm_dialog_pick(ConfirmBtn::Cancel, event_loop);
                            }
                            _ => {}
                        }
                        window.request_redraw();
                    }
                    return;
                }
                // KASATERM_KEY_DEBUG=1 → dump every key event with its
                // modifier snapshot. Used to debug "Cmd+= doesn't zoom"
                // class issues where it's unclear whether the OS even
                // forwards the chord to us or our handler ignores it.
                if std::env::var_os("KASATERM_KEY_DEBUG").is_some() {
                    eprintln!(
                        "[key] state={:?} physical={:?} logical={:?} text={:?} super={} ctrl={} shift={} alt={}",
                        event.state,
                        event.physical_key,
                        event.logical_key,
                        event.text,
                        self.modifiers.super_key(),
                        self.modifiers.control_key(),
                        self.modifiers.shift_key(),
                        self.modifiers.alt_key(),
                    );
                }
                // Cmd+Q (macOS) / Ctrl+Shift+Q (Win/Linux): quit, but raise the
                // confirm modal first when a job is running. macOS hands Cmd+Q
                // to us as a key event — we never register an app-menu Quit item
                // that would otherwise swallow it. Routes through the same
                // confirm path as the red-light close so both agree.
                if matches!(event.state, ElementState::Pressed)
                    && !event.repeat
                    && self.host_mod()
                    && matches!(
                        event.physical_key,
                        winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyQ)
                    )
                {
                    if !self.confirm_or_close_window() {
                        event_loop.exit();
                    }
                    return;
                }
                // Cmd+Shift+A (macOS) / Ctrl+Shift+A: SCHALE OS(아로나) 패널 토글 —
                // 터미널로 작업하다 한 키로 전환(거노). PTY 로는 안 흘린다.
                if matches!(event.state, ElementState::Pressed)
                    && !event.repeat
                    && self.host_mod()
                    && self.modifiers.shift_key()
                    && matches!(
                        event.physical_key,
                        winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyA)
                    )
                {
                    self.toggle_arona_panel(event_loop);
                    window.request_redraw();
                    return;
                }
                self.forward_key(&event);
            }
            WindowEvent::DroppedFile(path) => {
                // 이미지 파일을 떨구면 클립보드에 비트맵으로 실은 뒤
                // Ctrl+V(0x16)를 위임한다 — claude code가 osascript로 클립보드
                // PNG를 직접 읽어 [Image] 칩으로 첨부한다. 경로 텍스트만 박던
                // 옛 방식은 claude 가 이미지로 인식 못 해 칩이 안 떴다.
                let is_img = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| {
                        matches!(
                            e.to_ascii_lowercase().as_str(),
                            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
                        )
                    })
                    .unwrap_or(false);
                if is_img {
                    if let Ok(img) = image::open(&path) {
                        let rgba = img.to_rgba8();
                        let (w, h) = rgba.dimensions();
                        let data = arboard::ImageData {
                            width: w as usize,
                            height: h as usize,
                            bytes: std::borrow::Cow::Owned(rgba.into_raw()),
                        };
                        if let Ok(mut cb) = arboard::Clipboard::new() {
                            if cb.set_image(data).is_ok() {
                                self.send_bytes(&[0x16]);
                                return;
                            }
                        }
                    }
                }
                // 비이미지(코드 파일 등) 또는 디코드/클립보드 실패 → 경로 입력.
                // iTerm 동작: shell-quoted 경로 + 끝 공백. 작은따옴표로 공백을
                // 한 토큰으로 묶고, 경로 속 따옴표는 '\'' 로 escape.
                let p = path.to_string_lossy();
                let quoted = format!("'{}' ", p.replace('\'', "'\\''"));
                self.send_bytes(quoted.as_bytes());
            }
            WindowEvent::RedrawRequested => {
                self.render_frame();
                self.maybe_update_window_title();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Live-resize flush: if a Resized arrived while the user was dragging
        // an edge we stashed it and skipped the actual resize work. Once the
        // user lets go, inLiveResize flips false and we replay the final size
        // here — surface.configure + PTY reshape + render happen once,
        // off the critical path of the live-resize tracking loop.
        if let (Some(window), Some(size)) =
            (self.window.clone(), self.pending_resize)
        {
            if !gpu::is_in_live_resize(&window) {
                if std::env::var_os("KASATERM_RESIZE_DEBUG").is_some() {
                    eprintln!(
                        "[rsz {}ms] about_to_wait flush {}x{}",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_millis()
                            % 100000,
                        size.width,
                        size.height
                    );
                }
                self.pending_resize = None;
                let gpu_mode = self.gpu.is_some();
                gpu::with_disabled_layer_actions(|| {
                    if gpu_mode {
                        if let Some(g) = self.gpu.as_mut() {
                            g.resize(size.width, size.height);
                        }
                    }
                    let (cols, rows) = self.window_cells();
                    if (cols, rows) != self.last_resized_cells {
                        self.last_resized_cells = (cols, rows);
                        self.resize_backend(cols, rows);
                    }
                    self.chrome_dirty = true;
                    self.render_frame();
                });
            }
        }
        // Drain menu clicks from muda's global channel. The "Git 패널" item
        // toggles the in-window git column (open/close).
        while let Ok(ev) = muda::MenuEvent::receiver().try_recv() {
            if self.git_menu_item.as_ref().map(|m| m.id()) == Some(&ev.id) {
                self.toggle_git_col();
            } else if self.session_menu_item.as_ref().map(|m| m.id()) == Some(&ev.id) {
                self.toggle_session_panel(event_loop);
            } else if self.board_menu_item.as_ref().map(|m| m.id()) == Some(&ev.id) {
                self.toggle_board_panel(event_loop);
            } else if self.arona_menu_item.as_ref().map(|m| m.id()) == Some(&ev.id) {
                self.toggle_arona_panel(event_loop);
            }
        }
        // Headless git-panel demo (expand diff / open modal) before the capture.
        if let Some((at, action)) = self.pending_autogit.clone() {
            if std::time::Instant::now() >= at {
                self.run_autogit(&action);
                self.pending_autogit = None;
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
        }
        // Headless verification: arm a GPU frame-readback capture once its
        // deadline passes (before autoquit, so the capture lands first).
        if let Some((at, path)) = self.pending_capture.clone() {
            if std::time::Instant::now() >= at {
                if let Some(g) = self.gpu.as_mut() {
                    g.capture_next = Some(path);
                }
                self.pending_capture = None;
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
        }
        // Headless verification: clean-exit once the autoquit deadline passes
        // so save-on-exit (and the next launch's restore) can be tested.
        if let Some(at) = self.autoquit_at {
            if std::time::Instant::now() >= at {
                event_loop.exit();
                return;
            }
        }
        // Fire any queued session-restore commands whose delay has elapsed.
        // Each carries its own PtySession so a resume reaches the right pane in
        // any session (active or stashed background).
        if !self.pending_restores.is_empty() {
            let now = std::time::Instant::now();
            self.pending_restores.retain(|(sess, cmd, at)| {
                if now >= *at {
                    let _ = sess.send_bytes(cmd.as_bytes());
                    false
                } else {
                    true
                }
            });
        }
        // Reap dead pty sessions before anything else — a closed shell
        // should disappear from the layout on the very next loop turn
        // so the user sees the gap collapse immediately.
        self.reap_dead_panes(event_loop);
        // Refresh per-pane busy state (Claude's working spinner → header bar +
        // completion toast). Self-throttled, so this is cheap per loop turn.
        self.refresh_pane_activity();
        // Drain socket commands from external cmux clients. These run
        // through the same split/focus/send paths Cmd+D etc use, so
        // visible behavior is identical regardless of whether the
        // trigger came from a keystroke or a JSON-RPC call.
        // Fire any due KASATERM_AUTOSPLIT step before parking. No-op
        // when no plan is queued.
        self.run_pending_autosplits();
        self.run_pending_autowindows();
        self.run_pending_autodrag();
        self.run_pending_autopanemove();
        self.run_pending_autotoggle();
        self.run_pending_autoarona(event_loop);
        self.run_pending_onboarding(event_loop);
        self.run_pending_autotabs();
        self.run_pending_autoopen();
        self.run_pending_autoconfirm();
        // Pure event-driven loop, like Ghostty. A WaitUntil timer poll
        // gets coalesced by macOS, so a cross-thread wake (PTY echo via
        // the proxy) landed anywhere from 6ms to ~290ms late — that was
        // the inconsistent input lag. With `Wait` the loop sleeps with
        // zero latency until a real event arrives:
        //   - keystrokes  → window_event
        //   - PTY echo     → proxy UserEvent (ScreenUpdate thread)
        //   - cursor blink → proxy UserEvent (dedicated blink thread)
        // Each of those drives a redraw directly, so there's no timer in
        // the hot path to be coalesced.
        //
        // Exception: while the launch build banner is still fading we DO
        // need a timer, since nothing else is producing frames. Re-arm a
        // ~30fps WaitUntil until the fade finishes, then fall back to the
        // idle Wait. (new_events → request_redraw on the timer fire.)
        // The copy toast fade needs the same treatment as the launch banner.
        // (echo-stale 격리) busy 30fps 펌프 임시 제거 — version/copy 토스트만
        // WaitUntil, 나머지는 Wait. ws lock 경합이 echo stream을 막는지 확인.
        if self.version_alpha() > 0.0
            || self.copy_toast_alpha() > 0.0
            // Sticky approval toast doesn't animate — only a *fading* collab
            // toast needs the timer pump. (A blocked pane can sit for minutes;
            // pumping 30fps the whole time would burn battery for nothing.)
            || (self.collab_toast_alpha() > 0.0 && self.collab.toast_action.is_none())
            || self.any_notify_flash()
            // A busy pane's header working bar sweeps every frame — pump ~30fps
            // so the bar animates and the working→idle flip is caught promptly.
            // `blocked`/`waiting` (approval prompt) are static states: no pump.
            || self
                .pane_activity
                .values()
                .any(|a| a.status == "working")
            || self.pending_capture.is_some()
            || self.pending_autogit.is_some()
            || self.autoquit_at.is_some()
            // An unseen-notification window tab blinks (synced to the cursor
            // blink) until the user switches to it — pump frames so it pulses.
            || !self.window_alert.is_empty()
        {
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + std::time::Duration::from_millis(33),
            ));
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }

    fn new_events(
        &mut self,
        _event_loop: &ActiveEventLoop,
        cause: winit::event::StartCause,
    ) {
        // The blink-timer fire path. When winit wakes us because the
        // WaitUntil deadline elapsed (no other events arrived), repaint
        // so the cursor block toggles its phase. Other wake causes
        // (input, redraw, init) drive their own redraws.
        if matches!(cause, winit::event::StartCause::ResumeTimeReached { .. }) {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }
}

impl App {
    /// Resolve a confirm-close modal: 취소 just dismisses; 닫기 runs the pending
    /// action (a `Window` close needs the event loop to exit, the rest go
    /// through `do_close`).
    pub(crate) fn confirm_dialog_pick(&mut self, btn: ConfirmBtn, event_loop: &ActiveEventLoop) {
        let Some(dlg) = self.confirm_close.take() else { return };
        self.chrome_dirty = true;
        if btn == ConfirmBtn::Cancel {
            return;
        }
        match dlg.action {
            PendingClose::Window => event_loop.exit(),
            other => self.do_close(other),
        }
    }
}

/// Build a git-column snapshot from `git_status`, split into Staged Changes /
/// Changes (VSCode model; no dedup, so a partially-staged file shows in both).
/// Returns `None` on a transient git failure so the caller keeps the last good
/// snapshot. Shared by the 1.2s poller and the per-click stage/unstage refresh
/// so a + / − press reflects immediately instead of waiting for the next tick.
fn fetch_git_col_view(cwd: &std::path::Path) -> Option<GitColView> {
    let v = kasa_mcp::git::git_status(cwd);
    if v.get("error").is_some() {
        return None;
    }
    let mut view = GitColView {
        cwd: Some(cwd.to_path_buf()),
        ..Default::default()
    };
    if v.get("no_repo").and_then(|b| b.as_bool()).unwrap_or(false) {
        view.no_repo = true;
        return Some(view);
    }
    view.branch = v.get("branch").and_then(|s| s.as_str()).unwrap_or("").to_string();
    view.ahead = v.get("ahead").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
    view.behind = v.get("behind").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
    view.insertions = v.get("insertions").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
    view.deletions = v.get("deletions").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
    view.clean = v.get("clean").and_then(|b| b.as_bool()).unwrap_or(false);
    if let Some(arr) = v.get("staged").and_then(|a| a.as_array()) {
        for p in arr.iter().filter_map(|p| p.as_str()) {
            view.staged.push(('A', p.to_string()));
        }
    }
    for (key, marker) in [("modified", 'M'), ("untracked", 'U')] {
        if let Some(arr) = v.get(key).and_then(|a| a.as_array()) {
            for p in arr.iter().filter_map(|p| p.as_str()) {
                view.unstaged.push((marker, p.to_string()));
            }
        }
    }
    view.branches = kasa_mcp::git::git_branches(cwd);
    view.numstat = kasa_mcp::git::git_numstat(cwd);
    view.recent_commits = kasa_mcp::git::git_log(cwd, 5)
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    let h = c.get("hash")?.as_str()?.to_string();
                    let s = c.get("subject").and_then(|s| s.as_str()).unwrap_or("").to_string();
                    Some((h, s))
                })
                .collect()
        })
        .unwrap_or_default();
    Some(view)
}
