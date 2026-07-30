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
        // 배정 캐릭터를 PaneState 에 동기 — has_header 가 이걸 보고 단일 pane 도 헤더 띠를
        // 띄운다(거노: 터미널에도 학생 이름). 매 업데이트라 교체 시 다음 프레임에 반영.
        let pane_char = ws.pane_character.get(&update.pane_id).cloned();
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
        pane.character = pane_char;
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
        let marker_backend = self.socket_backend.clone();
        // statusline 세션 id 마커(⟦sid8⟧)의 마지막 관측값 — 값이 바뀔 때만 rebind 를
        // 태워 같은 마커의 재렌더(매 프레임)를 무시한다.
        let mut last_marker: Option<String> = None;
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
                // OSC 777 desktop notification — drain before the coalesce
                // merge below rebuilds `update` and would drop it.
                if let Some((title, body)) = update.notify.take() {
                    let _ = proxy.send_event(UserEvent::Notify {
                        surface_id: update.pane_id.clone(),
                        title,
                        body,
                    });
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
                        Ok(mut next) if !next.eof => {
                            // OSC 777 from a coalesced frame — fire before the
                            // merge below drops `next.notify`.
                            if let Some((title, body)) = next.notify.take() {
                                let _ = proxy.send_event(UserEvent::Notify {
                                    surface_id: next.pane_id.clone(),
                                    title,
                                    body,
                                });
                            }
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
                // 세션 진입 즉시 감지(거노): dirty 행에 statusline 세션 id 마커가 있으면
                // 그 자리에서 rebind — 3s 폴러를 기다리지 않는다. 마커는 세션 화면의
                // 일부라 agents 피커로 진입한 첫 리드로우에 반드시 실려 온다. '⟦' 스캔은
                // 문자 비교뿐이라 스트리밍 버스트에도 공짜에 가깝다. rebind 는 apply
                // "후"에 태운다 — 그리드를 다시 읽으므로 마커가 반영된 뒤여야 한다.
                let marker = update.dirty.iter().rev().find_map(|(_, row)| {
                    row.iter().any(|c| c.ch == '⟦').then(|| {
                        let s: String = row.iter().map(|c| c.ch).collect();
                        crate::socket::screen_marker_sid8(&s)
                    })?
                });
                let pane_for_marker = update.pane_id.clone();
                let mut ws = ws_screens.lock().unwrap();
                Self::apply_screen_update(&mut ws, update);
                drop(ws);
                if let Some(w) = win_screens.as_ref() {
                    w.request_redraw();
                }
                // Wake the loop even if it's parked on a WaitUntil —
                // request_redraw alone doesn't do that reliably on macOS.
                let _ = proxy.send_event(UserEvent::Redraw);
                if let (Some(m), Some(be)) = (marker, marker_backend.as_ref()) {
                    if last_marker.as_deref() != Some(m.as_str()) {
                        be.rebind_pane_marker(&pane_for_marker, &m);
                        last_marker = Some(m);
                    }
                }
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
    /// pane 생성 시 캐릭터 자동 배정 — /tmp 마커·session-id 기록 후 셸 env 를 반환.
    /// pending_character(new_room_with_character 가 세팅) 우선, 없으면 통합 풀에서
    /// 안 겹친 캐릭터 랜덤. characters.json 없으면 빈 vec(무테마 = skip).
    /// board(socket.rs)는 같은 /tmp 마커를 읽어 row.character 를 채운다.
    pub(crate) fn assign_character_env(
        &mut self,
        id: &str,
        cwd: Option<&str>,
        room: Option<&str>,
    ) -> Vec<(String, String)> {
        let Some(cwd) = cwd else { return Vec::new() };
        let Some(chars) = kasa_mcp::character::characters_json() else {
            return Vec::new();
        };
        let rslug = kasa_mcp::character::rslug(std::path::Path::new(cwd), room);
        // 통합 풀(member_names = leader/leaders/members 병합) — god 개념 폐기(거노
        // 2026-07-13): 아로나·프라나도 특별 클래스 없이 동등하게 랜덤 배정.
        let members = kasa_mcp::character::member_names(&chars);
        // 프로젝트(방)를 넘어 같은 학생이 겹치지 않게, 이 방 live pane + 전 방 마커를 모두
        // taken 으로 본다(거노: 미도리 둘 — 방-로컬 배정이라 다른 방 미도리를 못 봤다).
        // ws.pane_character/read_marker(이 방 live) + assigned_global(전 방). 닫힌 pane
        // 마커는 cleanup_collab_markers 가 지우므로 대체로 live 만 남는다.
        let taken: std::collections::HashSet<String> = {
            let ws = self.ws.lock().unwrap();
            let mut t: std::collections::HashSet<String> = ws
                .panes
                .keys()
                .filter(|p| p.as_str() != id)
                .filter_map(|p| {
                    ws.pane_character
                        .get(p)
                        .cloned()
                        .or_else(|| kasa_mcp::character::read_marker(&rslug, p))
                })
                .collect();
            t.extend(kasa_mcp::character::assigned_global());
            t
        };
        // pending(사용자 지정 캐릭터)은 중복이어도 존중 — 같은 학생 허용, 색은
        // character_ordinal 변주로 구분(거노). 랜덤 배정만 taken 을 피한다.
        let name = match self.pending_character.take() {
            Some(n) => n,
            None => {
                let free: Vec<String> =
                    members.iter().filter(|n| !taken.contains(n.as_str())).cloned().collect();
                let pick = kasa_mcp::character::pick_random(&free, id)
                    .or_else(|| kasa_mcp::character::pick_random(&members, id));
                match pick {
                    Some(n) => n,
                    None => return Vec::new(),
                }
            }
        };
        // 학생 명령(`시로코`)이 남긴 persona override 는 이 spawn 의 fresh env 보다
        // 오래된 정체성 — 지워서 이 pane 의 다음 claude 가 env 기준으로 돌아가게.
        if let Ok(shim) = std::env::var("KASATERM_TMUX_SHIM_DIR") {
            for ext in ["character", "persona"] {
                let _ = std::fs::remove_file(
                    std::path::Path::new(&shim).join(format!("repersona-{id}.{ext}")),
                );
            }
        }
        let sid = kasa_mcp::character::new_session_id();
        let _ = kasa_mcp::character::write_marker(&rslug, id, &name);
        self.pane_session_id.insert(id.to_string(), sid.clone());
        // 세션→캐릭터 영속 바인딩(거노 ④): 같은 세션이 --resume 등으로 다시 붙으면 같은
        // 캐릭터를 재사용하도록 스폰 시점에 기록(apply_session_character 가 조회).
        let _ = kasa_mcp::character::bind_session_character(&sid, &name);
        self.ws.lock().unwrap().pane_character.insert(id.to_string(), name.clone());
        let mut env = vec![
            ("KASATERM_CHARACTER".to_string(), name.clone()),
            ("KASATERM_SESSION_ID".to_string(), sid.clone()),
        ];
        if let Some(p) = kasa_mcp::character::persona_for(&chars, &name) {
            env.push(("KASATERM_PERSONA".to_string(), p));
        }
        env
    }

    /// Spawn the first shell pane for the *current* (already-cleared) session.
    /// Mirrors start_pty's pane bring-up with a fresh pane id and no socket
    /// (re)init — used by new_session.
    pub(crate) fn spawn_session_pane(&mut self) -> Result<()> {
        let (cols, rows) = self.window_cells();
        let cwd = resolve_initial_cwd();
        let id = format!("%{}", self.next_pane_id);
        self.next_pane_id += 1;
        // 방별 분리(거노): 이 pane 이 새 방이면 KASATERM_ROOM 을 셸 env 로 주입해 collab
        // 훅이 방별 slug 를 쓰게 한다. pane_room 에도 기록(Rust collab slug 계산용).
        let mut env = crate::proxy_env(&id);
        let room = self.pending_room.take();
        if let Some(ref room) = room {
            env.push(("KASATERM_ROOM".to_string(), room.clone()));
            self.ws.lock().unwrap().pane_room.insert(id.clone(), room.clone());
        }
        // 캐릭터 자동 배정(거노): pending(사용자 지정) 우선, 없으면 통합 풀 랜덤. 마커·
        // session-id 기록 후 KASATERM_CHARACTER/SESSION_ID/PERSONA env 를 더한다(claude shim 적용).
        env.extend(self.assign_character_env(&id, cwd.as_deref(), room.as_deref()));
        let session = Arc::new(kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            shell: self.pending_shell.take().or_else(resolve_default_shell),
            cwd: cwd.clone(),
            cols,
            rows,
            env,
            pane_id: id.clone(),
            initial_scrollback: Vec::new(),
        })?);
        self.pump_pty_screens(session.screens.clone(), id.clone());
        self.pty.insert(id.clone(), session.clone());
        self.pty_layout = Some(kasa_pty::PtyLayout::single(&id));
        self.ws.lock().unwrap().active_pane = Some(id);
        Ok(())
    }

    /// bind-transcript 로 pane 의 실제 세션 id 를 인지한 시점의 캐릭터 영속화(거노 ④):
    /// 부모(포크/백그라운드)가 있으면 그 학생을 우선 상속하고, 없으면 세션 매핑으로
    /// 이름표를 교정(respawn 없음 — persona 는 스폰 시 고정, label·마커만 갱신,
    /// --resume 둔갑 방지), 그것도 없으면 현재 배정을 저장해 다음 resume 이 재사용한다.
    pub(crate) fn apply_session_character(&mut self, pane: &str, sid: &str) {
        let cur = self.ws.lock().unwrap().pane_character.get(pane).cloned();
        // 우선순위: 세션 자신의 바인딩 > 부모 상속 > env anchor. 예전엔 부모가 바인딩을
        // 덮었지만("첫 호출에 박힌 랜덤 바인딩 교정"용) — 지금 바인딩은 전부 의도적
        // 기록(ResumeSession 해석/신선 배정, lazy own, 여기 None-arm 영속화)이라 부모가
        // 이기면 오히려 진실이 뒤집힌다: 미도리로 확정된 포크 세션(2535079b)의 부모
        // (b18e41d2)가 히마리라서, BgAgentsChanged 재적용마다 미도리→히마리로 둔갑+
        // 재바인딩되는 지뢰였다(거노 07-16). 부모는 자기 바인딩이 없을 때만.
        match kasa_mcp::character::session_character(sid) {
            Some(mapped) => {
                if cur.as_deref() != Some(mapped.as_str()) {
                    self.relabel_pane(pane, &mapped);
                }
                return;
            }
            None => {}
        }
        // 포크/백그라운드 세션은 부모 대화의 연장 — 자기 바인딩이 없으면 부모 학생을
        // 상속하고 sid 에 영속화한다. 첫 호출 때 bg_agents 가 비어(폴러 3초 주기) 이
        // 분기를 놓쳐도, 폴러의 BgAgentsChanged 재적용이 뒤늦게 부모를 물려준다.
        let parent_char = self
            .bg_agents
            .lock()
            .ok()
            .and_then(|m| m.get(sid).cloned())
            .flatten()
            .and_then(|parent| kasa_mcp::character::session_character(&parent));
        match parent_char {
            Some(pc) => {
                if cur.as_deref() != Some(pc.as_str()) {
                    self.relabel_pane(pane, &pc);
                }
                let _ = kasa_mcp::character::bind_session_character(sid, &pc);
            }
            None => {
                // stem 매핑도 부모도 없는 포크/재접속(claude 가 transcript id 를 새로 발급,
                // parentSessionId 부재) — 랜덤 cur 를 정본으로 굳히기 전에 pane 프로세스 env 의
                // KASATERM_SESSION_ID(스폰 때 학생에 바인딩된 원본 anchor, env 상속으로 보존)로
                // 진짜 학생을 복원한다(거노: 백그라운드 재접속에서 미도리→유우카 둔갑).
                let anchored = self
                    .pty
                    .get(pane)
                    .and_then(|p| p.shell_pid())
                    .and_then(|pid| kasa_pty::process_env_var(pid, "KASATERM_SESSION_ID"))
                    .filter(|env_sid| env_sid.as_str() != sid)
                    .and_then(|env_sid| kasa_mcp::character::session_character(&env_sid));
                match anchored {
                    Some(true_char) => {
                        if cur.as_deref() != Some(true_char.as_str()) {
                            self.relabel_pane(pane, &true_char);
                        }
                        // stem 으로도 바로 잡히게 영속화 — 다음 board 폴링·재접속 안정화.
                        let _ = kasa_mcp::character::bind_session_character(sid, &true_char);
                    }
                    None => {
                        // 첫 인지 — 이 세션의 정본 캐릭터로 현재 배정을 영속화.
                        if let Some(cur) = cur.filter(|c| !c.is_empty()) {
                            let _ = kasa_mcp::character::bind_session_character(sid, &cur);
                        }
                    }
                }
            }
        }
    }

    /// pane 캐릭터 이름표 교정 — pane_character + board /tmp 마커 + redraw. 부모
    /// 상속·세션 매핑 두 경로가 공유한다. 실존 pane 만(훅 오호출·죽은 pane 가드).
    fn relabel_pane(&mut self, pane: &str, character: &str) {
        if !self.ws.lock().unwrap().panes.contains_key(pane) {
            return;
        }
        self.ws
            .lock()
            .unwrap()
            .pane_character
            .insert(pane.to_string(), character.to_string());
        // board 도 같은 이름을 보게 /tmp 마커 동기(swap_character 의 cwd/room 관례).
        if let Some(cwd) = self.pane_cwd_cache.get(pane).cloned() {
            let room = self.ws.lock().unwrap().pane_room.get(pane).cloned();
            let rslug = kasa_mcp::character::rslug(&cwd, room.as_deref());
            let _ = kasa_mcp::character::write_marker(&rslug, pane, character);
        }
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// pane 캐릭터 재배정, respawn 없음 — 학생 명령(`시로코`)이 claude 실행 직전에
    /// `/repersona` 로 호출한다. persona 는 래퍼가 override 파일로 직접 싣고 여기선
    /// GUI 상태(헤더·테두리·board 마커·세션 바인딩)만 새 캐릭터로 맞춘다. 중복 허용
    /// — 같은 학생 pane 은 색 변주(character_ordinal)로 구분(거노).
    pub(crate) fn repersona_pane(&mut self, pane: &str, character: &str) {
        if !self.ws.lock().unwrap().panes.contains_key(pane) {
            return;
        }
        // 로스터 밖 이름 가드 — 엔드포인트로 들어오는 자유 문자열이 헤더/마커를
        // 오염하지 않게. 래퍼는 characters.json 기준으로만 설치되니 정상 경로는 통과.
        let Some(chars) = kasa_mcp::character::characters_json() else { return };
        if !kasa_mcp::character::member_names(&chars).iter().any(|n| n == character) {
            eprintln!("[repersona] unknown character '{character}' — ignored");
            return;
        }
        self.ws.lock().unwrap().pane_character.insert(pane.to_string(), character.to_string());
        if let Some(cwd) = self.pane_cwd_cache.get(pane).cloned() {
            let room = self.ws.lock().unwrap().pane_room.get(pane).cloned();
            let rslug = kasa_mcp::character::rslug(&cwd, room.as_deref());
            let _ = kasa_mcp::character::write_marker(&rslug, pane, character);
        }
        // --resume 가 같은 캐릭터로 돌아오게 세션 바인딩도 갱신.
        if let Some(sid) = self.pane_session_id.get(pane) {
            let _ = kasa_mcp::character::bind_session_character(sid, character);
        }
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// pane 캐릭터 교체 — persona 는 셸 spawn 시 고정이라 PTY 를 새 persona 로 respawn
    /// 한다(대화 리셋, 거노 확인 후). 같은 pane id·leaf 유지라 레이아웃·자리 그대로,
    /// 헤더/board 캐릭터만 다음 화면에 갱신(assign_character_env 가 ws.pane_character·마커
    /// 를 덮음).
    pub(crate) fn swap_character(&mut self, pane: &str, character: &str) {
        let cwd = self.pane_cwd_cache.get(pane).map(|p| p.to_string_lossy().into_owned());
        let room = self.ws.lock().unwrap().pane_room.get(pane).cloned();
        let (cols, rows) = self.window_cells();
        // old PTY 종료(셸·claude 죽음). pump 스레드는 EOF 로 빠진다.
        self.pty.remove(pane);
        // 새 persona 강제 — assign_character_env 가 pending 우선 사용해 마커·env 갱신.
        self.pending_character = Some(character.to_string());
        let mut env = crate::proxy_env(pane);
        if let Some(ref r) = room {
            env.push(("KASATERM_ROOM".to_string(), r.clone()));
        }
        env.extend(self.assign_character_env(pane, cwd.as_deref(), room.as_deref()));
        match kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            shell: resolve_default_shell(),
            cwd,
            cols,
            rows,
            env,
            pane_id: pane.to_string(),
            initial_scrollback: Vec::new(),
        }) {
            Ok(session) => {
                let sess = Arc::new(session);
                self.pump_pty_screens(sess.screens.clone(), pane.to_string());
                self.pty.insert(pane.to_string(), sess.clone());
                // old PTY 의 EOF 가 이 pane id 를 dead_panes 에 넣었을 수 있다 — 같은 id 로
                // respawn 했으니 그 stale 죽음표시를 지워 reap 이 새 pane 을 닫지 않게(거노:
                // 캐릭터 변경하면 pane 이 닫히던 버그). reap 에 contains_key 가드도 있지만 명시.
                self.dead_panes.lock().unwrap().retain(|x| x != pane);
                // 새 PTY 는 셸 프롬프트만 — 교체는 돌던 claude 를 죽이므로, 프롬프트가 뜰 즈음
                // claude 를 직접 주입해 새 persona 로 다시 시작한다(거노: 캐릭터 교체 = claude 새로.
                // 초기 부팅은 셸만 띄워도 됐지만, 교체는 claude 가 꺼진 채 셸만 남던 게 버그였다).
                let at = std::time::Instant::now() + std::time::Duration::from_millis(900);
                self.pending_restores.push((sess, "claude\r".to_string(), at));
                self.resize_backend(cols, rows);
                self.publish_pty_layout();
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            Err(e) => eprintln!("[swap_character] respawn failed: {e:#}"),
        }
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
        self.win_tab_reveal(self.active_window);
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
        self.win_tab_reveal(idx);
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
        // Keep the same tabs in view when one before the strip window closes;
        // out-of-range values are clamped by sidebar_layout either way.
        if idx < self.win_tab_first {
            self.win_tab_first -= 1;
        }
        self.win_tab_reveal(self.active_window);
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
            // window.rename override 가 파생 이름보다 우선한다 —
            // 지정 pane 이 대표 leaf 가 아니어도 유지돼야 한다.
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
                .and_then(|id| self.pane_current_cwd(id))
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
        let prev = prev_pane.and_then(|id| self.pane_current_cwd(id));
        resolve_spawn_cwd(prev)
    }
    /// Recompute the sidebar file tree when its root (the active pane's cwd)
    /// changes — pane switch or `cd`. Cheap string compare per frame; the
    /// read_dir walk only runs on a real change (or after expand/collapse,
    /// which calls `rebuild_file_tree_nodes` directly).
    /// `cwd` 를 감싸는 가장 가까운 git 레포 루트(1-엔트리 캐시 경유).
    /// 레포 밖이면 None — 호출부가 cwd 를 그대로 쓴다.
    pub(crate) fn anchored_tree_root(&mut self, cwd: &std::path::Path) -> Option<std::path::PathBuf> {
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
            .file_tree.fs_dirty
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            self.rebuild_file_tree_nodes();
        }
        let active = self.ws.lock().ok().and_then(|w| w.active_pane.clone());
        let root = active
            .as_ref()
            // "pane 이 보는 경로"(statusline report / transcript bind)가 셸 cwd 보다
            // 우선 — bg-attach 뷰 pane 은 셸이 spawn 디렉토리(~/Desktop)에 머물러
            // 파일트리가 pane 내용과 다른 프로젝트를 보여줬다(거노).
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
    /// "파일 열기" 설정이 내장 편집기가 아니면 그쪽으로 보내고 `true`. `false` 면
    /// 호출자가 내장 경로를 그대로 이어 간다 — 편집기를 못 찾았을 때도 여기로
    /// 떨어져, 설정이 어긋나 있어도 파일은 늘 열린다.
    fn open_file_elsewhere(&mut self, path: &std::path::Path) -> bool {
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
    /// 내장 편집기로 되돌린다: 이 맥의 기본 연결 프로그램은 거노가 목록에서
    /// 일부러 뺀 앱이라, 폴백이 그쪽으로 가면 고친 게 도로 나타난다.
    fn open_file_in_app(&mut self, path: &std::path::Path) -> bool {
        let want = socket::read_file_open_app();
        if want.trim().is_empty() {
            crate::proc::open_path_default(path);
            return true;
        }
        match crate::proc::open_with_apps().iter().find(|(name, _)| *name == want) {
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
    fn open_file_in_editor_pane(&mut self, path: &std::path::Path) -> bool {
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
        let Ok(pane) = self.split_active_pane(kasa_pty::SplitDir::Horizontal) else {
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
        self.pending_restores.push((sess, format!("{}\r", editor_command_line(&cmd, path)), at));
        true
    }

    /// Open a sidebar file in a fresh split pane (right of the active pane).
    /// Images decode into an `Image` pane; real markdown renders as a laid-out
    /// doc; any other text loads as a fenced code block so the highlighter
    /// colors it. Re-opening a file already on screen just focuses its pane
    /// instead of stacking duplicate splits. PTY-less — `resize_backend` skips
    /// leaves with no `self.pty` entry, so the new pane never spawns a shell.
    pub(crate) fn open_file_split(&mut self, path: std::path::PathBuf) {
        self.open_file(path, None, false);
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
        if self.tmux.is_some() {
            return;
        }
        // "파일 열기" 설정은 **사람이 연 것**에만 적용한다. `as_tab` 은 에이전트·
        // 소켓의 미리보기 요청이라, 그것까지 pane 을 새로 열면 파일을 보여 달랄
        // 때마다 화면이 쪼개진다.
        if !as_tab && !crate::is_image_path(&path) && self.open_file_elsewhere(&path) {
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
                    p.active_tab = tab_idx.min(p.tabs.len().saturating_sub(1));
                    p.dirty = true;
                }
                ws.active_pane = Some(id);
            }
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
                doc,
                is_md_doc: is_md,
                raw_mode: !is_md,
                edit_lines,
                cur_line: 0,
                cur_col: 0,
                scroll: 0,
                h_scroll: 0.0,
                modified: false,
                sel_anchor: None,
                undo_stack: Vec::new(),
                redo_stack: Vec::new(),
                last_edit: EditKind::Break,
            find: None,
            edited_at: None,
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
        let tab_outer: Option<String> = if as_tab {
            let ws = self.ws.lock().unwrap();
            target
                .as_deref()
                .and_then(|t| ws.outer_for_pty(t))
                .filter(|o| ws.panes.contains_key(o))
                .or_else(|| ws.panes.contains_key(&active).then(|| active.clone()))
        } else {
            None
        };
        if let Some(outer) = tab_outer {
            let mut ws = self.ws.lock().unwrap();
            if let Some(pane) = ws.panes.get_mut(&outer) {
                pane.tabs.push(tab);
                pane.active_tab = pane.tabs.len() - 1;
                pane.dirty = true;
            }
            ws.active_pane = Some(outer);
            drop(ws);
            self.chrome_dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }

        let ps = PaneState { tabs: vec![tab], dirty: true, ..Default::default() };
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
    /// macOS `.md` 더블클릭(odoc Apple Event)/argv → 새 워크스페이스(사이드바 탭)에
    /// 마크다운 뷰어를 단독 pane(풀스크린)으로 띄운다. `open_file_split`(현재 창
    /// split)과 달리 기존 워크스페이스를 안 건드리고 새 윈도우 슬롯을 만든다.
    /// PTY 없는 pane이라 셸 spawn 은 안 한다 — `resize_backend`/키 입력은 PTY miss
    /// 로 자동 skip(이미지 pane 과 같은 PTY-less 선례).
    pub(crate) fn open_markdown_window(&mut self, path: std::path::PathBuf) {
        if self.tmux.is_some() {
            return;
        }
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        // 이미 열려 있으면 그 워크스페이스로 전환만(중복 탭 방지).
        let existing = {
            let ws = self.ws.lock().unwrap();
            ws.panes.iter().find_map(|(id, p)| {
                p.tabs
                    .iter()
                    .any(|t| t.preview_path.as_deref() == Some(path.as_path()))
                    .then(|| id.clone())
            })
        };
        if let Some(pid) = existing {
            if let Some(wi) = self.window_of_pane(&pid) {
                self.switch_window(wi);
            }
            self.ws.lock().unwrap().active_pane = Some(pid);
            self.chrome_dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }

        let raw = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[open-md] 파일 읽기 실패 {}: {e}", path.display());
                return;
            }
        };
        let new_id = format!("%{}", self.next_pane_id);
        self.next_pane_id += 1;
        let doc = Arc::new(build_markdown_doc(&path, &raw));
        let title = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string());
        let mut tab = PaneTab::default();
        // `.md` 는 렌더뷰(raw_mode=false) — 거노 확정. open_file_split 의 .md 분기 동일.
        tab.content = PaneContent::Markdown(MarkdownPane {
            doc,
            is_md_doc: true,
            raw_mode: false,
            edit_lines: Arc::default(),
            cur_line: 0,
            cur_col: 0,
            scroll: 0,
            h_scroll: 0.0,
            modified: false,
            sel_anchor: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit: EditKind::Break,
            find: None,
            edited_at: None,
        });
        tab.title = title;
        tab.title_pinned = true;
        tab.preview_path = Some(path.clone());
        let ps = PaneState { tabs: vec![tab], dirty: true, ..Default::default() };

        // 새 윈도우 슬롯 — new_window 의 슬롯 스왑만 차용(spawn_session_pane 제외).
        self.windows[self.active_window] = self.pty_layout.take();
        self.windows.push(None);
        self.active_window = self.windows.len() - 1;
        self.win_tab_reveal(self.active_window);

        // 마크다운 pane = 새 윈도우의 유일한 leaf → ws.layout=None → 풀스크린 fallback.
        self.ws.lock().unwrap().panes.insert(new_id.clone(), ps);
        self.pty_layout = Some(kasa_pty::PtyLayout::single(new_id.as_str()));
        self.ws.lock().unwrap().active_pane = Some(new_id);

        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows); // PTY 없는 leaf 는 self.pty miss → no-op
        self.publish_pty_layout();
        self.window_labels_at = None; // 다음 paint 에 사이드바 라벨 재계산(파일명 폴백)
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
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
    /// hit-test agree on every rect. Overflow: the strip shows a contiguous
    /// run of whole tabs starting at `win_tab_first` (no partial clipping —
    /// the renderer has no scissor). This only clamps `first` into range;
    /// keeping the *active* tab in view is `win_tab_reveal`'s job at
    /// switch/create time, so a free wheel-scroll is never yanked back.
    pub(crate) fn sidebar_layout(
        &self,
        win_h: f32,
    ) -> (
        Vec<(usize, (f32, f32, f32, f32))>,
        Vec<(usize, (f32, f32, f32, f32))>,
        (f32, f32, f32, f32),
    ) {
        let n = self.windows.len();
        if self.tabs_on_top {
            // Horizontal tabs in the title strip (Windows Terminal-style).
            // Same return shape as the vertical layout so the paint loop and
            // every click/drag hit-test keep working off the cached rects.
            let win_w = self
                .window
                .as_ref()
                .map(|w| w.inner_size().width as f32 / self.effective_scale())
                .unwrap_or(1200.0);
            let (tbx, _, tbw, _) = self.file_tree_toggle_rect();
            // 14px slots at both ends host the overflow chevrons — reserved
            // unconditionally so geometry doesn't depend on overflow state.
            let x0 = tbx + tbw + 10.0 + 14.0;
            // Right-side chip cluster (arona + settings + git-col) stays clear.
            let right_reserved = 110.0 + 14.0;
            let plus_w = 26.0;
            let gap = 4.0;
            let avail = (win_w - right_reserved - x0 - plus_w - 6.0).max(60.0);
            // Whole tabs that fit at the 72px minimum width; hidden rest is
            // reachable by wheel (win_tab_first) and stays out of the rects.
            let n_vis = n.min((((avail + gap) / (72.0 + gap)) as usize).max(1));
            let first = self.win_tab_first.min(n.saturating_sub(n_vis));
            let tab_w = ((avail - gap * n_vis.saturating_sub(1) as f32) / n_vis.max(1) as f32)
                .clamp(72.0, 170.0);
            let tab_h = 26.0;
            let y = (TITLE_HEIGHT - tab_h) / 2.0;
            let mut tabs = Vec::with_capacity(n_vis);
            let mut closes = Vec::new();
            for (vi, i) in (first..n.min(first + n_vis)).enumerate() {
                let x = x0 + vi as f32 * (tab_w + gap);
                tabs.push((i, (x, y, tab_w, tab_h)));
                if n > 1 {
                    let cs = 14.0;
                    closes.push((i, (x + tab_w - cs - 5.0, y + (tab_h - cs) / 2.0, cs, cs)));
                }
            }
            let plus = (x0 + tabs.len() as f32 * (tab_w + gap), y, plus_w, tab_h);
            return (tabs, closes, plus);
        }
        let tab_x = SIDEBAR_TAB_INSET;
        let tab_w = (self.sidebar_w_logical - 2.0 * SIDEBAR_TAB_INSET).max(0.0);
        // 10px slot above the first tab hosts the overflow chevron-up.
        let top = TITLE_HEIGHT + 18.0;
        let stride = SIDEBAR_TAB_H + SIDEBAR_TAB_GAP;
        // Rows that fit above the "+" button; the dock strip eats the bottom
        // of the column, and 24px stays free for "+"-adjacent chrome + the
        // chevron-down overflow hint.
        let dock_h = if self.docked.is_empty() { 0.0 } else { DOCK_HEIGHT };
        let avail_h = (win_h - dock_h - top - 28.0 - 24.0).max(stride);
        let n_vis = n
            .min((((avail_h + SIDEBAR_TAB_GAP) / stride) as usize).max(1));
        let first = self.win_tab_first.min(n.saturating_sub(n_vis));
        let mut tabs = Vec::with_capacity(n_vis);
        let mut closes = Vec::new();
        for (vi, i) in (first..n.min(first + n_vis)).enumerate() {
            let y = top + vi as f32 * stride;
            tabs.push((i, (tab_x, y, tab_w, SIDEBAR_TAB_H)));
            if n > 1 {
                let cs = 14.0;
                closes.push((i, (tab_x + tab_w - cs - 3.0, y + 3.0, cs, cs)));
            }
        }
        let plus_y = top + tabs.len() as f32 * stride;
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
        Ok(())
    }
    /// Serialize every session (active + stashed) as a layout tree so the next
    /// launch can restore the full multi-pane, multi-session workspace.
    ///
    /// Split out from `save_session_state` so the autosave path can hash the
    /// result and skip an identical write — the two must produce byte-identical
    /// state or a Cmd+Q would look like a change and rewrite for nothing.
    pub(crate) fn session_state_json(&self) -> Option<serde_json::Value> {
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
                windows_json.push(Self::layout_to_json(layout, pty, &ws_guard, &self.pane_claude_sid));
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
            return None;
        }
        Some(serde_json::json!({
            "active_session": self.active_session,
            "sessions": sessions_json,
        }))
    }
    /// Write the restore snapshot on exit.
    ///
    /// 복원 창이 아직 떠 있으면 쓰지 않는다 — 그 화면은 사용자가 "복원"을 고르기
    /// 전의 빈 새 세션이라, 여기서 저장하면 **되살리려던 작업 공간을 그 빈 세션으로
    /// 덮어써** 영영 잃는다(복원할지 말지 못 정하고 그냥 껐을 때). autosave_session
    /// 과 같은 이유.
    pub(crate) fn save_session_state(&self) {
        if self.restore_prompt.is_some() {
            return;
        }
        if let Some(state) = self.session_state_json() {
            socket::write_session_state(&state);
        }
    }
    /// 강제 종료 대비 자동 스냅샷. `exiting()` 만으로는 Cmd+Q(정중한 종료) 때만
    /// 저장돼, SIGKILL·크래시·정전이면 그 세션의 작업이 디스크에 아예 안 남고
    /// 복원 창은 **직전에 정상 종료했던 시점**의 낡은 상태를 띄운다.
    ///
    /// 실제로 바뀐 경우에만 쓴다. 이 앱은 마우스만 움직여도 깨어나므로 wake 를
    /// 곧 변경으로 보면 몇 초마다 같은 내용을 다시 쓰게 된다 — 직렬화는 하되
    /// 해시가 같으면 디스크는 건드리지 않는다.
    pub(crate) fn autosave_session(&mut self) {
        self.session_saved_at = std::time::Instant::now();
        self.session_touched = false;
        // 복원 창이 떠 있는 동안은 절대 저장하지 않는다 — 사용자가 "복원"을 고르기
        // 전의 화면은 빈 새 세션이라, 자동 저장이 복원 대상 자체를 덮어써 버린다
        // (되돌릴 수 없는 자해). 선택이 끝나면 그 클릭이 다시 touched 를 세운다.
        if self.restore_prompt.is_some() {
            return;
        }
        let Some(state) = self.session_state_json() else { return };
        let body = state.to_string();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&body, &mut h);
        let sum = std::hash::Hasher::finish(&h);
        if self.session_saved_hash == Some(sum) {
            return;
        }
        self.session_saved_hash = Some(sum);
        socket::write_session_state(&state);
    }
    /// Walk a live PtyLayout into the nested JSON the restore loader reads,
    /// resolving each leaf's pane id to its cwd/claude record.
    pub(crate) fn layout_to_json(
        layout: &kasa_pty::PtyLayout,
        pty: &HashMap<String, Arc<kasa_pty::PtySession>>,
        ws: &Workspace,
        pane_claude_sid: &HashMap<String, String>,
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
                    // pane id 자체를 저장한다. 이게 없으면 복원이 `%1` 부터 새로
                    // 번호를 매기는데, `--resume` 으로 되살아난 학생은 재시작 **전**
                    // 의 surface_id 를 대화 기록째 기억하고 있다 → `tell %5` 가 없는
                    // pane 이거나 그 사이 다른 pane 이 물려받은 번호로 배달된다
                    // (거노: "재시작하면 학생들이 tell 을 이상한 pane 에 쓴다").
                    obj.insert("pane_id".to_string(), serde_json::json!(pane_id));
                    let sb = ws
                        .panes
                        .get(pane_id)
                        .map(scrollback_lines)
                        .unwrap_or_default();
                    obj.insert("scrollback".to_string(), serde_json::json!(sb));
                    // 캐릭터 영속(거노: 재시작하면 미도리로 둔갑): pane_character 는
                    // claude 프로세스 감지(was_claude)와 무관하게 살아있으므로, 감지가
                    // 실패해도 캐릭터는 여기서 확실히 저장한다.
                    if let Some(name) = ws.pane_character.get(pane_id) {
                        obj.insert("character".to_string(), serde_json::json!(name));
                    }
                    // per-pane 실제 세션은 SocketSessionBound 로 채워진 pane_claude_sid
                    // (정본)로 최우선 확정한다. 예전엔 argv(pane_record)·cwd 최신 jsonl 로
                    // 폴백했는데, argv 없는 fresh `claude` 여럿이 같은 cwd 면 전부 cwd 최신
                    // 세션 하나로 뭉쳐 재시작 시 여러 pane 이 다 같은 대화+캐릭터(미도리)로
                    // 복원됐다(거노: 다른 세션이 다 미도리로 뭉침). cwd 최신 폴백을 제거하고
                    // pane_claude_sid 로만 session_id 를 확정한다 — 없으면 pane_record 의
                    // argv sid, 그것도 없으면 restore_leaf 가 fresh claude 로 복원.
                    if let Some(sid) = pane_claude_sid.get(pane_id) {
                        obj.insert("session_id".to_string(), serde_json::json!(sid));
                    }
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
                    "a": Self::layout_to_json(a, pty, ws, pane_claude_sid),
                    "b": Self::layout_to_json(b, pty, ws, pane_claude_sid),
                }})
            }
        }
    }
    /// Count leaves that were running claude across the whole saved state — the
    /// number the restore prompt shows. 총 pane 수는 `count_panes`.
    ///
    /// 예전엔 `character` 가 붙어 있으면 claude pane 으로 셌다. 캐릭터는 claude
    /// 여부와 무관하게 **spawn 때 모든 pane 에 배정**되므로(assign_character_env),
    /// 순수 셸 3개짜리 창이 "claude 세션 3개"로 표시됐다. 감지 실패 보정은
    /// session_id 로 한다 — 그건 claude 가 실제로 세션을 바인딩했을 때만 붙어,
    /// 저장 시점에 claude 가 포그라운드가 아니어도 남는다.
    pub(crate) fn count_claude_panes(state: &serde_json::Value) -> usize {
        fn walk(node: &serde_json::Value, n: &mut usize) {
            if let Some(leaf) = node.get("leaf") {
                let was_claude = leaf
                    .get("was_claude")
                    .and_then(|b| b.as_bool())
                    .unwrap_or(false);
                let bound_sid = leaf
                    .get("session_id")
                    .and_then(|c| c.as_str())
                    .is_some_and(|s| !s.is_empty());
                if was_claude || bound_sid {
                    *n += 1;
                }
            } else if let Some(split) = node.get("split") {
                if let Some(a) = split.get("a") {
                    walk(a, n);
                }
                if let Some(b) = split.get("b") {
                    walk(b, n);
                }
            }
        }
        let mut n = 0;
        if let Some(sessions) = state.get("sessions").and_then(|s| s.as_array()) {
            for s in sessions {
                if let Some(windows) = s.get("windows").and_then(|w| w.as_array()) {
                    for w in windows {
                        walk(w, &mut n);
                    }
                }
            }
        }
        n
    }
    /// 저장된 상태의 전체 pane(leaf) 수. claude 가 하나도 없는 순수 셸 작업 공간도
    /// 레이아웃·스크롤백은 복원할 값이 있으므로, 프롬프트를 띄울지는 이 수로 정한다
    /// (claude 수로 정하면 셸만 쓰던 창은 강제 종료 후 아무것도 못 되살린다).
    pub(crate) fn count_panes(state: &serde_json::Value) -> usize {
        fn walk(node: &serde_json::Value, n: &mut usize) {
            if node.get("leaf").is_some() {
                *n += 1;
            } else if let Some(split) = node.get("split") {
                if let Some(a) = split.get("a") {
                    walk(a, n);
                }
                if let Some(b) = split.get("b") {
                    walk(b, n);
                }
            }
        }
        let mut n = 0;
        if let Some(sessions) = state.get("sessions").and_then(|s| s.as_array()) {
            for s in sessions {
                if let Some(windows) = s.get("windows").and_then(|w| w.as_array()) {
                    for w in windows {
                        walk(w, &mut n);
                    }
                }
            }
        }
        n
    }
    /// Rebuild the workspace saved by `save_session_state` (user chose 복원):
    /// recreate each window's split layout, spawn a pane per leaf seeded with
    /// its saved scrollback, and queue `claude --resume <id>` for panes that
    /// were running claude so the conversation — and, via the shim, the student
    /// identity — comes back.
    ///
    /// Only the active session's windows are restored into the live fields;
    /// detached sessions aren't wired up (`self.sessions` is always `[None]`),
    /// so the saved `sessions` array carries exactly one entry in practice.
    pub(crate) fn restore_session_state(&mut self, state: &serde_json::Value) {
        let Some(sessions) = state.get("sessions").and_then(|s| s.as_array()) else {
            return;
        };
        let active = state
            .get("active_session")
            .and_then(|n| n.as_u64())
            .unwrap_or(0) as usize;
        let Some(session) = sessions.get(active).or_else(|| sessions.first()) else {
            return;
        };
        let Some(windows) = session.get("windows").and_then(|w| w.as_array()) else {
            return;
        };
        let active_window = session
            .get("active_window")
            .and_then(|n| n.as_u64())
            .unwrap_or(0) as usize;
        // Tear down the blank session start_pty just spawned: drop its PTY and
        // clear its pane state so the rebuilt layout starts from an empty slate.
        // The socket server (start_socket_pty) stays up — only panes are rebuilt.
        self.pty.clear();
        {
            let mut ws = self.ws.lock().unwrap();
            ws.panes.clear();
            ws.active_pane = None;
        }
        self.pty_layout = None;
        self.windows.clear();
        let (cols, rows) = self.window_cells();
        for (j, w) in windows.iter().enumerate() {
            let tree = self.restore_window_layout(w, cols, rows);
            if j == active_window {
                self.pty_layout = tree;
                self.windows.push(None);
            } else {
                self.windows.push(tree);
            }
        }
        self.active_window = active_window.min(self.windows.len().saturating_sub(1));
        // Never leave the user staring at a blank window: if every leaf in the
        // active window failed to spawn (or a corrupt index left no live slot),
        // reset to a single fresh pane so the invariant (active slot == None)
        // holds and something is on screen.
        if self.pty_layout.is_none() {
            self.windows = vec![None];
            self.active_window = 0;
            let _ = self.spawn_session_pane();
        }
        if let Some(first) = self
            .pty_layout
            .as_ref()
            .and_then(|l| l.leaves().first().map(|s| s.to_string()))
        {
            self.ws.lock().unwrap().active_pane = Some(first);
        }
        self.chrome_dirty = true;
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(win) = self.window.as_ref() {
            win.request_redraw();
        }
    }
    /// Recursively rebuild one window's BSP tree from its saved JSON, spawning a
    /// pane per surviving leaf. A leaf whose record is null (cwd/pid unresolved
    /// at save) or whose PTY fails to spawn is dropped, and a split with one
    /// dead child collapses to the survivor so the tree never carries an empty
    /// half.
    fn restore_window_layout(
        &mut self,
        node: &serde_json::Value,
        cols: u16,
        rows: u16,
    ) -> Option<kasa_pty::PtyLayout> {
        if let Some(leaf) = node.get("leaf") {
            if leaf.is_null() {
                return None;
            }
            let id = self.restore_leaf(leaf, cols, rows)?;
            return Some(kasa_pty::PtyLayout::Leaf { pane_id: id });
        }
        if let Some(split) = node.get("split") {
            let dir = match split.get("dir").and_then(|d| d.as_str()) {
                Some("v") => kasa_pty::SplitDir::Vertical,
                _ => kasa_pty::SplitDir::Horizontal,
            };
            let ratio = split.get("ratio").and_then(|r| r.as_f64()).unwrap_or(0.5) as f32;
            let a = split
                .get("a")
                .and_then(|a| self.restore_window_layout(a, cols, rows));
            let b = split
                .get("b")
                .and_then(|b| self.restore_window_layout(b, cols, rows));
            return match (a, b) {
                (Some(a), Some(b)) => Some(kasa_pty::PtyLayout::Split {
                    dir,
                    ratio,
                    a: Box::new(a),
                    b: Box::new(b),
                }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            };
        }
        None
    }
    /// Spawn one restored pane from its saved record and, when it was running
    /// claude, queue the resume command. Returns the new pane id, or None if the
    /// PTY failed to start (caller then collapses the split).
    fn restore_leaf(
        &mut self,
        rec: &serde_json::Value,
        cols: u16,
        rows: u16,
    ) -> Option<String> {
        let saved = rec.get("pane_id").and_then(|v| v.as_str());
        let taken = |s: &str| self.pty.contains_key(s);
        let id = pick_restore_id(saved, taken, &mut self.next_pane_id);
        let cwd = rec
            .get("cwd")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
            .or_else(resolve_initial_cwd);
        let was_claude = rec
            .get("was_claude")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        let session_id = rec
            .get("session_id")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string());
        // 저장된 캐릭터를 되살린다(거노: 재시작하면 랜덤 둔갑). pending 으로 세팅하면
        // assign_character_env 가 랜덤 대신 이걸 재사용하고, 저장 세션 id 가 있으면 그
        // 원본 sid 에 캐릭터를 다시 bind 해 --resume 후 shim 교정·다음 재시작까지 영속화한다.
        let saved_char = rec
            .get("character")
            .and_then(|c| c.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        if let Some(ref c) = saved_char {
            self.pending_character = Some(c.clone());
            if let Some(ref sid) = session_id {
                let _ = kasa_mcp::character::bind_session_character(sid, c);
            }
        }
        let scrollback: Vec<String> = rec
            .get("scrollback")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let mut env = crate::proxy_env(&id);
        env.extend(self.assign_character_env(&id, cwd.as_deref(), None));
        let session = match kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            shell: resolve_default_shell(),
            cwd: cwd.clone(),
            cols,
            rows,
            env,
            pane_id: id.clone(),
            initial_scrollback: scrollback,
        }) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                eprintln!("[restore] pane {id} spawn failed: {e:#}");
                return None;
            }
        };
        self.pump_pty_screens(session.screens.clone(), id.clone());
        if let Some(ref c) = cwd {
            self.pane_cwd_cache
                .insert(id.clone(), std::path::PathBuf::from(c));
        }
        self.pty.insert(id.clone(), session.clone());
        // Bring claude back: --resume the saved conversation (the shim
        // re-attaches team/persona/character from the session id), or a fresh
        // claude when the pane ran claude but no session id was captured.
        // Plain-shell panes restore to just their shell + scrollback. 900ms
        // mirrors swap_character's wait for the shell prompt before injection.
        // was_claude 감지가 실패했어도 캐릭터+저장 sid 가 있으면 claude 학생 pane 이었던
        // 것이라 --resume 으로 대화를 복원한다(감지 실패 시 셸만 뜨던 회귀 차단).
        if was_claude || (saved_char.is_some() && session_id.is_some()) {
            // --resume 대상 대화가 실재할 때만 resume 한다. 저장된 sid 의 jsonl 이
            // 사라졌으면 claude 가 "No conversation found" 를 뱉고 빈 셸만 남아 학생
            // pane 이 통째 죽는다(거노: %3 시로코 복원 실패 — claude 세션이 없어 board
            // 순회에서 빠졌다). 그땐 fresh claude 로 폴백해 최소한 학생 pane(캐릭터는
            // env/marker 로 유지)은 살린다 — 대화는 잃지만 pane 이 통째 죽는 것보다 낫다.
            let resumable = session_id
                .as_deref()
                .and_then(socket::transcript_path_for_session)
                .map(|p| p.exists())
                .unwrap_or(false);
            let cmd = match &session_id {
                Some(sid) if resumable => format!("claude --resume {sid}\r"),
                _ => "claude\r".to_string(),
            };
            let at = std::time::Instant::now() + std::time::Duration::from_millis(900);
            self.pending_restores.push((session, cmd, at));
        }
        Some(id)
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
    pub(crate) fn start_socket_pty(&mut self) {
        let backend = Arc::new(socket::PtyBackend::new(
            self.proxy.clone(),
            self.ws.clone(),
            self.collab.attention.clone(),
            self.pane_status_pub.clone(),
            self.bg_agents.clone(),
        ));
        // GUI 쪽에도 핸들 보관 — ResumeSession 이 attach/재개 pane 의 transcript 를
        // bind hook 없이 즉석 확정(bind_transcript)할 때 쓴다.
        self.socket_backend = Some(backend.clone());
        self.start_socket_with(backend);
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
fn git_repo_root(start: &std::path::Path) -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok().map(std::path::PathBuf::from);
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

/// 복원되는 pane 이 쓸 id 를 고른다. **저장된 id 를 최우선**으로 되살린다 —
/// `--resume` 으로 되살아난 학생은 재시작 전의 surface_id 를 대화 기록째 기억하고
/// 있어서, 번호를 새로 매기면 `tell` 이 없는 pane 이거나 그 사이 다른 pane 이
/// 물려받은 번호로 배달된다(거노: "재시작하면 학생들이 tell 을 이상한 pane 에 쓴다").
///
/// 저장본에 id 가 없거나(옛 포맷) 이미 쓰이는 번호면 새로 발급한다. 되살린 번호가
/// 카운터보다 크면 카운터를 그 위로 밀어, 이후 split 이 같은 번호를 다시 내주지
/// 않게 한다.
fn pick_restore_id(saved: Option<&str>, taken: impl Fn(&str) -> bool, next: &mut u32) -> String {
    if let Some(s) = saved {
        if let Some(n) = s.strip_prefix('%').and_then(|d| d.parse::<u32>().ok()) {
            if !taken(s) {
                *next = (*next).max(n + 1);
                return s.to_string();
            }
        }
    }
    let s = format!("%{next}");
    *next += 1;
    s
}

/// 편집기 명령과 파일 경로로 셸에 칠 한 줄을 만든다. `{}` 가 있으면 그 자리에,
/// 없으면 맨 뒤에 경로가 들어간다(`code -w {} --goto 1` 처럼 인자 뒤에 뭔가 더
/// 붙는 편집기가 있다). 경로는 홑따옴표로 감싸고 내부 `'` 를 POSIX 방식으로
/// 끊어 붙인다 — 공백·한글·따옴표가 든 파일명이 명령을 쪼개지 못하게.
fn editor_command_line(cmd: &str, path: &std::path::Path) -> String {
    let q = format!("'{}'", path.display().to_string().replace('\'', r"'\''"));
    if cmd.contains("{}") {
        cmd.replace("{}", &q)
    } else {
        format!("{} {q}", cmd.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::{editor_command_line, git_repo_root, pick_restore_id};

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
    fn restore_keeps_the_saved_pane_id() {
        let mut next = 1;
        assert_eq!(pick_restore_id(Some("%9"), |_| false, &mut next), "%9");
        // 되살린 번호 위로 카운터가 밀려야 다음 split 이 %9 를 다시 안 준다.
        assert_eq!(next, 10);
    }

    #[test]
    fn restore_falls_back_when_the_id_is_missing_or_taken() {
        // 옛 저장본엔 pane_id 가 없다.
        let mut next = 3;
        assert_eq!(pick_restore_id(None, |_| false, &mut next), "%3");
        assert_eq!(next, 4);
        // 이미 살아 있는 번호는 뺏지 않는다.
        let mut next = 3;
        assert_eq!(pick_restore_id(Some("%1"), |s| s == "%1", &mut next), "%3");
        assert_eq!(next, 4);
        // `%` 없는 쓰레기 값도 폴백.
        let mut next = 5;
        assert_eq!(pick_restore_id(Some("garbage"), |_| false, &mut next), "%5");
    }

    #[test]
    fn restore_never_lowers_the_counter() {
        let mut next = 20;
        assert_eq!(pick_restore_id(Some("%2"), |_| false, &mut next), "%2");
        assert_eq!(next, 20);
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
