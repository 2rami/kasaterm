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
        // pid 라우팅이 터미널 아닌 탭(이미지/md 미리보기)에 떨어질 수 있다 — 여기서
        // expect 로 죽으면 호출자가 ws 락을 쥔 채 unwind 해 poison 이 GUI 전체로
        // 번진다. 프레임 하나를 버리는 쪽이 맞다.
        let Some(tp) = tab.term_mut() else { return };
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
        tp.inline_images = update.inline_images;
        tp.mouse_enabled = update.mouse_enabled;
        tp.mouse_sgr = update.mouse_sgr;
        tp.app_cursor = update.app_cursor;
        tp.bracketed_paste = update.bracketed_paste;
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
                            // A resize makes row numbers and widths belong to
                            // a different grid. Combining both generations
                            // leaves narrow panes with rows from the transient
                            // size until a later full redraw happens.
                            if (next.cols, next.rows) != (update.cols, update.rows) {
                                update = next;
                                continue;
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
        // 배정 풀 — 골라 둔 명단이 있으면 그것만, 없으면 전원(지금까지의 동작).
        let members = kasa_mcp::character::assignable_names(&chars);
        // `KASATERM_ASSIGN_DEBUG=1` — 풀이 왜 그 크기인지 찍는다. 「골랐는데 안 고른
        // 애가 나온다」는 신고가 왔을 때 설정을 읽었는지부터 갈라야 하는데, 그걸
        // 밖에서 볼 방법이 이것 말고 없다.
        if std::env::var_os("KASATERM_ASSIGN_DEBUG").is_some() {
            let all = kasa_mcp::character::member_names(&chars).len();
            eprintln!("[assign] pool={} / roster={all} — {:?}", members.len(), members);
        }
        // 프로젝트(방)를 넘어 같은 학생이 겹치지 않게, 이 방 live pane + 전 방 마커를 모두
        // taken 으로 본다(거노: 미도리 둘 — 방-로컬 배정이라 다른 방 미도리를 못 봤다).
        // ws.pane_character/read_marker(이 방 live) + assigned_global(전 방). 닫힌 pane
        // 마커는 cleanup_collab_markers 가 지우므로 대체로 live 만 남는다.
        // 이 방 live 만 따로 들고 있는다 — 전 방 마커까지 합친 taken 이 학생 총원을
        // 넘기면 고를 것이 하나도 안 남는데, 그때 members 전체로 되돌아가면 **같은 방
        // 안에서도** 겹친다. 실측 2026-08-09: 마커 17개 > 총원 12명이라 배정 풀이
        // 통째로 말라 아루가 셋이 됐다. 마커는 pane 을 정상적으로 닫을 때만 지워지므로
        // 앱을 재시작하면 옛 마커가 그대로 남아 이 고갈이 시간이 갈수록 잦아진다.
        // `all_taken` 은 중복을 살린 사본이다 — 풀이 마른 뒤 「가장 적게 쓰인 학생」을
        // 고르려면 있고 없고가 아니라 **몇 번 쓰였나**를 알아야 한다.
        let mut all_taken: Vec<String> = Vec::new();
        let (taken, taken_local): (
            std::collections::HashSet<String>,
            std::collections::HashSet<String>,
        ) = {
            let ws = self.ws.lock().unwrap();
            // **`ws.panes` 로만 돌면 안 된다** — split 로 생긴 leaf 는 보조탭이 생기기
            // 전까지 `PaneState` 가 없다(희소, main.rs `pane_font_scales` 주석). 그래서
            // 예전엔 방금 쪼갠 pane 들이 taken 에 안 잡혀 **연달아 쪼개면 같은 학생이
            // 둘 나왔다**(실측 2026-08-06 `split --count`: 모모이 둘·프라나 둘. 거노가
            // 전에 신고한 "미도리 둘"과 같은 증상, 원인만 다른 갈래).
            // 마커(`assigned_global`)도 못 메운다 — 그건 claude 가 뜰 때 쓰이므로 갓
            // 만든 pane 엔 아직 없다. 배정의 정본은 `pane_character` 다.
            let here: Vec<String> = ws
                .panes
                .keys()
                .chain(ws.pane_character.keys())
                .filter(|p| p.as_str() != id)
                // 「이 방」= rslug(프로젝트 cwd + 명시 room)다. `pane_character` 는
                // 앱 전역 맵이라 거르지 않으면 다른 방 학생까지 here 에 들어와,
                // ①첫 pane 이어도 here 가 안 비어 prefer_fresh_school 이 영영 안
                // 불리고 ②prefer_same_school 이 남의 방 학원으로 끌어당겨 **앱
                // 전체가 최초 학원 하나로 수렴**했다(2026-08-19 실측: 서로 다른 방
                // 다섯의 학생 5명 전원 밀레니엄 — 우연 확률 ≈0.9%. 방마다 학원을
                // 가르는 c999e10 의 절반이 이 스코프 누락으로 죽어 있었다).
                // cwd 를 아직 모르는 pane 은 같은 방으로 친다 — 같은 방을 놓쳐
                // 같은 얼굴이 나란히 서는 쪽이, 다른 방과 학원이 뭉치는 쪽보다 나쁘다.
                .filter(|p| {
                    self.pane_cwd_cache.get(p.as_str()).is_none_or(|c| {
                        let room = ws.pane_room.get(p.as_str()).cloned();
                        kasa_mcp::character::rslug(c, room.as_deref()) == rslug
                    })
                })
                .filter_map(|p| {
                    ws.pane_character
                        .get(p)
                        .cloned()
                        .or_else(|| kasa_mcp::character::read_marker(&rslug, p))
                })
                .collect();
            let local: std::collections::HashSet<String> = here.iter().cloned().collect();
            all_taken.extend(here);
            all_taken.extend(kasa_mcp::character::assigned_global());
            (all_taken.iter().cloned().collect(), local)
        };
        // pending(사용자 지정 캐릭터)은 중복이어도 존중 — 같은 학생 허용, 색은
        // character_ordinal 변주로 구분(거노). 랜덤 배정만 taken 을 피한다.
        let name = match self.pending_character.take() {
            Some(n) => n,
            None => {
                let free: Vec<String> =
                    members.iter().filter(|n| !taken.contains(n.as_str())).cloned().collect();
                // 고갈되면 곧장 전체로 되돌아가지 않고 **이 방 live 만** 피해 한 번 더
                // 고른다. 다른 방과 겹치는 것은 이름에 pane 번호가 붙어 구분되지만,
                // 같은 방에서 겹치면 화면에 같은 얼굴이 나란히 서서 누가 누군지 사라진다.
                let free_local: Vec<String> = members
                    .iter()
                    .filter(|n| !taken_local.contains(n.as_str()))
                    .cloned()
                    .collect();
                // 그마저 마르면 **가장 적게 쓰인 학생들** 중에서 고른다 — 전체 랜덤은
                // 이미 셋인 학생을 넷으로 만든다(`least_used` 주석에 실측).
                let least = kasa_mcp::character::least_used(&members, &all_taken);
                // 이 방에 이미 학생이 있으면 **같은 학원**에서 먼저 고른다. 첫 배정이
                // 그 방의 학원을 정하고, 이후 pane 들이 거기 붙어 한 덩어리로 읽힌다.
                // 학원이 마르면 아래 폴백으로 내려간다 — 학원을 맞추는 것보다 같은
                // 방에서 안 겹치는 게 먼저다.
                let here: Vec<String> = taken_local.iter().cloned().collect();
                let same_school = kasa_mcp::character::prefer_same_school(&chars, &free, &here);
                // 이 방의 첫 학생이면 반대로 **다른 방이 안 쓰는 학원**을 고른다 —
                // 그 한 명이 이 방의 학원을 정하므로, 여기서 갈라 두면 방마다 다른
                // 학원이 선다. 학원보다 방이 많아지면 빈 목록이 와 아래로 흐른다.
                let fresh_school = if here.is_empty() {
                    kasa_mcp::character::prefer_fresh_school(&chars, &free, &all_taken)
                } else {
                    Vec::new()
                };
                let pick = kasa_mcp::character::pick_random(&same_school, id)
                    .or_else(|| kasa_mcp::character::pick_random(&fresh_school, id))
                    .or_else(|| kasa_mcp::character::pick_random(&free, id))
                    .or_else(|| kasa_mcp::character::pick_random(&free_local, id))
                    .or_else(|| kasa_mcp::character::pick_random(&least, id))
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
            // 넷을 함께 지운다 — 셋만 지우면 남은 하나(모델·통로)가 새 학생에게
            // 따라붙어 이름과 얼굴만 바뀌고 앞 학생의 모델로 도는 상태가 된다.
            for ext in ["character", "persona", "model", "backend"] {
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
            // teammate 이름 꼬리 — 셰임이 `<슬러그>-p<번호>` 뒤에 그대로 붙인다.
            ("KASATERM_AGENT_SUFFIX".to_string(), crate::agent_name_suffix()),
        ];
        // 활성 로스터에 없는 이름(재배정·resume 으로 온 다른 테마 학생)은 합집합
        // 조회로 원 소속 테마의 말투를 찾는다 — 없으면 이름만 남고 말투가 빈다.
        //
        // ⚠️ 「말투」 토글이 꺼져 있으면 **env 자체를 안 넣는다.** 소비하는 쪽이 셋이라
        // (claude shim 의 `--append-system-prompt`, codex 의 `AGENTS.md`, agy 의 agent
        // 파일) 각자 게이트를 달면 언젠가 한 곳이 빠진다 — 실제로 codex 쪽엔 없었고,
        // 그 래퍼는 정적 문자열이라 Rust 값을 박을 자리도 없다. 근원에서 한 번 막는다.
        // 캐릭터 이름·색·그림은 그대로다 — 토글의 뜻은 「말투만 끄기」다.
        if socket::read_claude_persona() {
            if let Some(p) = kasa_mcp::character::persona_for(&chars, &name)
                .or_else(|| kasa_mcp::character::persona_for_any(&name))
            {
                env.push(("KASATERM_PERSONA".to_string(), p));
            }
        }
        // 학생별 모델·실행 통로 — claude shim 이 전역 노브보다 이것을 먼저 본다
        // (2026-08-24 지시: 학생 한 명당 모델 선택). shim 은 부팅 1회 생성이라
        // 학생마다 다른 값을 구워 넣을 수가 없다. env 로 내려서 shim 안에서
        // 참조하는 것이 유일한 길이고, 그래서 이 자리가 정본이다.
        if let Some(m) = kasa_mcp::character::model_for(&chars, &name) {
            env.push(("KASATERM_MODEL".to_string(), m));
        }
        if let Some(b) = kasa_mcp::character::backend_for(&chars, &name) {
            env.push(("KASATERM_BACKEND".to_string(), b));
        }
        env
    }

    /// 지금 어딘가에 등록돼 있는 pane 번호 전부.
    ///
    /// pane 을 담는 곳이 셋이라 셋을 다 봐야 한다. `self.pty` 만 보면 PTY 없이
    /// `ws.panes` 에만 사는 미리보기·마크다운 pane 을 덮어쓰고, `ws.panes` 만 보면
    /// split 직후 아직 `PaneState` 가 없는 leaf 를 덮어쓴다(희소 저장이라 보조탭이
    /// 생기기 전까지 없다). 레이아웃 트리는 비활성 창(방 별도창)까지 훑는다.
    pub(crate) fn used_pane_ids(&self) -> std::collections::HashSet<String> {
        let mut used: std::collections::HashSet<String> = self.pty.keys().cloned().collect();
        used.extend(self.ws.lock().unwrap().panes.keys().cloned());
        for l in self.windows.iter().flatten().chain(self.pty_layout.as_ref()) {
            used.extend(l.leaves().into_iter().map(str::to_string));
        }
        // 되살리기 목록에서 **아직 도는 것**의 번호도 쓰는 중이다. 레코드는 pane
        // 번호로 프로세스를 가리키는데, 그 번호를 새 pane 이 물려받으면 레코드가
        // 정리될 때(개수 상한·15분 idle·인포의 ×) 남의 살아 있는 셸을 끈다 —
        // 2026-08-24 에 거노가 두 번 목격한 「검은 빈칸」이 그것이다.
        //
        // `alive` 만 세는 것이 요점이다. 이미 죽은 레코드는 정리해도 아무것도 안
        // 놓으므로(세 정리 경로가 모두 `c.alive` 로 거른다) 번호를 잡을 이유가
        // 없고, 잡으면 닫은 번호를 되쓰는 성질이 죽어 하루 쓰면 `%116` 이 된다.
        // 위험한 건 「살아 있다고 적혔는데 실은 죽은」 레코드뿐인데, 그건 여기
        // 걸린다.
        used.extend(
            self.closed_panes
                .iter()
                .filter(|c| c.alive)
                .map(|c| c.pane_id.clone()),
        );
        used
    }
    /// 지금 안 쓰는 **가장 작은** pane 번호. 예전엔 단조 증가 카운터라 열고 닫기를
    /// 반복한 하루치가 `%116` 같은 번호로 쌓였다 — 학생 이름(`아루-p116`)에도 붙고
    /// `tell`·`dismiss` 로 부를 때마다 그걸 봐야 했다(거노: "pane 번호는 계속 늘어난다").
    ///
    /// 번호 재사용이 위험했던 자리는 collab 마커다: 닫힌 pane 의 `kasaterm-bound-_N` 이
    /// 남은 채 같은 번호가 다시 나면 죽은 세션이 산 것처럼 붙는다. 그래서 닫을 때
    /// [`Self::cleanup_collab_markers`] 가 지우고, 앱이 죽어 그 경로를 못 탄 잔재는
    /// 부팅 sweep(`character::sweep_stale_markers`)이 걷는다.
    pub(crate) fn alloc_pane_id(&mut self) -> String {
        next_free_pane_id(&self.used_pane_ids())
    }

    /// Spawn the first shell pane for the *current* (already-cleared) session.
    /// Mirrors start_pty's pane bring-up with a fresh pane id and no socket
    /// (re)init — used by new_session.
    pub(crate) fn spawn_session_pane(&mut self) -> Result<()> {
        let (cols, rows) = self.window_cells();
        let cwd = resolve_initial_cwd();
        let id = self.alloc_pane_id();
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
        self.insert_pty(id.clone(), session.clone());
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
                        if let Some(cur) = cur.filter(|c| !c.is_empty()) {
                            let _ = kasa_mcp::character::bind_session_character(sid, &cur);
                        } else if let Some(chars) = kasa_mcp::character::characters_json() {
                            // Windows에서는 `ps eww`로 스폰 시점의 환경변수를 복구할 수
                            // 없으므로, 캐릭터 없이 복원된 pane은 SessionStart에서 보충한다.
                            let members = kasa_mcp::character::assignable_names(&chars);
                            let taken: std::collections::HashSet<String> = self
                                .ws
                                .lock()
                                .unwrap()
                                .pane_character
                                .values()
                                .cloned()
                                .collect();
                            let free: Vec<String> = members
                                .iter()
                                .filter(|name| !taken.contains(name.as_str()))
                                .cloned()
                                .collect();
                            if let Some(name) = kasa_mcp::character::pick_random(&free, sid)
                                .or_else(|| kasa_mcp::character::pick_random(&members, sid))
                            {
                                self.relabel_pane(pane, &name);
                                let _ = kasa_mcp::character::bind_session_character(sid, &name);
                            }
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
    /// 이 pane 의 다음 claude 가 쓸 정체성을 파일로 남긴다(학생 명령과 같은 규약).
    /// spawn 때 지워지므로 새 pane 에는 안 따라간다.
    ///
    /// 「말투」 토글이 꺼져 있으면 **빈 파일**을 쓴다 — 파일이 아예 없으면 shim 이
    /// spawn 때의 env 로 되돌아가 옛 말투가 되살아난다. 빈 내용은 「말투 없음」이라는
    /// 명시적 뜻이다.

    pub(crate) fn repersona_pane(&mut self, pane: &str, character: &str) {
        if !self.ws.lock().unwrap().panes.contains_key(pane) {
            return;
        }
        // 로스터 밖 이름 가드 — 엔드포인트로 들어오는 자유 문자열이 헤더/마커를
        // 오염하지 않게. 활성만 보면 진행 중 pane 을 다른 테마 학생으로 바꾸는
        // 기능(2026-08-24 지시)이 죽으므로, 아는 명부의 합집합(활성∪번들∪설치
        // 테마)으로 본다 — 렌더 쪽 이름 조회와 같은 경계다.
        if crate::theme::character_slug_any(character).is_none() {
            eprintln!("[repersona] unknown character '{character}' — ignored");
            return;
        }
        self.ws.lock().unwrap().pane_character.insert(pane.to_string(), character.to_string());
        if let Some(cwd) = self.pane_cwd_cache.get(pane).cloned() {
            let room = self.ws.lock().unwrap().pane_room.get(pane).cloned();
            let rslug = kasa_mcp::character::rslug(&cwd, room.as_deref());
            let _ = kasa_mcp::character::write_marker(&rslug, pane, character);
        }
        // --resume 가 같은 캐릭터로 돌아오게 세션 바인딩도 갱신 — pane 이 물고 있는 sid 를
        // **모두** 맞춘다. spawn anchor(pane_session_id)와 claude transcript stem
        // (pane_claude_sid)은 다를 수 있는데(claude 가 자기 세션 id 를 새로 발급), info
        // 그림과 persona 재주입은 **stem** 을 읽는다(chrome.rs display_tab_char·http.rs
        // /persona). stem 을 빼먹으면 재배정해도 옛 테마 캐릭터의 얼굴·말투가 남는다
        // (거노 실측: 배정은 히후미인데 info·말투는 고블린).
        for sid in [self.pane_session_id.get(pane), self.pane_claude_sid.get(pane)]
            .into_iter()
            .flatten()
        {
            let _ = kasa_mcp::character::bind_session_character(sid, character);
        }
        // 말투도 새 캐릭터 것으로 — **다음에 이 pane 에서 claude 가 뜰 때부터**.
        //
        // 도는 프로세스의 시스템 프롬프트는 못 바꾸므로 지금 대화 중인 상대는 옛
        // 말투 그대로다(그걸 바꾸려면 대화를 끊어야 한다 — 그게 「캐릭터 교체」다).
        // 그런데 바인딩만 갱신하면 **다시 띄워도 옛 말투로 뜬다**: shim 이 spawn 때
        // 고정된 `KASATERM_PERSONA`(옛 캐릭터)로 `--append-system-prompt` 를 붙이고,
        // SessionStart 훅은 그 인자가 보이면 자기 주입을 건너뛰기 때문이다. 학생
        // 명령(`시로코`)이 쓰는 override 파일에 새 말투를 남겨 그 사슬을 끊는다.
        write_persona_override(pane, character);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// 학생 교체 진입점 — 말투가 켜져 있으면 **다시 띄울지 먼저 묻는다**.
    ///
    /// 이름·얼굴·색은 `repersona_pane` 만으로 즉시 바뀌지만 말투는 그렇지 않다.
    /// 그래서 말투가 켜져 있고 되띄울 에이전트가 실제로 도는 pane 일 때만 카드를
    /// 띄운다 — 「말투 오프돼있으면 그냥 껍데기만바뀌게」(2026-08-25 지시). 셸만
    /// 있는 pane 도 같다: 되띄울 대화가 없으니 물을 것이 없다.
    pub(crate) fn ask_or_repersona(&mut self, pane: &str, name: &str) {
        let agent = self.pty.get(pane).and_then(|p| p.active_agent());
        let agent = agent.as_ref().map(|a| a.as_str());
        // `restart_pane_agent` 와 **같은 규칙으로** 미리 잰다. 여기서 다르게 재면
        // 카드가 「대화는 그대로」라고 약속해 놓고 실제로는 새로 시작한다.
        let has_convo = self.pane_claude_sid.get(pane).is_some_and(|s| {
            if agent == Some("codex") {
                socket::codex_rollout_for_session(s).is_some()
            } else {
                socket::transcript_path_for_session(s).is_some()
            }
        });
        let resumable = match plan_character_swap(socket::read_claude_persona(), agent, has_convo) {
            SwapPlan::Now => {
                self.repersona_pane(pane, name);
                self.set_toast(format!("{pane} → {name}"));
                return;
            }
            SwapPlan::Ask { resumable } => resumable,
        };
        self.character_swap_confirm = Some(PendingCharacterSwap {
            pane: pane.to_string(),
            to: name.to_string(),
            resumable,
            rects: Vec::new(),
        });
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// 카드에서 고른 결과. 취소는 **아무 일도 안 한다**.
    pub(crate) fn character_swap_pick(&mut self, btn: CharacterSwapBtn) {
        let Some(p) = self.character_swap_confirm.take() else { return };
        if btn != CharacterSwapBtn::Cancel {
            // 어느 쪽이든 마커·바인딩·말투 파일이 먼저 새 학생으로 서야 한다 —
            // 되띄우기가 `assign_character_env` 로 env 를 다시 세울 때 그것을 읽는다.
            self.repersona_pane(&p.pane, &p.to);
        }
        let msg = match btn {
            CharacterSwapBtn::Cancel => None,
            CharacterSwapBtn::ShellOnly => {
                Some(format!("{} → {} · 말투는 다음에 띄울 때부터", p.pane, p.to))
            }
            CharacterSwapBtn::Relaunch => Some({
                // **되띄우기가 캐릭터를 다시 고르지 못하게 못 박는다.** 그 경로는
                // `assign_character_env` 로 env 를 새로 세우는데, 그 함수는 고른
                // 명단(`assignable_names`)에서 뽑으므로 **명단 밖 학생으로 바꾼
                // 경우 방금 지정한 이름이 그 자리에서 다른 학생으로 갈아치워진다**
                // (2026-08-26 지시: 「다른거로 바꿔도 테마 적용돼있으면 다른테마
                // 캐릭터로 안바뀌어」). pending 은 그 선택보다 우선한다.
                self.pending_character = Some(p.to.clone());
                let ok = self.restart_pane_agent(&p.pane);
                // 되띄우기가 실패하면 pending 이 남아 **다음에 뜨는 엉뚱한 pane** 이
                // 그 학생을 물고 간다. 쓰였든 아니든 여기서 걷는다.
                self.pending_character = None;
                if ok {
                    format!("{} → {} · 대화를 이어서 다시 띄웠어요", p.pane, p.to)
                } else {
                    // 되띄우기가 조용히 실패하면 사용자는 말투까지 바뀐 줄 안다.
                    format!("{} → {} · 다시 띄우지 못해 말투는 다음부터", p.pane, p.to)
                }
            }),
        };
        if let Some(m) = msg {
            self.set_toast(m);
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
                self.insert_pty(pane.to_string(), sess.clone());
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
    /// 도는 에이전트를 **같은 pane 자리에서** 새 계정으로 다시 띄운다.
    ///
    /// 계정은 `CLAUDE_SECURESTORAGE_CONFIG_DIR` = 프로세스 env 라 pane 이 뜰 때 박히고,
    /// 도는 프로세스의 env 는 누구도 못 바꾼다. 그래서 계정을 전환해도 이미 열려 있는
    /// pane 은 옛 계정으로 계속 돌았다(거노 2026-08-13: "전환하면 인포랑 하단은 바뀌는데
    /// pane안에 세션이 인식못하나봐"). Orca 도 같은 한계를 재시작으로 푼다
    /// (`CodexRestartChip` → `queueCodexPaneRestarts`) — 자동 승계는 저쪽에도 없다.
    ///
    /// `swap_character` 와 같은 골격이다: 같은 pane id 로 PTY 를 갈아끼우고 셸 프롬프트가
    /// 뜰 즈음 명령을 주입한다. 다른 점은 주입하는 명령뿐 — 캐릭터는 그대로 두고
    /// `restore_agent_command` 로 **하던 대화를 이어서** 띄운다(claude·codex·agy 각각).
    ///
    /// 대화 파일이 없으면 resume 을 걸지 않는다. 그 상태로 `--resume` 하면 claude 가
    /// "No conversation found" 를 뱉고 빈 셸만 남아 학생 pane 이 통째 죽는다 — 대화를
    /// 잃더라도 fresh 로 띄우는 편이 낫다(restore_leaf 와 같은 판단).
    ///
    /// 반환값은 「정말 다시 띄웠나」다. false 면 부르는 쪽이 알림을 되돌려야 한다 —
    /// 재시작이 조용히 실패하면 사용자는 옛 계정으로 도는 pane 을 새 계정이라 믿는다.
    pub(crate) fn restart_pane_agent(&mut self, pane: &str) -> bool {
        // 지금 그 pane 에서 **실제로 도는** 하네스. 셸만 있는 pane 은 되띄울 것이 없다.
        let Some(agent) = self.pty.get(pane).and_then(|p| p.active_agent()) else {
            return false;
        };
        let agent = agent.as_str();
        let sid = self.pane_claude_sid.get(pane).cloned();
        let resumable = sid.as_deref().is_some_and(|s| {
            if agent == "codex" {
                socket::codex_rollout_for_session(s).is_some()
            } else {
                socket::transcript_path_for_session(s).is_some()
            }
        });
        let cwd = self.pane_cwd_cache.get(pane).map(|p| p.to_string_lossy().into_owned());
        let room = self.ws.lock().unwrap().pane_room.get(pane).cloned();
        // 끄기 직전 이 pane 이 쓰던 모델·effort — 되띄울 때 그대로 잇는다. 안 실으면
        // shim 기본 모델로 떨어져 사용자가 /model·/effort 를 다시 쳐야 했다(2026-08-16
        // 「모델이랑 에포트도 안됐었어」). 세션 저장이 leaf 에 싣는 것과 같은 스냅샷
        // 이고, 옛 PTY 를 지우기 전에 떠야 값이 남아 있다.
        let (model, effort) =
            self.agent_cfg_snapshot().get(pane).cloned().unwrap_or_default();
        let (cols, rows) = self.window_cells();
        // 옛 PTY 종료 — 여기서 옛 계정 토큰을 문 프로세스가 사라진다. pump 스레드는
        // EOF 로 빠진다.
        self.pty.remove(pane);
        let mut env = crate::proxy_env(pane);
        if let Some(ref r) = room {
            env.push(("KASATERM_ROOM".to_string(), r.clone()));
        }
        // 캐릭터는 유지한다 — 계정이 바뀌었다고 학생이 바뀔 이유가 없다.
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
                self.insert_pty(pane.to_string(), sess.clone());
                // 옛 PTY 의 EOF 가 이 id 를 dead_panes 에 넣었을 수 있다 — 같은 id 로
                // 되띄웠으니 지운다(swap_character 와 같은 이유).
                self.dead_panes.lock().unwrap().retain(|x| x != pane);
                // 빈 값 거르기는 restore_agent_command 몫이다(빈 문자열이면 플래그를
                // 아예 안 붙인다). 명령 끝 '\r' 도 거기서 이미 붙는다.
                let cmd = restore_agent_command(
                    Some(agent),
                    sid.as_deref(),
                    resumable,
                    Some(model.as_str()),
                    Some(effort.as_str()),
                );
                let at = std::time::Instant::now() + std::time::Duration::from_millis(900);
                self.pending_restores.push((sess, cmd, at));
                self.resize_backend(cols, rows);
                self.publish_pty_layout();
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
                true
            }
            Err(e) => {
                eprintln!("[restart_pane_agent] respawn failed: {e:#}");
                false
            }
        }
    }

    /// 그 pane 의 claude 가 **실제로 어느 계정으로 떠 있는지** 실측한다 — 프로세스
    /// env 의 자격증명 저장소 경로를 읽는다. `Some("")` = 기본 로그인으로 떠 있음,
    /// `None` = 실측 실패(claude 가 없거나 `ps` 가 안 되는 플랫폼).
    ///
    /// 전환 이벤트를 기록해 두고 추정하는 방식은 앱이 못 본 전환(재시작 전의 전환,
    /// 손으로 띄운 claude)에서 어긋난다 — 도는 프로세스의 env 가 유일한 진실이다.
    pub(crate) fn pane_claude_boot_account_dir(&self, pane: &str) -> Option<String> {
        let shell = self.pty.get(pane)?.shell_pid()?;
        let (kind, pid) =
            kasa_pty::agent_pid_for_shell(&kasa_pty::process_table_shared(), shell)?;
        if kind != kasa_pty::AgentKind::Claude {
            return None;
        }
        // `ps eww` 는 유닉스 전용이고 같은 사용자 소유 프로세스의 env 를 보여준다.
        // Windows 에서는 실패해 None — 호출부가 "어긋났을 수 있음" 으로 보수 처리한다.
        let out = crate::proc::command("ps")
            .args(["eww", "-o", "command=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        if !out.status.success() || out.stdout.is_empty() {
            return None;
        }
        Some(parse_securestorage_dir(&String::from_utf8_lossy(&out.stdout)))
    }

    /// 계정 id → 화면 이름. 자동 전환 토스트가 쓰던 규칙 그대로다 — 이름 없는
    /// 슬롯은 「계정 N」, 목록에 없는 id(기본 로그인 포함)는 기본 계정 표기.
    pub(crate) fn claude_account_display(&self, id: &str) -> String {
        match self.set_claude_accounts.iter().position(|a| a.id == id) {
            Some(i) => crate::settings::account_display(
                id,
                &self.set_claude_accounts[i].label,
                &format!("계정 {}", i + 2),
            ),
            None => crate::settings::account_display("", "", "기본 계정"),
        }
    }

    /// 계정 전환을 **떠 있는 pane 에까지** 적용한다 — 자동·수동 전환이 같은 꼬리를 탄다.
    ///
    /// 도는 프로세스의 env 는 못 바꾸므로(`restart_pane_agent` doc) 반영 수단은
    /// 재시작뿐이다. 전에는 자동 전환이 「⟳ 재시작」 칩만 띄우고 수동 전환은 그마저
    /// 없어서, 전환해 놓고 pane 안 /status 가 옛 계정인 것을 보고 "바로 안 된다"가
    /// 됐다(거노 2026-08-15: "재시작칩없이 나도 그렇게 되게해줘"). 이제 쉬는 pane 은
    /// 그 자리에서 대화를 이어 재시작하고, 일하는 중인 pane 은 칩을 단 채 남겼다가
    /// 턴이 끝나면 틱(`run_pending_account_restarts`)이 마저 돌린다 — 일하는 학생을
    /// 중간에 끊으면 진행 중이던 턴이 통째로 죽기 때문이다.
    ///
    /// 대상 판정은 전환 이벤트 추정이 아니라 **pane 별 실측**이다. 그래서 같은 계정을
    /// 다시 눌러도 「어긋난 pane 만」 맞춰 띄운다 — 앱이 못 본 전환으로 이미 어긋나
    /// 있던 pane 도 계정 버튼 한 번으로 수습된다.
    ///
    /// 반환: (전환 전 계정 이름, 새 계정 이름, 즉시 재시작한 수, 끝나길 기다리는 수,
    /// 보는 pane 이 대기 중인가, 작업대 갈아 끼우기 성공 여부).
    /// `character-pick` — 캐릭터 하나를 명단에 넣거나 뺀다.
    ///
    /// 반환은 「반영됐는가」 — 저장 뒤 파일을 다시 읽어 확인한다. 토글이라 「눌렀다」
    /// 만으로는 알 수 없다는 기존 규칙 그대로다.
    pub(crate) fn apply_character_pick(
        &mut self,
        theme: &str,
        name: Option<&str>,
        on: bool,
    ) -> Result<bool, String> {
        let name = name.map(str::trim).unwrap_or_default();
        if theme.is_empty() || name.is_empty() {
            return Err(crate::settings::reject(
                "character_pick_bad_id",
                "어느 테마의 누구인지 알 수 없어요".to_string(),
            ));
        }
        // 켜기는 그 테마에 실재하는 이름만 받는다. 안 막으면 오타 하나가 설정
        // 파일에 그대로 눌러앉는데, 배정 쪽은 유령을 조용히 걸러 내므로 **화면에만
        // 한 명 더 켜진 것처럼 보이고 실제로는 안 나오는** 상태가 된다.
        //
        // 끄기는 검사하지 않는다 — 그래야 이미 들어앉은 유령이나 이름이 바뀐
        // 항목을 화면에서 지울 길이 남는다.
        if on {
            let names = theme_roster_names(theme);
            if names.is_empty() {
                return Err(crate::settings::reject(
                    "theme_roster_missing",
                    "그 테마의 명단을 못 읽었어요".to_string(),
                ));
            }
            if !names.iter().any(|n| n == name) {
                return Err(crate::settings::reject_with_args(
                    "character_pick_unknown",
                    serde_json::json!({ "name": name }),
                    format!("{name} 은(는) 그 테마에 없어요"),
                ));
            }
        }
        let mut picks = kasa_mcp::character::all_picks();
        let slot = match picks.iter_mut().find(|(k, _)| k == theme) {
            Some(s) => s,
            None => {
                picks.push((theme.to_string(), Vec::new()));
                picks.last_mut().expect("just pushed")
            }
        };
        // 아무도 안 고른 테마를 처음 건드릴 때 「끄기」로 들어오면, 지금 전원이
        // 후보인 상태에서 한 명만 빼는 뜻이 된다 — 그러려면 나머지를 전부 켜 둬야
        // 한다. 안 그러면 명단이 빈 채로 남아 다시 전원 후보가 되어 아무 일도
        // 안 일어난 것처럼 보인다.
        if !on && slot.1.is_empty() {
            slot.1 = theme_roster_names(theme);
        }
        slot.1.retain(|n| n != name);
        if on && !slot.1.iter().any(|n| n == name) {
            slot.1.push(name.to_string());
        }
        socket::write_character_picks(&picks);
        self.invalidate_character_view();
        Ok(kasa_mcp::character::picks_of_theme(theme).iter().any(|n| n == name) == on)
    }

    /// `theme-pick-all` — 테마 하나를 통째로 켜거나 끈다.
    pub(crate) fn apply_theme_pick_all(&mut self, theme: &str, on: bool) -> Result<bool, String> {
        if theme.is_empty() {
            return Err(crate::settings::reject(
                "character_pick_bad_id",
                "어느 테마인지 알 수 없어요".to_string(),
            ));
        }
        let names = theme_roster_names(theme);
        if names.is_empty() {
            return Err(crate::settings::reject(
                "theme_roster_missing",
                "그 테마의 명단을 못 읽었어요".to_string(),
            ));
        }
        let mut picks = kasa_mcp::character::all_picks();
        picks.retain(|(k, _)| k != theme);
        if on {
            picks.push((theme.to_string(), names));
        }
        // 끄기는 키를 빼는 것으로 끝난다 — 빈 배열은 저장 때 어차피 걷힌다.
        socket::write_character_picks(&picks);
        self.invalidate_character_view();
        Ok(kasa_mcp::character::picks_of_theme(theme).is_empty() != on)
    }

    /// 명단이 바뀐 뒤 화면 쪽 캐시를 걷는다. 배정 캐시는 `write_character_picks` 가
    /// 이미 비웠다 — 여기는 그림·색·테마 카드 몫이라 **짝으로** 불러야 한다.
    fn invalidate_character_view(&mut self) {
        crate::theme::invalidate_roster();
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// 확인 없이 지금 바꾼다. 옛 `SettingsAction::ClaudeAccount` 팔의 본문 그대로다.
    pub(crate) fn claude_account_switch_now(&mut self, id: &str) {
        self.settings_input = None;
        let same = id == self.set_claude_account;
        // 바뀐 자리를 반짝여 준다 — 우상단 토스트만으로는 정작 계정 칩이 아무 변화
        // 없이 그대로라 「바뀐 줄 모르겠다」가 된다. 같은 계정을 다시 누른 경우엔
        // 켜지 않는다(아무것도 안 바뀌었는데 축포를 터뜨리는 꼴이다).
        if !same {
            self.account_flash = Some(std::time::Instant::now());
        }
        let (_, to_label, restarted, deferred, focused, live) =
            self.apply_claude_account_switch(id);
        self.set_toast(crate::session::account_switch_toast(
            &to_label, same, restarted, deferred, focused, live,
        ));
    }

    /// 다시 뜰 pane 이 있으면 물어보고, 없으면 지금 바꾼다.
    ///
    /// 영향이 0이면 **옛 경로와 완전히 같다** — 작업대로 뜬 pane 은 재시작이 필요
    /// 없고 갈아 끼우기만으로 다음 요청부터 새 계정이 되므로, 그게 보통의 경우다.
    pub(crate) fn ask_or_switch_claude_account(&mut self, to: &str, surface: ConfirmSurface) {
        let impact = self.preview_claude_account_switch(to);
        if !impact.needs_confirm() {
            self.claude_account_switch_now(to);
            return;
        }
        // 그릴 창이 없으면(헤드리스·단축키가 설정창 없이 부른 경우) 메인으로 접는다 —
        // 안 그러면 취소할 손도 없는 확인이 뜬다.
        let has_settings_window = self
            .aux_windows
            .iter()
            .any(|a| matches!(a.kind, crate::auxwin::AuxWindowKind::Settings));
        let surface = match surface {
            ConfirmSurface::Settings if !has_settings_window => ConfirmSurface::Main,
            s => s,
        };
        self.account_switch_confirm = Some(PendingAccountSwitch {
            to_label: self.claude_account_display(to),
            to: to.to_string(),
            impact,
            surface,
            rects: Vec::new(),
        });
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// 확인 카드에서 고른 결과. 취소는 **아무 일도 안 한다**.
    pub(crate) fn account_switch_pick(&mut self, btn: AccountSwitchBtn) {
        let Some(p) = self.account_switch_confirm.take() else { return };
        if btn == AccountSwitchBtn::Switch {
            self.claude_account_switch_now(&p.to);
        }
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// 전환 **뒤** 목표 저장소 경로. `apply` 안에 인라인돼 있던 계산을 들어낸 것이라
    /// 동작은 그대로다.
    pub(crate) fn claude_target_dir(&self, to: &str) -> String {
        crate::claude_auth::runtime_dir_for(to, to)
            .or_else(|| socket::claude_account_dir(to))
            .map_or(String::new(), |p| p.to_string_lossy().into_owned())
    }

    /// 전환 **전** 예측 — `swap_active` 를 안 돌리고 같은 답을 낸다(`predicted_target_dir`).
    pub(crate) fn predict_claude_target_dir(&self, to: &str) -> String {
        predicted_target_dir(
            to,
            crate::claude_auth::active_dir().is_some(),
            crate::claude_auth::vault_ready(to, socket::claude_account_dir),
            socket::claude_account_dir(to).as_deref(),
        )
    }

    /// 지금 떠 있는 claude pane 들의 판정 재료. `ps` 를 pane 마다 한 번 도므로
    /// **클릭에서만** 부른다(프레임마다 부르면 안 된다).
    pub(crate) fn claude_pane_facts(&mut self) -> Vec<PaneAccountFact> {
        let focused = self.ws.lock().unwrap().active_pane.clone();
        let ids: Vec<String> = self
            .pty
            .iter()
            .filter(|(_, p)| p.active_agent() == Some(kasa_pty::AgentKind::Claude))
            .map(|(id, _)| id.clone())
            .collect();
        ids.into_iter()
            .map(|id| {
                let boot_dir = self.pane_claude_boot_account_dir(&id);
                let resumable = self
                    .pane_claude_sid
                    .get(&id)
                    .is_some_and(|s| socket::transcript_path_for_session(s).is_some());
                PaneAccountFact {
                    focused: focused.as_deref() == Some(id.as_str()),
                    closed: self.stashed_record(&id).is_some(),
                    busy: account_restart_busy(
                        self.pane_prompt_wait.contains_key(&id),
                        self.pane_activity.get(&id).map(|a| (a.status.as_str(), a.bg_active)),
                    ),
                    boot_dir,
                    resumable,
                    id,
                }
            })
            .collect()
    }

    /// 「이 전환을 누르면 무슨 일이 일어나나」 — 아무것도 바꾸지 않고 세기만 한다.
    pub(crate) fn preview_claude_account_switch(&mut self, to: &str) -> AccountSwitchImpact {
        let target = self.predict_claude_target_dir(to);
        let facts = self.claude_pane_facts();
        account_switch_impact(&facts, &target)
    }

    pub(crate) fn apply_claude_account_switch(
        &mut self,
        to: &str,
    ) -> (String, String, usize, usize, bool, bool) {
        let from_label = self.claude_account_display(&self.set_claude_account.clone());
        let to_label = self.claude_account_display(to);
        // ① 작업대를 새 계정으로 갈아 끼운다. 이것만으로 **작업대를 보고 도는 pane 은
        // 전부** 다음 요청부터 새 계정이 된다 — 재시작도, 대화 끊김도 없다. claude 가
        // 요청 직전마다 저장소를 다시 읽고, 자기 것과 다른 토큰이 있으면 그대로
        // 채택하기 때문이다(claude_auth 모듈 머리말).
        let swapped = crate::claude_auth::swap_active(to, socket::claude_account_dir);
        // /status 가 보여주는 신원 캐시(~/.claude.json oauthAccount)도 함께 갈아
        // 끼운다 — 저장소만 바꾸면 과금은 새 계정인데 /status 는 옛말을 한다.
        // AlreadyActive 여도 부른다: 캐시는 다른 로그인(밖에서 친 claude /login)이
        // 언제든 덮을 수 있어, 「맞추기」 클릭이 그걸 바로잡는 손이 된다.
        if !matches!(
            swapped,
            crate::claude_auth::SwapOutcome::VaultEmpty
                | crate::claude_auth::SwapOutcome::WriteFailed
        ) {
            crate::claude_auth::adopt_oauth_account_cache(
                crate::mcp_panel_port(),
                socket::claude_account_dir(to),
            );
        }
        // ② 재시작이 필요한 pane 은 **작업대를 안 보는** 것들뿐이다 — 이 기능이 생기기
        // 전에 뜬 pane 은 특정 금고에 못 박혀 있어 갈아 끼우기가 안 닿는다.
        let target_dir = self.claude_target_dir(to);
        // 칩을 달지 말지는 **확인 카드가 세는 것과 같은 함수**로 가른다 — 두 곳에
        // 따로 판정하면 「3개가 다시 떠요」라고 물어 놓고 5개가 뜨는 일이 생긴다.
        for f in self.claude_pane_facts() {
            if pane_account_fate(&f, &target_dir) == PaneAccountFate::Unchanged {
                // 이미 목표 계정으로 도는 pane — 남아 있던 표시도 걷는다(A→B→A 복귀).
                self.pane_account_stale.remove(&f.id);
                continue;
            }
            // 칩의 「A → B」 표기. 실측이 안 되면(다른 플랫폼) 어긋났을 수 있다는
            // 쪽으로 보수 판정하고, 표기는 전환 전 활성 계정으로 쓴다.
            let boot_label = match f.boot_dir.as_deref() {
                Some(d) => self.claude_account_display(account_id_of_dir(d)),
                None => from_label.clone(),
            };
            self.pane_account_stale.insert(f.id, (boot_label, to_label.clone()));
        }
        if matches!(swapped, crate::claude_auth::SwapOutcome::WriteFailed) {
            // 조용히 넘어가면 「바꿨는데 안 바뀐다」가 된다. 재시작 폴백은 그대로
            // 도니 기능은 살지만, 왜 느린지는 로그에 남겨 둔다.
            eprintln!("[account] 작업대 갈아 끼우기 실패 — 재시작 폴백으로만 반영된다");
        }
        self.set_claude_account = to.to_string();
        // shim 재굽기가 재시작보다 **먼저**여야 새로 뜨는 claude 가 새 계정을 탄다.
        self.settings_save();
        let restarted = self.run_pending_account_restarts();
        let deferred = self.pane_account_stale.len();
        let focused_pending = self
            .ws
            .lock()
            .unwrap()
            .active_pane
            .as_ref()
            .is_some_and(|p| self.pane_account_stale.contains_key(p));
        let live = !matches!(
            swapped,
            crate::claude_auth::SwapOutcome::VaultEmpty
                | crate::claude_auth::SwapOutcome::WriteFailed
        );
        (from_label, to_label, restarted, deferred, focused_pending, live)
    }

    /// 「⟳ 재시작」 표시가 남은 pane 중 지금 쉬는 것을 새 계정으로 되띄운다.
    /// 300ms 활동 스캔 끝에 불려, 전환 때 일하던 pane 도 턴이 끝나는 대로 따라온다.
    /// 반환은 이번에 되띄운 수.
    pub(crate) fn run_pending_account_restarts(&mut self) -> usize {
        if self.pane_account_stale.is_empty() {
            self.pane_account_quiet_since.clear();
            return 0;
        }
        // 전환 판정과 같은 규칙 — 활성 계정이 작업대에 실려 있으면 목표는 빈 경로
        // (기본 자리)다. 금고 경로와 비교하면 이미 맞게 뜬 pane 을 또 되띄운다.
        let target_dir = crate::claude_auth::runtime_dir_for(
            &self.set_claude_account,
            &self.set_claude_account,
        )
        .map_or(String::new(), |p| p.to_string_lossy().into_owned());
        // 지금 사용자가 보고 있는 pane 은 자동으로 안 끊는다. 화면 밖 pane 이 조용히
        // 갈리는 것과, 대화하던 상대가 눈앞에서 사라지는 것은 전혀 다른 일이다
        // (2026-08-15 "하다가 계정전환하니까 너가 없어졌어"). 표시는 남으므로 헤더
        // 칩을 누르면 그때 갈린다 — 그 pane 만은 사용자가 시점을 고른다.
        let focused = self.ws.lock().unwrap().active_pane.clone();
        let pending: Vec<String> = self.pane_account_stale.keys().cloned().collect();
        let now = std::time::Instant::now();
        let mut restarted = 0usize;
        for id in pending {
            if focused.as_deref() == Some(id.as_str()) {
                self.pane_account_quiet_since.remove(&id);
                continue;
            }
            // 하네스가 이미 내려간 pane 은 계정 불일치도 없다 — 표시만 걷는다.
            let is_claude = self
                .pty
                .get(&id)
                .is_some_and(|p| p.active_agent() == Some(kasa_pty::AgentKind::Claude));
            if !is_claude {
                self.pane_account_stale.remove(&id);
                continue;
            }
            // 닫힌 pane 은 사용자 눈 밖에서 되띄우지 않는다 — 표시를 남겨 두면
            // 되살렸을 때 칩이 안내한다.
            if self.stashed_record(&id).is_some() {
                continue;
            }
            // 일하는 중·승인 대기·백그라운드 작업 중이면 끊지 않는다 — resume 은
            // 대화를 잇지만 진행 중이던 턴은 죽는다. 활동 기록이 아직 없는 pane 도
            // 다음 틱(300ms)까지 미룬다.
            let busy = account_restart_busy(
                self.pane_prompt_wait.contains_key(&id),
                self.pane_activity.get(&id).map(|a| (a.status.as_str(), a.bg_active)),
            );
            if busy {
                self.pane_account_quiet_since.remove(&id);
                continue;
            }
            // 조용해진 지 얼마나 됐나. 스피너가 도구 결과 사이에서 한 틱 사라지는
            // 틈은 이 문턱을 못 넘는다 — 그 틈에 끊으면 하던 턴이 통째로 죽는다.
            let quiet_since = *self.pane_account_quiet_since.entry(id.clone()).or_insert(now);
            if now.duration_since(quiet_since) < Self::ACCOUNT_RESTART_QUIET {
                continue;
            }
            // 그 사이 손으로 되띄웠을 수 있다 — 죽이기 직전 실측으로 한 번 더 확인.
            if self
                .pane_claude_boot_account_dir(&id)
                .is_some_and(|d| d == target_dir)
            {
                self.pane_account_stale.remove(&id);
                continue;
            }
            if self.restart_pane_agent(&id) {
                self.pane_account_stale.remove(&id);
                self.pane_account_quiet_since.remove(&id);
                restarted += 1;
            }
        }
        // 표시가 걷힌 pane 의 조용 기록은 남길 이유가 없다.
        self.pane_account_quiet_since
            .retain(|k, _| self.pane_account_stale.contains_key(k));
        restarted
    }

    /// 되띄우기 전에 요구하는 **연속 조용 시간**. 화면 판독은 300ms 박자라 이 값이면
    /// 열 번 넘게 연속으로 조용한 것을 본 셈이다. 짧으면 턴 중간의 깜빡임에 걸리고,
    /// 길면 전환이 굼떠 보인다.
    const ACCOUNT_RESTART_QUIET: std::time::Duration = std::time::Duration::from_secs(4);

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
        // 탭 pid 는 BSP leaf 가 아니다 — 화면을 든 건 그 탭이 사는 바깥 pane 이다.
        // 접지 않으면 「그 surface 가 어느 창에 있나」가 탭에 대해 항상 None 이 되고,
        // 그걸 존재 판정으로 쓰는 소켓 split 이 「없는 pane」이라며 거절했다. 그래서
        // 학생들이 split 을 포기하고 탭으로 우회했다(거노 2026-08-07: "갑자기 애들
        // 왜 탭안에 생성하지"). 접는 규칙은 `outer_for_pty` 한 곳에만 둔다.
        let pane = self
            .ws
            .lock()
            .ok()
            .and_then(|w| w.outer_for_pty(pane))
            .unwrap_or_else(|| pane.to_string());
        (0..self.windows.len()).find(|&i| {
            let layout = if i == self.active_window {
                self.pty_layout.as_ref()
            } else {
                self.windows[i].as_ref()
            };
            layout.is_some_and(|l| l.leaves().contains(&pane.as_str()))
        })
    }
    /// PTY 세션을 App 에 넣으면서 전역 레지스트리에도 등록한다.
    ///
    /// 웹 터미널·소켓 백엔드는 GUI 스레드를 거치지 않고 이 레지스트리로 세션에
    /// 붙으므로, **pane 을 만드는 모든 경로는 `self.pty.insert` 대신 이걸 써야**
    /// 한다. 한 곳이라도 빠뜨리면 그 pane 만 조용히 웹에서 안 보이는, 찾기 어려운
    /// 종류의 구멍이 난다 — 그래서 통로를 하나로 묶었다. 레지스트리는 Weak 이라
    /// 해제는 App 이 Arc 를 떨어뜨리는 것으로 저절로 된다.
    pub(crate) fn insert_pty(&mut self, id: String, sess: std::sync::Arc<kasa_pty::PtySession>) {
        kasa_pty::register_session(&id, &sess);
        self.pty.insert(id, sess);
    }
    pub(crate) fn switch_window(&mut self, idx: usize) {
        if idx >= self.windows.len() {
            return;
        }
        // 밖에 나가 있는 방은 메인에 그리지 않는다 — 같은 트리를 두 창이 그리면 한쪽
        // 입력이 다른 쪽에 안 비치는 유령 상태가 된다. 대신 그 창을 앞으로 가져온다.
        // (사이드바 탭 클릭도 여기로 오므로, 「밖에 있음」 탭을 누르면 창이 뜬다.)
        if let Some(i) = self.aux_windows.iter().position(|a| a.room_window() == Some(idx)) {
            self.aux_windows[i].window.focus_window();
            return;
        }
        if idx == self.active_window {
            return;
        }
        // 줌은 App 전역 상태인데 pane 은 방(윈도우)마다 다르다 — 줌한 채로 방을
        // 옮기면 그 방에 없는 pane 을 가리킨 유령 줌이 남아, 새 방이 「최대화된
        // 무언가」처럼 보이거나 되돌릴 대상이 없어진다. 방을 옮기는 순간 푼다.
        self.zoomed_pane = None;
        self.windows[self.active_window] = self.pty_layout.take();
        self.pty_layout = self.windows[idx].take();
        self.active_window = idx;
        // 빈 방이면 셸을 하나 띄워 되살린다. 예전엔 여기 오기 전에 `windows[idx]
        // .is_none()` 으로 막았는데, 그러면 그 방은 **활성으로 만들 수 없고 활성이
        // 아니면 닫을 수도 없다** — 사이드바에는 계속 보이는데 눌러도 아무 일이
        // 없었다(거노 2026-08-25 「방을 닫을수도 pane을 닫을수도 없어 복구도
        // 안되고」). 막는 대신 들여보내고 쓸 수 있는 방으로 만든다.
        if self.pty_layout.is_none() {
            if let Err(e) = self.spawn_session_pane() {
                eprintln!("[window] 빈 방 {idx} 되살리기 실패: {e:#}");
            }
        }
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

    /// 방(윈도우) 탭을 끌어 순서를 바꾼다. `from` 을 뽑아 `to` 자리에 꽂는다.
    /// `to` 는 **뽑기 전** 기준의 삽입 슬롯(0..=len)이라, 드래그 중 그리는 삽입선
    /// 위치를 그대로 넘기면 된다.
    ///
    /// 인덱스가 곧 신원인 상태가 넷이다 — 활성 방, 이름 오버라이드, 알림 마킹,
    /// 라벨 캐시. 벡터만 흔들고 이것들을 두면 방을 옮긴 순간 이름이 남의 방에
    /// 붙고 알림 점이 엉뚱한 탭에서 뛰므로, 같은 remap 을 넷 다 통과시킨다.
    /// 세션 저장(`session_state_json`)과 board 의 `window_idx` 는 이 벡터 순서를
    /// 매번 다시 읽으니 저절로 따라온다.
    pub(crate) fn reorder_window(&mut self, from: usize, to: usize) {
        let n = self.windows.len();
        if from >= n || to > n {
            return;
        }
        // 뽑고 난 뒤 기준의 착지 인덱스. 제자리면 옮길 것이 없다.
        let dst = if to > from { to - 1 } else { to };
        if dst == from {
            return;
        }
        // 활성 방의 트리는 슬롯이 아니라 pty_layout 에 있다 — 슬롯만 옮기면 활성
        // 방의 내용이 통째로 빠진다. 제자리에 돌려놓고 옮긴 뒤 새 자리에서 꺼낸다.
        self.windows[self.active_window] = self.pty_layout.take();
        let slot = self.windows.remove(from);
        self.windows.insert(dst, slot);
        let remap = move |i: usize| crate::remap_window_index(i, from, dst);
        self.active_window = remap(self.active_window);
        let overrides = std::mem::take(&mut self.window_name_override);
        self.window_name_override =
            overrides.into_iter().map(|(i, name)| (remap(i), name)).collect();
        let alerts = std::mem::take(&mut self.window_alert);
        self.window_alert = alerts.into_iter().map(remap).collect();
        let expanded = std::mem::take(&mut self.expanded_windows);
        self.expanded_windows = expanded.into_iter().map(remap).collect();
        // 도는 중인 펼침 모션도 방을 인덱스로 가리킨다. 0.16초짜리라 그 안에 방을
        // 끌어 옮기는 일은 드물지만, 인덱스를 키로 쓰는 필드가 **예외 없이** 여기를
        // 지나야 다음 사람이 이 목록을 믿는다.
        self.expand_anim = self.expand_anim.map(|(i, opening, at)| (remap(i), opening, at));
        // 밖에 나가 있는 방도 인덱스로 자기 트리를 찾는다 — 여기서 안 옮기면 재배치
        // 한 번에 별도 창이 남의 방을 그린다.
        // 꺼낸 pane 도 나온 방을 인덱스로 들고 있다 — 안 옮기면 되돌릴 때 남의 방에서
        // 튀어나온다. 인덱스를 키로 쓰는 필드는 예외 없이 이 목록을 지난다.
        for a in self.aux_windows.iter_mut() {
            match &mut a.kind {
                crate::auxwin::AuxWindowKind::Room { window, .. }
                | crate::auxwin::AuxWindowKind::Terminal { window, .. } => {
                    *window = remap(*window);
                }
                _ => {}
            }
        }
        // 되살리기 대기 중인 pane 도 자기 방을 인덱스로 가리킨다 — 안 옮기면 ⌘⇧T 가
        // 엉뚱한 방에서 pane 을 꺼낸다.
        for c in self.closed_panes.iter_mut() {
            c.window = remap(c.window);
        }
        self.pty_layout = self.windows[self.active_window].take();
        // 라벨은 인덱스 병렬 배열이라 캐시를 버려 다음 paint 에 다시 뽑게 한다.
        self.window_labels_at = None;
        self.win_tab_reveal(self.active_window);
        self.session_touched = true;
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// 닫히기 직전의 pane 을 되살릴 수 있게 적어 둔다. `remove_pane` 이 유일한
    /// 호출자다 — ⌘W·헤더 ×·CLI 어느 경로로 닫아도 거길 지나므로 한 곳에서 잡힌다.
    ///
    /// 레코드는 세션 저장이 쓰는 것과 **같은 형식**(`layout_to_json` 의 leaf 본문)이다.
    /// 그래서 되살리기가 `restore_leaf` 재사용이 되고, claude 였던 pane 은 `--resume`
    /// 까지 그 함수가 알아서 딸려 온다.
    ///
    /// `alive` 는 이 pane 의 PTY 가 계속 도는지다 — 사용자가 닫은 것(`hide_pane`)은
    /// 참이라 되살리기가 재부착이 되고, 셸이 스스로 끝난 것(`reap_dead_panes`)은
    /// 거짓이라 레코드로 새로 띄운다.
    /// `stashed` 는 사이드바 「숨기기」로 치운 것 — 두 정리 루프(개수 상한·idle reap)가
    /// 건너뛴다. 닫기(⌘W)는 `false` 로 들어와 종전대로 정리 대상이다.
    pub(crate) fn record_closed_pane(&mut self, pane: &str, alive: bool, stashed: bool) {
        if self.tmux.is_some() || !self.pty.contains_key(pane) {
            return;
        }
        // 되돌릴 자리를 가리킬 닻 — 같은 트리의 이웃 leaf(뒤쪽 우선, 없으면 앞).
        let neighbor = self.pty_layout.as_ref().and_then(|t| {
            let leaves = t.leaves();
            let i = leaves.iter().position(|l| *l == pane)?;
            leaves
                .get(i + 1)
                .or_else(|| leaves.get(i.wrapping_sub(1)))
                .map(|s| s.to_string())
        });
        // ws 락 **밖에서** 먼저 뜬다 — 이 스냅샷은 백엔드의 다른 뮤텍스를 잡으므로,
        // 락 안에서 부르면 두 락의 획득 순서가 뒤엉킬 자리를 만든다.
        let agent_cfg = self.agent_cfg_snapshot();
        let (rec, character) = {
            let ws = self.ws.lock().unwrap();
            let rec = Self::layout_to_json(
                &kasa_pty::PtyLayout::single(pane),
                &self.pty,
                &ws,
                &self.pane_claude_sid,
                &agent_cfg,
            );
            // 되살리기 목록의 학생 이름도 「클로드가 돌던 pane 인가」 관문을 지난다 —
            // 배정은 spawn 때 **모든** pane 에 되므로(`assign_character_env`) 안 걸면
            // 순수 셸을 닫아도 `%7 이로하 · tmuxify` 로 남는다(거노 2026-08-20).
            // 닫는 순간 claude 가 이미 내려갔을 수 있어 바인딩된 세션 id 도 함께 본다 —
            // `count_claude_panes` 가 쓰는 기준과 같다.
            let was_agent = self.pane_claude_sid.contains_key(pane)
                || self.pty.get(pane).and_then(|p| p.active_agent()).is_some();
            let ch = was_agent
                .then(|| ws.pane_character.get(pane).cloned())
                .flatten()
                .unwrap_or_default();
            (rec, ch)
        };
        let Some(rec) = rec.get("leaf").cloned().filter(|r| !r.is_null()) else {
            return;
        };
        // cwd 캐시는 `lsof` 로 채워져 갓 만든 pane 에선 아직 비어 있다 — 그때는
        // 레코드에 실린 cwd 로 되짚는다(복원도 그 값을 쓰므로 어긋날 일이 없다).
        let folder = self
            .pane_view_cwd
            .get(pane)
            .or_else(|| self.pane_cwd_cache.get(pane))
            .map(|p| p.to_string_lossy().into_owned())
            .or_else(|| rec.get("cwd").and_then(|c| c.as_str()).map(|s| s.to_string()))
            .and_then(|s| s.rsplit('/').find(|t| !t.is_empty()).map(|t| t.to_string()))
            .unwrap_or_default();
        let window = self.window_of_pane(pane).unwrap_or(self.active_window);
        self.push_closed_pane(crate::ClosedPane {
            rec,
            pane_id: pane.to_string(),
            character,
            folder,
            neighbor,
            window,
            alive,
            stashed,
            // 놀고 있는지는 다음 활동 스캔이 판정한다 — 닫는 순간의 상태로 못 박으면
            // 마침 응답 중이던 pane 이 곧바로 유휴로 몰린다.
            idle_since: None,
            preview: None,
        });
        self.chrome_dirty = true;
    }

    /// 그 번호를 **지금 물고 있는** 되살리기 레코드. 죽은 레코드는 여기 안 걸린다.
    ///
    /// pane 번호는 재사용된다 — [`Self::used_pane_ids`] 가 `alive` 인 레코드의
    /// 번호만 잡아 두므로, 이미 죽은 레코드의 번호는 다음 pane 에 그대로 다시
    /// 나간다. 그래서 번호만 맞춰 보면 **살아서 도는 남**이 옛 묘비 때문에
    /// 「닫힌 pane」으로 판정된다(2026-08-25: 방 6 의 `%21` 이 그랬다. 모모이가
    /// 멀쩡히 일하는데 인포가 그 pane 을 되살리기 칸으로 보내, 그 방에 설 pane 이
    /// 하나도 없어 **방 자체가 목록에서 사라졌다**).
    ///
    /// 되살리기 목록에 같은 번호가 여럿 뜨는 것은 정상이다 — 서로 다른 pane 의
    /// 서로 다른 대화라 하나로 합치면 안 된다.
    pub(crate) fn stashed_record(&self, pane: &str) -> Option<&crate::ClosedPane> {
        stashed_in(&self.closed_panes, pane)
    }

    /// 위와 같은 판정의 인덱스 판. 살아 있는 것을 먼저 집고, 없으면 죽은 기록에서
    /// 찾는다 — 목록에서 지우는 조작은 묘비에도 걸려야 한다.
    pub(crate) fn closed_pane_index(&self, pane: &str) -> Option<usize> {
        closed_index_in(&self.closed_panes, pane)
    }

    /// 닫힘 스택에 넣고 상한을 정리한다 — pane 닫기와 미리보기 탭 닫기가 같은
    /// 스택을 쓰므로 ⌘⇧T 가 「가장 최근에 닫은 것」 순서를 하나로 지킨다.
    ///
    /// 오래된 것부터 버린다 — 레코드마다 스크롤백이 통째 붙어 있고, 살아 있는
    /// 것은 프로세스까지 물고 있다. 여기서 놓지 않으면 닫기만 반복해도 셸이
    /// 무한정 쌓인다.
    /// 상한은 **정리 대상만** 센다. 숨긴 것(`stashed`)은 세지도 놓지도 않는다 —
    /// 숨겨 둔 학생 여럿 때문에 방금 닫은 pane 이 밀려 죽으면 안 된다.
    pub(crate) fn push_closed_pane(&mut self, c: crate::ClosedPane) {
        self.closed_panes.push(c);
        while self.closed_panes.iter().filter(|c| !c.stashed).count() > crate::CLOSED_PANE_KEEP {
            let Some(i) = self.closed_panes.iter().position(|c| !c.stashed) else { break };
            let c = self.closed_panes.remove(i);
            if c.alive {
                self.kill_hidden_pane(&c.pane_id);
            }
        }
    }

    /// 닫아 둔 pane 중 **잊힌 것**을 놓는다 — 내리 노는 상태가 `CLOSED_PANE_IDLE_REAP`
    /// 를 넘으면 프로세스를 끈다.
    ///
    /// 닫아도 안 죽이는 건 의도다(`hide_pane`): 그 안의 claude 가 하던 일을 계속하고,
    /// 되살리기가 재부착이 된다. 문제는 놓는 계기가 개수 상한뿐이었다는 것 — 그건
    /// **다음 닫기가 있어야** 도니, 몇 개 닫고 손 떼면 그 셸들이 무기한 남았다.
    ///
    /// 그래서 일하는 것과 잊힌 것을 가른다. 일하는 중이면 타이머가 매번 풀리므로
    /// 닫아 두고 계속 돌리는 용법은 그대로 산다.
    pub(crate) fn reap_idle_closed_panes(&mut self) {
        if self.closed_panes.is_empty() {
            return;
        }
        let now = Instant::now();
        // ws 락과 `closed_panes` 를 동시에 빌릴 수 없어 판정을 먼저 걷어 온다.
        let working: Vec<bool> = {
            let ws = self.ws.lock().unwrap();
            self.closed_panes
                .iter()
                .map(|c| {
                    ws.panes
                        .get(&c.pane_id)
                        .and_then(|p| p.term())
                        .is_some_and(crate::input::term_is_working)
                })
                .collect()
        };
        let limit = crate::closed_pane_idle_reap();
        let mut doomed: Vec<usize> = Vec::new();
        for (i, c) in self.closed_panes.iter_mut().enumerate() {
            // 이미 죽은 pane 은 레코드로만 되살아나므로 셀 것이 없다.
            // 숨긴 것도 시간을 안 센다 — **놀고 있는 게 정상이고 그래서 치운 것**이다.
            // 여기서 세면 15분 뒤 조용히 죽어, 돌아온 사용자가 빈 셸을 보게 된다.
            if !c.alive || c.stashed {
                continue;
            }
            if working[i] {
                c.idle_since = None;
                continue;
            }
            let since = *c.idle_since.get_or_insert(now);
            if now.duration_since(since) >= limit {
                doomed.push(i);
            }
        }
        if doomed.is_empty() {
            return;
        }
        // 뒤에서부터 — 앞을 지우면 뒤 인덱스가 밀린다.
        for i in doomed.into_iter().rev() {
            let c = self.closed_panes.remove(i);
            self.kill_hidden_pane(&c.pane_id);
        }
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// 인포의 × — 되살리기 목록에서 지우고, 아직 돌고 있으면 프로세스까지 끈다.
    pub(crate) fn discard_closed_pane_at(&mut self, idx: usize) {
        if idx >= self.closed_panes.len() {
            return;
        }
        let c = self.closed_panes.remove(idx);
        if c.alive {
            self.kill_hidden_pane(&c.pane_id);
        }
        // 마지막 하나를 지우면 하단바가 접힌다 — 그만큼 그리드가 다시 늘어야 한다.
        // 안 그러면 바가 있던 40px 이 빈 띠로 남는다(닫을 때는 `hide_pane` 이 이미
        // 같은 일을 한다).
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// ⌘⇧T — 가장 최근에 닫은 pane 을 되살린다.
    pub(crate) fn reopen_closed_pane(&mut self) {
        let Some(c) = self.closed_panes.pop() else { return };
        self.reopen_pane_record(c);
    }

    /// 인포의 닫힘 줄 클릭용 — 스택 가운데 하나를 지목해 되살린다.
    pub(crate) fn reopen_closed_pane_at(&mut self, idx: usize) {
        if idx >= self.closed_panes.len() {
            return;
        }
        let c = self.closed_panes.remove(idx);
        self.reopen_pane_record(c);
    }

    /// 닫힌 방이 아직 있으면 그 방으로 돌아가 원래 이웃 옆에 되살린다. 이웃이 그 사이
    /// 사라졌으면 활성 pane 옆으로 — 자리를 못 찾았다고 되살리기를 포기하진 않는다.
    fn reopen_pane_record(&mut self, c: crate::ClosedPane) {
        // 밖에 나가 있는 방으로는 보내지 않는다 — `switch_window` 가 그 창을 앞으로
        // 보낼 뿐 메인은 그대로라, 되살린 pane 이 보이지 않는 방에 들어가 버린다.
        if c.window < self.windows.len()
            && c.window != self.active_window
            && !self.window_is_undocked(c.window)
        {
            self.switch_window(c.window);
        }
        // 미리보기 탭 레코드 — pane 이 아니라 보조 탭이었으니 `open_file` 로 다시
        // 연다. 원래 붙어 있던 pane 이 사라졌으면 open_file 이 활성 pane 으로 폴백.
        if let Some((outer, path)) = c.preview.clone() {
            self.open_file(path.clone(), Some(outer), true);
            // `as_tab` 은 배경 탭 규약이지만 ⌘⇧T 는 「다시 보여 달라」다 —
            // 되살린 탭을 앞으로 끌어낸다.
            {
                let mut ws = self.ws.lock().unwrap();
                let found = ws.panes.iter().find_map(|(id, p)| {
                    p.tabs
                        .iter()
                        .position(|t| t.preview_path.as_deref() == Some(path.as_path()))
                        .map(|i| (id.clone(), i))
                });
                if let Some((id, i)) = found {
                    if let Some(p) = ws.panes.get_mut(&id) {
                        p.active_tab = i;
                        p.dirty = true;
                    }
                    ws.active_pane = Some(id);
                }
            }
            self.chrome_dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            eprintln!("[reopen] 미리보기 {} 되살림", path.display());
            return;
        }
        // 아직 돌고 있으면 새로 띄우지 않는다 — 그 pane 은 화면에서만 빠져 있었을
        // 뿐 셸도 claude 도 그대로다. `--resume` 으로 대화를 되감으면 오히려 하던
        // 일이 끊긴다. `alive` 를 믿지 말고 실제 PTY 로 확인하는 건, 숨긴 사이에
        // 셸이 스스로 끝났을 수 있어서다(그때는 아래 레코드 경로로 흘러간다).
        let attached = c.alive && self.pty.contains_key(&c.pane_id);
        let new_id = if attached {
            c.pane_id.clone()
        } else {
            let (cols, rows) = self.window_cells();
            let Some(id) = self.restore_leaf(&c.rec, cols, rows) else {
                eprintln!("[reopen] {} 되살리기 실패 — PTY 를 못 띄웠다", c.pane_id);
                return;
            };
            id
        };
        let anchor = c
            .neighbor
            .filter(|n| {
                self.pty.contains_key(n)
                    && self
                        .pty_layout
                        .as_ref()
                        .is_some_and(|t| t.leaves().iter().any(|l| *l == n.as_str()))
            })
            .or_else(|| self.ws.lock().unwrap().active_pane.clone());
        let grafted = match (anchor, self.pty_layout.as_mut()) {
            (Some(a), Some(tree)) => {
                tree.split_leaf(&a, kasa_pty::SplitDir::Horizontal, new_id.clone())
            }
            _ => false,
        };
        if !grafted {
            // 트리가 비었거나 닻이 사라졌다 — 이 pane 을 유일 leaf 로 세운다.
            self.pty_layout = Some(kasa_pty::PtyLayout::single(new_id.as_str()));
        }
        {
            let mut ws = self.ws.lock().unwrap();
            ws.active_pane = Some(new_id.clone());
            for pane in ws.panes.values_mut() {
                pane.dirty = true;
            }
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        self.session_touched = true;
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        eprintln!("[reopen] {} → {new_id} 되살림", c.pane_id);
    }

    /// pane 하나로 포커스를 옮긴다 — 다른 방(윈도우)에 있으면 그 방부터 앞으로
    /// 가져온다. `active_pane` 만 바꾸면 안 보이는 윈도우의 pane 이 선택돼 화면은
    /// 그대로다. `switch_window` 가 leaves[0] 로 `active_pane` 을 덮으므로 순서는
    /// 반드시 **방 전환 → pane 지정**이다.
    ///
    /// 실재하는 leaf 일 때만 옮긴다 — 캐릭터·작업명 같은 집계 id 로 `active_pane`
    /// 을 덮으면 다음 `/layout` 폴에서 그 타일이 빠져 pane 이 닫힌 것처럼 보였다
    /// (거노: 캐릭터 클릭→학생 선택하면 닫힘).
    pub(crate) fn focus_pane(&mut self, pane: &str) -> bool {
        let Some(wi) = self.window_of_pane(pane) else {
            return false;
        };
        if wi != self.active_window {
            self.switch_window(wi);
        }
        if let Ok(mut ws) = self.ws.lock() {
            ws.active_pane = Some(pane.to_string());
        }
        self.chrome_dirty = true;
        true
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
            self.overlay_room_rename_label();
            return;
        }
        let n = self.windows.len();
        let mut out = Vec::with_capacity(n);
        let ws = self.ws.lock().unwrap();
        for i in 0..n {
            // Representative pane = first leaf of the window's layout. The
            // active window's tree lives in pty_layout; the rest in windows[i].
            let leaves: Vec<String> = {
                let layout = if i == self.active_window {
                    self.pty_layout.as_ref()
                } else {
                    self.windows.get(i).and_then(|o| o.as_ref())
                };
                layout.map_or(Vec::new(), |l| l.leaves().iter().map(|s| s.to_string()).collect())
            };
            let repr = leaves.first().cloned();
            // 방을 대표하는 cwd — 첫 leaf 가 아니라 **학생이 앉은 곳의 최빈값**이다.
            // 첫 pane 하나로 이름을 지으면 그 pane 이 뭘 띄웠는지에 따라 방 이름이
            // 흔들려, 사이드바에서 자리로 방을 찾던 눈이 매번 다시 읽어야 했다.
            // 좁히는 규칙은 `room_home_cwd` 에 있다.
            let cwds: Vec<(std::path::PathBuf, bool)> = leaves
                .iter()
                .filter_map(|id| {
                    // 학생 판정은 `pane_record` 가 쓰는 기준과 같다 — 바인딩된 세션
                    // id 도 함께 봐서, claude 가 잠시 내려간 pane 이 셸로 강등돼
                    // 방 이름이 흔들리는 일이 없다.
                    let agent = self.pane_claude_sid.contains_key(id)
                        || self.pty.get(id).and_then(|p| p.active_agent()).is_some();
                    self.pane_current_cwd(id).map(|p| (p, agent))
                })
                .collect();
            let home = room_home_cwd(&cwds);
            // 손으로 붙인 이름은 파생을 항상 이긴다 — 지정 pane 이 대표 leaf 가
            // 아니어도, 방을 옮겨도 유지돼야 한다.
            let name = self
                .window_name_override
                .get(&i)
                .cloned()
                .or_else(|| {
                    home.as_ref()
                        .and_then(|p| p.file_name())
                        .map(|s| s.to_string_lossy().into_owned())
                        .filter(|s| !s.is_empty())
                })
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
            let cwd = home
                .as_ref()
                .map(|p| Self::shorten_cwd(p))
                .unwrap_or_default();
            out.push((name, cwd));
        }
        drop(ws);
        self.window_labels = out;
        self.window_labels_at = Some(now);
        self.overlay_room_rename_label();
    }

    /// 편집 중인 방의 라벨을 버퍼(+조합 중인 글자+캐럿)로 덮는다. 별도 입력칸을
    /// 띄우지 않고 라벨 자리를 그대로 쓰는 Finder 식 편집이다.
    ///
    /// **재계산 안이 아니라 밖에서 덮는 게 핵심이다.** 위 캐시는 1초짜리고 cwd 를
    /// `lsof` 로 캐느라 비싸서 매 키마다 깰 수가 없는데, 합성을 그 안에 두면 타이핑이
    /// 1초씩 뭉쳐 나온다(거노: "이름 바꾸는 게 버벅여").
    fn overlay_room_rename_label(&mut self) {
        let Some((idx, buf)) = self.room_rename.editing.as_ref() else { return };
        let composing = match self.ime_focus {
            Some(crate::ImeFocus::RoomRename(i)) if i == *idx => self.preedit.as_str(),
            _ => "",
        };
        // 캐럿은 커서 자리다 — 늘 끝에 붙이면 가운데를 고치는 동안 커서가 어디 있는지
        // 화면이 거짓말을 한다. 조합 중인 글자는 커서 바로 앞에 온다.
        let (before, after) = crate::lineedit::split(buf, self.room_rename.cursor);
        let text = format!("{before}{composing}\u{258c}{after}");
        if let Some(slot) = self.window_labels.get_mut(*idx) {
            slot.0 = text;
        }
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
        if kasa_socket::home_dir().as_deref() == Some(p) {
            return "~".to_string();
        }
        p.file_name()
            .and_then(|s| s.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| "/".to_string())
    }
    pub(crate) fn shorten_cwd(p: &std::path::Path) -> String {
        let s = tilde_home(&p.to_string_lossy());
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

        let active = self.ws.lock().unwrap().active_pane.clone();
        let Some(active) = active else {
            return;
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
                // **백그라운드 탭이다** — 활성 탭도 활성 pane 도 안 건드린다(거노
                // 2026-08-13). 학생이 이미지를 보내면 그 pane 의 대화가 통째로 이미지에
                // 덮이고, 키보드 포커스까지 그 pane 으로 끌려가 다른 데서 타이핑 중이면
                // 뺏긴다. 그림 자체는 OSC 1337 인라인으로 대화 흐름 안에 이미 뜨므로
                // (ad6c04d), 이 탭은 「크게 볼 때 누르는 자리」로 족하다.
                pane.dirty = true;
            }
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
            // Active pane isn't in the tree — undo the orphan insert. 번호는 따로
            // 되돌릴 게 없다: 등록을 지우면 alloc_pane_id 가 다시 빈 번호로 본다.
            self.ws.lock().unwrap().panes.remove(&new_id);
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
        let new_id = self.alloc_pane_id();
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
                is_repo: is_git_repo(&root),
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
                let p = e.path();
                let is_repo = is_dir && is_git_repo(&p);
                out.push(FileNode { path: p, name: name.clone(), is_dir, depth: 0, ignored: false, is_repo });
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
                let p = e.path();
                let is_repo = is_dir && is_git_repo(&p);
                Some(FileNode { path: p, name, is_dir, depth, ignored: false, is_repo })
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
    /// 이 띠는 클립을 안 세운다). This only clamps `first` into range;
    /// keeping the *active* tab in view is `win_tab_reveal`'s job at
    /// switch/create time, so a free wheel-scroll is never yanked back.
    /// `i` 번 방의 pane id 들. 활성 방의 트리만 `pty_layout` 에 나가 있어 슬롯이
    /// 비는데, 그걸 모르고 `windows[i]` 만 보면 지금 보고 있는 방이 늘 빈 방이 된다
    /// — 사이드바·라벨·상태 점이 다 이 갈래를 각자 쓰고 있어 한 곳으로 모은다.
    pub(crate) fn window_leaves(&self, i: usize) -> Vec<String> {
        let layout = if i == self.active_window {
            self.pty_layout.as_ref()
        } else {
            self.windows.get(i).and_then(|o| o.as_ref())
        };
        layout.map_or(Vec::new(), |l| l.leaves().iter().map(|s| s.to_string()).collect())
    }
    pub(crate) fn sidebar_layout(
        &self,
        win_h: f32,
    ) -> (
        Vec<(usize, (f32, f32, f32, f32))>,
        Vec<(usize, (f32, f32, f32, f32))>,
        (f32, f32, f32, f32),
        Vec<(usize, String, (f32, f32, f32, f32))>,
        // 배치도 칸 — 목록 행과 **같은 모양**이라 히트 벡터에 그대로 합칠 수 있다.
        // 그러면 칸 클릭·드래그·우클릭이 행과 똑같이 동작한다(공짜로 따라온다).
        Vec<(usize, String, (f32, f32, f32, f32))>,
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
            // 가로 탭엔 아래로 펼 자리가 없다 — pane 목록도 배치도도 세로 전용.
            return (tabs, closes, plus, Vec::new(), Vec::new());
        }
        let tab_x = SIDEBAR_TAB_INSET;
        let tab_w = (self.sidebar_w_logical - 2.0 * SIDEBAR_TAB_INSET).max(0.0);
        // 10px slot above the first tab hosts the overflow chevron-up.
        let top = TITLE_HEIGHT + 18.0;
        let stride = SIDEBAR_TAB_H + SIDEBAR_TAB_GAP;
        // Rows that fit above the "+" button; the dock strip eats the bottom
        // of the column, and 24px stays free for "+"-adjacent chrome + the
        // chevron-down overflow hint.
        // 상태줄도 바닥을 먹는다. 안 빼면 마지막 방 카드가 그 위로 넘치는데,
        // 사이드바는 클립을 안 세우므로 **잘리지 않고 그대로 덮어 그려진다** — 화면은
        // 멀쩡해 보이고 카드만 엉뚱한 자리에 있는 종류의 버그가 된다.
        let bottom_h =
            if self.docked.is_empty() { 0.0 } else { DOCK_HEIGHT } + self.status_h();
        // 트레이(+ · 피드백 · 설정)가 바닥을 먹는다 — 목록은 그 위까지만. 24px 는
        // chevron-down 오버플로 힌트 자리.
        let avail_h = (win_h - bottom_h - top - SIDEBAR_TRAY_H - 24.0).max(stride);
        // 스크롤 시작점은 **접힌 높이 기준**으로 잡는다 — 펼침은 방금 사용자가
        // 편 것이라 그만큼 뒤가 밀리는 게 자연스럽고, 가변 높이로 역산하면 펼칠
        // 때마다 목록이 통째로 점프한다.
        let n_vis = n
            .min((((avail_h + SIDEBAR_TAB_GAP) / stride) as usize).max(1));
        let first = self.win_tab_first.min(n.saturating_sub(n_vis));
        let mut tabs = Vec::with_capacity(n_vis);
        let mut closes = Vec::new();
        let mut rows = Vec::new();
        let mut mini = Vec::new();
        // 펼친 방은 카드가 pane 수만큼 길어진다 — 고정 stride 를 쓰던 자리를 누적
        // y 로 바꾼 이유가 이것이다. 넘치는 방은 그리지 않는다(사이드바는 클립을
        // 안 세워서 반쪽 카드가 트레이를 침범한다).
        //
        // 여기는 배치 계산이라 시저를 세워도 이 규칙은 남는다 — 이 rect 들은 그리기와
        // 클릭 판정이 함께 쓰는 값이고, 클릭은 시저가 안 자른다.
        let mut y = top;
        for i in first..n {
            let leaves = self.window_leaves(i);
            // 숨긴 pane 은 트리에 없어 배치도에 칸이 없다 — 지도 아래 꼬리 줄로 둔다.
            // 어디에도 안 보이면 되살릴 길이 없고, 트리에서 빠졌을 뿐 PTY 는 돈다.
            let hidden: Vec<String> = self
                .closed_panes
                .iter()
                .filter(|c| c.stashed && c.alive && c.window == i)
                .map(|c| c.pane_id.clone())
                .collect();
            // 학생이 하나인 방도 편다. "점 하나가 이미 그 하나를 말한다"고 봤는데,
            // 그 한 줄이 **누가 있고 무슨 상태인지의 전부**라 접어 두면 학생 하나짜리
            // 방에선 그 학생을 볼 길이 통째로 사라졌다(거노, 두 번). 손잡이 쪽은 이미
            // 폈는데 여기가 안 따라와 버튼만 있고 아무것도 안 나오는 상태였다 — 두
            // 조건이 갈리면 손이 닿지 않는 펼침이 생긴다.
            // 펴는 중이면 0..1 사이 — 카드가 그만큼만 자란다.
            let t = if leaves.is_empty() { 0.0 } else { self.expand_progress(i) };
            // 펼친 카드는 **배치도 하나**다. 예전엔 목록 뷰로 갈아 끼울 수 있었는데,
            // 그 목록은 info 탭이 방→pane→탭→프로세스로 이미 그리는 것의 얕은
            // 사본이었다(2026-08-24 지시: "목록표시는 info에서 보면되고").
            // 칸이 얼굴을 담아야 하므로 높이가 pane 수를 따라간다 — 여섯 칸을 46px
            // 안에 우겨넣으면 한 칸이 7px 이라 얼굴이 안 들어간다.
            let body_h = (36.0 + 13.0 * leaves.len() as f32).clamp(46.0, 150.0);
            let full_h = body_h + hidden.len() as f32 * SIDEBAR_ROW_H + SIDEBAR_ROW_PAD;
            let list_h = (full_h * t).round();
            let h = SIDEBAR_TAB_H + list_h;
            if !tabs.is_empty() && y + h > top + avail_h {
                break;
            }
            tabs.push((i, (tab_x, y, tab_w, h)));
            if n > 1 {
                let cs = 14.0;
                // Centered on the *name* row (drawn at y+11, 13.5px) rather than
                // pinned to the card top — the two-line tab put the × above the
                // title it belongs to, reading as detached from both lines.
                closes.push((i, (tab_x + tab_w - cs - 3.0, y + 11.0, cs, cs)));
            }
            if list_h > 0.0 {
                // 카드 안에 온전히 들어온 줄만 낸다 — 사이드바는 클립을 안 세워서
                // 반쪽 줄이 카드 밖으로 삐져나온다. 그래서 목록이 아래에서 한 줄씩
                // 드러난다.
                let bottom = y + h;
                // 배치도 — 카드 머리 바로 아래. `leaf_rects` 가 BSP 트리를 사각형으로
                // 이미 풀어 주므로 여기서 재귀할 것이 없다.
                let ma = (tab_x + 10.0, y + SIDEBAR_TAB_H + 3.0, tab_w - 20.0, body_h - 8.0);
                if ma.1 + ma.3 <= bottom && ma.2 > 0.0 {
                    // 활성 방의 트리는 `windows[i]` 가 아니라 `pty_layout` 에 있다
                    // (그 슬롯은 None 이다) — `window_leaves` 와 같은 갈래를 쓴다.
                    let tree = if i == self.active_window {
                        self.pty_layout.as_ref()
                    } else {
                        self.windows.get(i).and_then(|o| o.as_ref())
                    };
                    // 1000 을 기준으로 뽑는다. 작은 값(예: 100)을 넣으면 u16 반올림에
                    // 얇은 pane 이 0 폭으로 뭉개진다.
                    const G: f32 = 1000.0;
                    let cells = tree.map(|t| t.leaf_rects(1000, 1000)).unwrap_or_default();
                    for (id, cx, cy, cw, ch) in cells {
                        // 1px 씩 깎아 칸 사이에 틈을 낸다 — 붙여 놓으면 분할선이 안 보여
                        // 한 덩어리로 읽힌다.
                        mini.push((
                            i,
                            id,
                            (
                                ma.0 + cx as f32 / G * ma.2,
                                ma.1 + cy as f32 / G * ma.3,
                                (cw as f32 / G * ma.2 - 1.0).max(2.0),
                                (ch as f32 / G * ma.3 - 1.0).max(2.0),
                            ),
                        ));
                    }
                }
                // 배치도 아래 꼬리에는 숨긴 pane 만 줄로 남는다 — 숨긴 것은 트리에
                // 없어 칸이 없으므로 배치도로는 말할 방법이 아예 없다.
                for (k, id) in hidden.iter().enumerate() {
                    let ry = y
                        + SIDEBAR_TAB_H
                        + body_h
                        + SIDEBAR_ROW_PAD / 2.0
                        + k as f32 * SIDEBAR_ROW_H;
                    if ry + SIDEBAR_ROW_H > bottom {
                        break;
                    }
                    rows.push((i, id.clone(), (tab_x + 8.0, ry, tab_w - 16.0, SIDEBAR_ROW_H)));
                }
            }
            y += h + SIDEBAR_TAB_GAP;
        }
        // `+` 는 목록 꼬리가 아니라 하단 트레이의 왼쪽 칸이다 — 세션이 늘어도 자리가
        // 안 움직인다. 트레이가 없는 배치(top 탭·사이드바 접힘)에서는 어차피 이
        // 분기를 안 타므로 폴백은 목록 꼬리 그대로.
        let plus = self.sidebar_tray_rects(win_h).map_or_else(
            || (tab_x, y, tab_w, 28.0),
            |(_, p, ..)| p,
        );
        (tabs, closes, plus, rows, mini)
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
    /// 저장이 leaf 에 실을 surface_id → (model, effort). 소켓 백엔드가 없으면 빈 맵.
    ///
    /// 값을 모으는 쪽은 백엔드다(claude 는 statusline 보고, codex 는 transcript 머리).
    /// App 에 같은 맵을 하나 더 두지 않고 그때그때 떠 오는 이유는, App struct 의 필드
    /// 정의가 워커 여럿이 동시에 못 만지는 병목이기 때문이다(CLAUDE.md).
    pub(crate) fn agent_cfg_snapshot(&self) -> HashMap<String, (String, String)> {
        let mut cfg = self
            .socket_backend
            .as_ref()
            .map(|b| b.agent_cfg_snapshot())
            .unwrap_or_default();
        // ultracode 는 statusline 이 xhigh 로 보고한다(claude 가 effort 페이로드에 안
        // 실음) — 그대로 저장하면 재시작 복원이 xhigh 로 잇는다(2026-08-15 신고
        // 「울트라코드로 이어가는거 왜안돼」). 마커 판정(pane_ultracode — 입력박스
        // 보라 글로우와 같은 근거)이 참인 pane 은 여기서 덮는다. `--effort ultracode`
        // 는 CLI 가 실제로 받아 켠다(print 자기보고 실측: ultracode→ON, xhigh→OFF).
        for pane in &self.pane_ultracode {
            cfg.entry(pane.clone()).or_default().1 = "ultracode".to_string();
        }
        cfg
    }

    pub(crate) fn session_state_json(&self) -> Option<serde_json::Value> {
        let mut sessions_json = Vec::new();
        // 창별 워크스페이스 락을 잡기 전에 한 번만 뜬다(락 순서 얽힘 방지).
        let agent_cfg = self.agent_cfg_snapshot();
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
                windows_json.push(Self::layout_to_json(
                    layout,
                    pty,
                    &ws_guard,
                    &self.pane_claude_sid,
                    &agent_cfg,
                ));
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
        agent_cfg: &HashMap<String, (String, String)>,
    ) -> serde_json::Value {
        match layout {
            kasa_pty::PtyLayout::Leaf { pane_id } => {
                let mut rec = pty
                    .get(pane_id)
                    .map(|s| socket::pane_record(s))
                    .unwrap_or(serde_json::Value::Null);
                // 웹 pane 은 PTY 가 없어 record 가 Null 로 떨어져 재시작하면
                // 그 자리가 통째로 증발했다(복원이 Null leaf 를 버린다). 주소만
                // 있으면 되살릴 수 있으니 web_url 을 실은 최소 record 를 만든다
                // — 복원 쪽 분기는 restore_leaf 의 web_url 가지.
                if rec.is_null() {
                    if let Some(url) = ws
                        .panes
                        .get(pane_id)
                        .and_then(|p| p.tabs.iter().find_map(|t| t.web()))
                        .map(|w| w.url.clone())
                    {
                        rec = serde_json::json!({ "web_url": url });
                    }
                }
                // Attach the pane's scrollback (text lines) so restore can
                // repaint what was on screen. Only when we have a real record.
                if let Some(obj) = rec.as_object_mut() {
                    // pane id 자체를 저장한다. 이게 없으면 복원이 `%1` 부터 새로
                    // 번호를 매기는데, `--resume` 으로 되살아난 학생은 재시작 **전**
                    // 의 surface_id 를 대화 기록째 기억하고 있다 → `tell %5` 가 없는
                    // pane 이거나 그 사이 다른 pane 이 물려받은 번호로 배달된다
                    // (거노: "재시작하면 학생들이 tell 을 이상한 pane 에 쓴다").
                    obj.insert("pane_id".to_string(), serde_json::json!(pane_id));
                    // 캐릭터 영속(거노: 재시작하면 미도리로 둔갑): pane_character 는
                    // claude 프로세스 감지(was_claude)와 무관하게 살아있으므로, 감지가
                    // 실패해도 캐릭터는 여기서 확실히 저장한다.
                    if let Some(name) = ws.pane_character.get(pane_id) {
                        obj.insert("character".to_string(), serde_json::json!(name));
                    }
                    // 붙인 이름(`/rename`·`surface.rename`·`kasaspace_rename`). 이게
                    // 없으면 재시작마다 이름이 증발해 OSC 제목으로 되돌아갔다 — 이 앱은
                    // 종료 시 자기 설치를 하므로 껐다 켜는 일이 잦고, 그래서 이름을
                    // 붙이는 행위 자체가 몇 분짜리가 됐다.
                    //
                    // ⚠️ **핀이 섰을 때만 저장한다.** 핀 없는 `title` 은 안에서 도는
                    // 프로그램이 쏜 OSC 라, 그걸 굳혀 두면 다음에 켤 때 「사람이 정한
                    // 이름」인 척하면서 그 뒤의 OSC 를 영영 막는다.
                    if let Some(t) = ws
                        .panes
                        .get(pane_id)
                        .filter(|p| p.title_pinned)
                        .and_then(|p| p.title.as_deref())
                        .filter(|s| !s.trim().is_empty())
                    {
                        obj.insert("title".to_string(), serde_json::json!(t));
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
                    // 끄기 직전 쓰던 모델·effort. 없으면 키를 아예 안 넣는다 — 복원은
                    // "없으면 플래그를 안 붙인다"라, 빈 문자열을 남기면 되살릴 때
                    // `--model ''` 같은 게 나갈 위험만 는다.
                    if let Some((model, effort)) = agent_cfg.get(pane_id) {
                        if !model.is_empty() {
                            obj.insert("model".to_string(), serde_json::json!(model));
                        }
                        if !effort.is_empty() {
                            obj.insert("effort".to_string(), serde_json::json!(effort));
                        }
                    }
                    // Agent TUI는 새 화면을 다시 그리므로 옛 터미널 행을 넣지 않는다.
                    // 일반 셸은 실제 PTY history를 저장해야 재시작 뒤 출력이 남는다.
                    let restores_agent = obj
                        .get("was_agent")
                        .and_then(|v| v.as_str())
                        .is_some_and(|agent| matches!(agent, "claude" | "codex" | "agy"))
                        || obj
                            .get("was_claude")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                        || obj
                            .get("session_id")
                            .and_then(|v| v.as_str())
                            .is_some_and(|sid| !sid.is_empty());
                    let sb = if restores_agent {
                        Vec::new()
                    } else {
                        pty
                            .get(pane_id)
                            .map(|p| p.scrollback_text(SCROLLBACK_SAVE_MAX))
                            .or_else(|| ws.panes.get(pane_id).map(scrollback_lines))
                            .unwrap_or_default()
                    };
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
                    "a": Self::layout_to_json(a, pty, ws, pane_claude_sid, agent_cfg),
                    "b": Self::layout_to_json(b, pty, ws, pane_claude_sid, agent_cfg),
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
                let was_agent = saved_agent(leaf).is_some();
                let bound_sid = leaf
                    .get("session_id")
                    .and_then(|c| c.as_str())
                    .is_some_and(|s| !s.is_empty());
                if was_agent || bound_sid {
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
        self.restore_window_layout_at(node, cols, rows)
    }

    fn restore_window_layout_at(
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
            let probe = kasa_pty::PtyLayout::Split {
                dir,
                ratio,
                a: Box::new(kasa_pty::PtyLayout::single("a")),
                b: Box::new(kasa_pty::PtyLayout::single("b")),
            };
            let rects = probe.leaf_rects(cols, rows);
            let (_, _, _, aw, ah) = rects[0].clone();
            let (_, _, _, bw, bh) = rects[1].clone();
            let a = split
                .get("a")
                .and_then(|a| self.restore_window_layout_at(a, aw, ah));
            let b = split
                .get("b")
                .and_then(|b| self.restore_window_layout_at(b, bw, bh));
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
    /// Spawn one restored pane from its saved record and, when it was running an
    /// agent, queue the command that brings it back. Returns the new pane id, or
    /// None if the PTY failed to start (caller then collapses the split).
    fn restore_leaf(
        &mut self,
        rec: &serde_json::Value,
        cols: u16,
        rows: u16,
    ) -> Option<String> {
        let saved = rec.get("pane_id").and_then(|v| v.as_str());
        // 저장된 번호를 되살릴 수 있는지는 alloc 과 **같은 기준**으로 본다 — `self.pty`
        // 만 보면 이미 복원된 미리보기 pane 의 번호를 빼앗는다.
        let used = self.used_pane_ids();
        let id = pick_restore_id(saved, |s| used.contains(s))
            .unwrap_or_else(|| next_free_pane_id(&used));
        // 웹 pane — PTY 를 안 띄운다. 그리드 자리(WebPane)만 앉히고 자식 창은
        // pending_web_hosts 로 미룬다: 복원 경로엔 ActiveEventLoop 가 없어
        // 창을 만들 수 없다(about_to_wait 의 drain 이 다음 턴에 만든다).
        if let Some(url) = rec.get("web_url").and_then(|v| v.as_str()) {
            let host_id = self.alloc_web_host_id();
            let mut tab = crate::PaneTab::default();
            tab.content =
                crate::PaneContent::Web(crate::WebPane { url: url.to_string(), host_id });
            tab.title = Some(
                rec.get("title")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| crate::webpane::short_label(url)),
            );
            tab.title_pinned = true;
            let ps = crate::PaneState { tabs: vec![tab], dirty: true, ..Default::default() };
            self.ws.lock().unwrap().panes.insert(id.clone(), ps);
            self.pending_web_hosts.push((host_id, url.to_string()));
            return Some(id);
        }
        let cwd = rec
            .get("cwd")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
            .or_else(resolve_initial_cwd);
        let was_agent = saved_agent(rec);
        let session_id = rec
            .get("session_id")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string());
        // 저장된 캐릭터를 되살린다(거노: 재시작하면 랜덤 둔갑). pending 으로 세팅하면
        // assign_character_env 가 랜덤 대신 이걸 재사용하고, 저장 세션 id 가 있으면 그
        // 원본 sid 에 캐릭터를 다시 bind 해 --resume 후 shim 교정·다음 재시작까지 영속화한다.
        // 고른 명단 밖이면 **되살리지 않는다** — 그러면 아래 `assign_character_env` 가
        // 명단 안에서 새로 뽑는다. 저장된 이름을 무조건 되살리던 탓에, 명단을 바꿔도
        // 이미 배정된 학생은 재시작을 넘어 영원히 남았다(거노 2026-08-25 「설정에서
        // 원하는거 다 골랐는데 그거 반영안되고 선택안된학생도 스폰돼」 — 새 배정은
        // 멀쩡했고 옛 배정이 안 바뀐 것이었다).
        //
        // 대화는 안 끊긴다. 바뀌는 것은 이름·얼굴·말투뿐이고 `--resume` 은 그대로 탄다.
        let saved_char = rec
            .get("character")
            .and_then(|c| c.as_str())
            .filter(|s| !s.is_empty())
            .filter(|s| {
                let keep = kasa_mcp::character::is_assignable(s);
                if !keep {
                    eprintln!("[restore] {s} 는 고른 명단 밖 — 새로 배정한다");
                }
                keep
            })
            .map(|s| s.to_string());
        if let Some(ref c) = saved_char {
            self.pending_character = Some(c.clone());
            if let Some(ref sid) = session_id {
                let _ = kasa_mcp::character::bind_session_character(sid, c);
            }
        }
        let restores_agent = was_agent.is_some() || (saved_char.is_some() && session_id.is_some());
        let scrollback = restored_scrollback(rec, restores_agent);
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
        self.insert_pty(id.clone(), session.clone());
        // 붙인 이름을 되살린다. 핀도 같이 세워야 한다 — 안 세우면 되살린 이름이
        // pane 안 프로그램의 첫 OSC 에 곧바로 덮여, 저장한 보람이 몇 초 만에 사라진다
        // (claude 는 뜨자마자 제목을 쏜다). 저장 쪽이 핀 선 것만 넣으므로 여기 온
        // 값은 전부 사람이 정한 이름이다.
        if let Some(t) = rec
            .get("title")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
        {
            let mut ws = self.ws.lock().unwrap();
            let pane = ws.pane_mut(&id);
            pane.title = Some(t.to_string());
            pane.title_pinned = true;
        }
        // Bring the agent back: --resume the saved conversation (the shim
        // re-attaches team/persona/character from the session id), or a fresh
        // one when the pane ran an agent but no session id was captured.
        // Plain-shell panes restore to just their shell + scrollback. 900ms
        // mirrors swap_character's wait for the shell prompt before injection.
        // 하네스 감지가 실패했어도 캐릭터+저장 sid 가 있으면 claude 학생 pane 이었던
        // 것이라 --resume 으로 대화를 복원한다(감지 실패 시 셸만 뜨던 회귀 차단).
        if restores_agent {
            // --resume 대상 대화가 실재할 때만 resume 한다. 저장된 sid 의 jsonl 이
            // 사라졌으면 claude 가 "No conversation found" 를 뱉고 빈 셸만 남아 학생
            // pane 이 통째 죽는다(거노: %3 시로코 복원 실패 — claude 세션이 없어 board
            // 순회에서 빠졌다). 그땐 fresh claude 로 폴백해 최소한 학생 pane(캐릭터는
            // env/marker 로 유지)은 살린다 — 대화는 잃지만 pane 이 통째 죽는 것보다 낫다.
            //
            // 파일이 사는 곳이 하네스마다 다르다 — claude 는 `~/.claude/projects/<슬러그>/
            // <sid>.jsonl`, codex 는 `~/.codex/sessions/<Y>/<M>/<D>/rollout-<ts>-<sid>.jsonl`.
            let resumable = session_id
                .as_deref()
                .and_then(|sid| {
                    if was_agent == Some("codex") {
                        socket::codex_rollout_for_session(sid)
                    } else {
                        socket::transcript_path_for_session(sid)
                    }
                })
                .map(|p| p.exists())
                .unwrap_or(false);
            // 이어가기 실패는 무증상이었다 — 빈 세션이 학생 얼굴로 멀쩡히 떠서,
            // 대화를 잃은 줄 모른 채 계속 쓰게 된다(미도리 실측). 자리를 만들어
            // 준 것만으로는 부족하고 잃은 것을 말해 줘야 한다. 하네스와 무관하게
            // 같다 — codex 도 이제 이어가므로 잃으면 똑같이 말해야 한다.
            if session_id.is_some() && !resumable {
                self.collab.toast = Some((
                    format!(
                        "{} 이어갈 대화를 못 찾아 새로 시작합니다",
                        saved_char.as_deref().unwrap_or("이 pane 은")
                    ),
                    std::time::Instant::now(),
                ));
            }
            // `--effort ultracode` 로 되살린 pane 은 **transcript 에 아무 흔적을 안
            // 남긴다**(플래그 launch 는 enter attachment 를 안 쓴다 — 훅 주석의 실측).
            // 훅은 첫 프롬프트가 있어야 돌고 꼬리 스캔도 볼 것이 없으니, 앱이 자기가
            // 그렇게 띄웠다는 사실만이 유일한 근거다. 안 세워 두면 복원하자마자 다시
            // 끄는 것만으로 ultracode 가 xhigh 로 풀린다.
            if saved_effort(rec) == Some("ultracode") {
                self.mark_restored_ultracode(&id);
            }
            let cmd = restore_agent_command(
                was_agent,
                session_id.as_deref(),
                resumable,
                saved_model(rec),
                saved_effort(rec),
            );
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
        // 정본 포트. 이미 물려 있으면 spawn_http_server 가 임시 포트로 떨어지고,
        // 그러면 register_clients 가 전역 등록을 건너뛴다(주인 주소 보호).
        const CANONICAL_MCP_PORT: u16 = 8765;
        let http_port = match kasa_mcp::spawn_http_server(backend.clone(), CANONICAL_MCP_PORT) {
            Ok(port) => {
                eprintln!("[kasaspace-mcp] HTTP MCP on 127.0.0.1:{port}/mcp");
                std::env::set_var("KASASPACE_MCP_PORT", port.to_string());
                // No MCP auto-discovery: write our address into each AI
                // client's config so any agent on this machine finds us.
                kasa_mcp::register_clients(port, CANONICAL_MCP_PORT);
                Some(port)
            }
            Err(e) => {
                eprintln!("[kasaspace-mcp] HTTP MCP start failed: {e}");
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
        ));
        // GUI 쪽에도 핸들 보관 — ResumeSession 이 attach/재개 pane 의 transcript 를
        // bind hook 없이 즉석 확정(bind_transcript)할 때 쓴다.
        self.socket_backend = Some(backend.clone());
        self.start_socket_with(backend);
    }

    /// claude 가 자기 세션에 붙인 이름(`/rename`)을 그 pane 의 제목으로 옮긴다.
    ///
    /// pane 제목이 읽는 것은 두 가지뿐이었다 — 터미널이 쏘는 OSC 와 그것을 따라가는
    /// GUI 사본. claude 안에서 `/rename` 을 치면 이름은 **transcript 의 `custom-title`
    /// 레코드**로만 남고 OSC 로는 안 나가므로, 탭에는 옛 이름이 그대로 남았다
    /// (거노 2026-08-15 「소환할때 /rename 안되는거」). 실측으로 그 갈림을 확인했다:
    /// `/rename` 뒤에도 OSC 는 활동 요약이었고, 새 이름은 transcript 의 마지막
    /// custom-title 에만 있었다.
    ///
    /// **핀을 세워서** 옮긴다. 안 세우면 claude 가 계속 쏘는 활동 제목이 다음 프레임에
    /// 그대로 덮어, 이름이 한 번 깜빡이고 사라진다.
    ///
    /// ★ **사람이 정한 이름이 이긴다.** 지금 제목이 우리가 마지막에 심은 값과 다르면
    /// 그 사이 누군가(`surface.rename`·`kasaspace_rename`·파일 탭)가 직접 정한 것이라
    /// 비켜선다. 처음 보는 pane 에 이미 핀이 서 있으면 그것도 남의 것이다.
    ///
    /// 같은 꼬리에서 **ultracode 상태**도 함께 뽑는다(`ultra_verdict_in`). 두 사실이
    /// 같은 파일의 같은 512KB 에 있어서, 따로 읽으면 같은 바이트를 두 번 읽는다.
    pub(crate) fn sync_session_titles(&mut self) {
        // transcript 꼬리를 읽는 일이라 프레임 박자로 돌 것이 아니다. rename 도
        // `/effort` 도 사람이 한 번 치는 사건이라 이 정도면 「치자마자」로 읽힌다.
        {
            let mut last = session_title_scan().lock().unwrap();
            if last.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(2)) {
                return;
            }
            *last = Some(Instant::now());
        }
        self.sync_session_titles_now();
    }

    /// 박자를 무시한 한 바퀴 — 하네스가 한 프레임 안에서 시나리오를 이어 돌린다.
    /// 제품 경로는 언제나 `sync_session_titles` 로 들어온다.
    pub(crate) fn sync_session_titles_now(&mut self) {
        const WINDOW: u64 = 512 * 1024;
        let panes: Vec<(String, String)> = self
            .pane_claude_sid
            .iter()
            .filter(|(id, _)| self.pty.contains_key(id.as_str()))
            .map(|(id, sid)| (id.clone(), sid.clone()))
            .collect();
        for (pane_id, sid) in panes {
            let Some(path) = crate::socket::transcript_path_for_session(&sid) else { continue };
            let meta = std::fs::metadata(&path).ok();
            let mtime = meta.as_ref().and_then(|m| m.modified().ok());
            let len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            {
                let seen = session_titles().lock().unwrap();
                // 파일이 그대로면 이름도 상태도 그대로다 — 안 읽는다. 노는 pane 은
                // 여기서 전부 걸러지고, 읽는 것은 지금 말하고 있는 pane 뿐이다.
                if mtime.is_some()
                    && seen.get(&pane_id).is_some_and(|e| e.mtime == mtime && e.sid == sid)
                {
                    continue;
                }
            }
            // 512KB — `pane_bg_active` 가 같은 파일에 쓰는 창과 같은 값이다. 두 곳이
            // 따로 읽는 건 중복이지만, 캐시를 하나로 묶으면 `struct App` 에 필드가
            // 붙는다(병렬 작업 핫스팟, CLAUDE.md). 같은 파일이라 페이지 캐시가 받는다.
            let (tail, _) = crate::socket::read_tail(&path, WINDOW);
            let found = tail
                .lines()
                .filter_map(kasa_socket::sessions::custom_title_of_line)
                .last()
                .or_else(|| {
                    // 아직 아무 제목도 없는 갓 소환된 학생 — 받은 첫 지시(브리프)를
                    // 제목으로 굳힌다. nameSource=user 로 남겨 ①하네스 자동 제목이
                    // 덮지 않고(그쪽은 user 를 사람 개명으로 보고 보호) ②나중에 사람이
                    // /rename 하면 더 나중 레코드라 그게 이긴다. 한 번 심으면 다음
                    // 틱부터 custom-title 이 잡혀 이 폴백을 다시 안 탄다.
                    let label = kasa_socket::sessions::first_prompt_label(&tail)?;
                    append_boot_title(&path, &sid, &label);
                    Some(label)
                });
            let mut seen = session_titles().lock().unwrap();
            let entry = seen.entry(pane_id.clone()).or_default();
            // pane 이 다른 세션을 물면 기준선을 새로 잡는다 — 옛 대화의 마커를 이
            // 세션 것으로 읽으면 안 된다.
            if entry.sid != sid {
                *entry = PaneTranscript { sid: sid.clone(), ..Default::default() };
            }
            entry.mtime = mtime;
            // 이 세션을 처음 본 순간의 파일 크기가 기준선이다. 그 앞의 effort 마커는
            // **지난 실행**이 남긴 것이다 — 같은 jsonl 에 `--resume` 이 이어 쓰기
            // 때문이다. 훅(ultracode-mark.py)은 프로세스 시작 시각으로 같은 선을
            // 긋는데, 앱은 자기가 이 세션을 언제 물었는지 아니 파일 크기로 곧장
            // 자를 수 있다.
            let baseline = *entry.baseline.get_or_insert(len);
            // 기준선 이후로 자란 부분만 본다. 꼬리가 기준선보다 앞에서 시작하면 그
            // 앞부분을 잘라낸다.
            let tail_start = len.saturating_sub(WINDOW);
            let cut = usize::try_from(baseline.saturating_sub(tail_start)).unwrap_or(usize::MAX);
            if let Some(fresh) = tail.get(cut..).or_else(|| (cut >= tail.len()).then_some("")) {
                // 마커가 없으면 이전 판정을 지운다 — 세션을 갈아탄 뒤라면 옛 상태를
                // 물려받으면 안 된다. 마커가 있으면 그것이 훅 표식보다 최신이다.
                entry.ultra = ultra_verdict_in(fresh);
            }
            // 꼬리 밖으로 밀려 안 보이는 것은 「이름이 없어졌다」가 아니다 — 대화가
            // 길어지면 옛 스탬프는 512KB 밖으로 나간다. 심어 둔 이름을 걷지 않는다.
            let Some(name) = found else { continue };
            let mut ws = self.ws.lock().unwrap();
            let Some(pane) = ws.panes.get_mut(&pane_id) else { continue };
            if pane.title.as_deref() == Some(name.as_str()) {
                entry.title = Some(name);
                continue;
            }
            if pane.title_pinned && pane.title != entry.title {
                continue;
            }
            pane.title = Some(name.clone());
            pane.title_pinned = true;
            entry.title = Some(name);
            self.chrome_dirty = true;
        }
    }

    /// 꼬리 스캔이 내린 ultracode 판정 — `refresh_pane_ultracode` 가 훅 표식과 합칠 때
    /// 쓴다. 표가 작아(살아 있는 claude pane 수) 통째로 복제하는 편이 락을 들고
    /// 다니는 것보다 낫다.
    pub(crate) fn scanned_ultracode(&self) -> HashMap<String, bool> {
        session_titles()
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(pane, e)| e.ultra.map(|v| (pane.clone(), v)))
            .collect()
    }

    /// 이 pane 을 `--effort ultracode` 로 되살렸다고 표시한다. 제품 경로는
    /// `restore_leaf` 하나뿐이고, 하네스가 같은 문을 써야 재는 것과 도는 것이 같다.
    pub(crate) fn mark_restored_ultracode(&self, pane: &str) {
        restored_ultracode().lock().unwrap().insert(pane.to_string());
    }

    /// 복원이 `--effort ultracode` 로 되살린 pane 들 — 아직 아무 마커도 안 생긴
    /// 구간의 기준선이다. 사라진 pane 은 여기서 함께 걷는다(id 는 재사용된다).
    pub(crate) fn restored_ultracode_panes(&self) -> std::collections::HashSet<String> {
        let mut set = restored_ultracode().lock().unwrap();
        set.retain(|id| self.pty.contains_key(id));
        set.clone()
    }
}

/// 갓 소환된 학생 pane 에 "받은 첫 지시"를 pane 제목으로 굳히는 custom-title
/// 레코드를 transcript 에 덧붙인다. claude `/rename` 이 남기는 것과 같은 한 줄이되
/// nameSource=user 라 하네스(`kasaterm-title-sync.py`)가 사람 개명으로 보고 보호하고,
/// 나중에 진짜 `/rename` 이 오면 더 나중 레코드라 그게 이긴다. 라인 단위 append 라
/// 라이브 transcript 에 안전하다(claude·하네스도 같은 방식). 실패는 조용히 넘긴다 —
/// 다음 틱에 다시 시도하고, 그 사이 제목이 없을 뿐이다.
fn append_boot_title(path: &std::path::Path, sid: &str, title: &str) {
    use std::io::Write;
    let line = serde_json::json!({
        "type": "custom-title",
        "customTitle": title,
        "sessionId": sid,
        "nameSource": "user",
    })
    .to_string()
        + "\n";
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// pane 하나에 대해 transcript 꼬리에서 알아낸 것들.
#[derive(Default, Clone)]
struct PaneTranscript {
    /// 이 값이 그대로면 파일을 다시 안 읽는다.
    mtime: Option<std::time::SystemTime>,
    /// 우리가 마지막으로 심은 제목 — 남이 바꿨는지 가르는 기준.
    title: Option<String>,
    /// 이 세션을 처음 봤을 때의 파일 크기. 그 앞의 effort 마커는 지난 실행 것이다.
    baseline: Option<u64>,
    /// 기준선 이후 마지막 effort 마커. `None` = 이번 실행 구간엔 마커가 없다.
    ultra: Option<bool>,
    /// 기준선을 잡을 때의 세션 id — 바뀌면 위 값들을 통째로 버린다.
    sid: String,
}

/// pane 별 transcript 파생 상태.
///
/// `struct App` 이 아니라 모듈 static 인 이유는 그 struct 가 병렬 작업의 충돌
/// 핫스팟이기 때문이다(CLAUDE.md).
fn session_titles() -> &'static std::sync::Mutex<HashMap<String, PaneTranscript>> {
    static C: std::sync::OnceLock<std::sync::Mutex<HashMap<String, PaneTranscript>>> =
        std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// 마지막으로 훑은 시각 — 꼬리 읽기의 박자를 잡는다.
fn session_title_scan() -> &'static std::sync::Mutex<Option<Instant>> {
    static C: std::sync::OnceLock<std::sync::Mutex<Option<Instant>>> = std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// 복원이 `--effort ultracode` 로 되살린 pane 들. 훅의 argv 기준선과 같은 구실이다
/// (`collab-hooks/ultracode-mark.py` 의 `_proc_start`) — 그쪽은 `ps` 로 argv 를 캐고
/// 이쪽은 자기가 띄운 명령을 그냥 안다.
fn restored_ultracode() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static C: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// 이 구간에서 **가장 뒤에 있는** effort 마커의 판정. 없으면 `None`.
///
/// ⚠️ **니들 셋과 last-wins 규칙은 `collab-hooks/ultracode-mark.py` 의 `NEEDLES` 와
/// 같아야 한다.** 훅은 프롬프트를 보낼 때 돌고 이쪽은 앱이 훑는다 — 두 벌이 같은
/// 사실을 다르게 읽으면 글로우와 저장이 갈리고, 한쪽만 고친 날 조용히 어긋난다.
/// 훅이 python 이라 상수를 공유할 길이 없어 주석으로 못 박는다. 한쪽을 고치거든
/// 다른 쪽도 같이.
///
/// `Set effort level to` 는 `/effort` 가 화면에 뱉는 줄이라 **ultracode 가 아닌 값도
/// 잡아야 한다** — ultracode 에서 xhigh 로 내린 것도 이 줄로만 알 수 있다.
fn ultra_verdict_in(text: &str) -> Option<bool> {
    const ENTER: &str = r#""type":"ultra_effort_enter""#;
    const EXIT: &str = r#""type":"ultra_effort_exit""#;
    const CMD: &str = r#""content":"<local-command-stdout>Set effort level to "#;
    let mut best: Option<(usize, bool)> = None;
    let mut take = |at: Option<usize>, on: bool| {
        if let Some(i) = at {
            if best.is_none_or(|(b, _)| i > b) {
                best = Some((i, on));
            }
        }
    };
    take(text.rfind(ENTER), true);
    take(text.rfind(EXIT), false);
    if let Some(i) = text.rfind(CMD) {
        take(Some(i), text[i + CMD.len()..].starts_with("ultracode"));
    }
    best.map(|(_, on)| on)
}

/// 경로 앞의 홈을 `~` 로 접는다. 홈을 못 찾으면 원본 그대로.
///
/// 같은 코드가 pane 라벨·상태바·미리보기에 네 벌 흩어져 있었고, 전부 `HOME` 을
/// 직접 읽어 Windows(GUI 프로세스엔 HOME 이 없다)에서 한 곳도 안 접혔다.
/// `home_dir()` 은 `USERPROFILE` 까지 본다.
/// `ps eww` 한 줄에서 자격증명 저장소 경로를 뽑는다. 변수가 아예 없으면 `""`
/// (= 기본 로그인으로 떠 있음). 공백이 든 경로는 첫 토막만 잡혀 어긋난 값이
/// 되지만, 그건 "다르다" 쪽 판정이라 재시작으로 수습되는 무해한 방향이다.
fn parse_securestorage_dir(ps_line: &str) -> String {
    ps_line
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix("CLAUDE_SECURESTORAGE_CONFIG_DIR="))
        .unwrap_or("")
        .to_string()
}

/// 저장소 경로 → 계정 id(`…/claude-accounts/<id>` 의 꼬리). `""` 는 기본 로그인이라
/// 그대로 — `claude_account_display` 가 목록에 없는 id 를 기본 계정으로 접는다.
fn account_id_of_dir(dir: &str) -> &str {
    if dir.is_empty() {
        return "";
    }
    dir.rsplit(['/', '\\']).next().unwrap_or(dir)
}

/// 전환 토스트 문구 — 자동·메뉴·설정창 세 진입점이 같은 문장을 쓴다.
/// `same` 은 이미 활성인 계정을 다시 누른 경우(전환이 아니라 「맞추기」).
/// `live` 는 작업대 갈아 끼우기가 성공한 경우 — 떠 있는 pane 이 **다음
/// 메시지부터** 새 계정이므로 「다음에 뜨는 claude 부터」라고 말하면 거짓말이
/// 된다(2026-08-17 「토스트에 다음세션부터라고 뜨는데」). 실패(금고 비었음·
/// 쓰기 실패)일 때만 재시작 폴백이 전부라 옛 문장이 맞다.
/// 확인 카드를 **어느 창에 그리는가**. 카드를 눌렀는데 뒤 창에 확인이 뜨면 그건
/// 확인이 아니다 — 설정 별도창에서 누른 것은 그 창에 그린다.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ConfirmSurface {
    Main,
    Settings,
}

/// 학생을 바꿀 때 「다시 띄울까」를 묻는 카드의 상태.
///
/// 이름·얼굴·색은 바로 바뀌지만 **말투는 pane 이 뜰 때 정해져 굳는다** — 도는
/// 프로세스의 시스템 프롬프트는 누구도 못 바꾼다. 그래서 말투까지 지금 맞추려면
/// 다시 띄우는 수밖에 없고, 그건 사용자에게 물어야 하는 조작이다(2026-08-25 지시:
/// 「그럼 새로띄우게해 테마 바꾸면 확인버튼도 만들고」).
pub(crate) struct PendingCharacterSwap {
    pub pane: String,
    pub to: String,
    /// 이어붙일 대화가 있는가. 없으면 다시 띄우기가 지금 내용을 잃으므로 문구와
    /// 버튼 색이 갈린다 — 계정 전환 카드의 `fresh` 와 같은 규칙이다.
    pub resumable: bool,
    /// 그 창의 render 가 매 프레임 채운다(`PendingAccountSwitch::rects` 와 같은 이유).
    pub rects: Vec<(CharacterSwapBtn, (f32, f32, f32, f32))>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CharacterSwapBtn {
    Cancel,
    /// 이름·얼굴·색만 지금 바꾼다. 말투는 다음에 그 pane 에서 띄울 때부터.
    ShellOnly,
    /// 대화를 이어서 다시 띄운다 — 말투까지 지금 바뀐다.
    Relaunch,
}

/// 받침에 맞춘 「으로/로」. 학생 이름은 사람이 읽는 문장 안에 그대로 들어가므로
/// 조사가 어긋나면 바로 눈에 띈다(2026-08-25 실측 캡처: 「은랑 로 바꿀까요?」).
///
/// ㄹ 받침은 「로」다 — 「서울로」. 한글이 아닌 이름(로마자·기호)도 「로」로 둔다.
fn euro_ro(word: &str) -> &'static str {
    match word.chars().last() {
        Some(c) if ('가'..='힣').contains(&c) => {
            let jong = (c as u32 - 0xAC00) % 28;
            if jong == 0 || jong == 8 {
                "로"
            } else {
                "으로"
            }
        }
        _ => "로",
    }
}

/// 학생을 바꿀 때 물을 것인가, 그냥 바꿀 것인가.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SwapPlan {
    /// 확인 없이 지금 바꾼다 — 바뀌는 것이 이름·얼굴·색뿐이라 되돌릴 것이 없다.
    Now,
    /// 다시 띄울지 묻는다. `resumable` 이 false 면 되띄우기가 지금 내용을 버린다.
    Ask { resumable: bool },
}

/// 위 갈림의 순수부. `App` 을 안 들고 다니므로 테스트가 된다.
///
/// 카드를 띄우는 유일한 이유는 **다시 띄워야 하는 것**이다. 말투가 꺼져 있으면
/// 바뀌는 건 껍데기뿐이라 되띄울 이유가 없고(2026-08-25 지시: 「말투 오프돼있으면
/// 그냥 껍데기만바뀌게」), 에이전트가 안 도는 pane 도 되띄울 대화가 없다. 둘 중
/// 하나라도 해당하면 묻지 않고 바로 바꾼다 — 묻지 않아도 되는 것을 묻는 카드는
/// 그 자체가 방해다.
pub(crate) fn plan_character_swap(
    persona_on: bool,
    agent: Option<&str>,
    has_convo: bool,
) -> SwapPlan {
    if !persona_on || agent.is_none() {
        return SwapPlan::Now;
    }
    SwapPlan::Ask { resumable: has_convo }
}

/// 카드에 적을 제목과 본문. `App` 을 안 들고 다니므로 테스트가 된다.
pub(crate) fn character_swap_confirm_text(to: &str, resumable: bool) -> (String, Vec<String>) {
    let title = format!("이 자리를 {to}{} 바꿀까요?", euro_ro(to));
    let lines = if resumable {
        vec![
            "다시 띄우면 말투까지 바뀝니다 — 나눈 대화는 이어서 띄우니 그대로예요.".to_string(),
            "껍데기만 바꾸면 이름·얼굴·색만 지금 바뀌고, 말투는 다음에 띄울 때부터입니다."
                .to_string(),
        ]
    } else {
        vec![
            "이어붙일 대화가 없어, 다시 띄우면 지금 내용이 사라집니다.".to_string(),
            "껍데기만 바꾸면 이름·얼굴·색만 바뀌고 이 자리는 그대로예요.".to_string(),
        ]
    };
    (title, lines)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AccountSwitchBtn {
    Cancel,
    Switch,
}

/// 대기 중인 계정 전환 확인.
///
/// **여기 담긴 것 말고는 아무 상태도 미리 바뀌지 않는다** — 작업대 갈아 끼우기·신원
/// 캐시·⟳ 칩·설정 저장·재시작은 [전환] 을 눌러야 그때 돈다. 취소하면 이 값을
/// 버리는 것으로 끝이다.
pub(crate) struct PendingAccountSwitch {
    pub to: String,
    pub to_label: String,
    pub impact: AccountSwitchImpact,
    pub surface: ConfirmSurface,
    /// 그 창의 render 가 매 프레임 채운다. hit rect 를 별도 App 필드로 두는 것이
    /// 레포 관례지만, `struct App` 정의는 병렬 작업 충돌 핫스팟이라 여기 담아 필드
    /// 하나로 줄였다(CLAUDE.md 병렬 규칙).
    pub rects: Vec<(AccountSwitchBtn, (f32, f32, f32, f32))>,
}

/// 계정 전환이 pane 하나에 무엇을 하는지 정하는 데 필요한 **사실만**. `App` 을 안
/// 들고 다니므로 테스트가 된다.
pub(crate) struct PaneAccountFact {
    pub id: String,
    /// `ps` 로 실측한 부팅 저장소. `None` = 실측 실패(claude 가 아니거나 Windows) —
    /// 어긋났을 수 있다는 쪽으로 보수 판정한다.
    pub boot_dir: Option<String>,
    pub focused: bool,
    pub closed: bool,
    pub busy: bool,
    /// `--resume` 할 transcript 가 실재하나. false 면 대화를 잃고 새로 뜬다.
    pub resumable: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PaneAccountFate {
    /// 이미 목표 계정 — 아무 일도 안 일어난다.
    Unchanged,
    /// 쉬는 pane — 조용해진 지 4초가 지나면 되띄운다.
    RestartWhenQuiet,
    /// 일하는 중 — 턴이 끝난 뒤에 되띄운다.
    RestartAfterTurn,
    /// 보고 있는 pane — 자동으로 안 끊고 ⟳ 칩만 단다.
    ChipFocused,
    /// 닫힌 pane — 되살릴 때 칩이 안내한다.
    ChipClosed,
}

/// ⚠️ **판정 순서가 `run_pending_account_restarts` 와 같아야 한다.** 어긋나면 물어본
/// 내용과 실제로 벌어지는 일이 갈린다 — 「3개가 다시 떠요」라고 해 놓고 5개가 뜨는 식.
pub(crate) fn pane_account_fate(f: &PaneAccountFact, target_dir: &str) -> PaneAccountFate {
    if f.boot_dir.as_deref() == Some(target_dir) {
        return PaneAccountFate::Unchanged;
    }
    if f.focused {
        return PaneAccountFate::ChipFocused;
    }
    if f.closed {
        return PaneAccountFate::ChipClosed;
    }
    if f.busy {
        return PaneAccountFate::RestartAfterTurn;
    }
    PaneAccountFate::RestartWhenQuiet
}

/// 되띄우기를 미뤄야 하는 pane 인가. 러너와 계산기가 **같은 이 함수**를 쓴다.
///
/// 활동 기록이 아직 없는 pane(`None`)도 바쁜 것으로 친다 — 방금 뜬 pane 을 그 자리에서
/// 끊지 않으려는 기존 규칙 그대로다.
pub(crate) fn account_restart_busy(
    prompt_wait: bool,
    activity: Option<(&str, bool)>,
) -> bool {
    prompt_wait || activity.is_none_or(|(status, bg)| status != "idle" || bg)
}

/// 전환 한 번이 지금 떠 있는 pane 들에 하는 일의 총계.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct AccountSwitchImpact {
    pub unchanged: usize,
    pub restart_when_quiet: usize,
    pub restart_after_turn: usize,
    pub chip_focused: usize,
    pub chip_closed: usize,
    /// 위 재시작 대상 중 **이어붙일 대화가 없어** 새로 뜨는 수.
    pub fresh: usize,
    /// 어느 계정으로 떴는지 실측이 안 된 수(Windows 등).
    pub unmeasured: usize,
}

impl AccountSwitchImpact {
    /// 실제로 뜯겼다 다시 뜨는 pane 수.
    pub fn torn_down(&self) -> usize {
        self.restart_when_quiet + self.restart_after_turn
    }

    /// 물어봐야 하는가. **보고 있는 pane 과 닫힌 pane 은 세지 않는다** — 둘 다 자동으로
    /// 아무 일도 안 당하고, 보고 있는 pane 은 ⟳ 칩 자체가 이미 확인이라 게이트에 넣으면
    /// 한 가지를 두 번 묻는 꼴이 된다.
    pub fn needs_confirm(&self) -> bool {
        self.torn_down() > 0
    }
}

pub(crate) fn account_switch_impact(
    facts: &[PaneAccountFact],
    target_dir: &str,
) -> AccountSwitchImpact {
    let mut i = AccountSwitchImpact::default();
    for f in facts {
        let fate = pane_account_fate(f, target_dir);
        match fate {
            PaneAccountFate::Unchanged => i.unchanged += 1,
            PaneAccountFate::RestartWhenQuiet => i.restart_when_quiet += 1,
            PaneAccountFate::RestartAfterTurn => i.restart_after_turn += 1,
            PaneAccountFate::ChipFocused => i.chip_focused += 1,
            PaneAccountFate::ChipClosed => i.chip_closed += 1,
        }
        // 대화를 잃는 것은 **되띄우는 pane** 에서만 일어난다. 칩만 다는 pane 을 세면
        // 「대화가 날아간다」는 경고가 아무 일도 안 당하는 pane 때문에 켜진다.
        let restarting = matches!(
            fate,
            PaneAccountFate::RestartWhenQuiet | PaneAccountFate::RestartAfterTurn
        );
        if restarting && !f.resumable {
            i.fresh += 1;
        }
        if restarting && f.boot_dir.is_none() {
            i.unmeasured += 1;
        }
    }
    i
}

/// 확인 카드 문구 — (제목, 부제).
///
/// ⚠️ **「진행 중인 작업이 끊겨요」라고 쓰지 마라. 거짓말이다.**
/// `run_pending_account_restarts` 가 일하는 pane 을 일곱 겹으로 걸러 턴이 끝난 뒤에만
/// 되띄운다. 실제로 잃는 것은 셋뿐이다 — 그 pane 의 **화면(스크롤백)**, 입력창에 쳐
/// 놓고 안 보낸 글, 그리고 이어붙일 대화가 없는 pane 의 **대화 전체**.
pub(crate) fn account_switch_confirm_text(
    to_label: &str,
    i: &AccountSwitchImpact,
) -> (String, Vec<String>) {
    let title = format!("claude {}개가 다시 떠요", i.torn_down());
    // 절을 가운뎃점으로 잇지 않고 **줄로 나눈다** — 사정이 셋만 겹쳐도 한 줄이
    // 카드 밖으로 나가고, 그러면 정작 읽어야 할 마지막 절이 잘린다.
    let mut lines =
        vec![format!("{to_label} 로 바꾸면 대화는 이어지지만 그 pane 화면은 비워져요")];
    if i.restart_after_turn > 0 {
        lines.push(format!("작업 중 {}개는 턴이 끝난 뒤에 떠요", i.restart_after_turn));
    }
    if i.fresh > 0 {
        lines.push(format!("{}개는 이어붙일 대화가 없어 새로 떠요", i.fresh));
    }
    if i.chip_focused > 0 {
        lines.push("지금 보는 pane 은 ⟳ 를 눌러야 바뀌어요".to_string());
    }
    if i.unmeasured > 0 {
        lines.push(format!("{}개는 어느 계정인지 확인이 안 돼 함께 띄워요", i.unmeasured));
    }
    (title, lines)
}

/// 전환 **전에** 목표 저장소 경로를 예측한다.
///
/// `runtime_dir_for(to, to)` 를 그냥 부르면 안 되는 이유: 그 함수는 지문이 이미 `to` 를
/// 가리킬 때만 빈 경로(작업대)를 준다. 전환 전에는 지문이 아직 옛 계정이라 금고 경로가
/// 나오고, 그러면 작업대로 뜬 pane 이 **전부** 어긋남으로 잡혀 영향 수가 과대계상된다.
///
/// 예측이 틀릴 때는 **많이 세는 쪽**으로 틀린다 — 더 물어보는 것은 안전하고, 덜 묻는
/// 것은 사고다.
pub(crate) fn predicted_target_dir(
    to: &str,
    workbench_live: bool,
    vault_ready: bool,
    vault_dir: Option<&std::path::Path>,
) -> String {
    let vault = || vault_dir.map_or(String::new(), |p| p.to_string_lossy().into_owned());
    if to.is_empty() {
        // 기본 슬롯은 금고 경로 자체가 없다 — 작업대가 곧 그 자리다.
        return String::new();
    }
    if !workbench_live || !vault_ready {
        // `swap_active` 가 WriteFailed / VaultEmpty 로 물러날 자리 — 작업대는 안 갈리고
        // 재시작 폴백만 돈다.
        return vault();
    }
    String::new()
}

/// 그 테마의 전원 이름. `__base` 는 번들(활성 테마를 뺀 기본) 로스터다.
fn theme_roster_names(theme: &str) -> Vec<String> {
    let chars = if theme == kasa_mcp::character::BASE_THEME_KEY {
        kasa_mcp::character::base_characters_json()
    } else {
        kasa_mcp::character::theme_characters_json(theme)
    };
    chars.as_ref().map(kasa_mcp::character::member_names).unwrap_or_default()
}

pub(crate) fn account_switch_toast(
    to_label: &str,
    same: bool,
    restarted: usize,
    deferred: usize,
    focused_pending: bool,
    live: bool,
) -> String {
    let tail = if focused_pending {
        " · 지금 이 pane 은 ⟳ 를 누르면"
    } else {
        ""
    };
    if restarted == 0 && deferred == 0 {
        return if same {
            format!("{to_label} 그대로예요 — 떠 있는 claude 도 전부 이 계정이에요")
        } else if live {
            format!("{to_label} 로 전환했어요 — 떠 있는 claude 도 다음 메시지부터예요{tail}")
        } else {
            format!("{to_label} 로 전환했어요 (다음에 뜨는 claude 부터){tail}")
        };
    }
    let head = if same {
        format!("{to_label} 로 맞추는 중")
    } else {
        format!("{to_label} 로 전환")
    };
    let mut parts = Vec::new();
    if restarted > 0 {
        parts.push(format!("claude {restarted}개 대화 이어서 다시 띄움"));
    }
    // 보고 있는 pane 은 이 수에 들어가도 자동으로 안 돈다 — 그래서 문장 끝에서
    // 따로 말한다. 「끝나면 자동」만 적으면 기다리다 영영 안 바뀌는 것으로 보인다.
    let auto_deferred = deferred.saturating_sub(usize::from(focused_pending));
    if auto_deferred > 0 {
        parts.push(format!("작업 중 {auto_deferred}개는 끝나면 자동"));
    }
    format!("{head} — {}{tail}", parts.join(" · "))
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

fn restored_scrollback(rec: &serde_json::Value, restarting_agent: bool) -> Vec<String> {
    if restarting_agent {
        return Vec::new();
    }
    rec.get("scrollback")
        .and_then(|v| v.as_array())
        .map(|lines| {
            lines
                .iter()
                .filter_map(|line| line.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// 복원되는 pane 이 쓸 id 를 고른다. **저장된 id 를 최우선**으로 되살린다 —
/// `--resume` 으로 되살아난 학생은 재시작 전의 surface_id 를 대화 기록째 기억하고
/// 있어서, 번호를 새로 매기면 `tell` 이 없는 pane 이거나 그 사이 다른 pane 이
/// 물려받은 번호로 배달된다(거노: "재시작하면 학생들이 tell 을 이상한 pane 에 쓴다").
///
/// 저장본에 id 가 없거나(옛 포맷) 이미 쓰이는 번호면 새로 발급한다. 되살린 번호가
/// 카운터보다 크면 카운터를 그 위로 밀어, 이후 split 이 같은 번호를 다시 내주지
/// 않게 한다.
/// 저장된 leaf 가 어떤 하네스로 돌던 pane 인지 — 없으면 순수 셸.
///
/// 정본 키는 `was_agent`(`AgentKind::as_str` 이 쓴 id). 그 전 포맷은 `was_claude: true`
/// 뿐이라 **옛 저장본은 claude 로 읽는다** — 안 그러면 이번 판올림 한 번에 거노가 쓰던
/// 학생 pane 이 전부 셸로 되살아난다. 새 코드는 `was_agent` 만 쓴다(두 키를 같이 쓰면
/// 언젠가 갈린다).
///
/// 되읽기를 `AgentKind::from_id` 하나로 모은 이유: 예전엔 여기가 세 종류를 손으로
/// 나열했는데, 하네스가 서른이 된 지금 그 사본을 두면 표에만 있고 여기엔 없는
/// 하네스가 **재시작 한 번에 셸로 되살아난다**(학생·대화 이어가기가 통째로 빠진다).
fn saved_agent(rec: &serde_json::Value) -> Option<&'static str> {
    if let Some(kind) = rec
        .get("was_agent")
        .and_then(|v| v.as_str())
        .and_then(kasa_pty::AgentKind::from_id)
    {
        return Some(kind.as_str());
    }
    rec.get("was_claude")
        .and_then(|b| b.as_bool())
        .unwrap_or(false)
        .then_some("claude")
}

/// 임시 디렉토리 아래인가. 방 이름 후보에서 밀어내는 데 쓴다 — claude 가 pane 마다
/// 파는 `…/scratchpad/<슬러그>` 는 프로젝트가 아니라 세션 부산물이라, 그게 방
/// 이름이 되면 방이 무슨 일을 하는 곳인지 알려주지 못한다. 게다가 여러 방이 같은
/// 슬러그를 쓰면 사이드바에서 방끼리 구별되지 않는다(2026-08-24 지적: 서로 다른 두
/// 방이 나란히 `dogfood8-run3` 로 떴다).
fn is_temp_path(p: &std::path::Path) -> bool {
    // macOS 는 `/tmp`·`/var` 가 `/private` 아래 실경로를 갖는다 — cwd 를 `lsof` 로
    // 캐면 그 실경로로 돌아오므로 양쪽을 다 적는다.
    const ROOTS: &[&str] = &[
        "/tmp",
        "/private/tmp",
        "/var/tmp",
        "/private/var/tmp",
        "/var/folders",
        "/private/var/folders",
    ];
    if ROOTS.iter().any(|r| p.starts_with(r)) {
        return true;
    }
    // 위 목록에 없는 플랫폼 임시 루트(Windows 의 `%TEMP%`).
    let t = std::env::temp_dir();
    t.parent().is_some() && p.starts_with(&t)
}

/// 방을 대표하는 cwd — 사이드바 방 이름·부제의 원본. `panes` 는 leaf 순서대로
/// `(cwd, 학생이 앉은 pane 인가)`.
///
/// **방의 정체는 학생이 앉은 프로젝트다.** 곁다리 셸 하나가 다른 폴더에 있다고 방
/// 이름이 그리로 끌려가면, 사이드바에서 자리로 방을 찾던 눈이 매번 다시 읽어야
/// 한다(2026-08-24 지적: recall 방이 곁다리 셸 하나 때문에 임시폴더 이름을
/// 뒤집어썼다). 그래서 최빈값을 세기 전에 후보를 두 번 좁힌다.
///
/// ① 학생이 앉은 pane 이 하나라도 있으면 후보를 그 pane 들로 좁힌다 — 셸은 학생이
/// 하나도 없는 방(사람이 직접 쓰는 터미널 방)에서만 이름을 짓는다.
/// ② 그중 프로젝트 경로가 하나라도 있으면 임시 경로를 뺀다. **전부 임시면 남긴다** —
/// 스크래치패드에서만 도는 방은 그게 유일한 정체다.
///
/// 좁히고 남은 것의 최빈값, 동률이면 leaf 순서가 먼저인 쪽. leaves 순서가 고정이라
/// 같은 방이 늘 같은 이름을 얻는다.
fn room_home_cwd(panes: &[(std::path::PathBuf, bool)]) -> Option<std::path::PathBuf> {
    let agents: Vec<&std::path::PathBuf> =
        panes.iter().filter(|(_, a)| *a).map(|(p, _)| p).collect();
    let pool: Vec<&std::path::PathBuf> = if agents.is_empty() {
        panes.iter().map(|(p, _)| p).collect()
    } else {
        agents
    };
    let named: Vec<&std::path::PathBuf> =
        pool.iter().copied().filter(|p| !is_temp_path(p)).collect();
    let pool = if named.is_empty() { pool } else { named };

    pool.into_iter()
        .fold(Vec::<(&std::path::PathBuf, usize)>::new(), |mut acc, p| {
            match acc.iter_mut().find(|(q, _)| *q == p) {
                Some((_, c)) => *c += 1,
                None => acc.push((p, 1)),
            }
            acc
        })
        .into_iter()
        .reduce(|a, b| if b.1 > a.1 { b } else { a })
        .map(|(p, _)| p.clone())
}

/// 저장된 leaf 가 쓰던 모델 — 없으면 `None`(복원 명령에 플래그를 안 붙인다).
///
/// 옛 저장본엔 이 키가 없다. 그때 빈 문자열이 아니라 `None` 이어야 하는 이유는,
/// 호출부가 "없으면 플래그 자체를 뺀다"로 갈리기 때문이다 — 빈 값을 흘리면
/// `--model ''` 이 나가 하네스가 기본값도 못 고른다.
fn saved_model(rec: &serde_json::Value) -> Option<&str> {
    rec.get("model").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
}

/// 저장된 leaf 가 쓰던 reasoning effort — 없으면 `None`. `saved_model` 과 같은 규약.
fn saved_effort(rec: &serde_json::Value) -> Option<&str> {
    rec.get("effort").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
}

/// 복원된 pane 에 넣을 명령. 하네스 셋 × (이어가기/새로)라 순수 함수로 뺐다 —
/// `restore_leaf` 는 살아있는 PTY 없이 못 부르고, 그러면 이 분기를 테스트할 방법이
/// 사라진다.
///
/// ⚠️ **마지막 갈래가 claude 라는 게 함정이다.** 새 하네스를 여기 안 적으면 오류
/// 없이 claude 로 되살아나고, 이어가기까지 걸리면 남의 하네스 세션 id 로
/// `claude --resume` 을 친다. agy 를 붙일 때 실제로 그 상태였다(2026-08-11).
///
/// codex 도 이어간다. 셋을 실측으로 확인했다(2026-08-05):
/// - `codex resume <uuid>` 는 **pane 홈이 사라져도** 대화를 되살린다. shim 이 세운
///   pane 별 CODEX_HOME 은 GUI pid 별이라 재시작이면 통째로 없어지는데, `sessions` 가
///   `~/.codex/sessions` 심볼릭이라 실체가 남고 codex 가 거기서 찾아낸다(홈을 치우고
///   다른 pane 홈에서 resume 해 첫 질문까지 그대로 복원되는 것을 확인).
/// - 세션 id 는 `pane_claude_sid`(bind-transcript 훅)로 들어온다 — rollout 파일명에서
///   uuid 를 떼어낸 값이다. argv 로는 못 집는다.
/// - `resume --last` 는 쓰지 않는다. 그건 미러된 `~/.codex/sessions` 전체에서 최신 하나를
///   고르므로 **다른 pane·pane 밖 codex 의 대화**를 물어온다. id 가 없으면 새로 띄운다.
/// `model`/`effort` 는 끄기 직전 그 pane 이 쓰던 값이다(없으면 `None`). **없으면
/// 플래그 자체를 안 붙인다** — 빈 값을 넘기면 하네스가 기본값조차 못 고른다.
///
/// 문법이 셋 다 다르다(실측 2026-08-11): claude·agy 는 `--model`/`--effort` 플래그,
/// codex 는 `-m` 과 config 오버라이드(`-c model_reasoning_effort=`).
///
/// ⚠️ agy 에는 지금 model 이 실려 오지 않는다 — 전사본의 모델이 되먹일 수 없는
/// 표시용 이름(`Gemini 3.6 Flash (Low)`)이라 수집 쪽에서 일부러 안 담는다. 나중에
/// 담게 되거든 **`agy models` 목록과 대조하고 나서** 담아라: agy 는 없는 모델값에
/// 에러를 안 내고 조용히 기본값으로 돌아, 틀려도 아무 데도 안 남는다.
pub(crate) fn restore_agent_command(
    agent: Option<&str>,
    session_id: Option<&str>,
    resumable: bool,
    model: Option<&str>,
    effort: Option<&str>,
) -> String {
    // 값은 작은따옴표로 감싼다 — `claude-opus-5[1m]` 의 `[1m]` 이 zsh 글롭이라 무인용
    // 이면 "no matches found" 로 명령이 통째 실패한다. shim 쪽에서 같은 사고가 실제로
    // 났다(2026-07-27: 학생이 전부 구세대 Opus 로 떨어짐).
    let q = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
    let model = model.filter(|s| !s.is_empty());
    let effort = effort.filter(|s| !s.is_empty());
    let resume = session_id.filter(|_| resumable);
    let mut cmd = match (agent, resume) {
        (Some("codex"), Some(sid)) => format!("codex resume {sid}"),
        (Some("codex"), None) => "codex".to_string(),
        // agy 는 아직 세션 id 가 안 들어온다 — bind-transcript 훅이 claude·codex
        // shim 에만 걸려 있어서다. 그래도 하네스는 맞춰 띄운다: 여기 없으면 agy
        // pane 이 claude 로 되살아난다.
        (Some("agy"), Some(sid)) => format!("agy --conversation {sid}"),
        (Some("agy"), None) => "agy".to_string(),
        (_, Some(sid)) => format!("claude --resume {sid}"),
        (_, None) => "claude".to_string(),
    };
    if agent == Some("codex") {
        if let Some(m) = model {
            cmd.push_str(&format!(" -m {}", q(m)));
        }
        if let Some(e) = effort {
            cmd.push_str(&format!(" -c model_reasoning_effort={}", q(e)));
        }
    } else {
        // claude 는 shim 이 전역 `--model` 을 **앞에** 붙이는데, 뒤에 온 우리 값이
        // 이긴다(clap 은 같은 플래그를 마지막 것으로 덮는다). 그래서 pane 별 값이
        // 전역 설정을 넘어선다.
        if let Some(m) = model {
            cmd.push_str(&format!(" --model {}", q(m)));
        }
        if let Some(e) = effort {
            cmd.push_str(&format!(" --effort {}", q(e)));
        }
    }
    cmd.push('\r');
    cmd
}

/// pane 의 말투·모델·통로를 새 학생 것으로 갈아 둔다 — shim 이 다음 부팅에 읽는
/// override 파일(`repersona-<pane>.*`).
///
/// 도는 프로세스의 시스템 프롬프트는 못 바꾸므로 **지금 대화 중인 상대는 옛 말투
/// 그대로**고, 여기서 바꾸는 것은 다음에 그 pane 에서 claude 가 뜰 때부터다. 그런데
/// 바인딩·마커만 갱신하면 **다시 띄워도 옛 말투로 뜬다**: shim 이 spawn 때 고정된
/// `KASATERM_PERSONA`(옛 학생)로 `--append-system-prompt` 를 붙이고, SessionStart
/// 훅은 그 인자가 보이면 자기 주입을 건너뛰기 때문이다. 이 파일들이 그 사슬을 끊는다.
///
/// `self` 를 안 쓰므로 자유함수다 — 캐릭터를 갈아 끼우는 자리가 GUI(`App`)와 board
/// 빌드(`PtyBackend`) 양쪽에 있고, 한쪽만 갱신하면 그 경로로 바뀐 pane 만 말투가
/// 어긋난 채 남는다.
pub(crate) fn write_persona_override(pane: &str, character: &str) {
        let Ok(shim) = std::env::var("KASATERM_TMUX_SHIM_DIR") else { return };
        let dir = std::path::Path::new(&shim);
        let persona = if socket::read_claude_persona() {
            kasa_mcp::character::characters_json()
                .and_then(|c| kasa_mcp::character::persona_for(&c, character))
                .or_else(|| kasa_mcp::character::persona_for_any(character))
                .unwrap_or_default()
        } else {
            String::new()
        };
        let base = dir.join(format!("repersona-{pane}"));
        let _ = std::fs::write(base.with_extension("persona"), persona);
        let _ = std::fs::write(base.with_extension("character"), character);
        // 모델·통로도 새 캐릭터 것으로 — 안 맞추면 이름과 얼굴만 바뀌고 앞 학생의
        // 모델로 계속 돈다(학생 명령이 넷을 함께 쓰는 것과 같은 이유).
        let chars = kasa_mcp::character::characters_json();
        let pick = |f: fn(&serde_json::Value, &str) -> Option<String>| {
            chars.as_ref().and_then(|c| f(c, character)).unwrap_or_default()
        };
        let _ = std::fs::write(
            base.with_extension("model"),
            pick(kasa_mcp::character::model_for),
        );
        let _ = std::fs::write(
            base.with_extension("backend"),
            pick(kasa_mcp::character::backend_for),
        );
    }


/// [`App::stashed_record`] 의 판정 본체. App 없이 검사할 수 있게 갈라 뒀다.
fn stashed_in<'a>(list: &'a [crate::ClosedPane], pane: &str) -> Option<&'a crate::ClosedPane> {
    list.iter().find(|c| c.alive && c.pane_id == pane)
}

/// [`App::closed_pane_index`] 의 판정 본체.
fn closed_index_in(list: &[crate::ClosedPane], pane: &str) -> Option<usize> {
    list.iter()
        .position(|c| c.alive && c.pane_id == pane)
        .or_else(|| list.iter().position(|c| c.pane_id == pane))
}

/// 쓰이는 번호 집합에서 빠진 **가장 작은** `%N`.
fn next_free_pane_id(used: &std::collections::HashSet<String>) -> String {
    (0u32..).map(|n| format!("%{n}")).find(|id| !used.contains(id)).unwrap_or_default()
}

/// 저장본의 pane 번호를 그대로 되살릴 수 있으면 그것. 없거나(옛 저장본) 형식이
/// 깨졌거나 이미 살아 있으면 `None` — 그때는 호출부가 `alloc_pane_id` 로 새로 받는다.
fn pick_restore_id(saved: Option<&str>, taken: impl Fn(&str) -> bool) -> Option<String> {
    let s = saved?;
    s.strip_prefix('%').and_then(|d| d.parse::<u32>().ok())?;
    (!taken(s)).then(|| s.to_string())
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
    use super::{
        account_restart_busy, account_switch_confirm_text, account_switch_impact,
        editor_command_line, git_repo_root, next_free_pane_id, pane_account_fate,
        pick_restore_id, predicted_target_dir, restored_scrollback, AccountSwitchImpact,
        PaneAccountFact, PaneAccountFate,
    };

    fn fact(boot: Option<&str>) -> PaneAccountFact {
        PaneAccountFact {
            id: "%1".into(),
            boot_dir: boot.map(str::to_string),
            focused: false,
            closed: false,
            busy: false,
            resumable: true,
        }
    }

    /// 판정 순서가 `run_pending_account_restarts` 와 같아야 한다. 어긋나면 물어본
    /// 내용과 실제로 벌어지는 일이 갈린다.
    #[test]
    fn a_pane_already_on_the_target_is_left_alone() {
        assert_eq!(pane_account_fate(&fact(Some("")), ""), PaneAccountFate::Unchanged);
        assert_eq!(
            pane_account_fate(&fact(Some("/v/acct-2")), ""),
            PaneAccountFate::RestartWhenQuiet
        );
        // 맞는 계정이면 보고 있어도 칩조차 안 뜬다 — Unchanged 가 focused 를 이긴다.
        let mut f = fact(Some(""));
        f.focused = true;
        assert_eq!(pane_account_fate(&f, ""), PaneAccountFate::Unchanged);
    }

    /// 실측이 안 된 pane 을 「그대로다」로 읽으면, 실제로는 재시작되는데 아무 말도
    /// 안 하고 넘어간다. 모를 때는 어긋난 쪽으로 센다.
    #[test]
    fn an_unmeasured_pane_is_never_counted_as_unchanged() {
        assert_ne!(pane_account_fate(&fact(None), ""), PaneAccountFate::Unchanged);
        let i = account_switch_impact(&[fact(None)], "");
        assert_eq!(i.unmeasured, 1);
        assert!(i.needs_confirm());
    }

    /// 보고 있는 pane 은 ⟳ 칩만 달리고 자동으로 안 끊긴다 — busy 여부보다 먼저다.
    #[test]
    fn the_pane_you_are_watching_is_only_chipped() {
        let mut f = fact(Some("/v/acct-2"));
        f.focused = true;
        f.busy = true;
        assert_eq!(pane_account_fate(&f, ""), PaneAccountFate::ChipFocused);
        let mut c = fact(Some("/v/acct-2"));
        c.closed = true;
        c.busy = true;
        assert_eq!(pane_account_fate(&c, ""), PaneAccountFate::ChipClosed);
    }

    /// 칩만 다는 pane 으로는 묻지 않는다 — 아무 일도 안 당하므로 물으면 한 가지를
    /// 두 번 묻는 꼴이다.
    #[test]
    fn only_panes_that_actually_restart_trigger_the_question() {
        let mut focused = fact(Some("/v/acct-2"));
        focused.focused = true;
        let mut closed = fact(Some("/v/acct-2"));
        closed.closed = true;
        assert!(!account_switch_impact(&[focused, closed], "").needs_confirm());

        assert!(!account_switch_impact(&[fact(Some(""))], "").needs_confirm());

        let mut busy = fact(Some("/v/acct-2"));
        busy.busy = true;
        assert!(account_switch_impact(&[busy], "").needs_confirm());
    }

    /// 「대화가 날아간다」 경고는 **되띄우는 pane** 에서만 켜져야 한다. 칩만 다는
    /// pane 이 그걸 켜면 아무 일도 안 당하는 pane 때문에 빨간 버튼이 뜬다.
    #[test]
    fn losing_the_conversation_is_counted_only_where_it_happens() {
        let mut chipped = fact(Some("/v/acct-2"));
        chipped.focused = true;
        chipped.resumable = false;
        assert_eq!(account_switch_impact(&[chipped], "").fresh, 0);

        let mut restarting = fact(Some("/v/acct-2"));
        restarting.resumable = false;
        assert_eq!(account_switch_impact(&[restarting], "").fresh, 1);
    }

    /// ⚠️ 문구가 사실과 갈리는 것을 막는 자물쇠. 일하는 pane 은 턴이 끝난 뒤에
    /// 되띄우므로 「작업이 끊긴다」는 **거짓말**이다.
    #[test]
    fn the_confirm_text_never_claims_work_gets_killed() {
        let i = AccountSwitchImpact { restart_when_quiet: 3, ..Default::default() };
        let (title, lines) = account_switch_confirm_text("지메일", &i);
        let sub = lines.join(" ");
        assert!(title.contains('3'));
        for lie in ["끊겨", "끊깁", "중단", "죽"] {
            assert!(!sub.contains(lie), "사실과 다른 문구: {sub}");
        }
        assert!(sub.contains("대화는 이어지지만"));
        // 없는 사정은 말하지 않는다.
        assert!(!sub.contains("새로 떠요"));
        assert!(!sub.contains("⟳"));

        let full = AccountSwitchImpact {
            restart_when_quiet: 1,
            restart_after_turn: 2,
            chip_focused: 1,
            fresh: 1,
            unmeasured: 1,
            ..Default::default()
        };
        let sub = account_switch_confirm_text("팀", &full).1.join(" ");
        assert!(sub.contains("턴이 끝난 뒤에"));
        assert!(sub.contains("새로 떠요"));
        assert!(sub.contains("⟳"));
        assert!(sub.contains("확인이 안 돼"));
    }

    /// 예측이 틀릴 때는 **많이 세는 쪽**으로 틀려야 한다 — 더 묻는 것은 안전하고
    /// 덜 묻는 것은 사고다.
    #[test]
    fn the_target_prediction_errs_toward_asking() {
        let vault = std::path::Path::new("/v/acct-2");
        assert_eq!(predicted_target_dir("", true, true, None), "");
        assert_eq!(predicted_target_dir("acct-2", true, true, Some(vault)), "");
        // 작업대를 못 쓰거나 금고가 껍데기면 갈아 끼우기가 안 되고 재시작으로만 반영된다.
        assert_eq!(predicted_target_dir("acct-2", false, true, Some(vault)), "/v/acct-2");
        assert_eq!(predicted_target_dir("acct-2", true, false, Some(vault)), "/v/acct-2");
    }

    /// 러너와 계산기가 같은 판정을 쓰는지. 활동 기록이 없는 pane 도 바쁜 것으로 친다.
    #[test]
    fn a_pane_with_no_activity_yet_is_treated_as_busy() {
        assert!(account_restart_busy(true, Some(("idle", false))));
        assert!(account_restart_busy(false, None));
        assert!(account_restart_busy(false, Some(("running", false))));
        assert!(account_restart_busy(false, Some(("idle", true))));
        assert!(!account_restart_busy(false, Some(("idle", false))));
    }

    /// 닫힌 번호를 되쓰는 것이 요점이다 — 안 그러면 하루 쓰면 `%116` 이 된다.
    #[test]
    fn pane_ids_fill_the_holes_left_by_closed_panes() {
        let used = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect();
        assert_eq!(next_free_pane_id(&used(&[])), "%0", "첫 pane 은 %0");
        assert_eq!(next_free_pane_id(&used(&["%0", "%1", "%2"])), "%3");
        // %1 이 닫혔으면 다음 pane 이 그 자리를 채운다(옛 카운터는 %3 을 줬다).
        assert_eq!(next_free_pane_id(&used(&["%0", "%2"])), "%1");
        // 번호는 정수 순서로 센다 — 사전순이면 %10 뒤에 %2 가 아니라 %9 를 놓친다.
        assert_eq!(next_free_pane_id(&used(&["%0", "%1", "%10", "%2"])), "%3");
    }

    #[test]
    fn agent_restore_drops_terminal_history_but_shell_restore_keeps_it() {
        let rec = serde_json::json!({ "scrollback": ["old output", "old prompt"] });
        assert!(restored_scrollback(&rec, true).is_empty());
        assert_eq!(
            restored_scrollback(&rec, false),
            vec!["old output".to_string(), "old prompt".to_string()]
        );
    }

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
        assert_eq!(pick_restore_id(Some("%9"), |_| false).as_deref(), Some("%9"));
    }

    #[test]
    fn restore_falls_back_when_the_id_is_missing_or_taken() {
        // 옛 저장본엔 pane_id 가 없다 → 호출부가 새 번호를 받는다.
        assert_eq!(pick_restore_id(None, |_| false), None);
        // 이미 살아 있는 번호는 뺏지 않는다.
        assert_eq!(pick_restore_id(Some("%1"), |s| s == "%1"), None);
        // `%` 없는 쓰레기 값도 폴백.
        assert_eq!(pick_restore_id(Some("garbage"), |_| false), None);
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

#[cfg(test)]
mod account_switch_tests {
    use super::{account_id_of_dir, account_switch_toast, parse_securestorage_dir};

    #[test]
    fn parses_securestorage_dir_from_ps_env_line() {
        // ps eww 실물 모양: command 뒤에 env 가 공백으로 이어진다.
        let line = "claude --resume abc TERM=xterm-256color \
             CLAUDE_SECURESTORAGE_CONFIG_DIR=/Users/kasa/.config/kasaterm/claude-accounts/acct-1 \
             HOME=/Users/kasa";
        assert_eq!(
            parse_securestorage_dir(line),
            "/Users/kasa/.config/kasaterm/claude-accounts/acct-1"
        );
        // 변수가 없으면 기본 로그인 — 실측 실패(None)와 구분되는 확정값이다.
        assert_eq!(parse_securestorage_dir("claude TERM=xterm HOME=/x"), "");
    }

    #[test]
    fn account_id_comes_from_dir_tail() {
        assert_eq!(account_id_of_dir("/a/b/claude-accounts/acct-3"), "acct-3");
        assert_eq!(account_id_of_dir(""), "");
    }

    #[test]
    fn toast_covers_all_shapes() {
        // 작업대 전환이 성공하면 도는 pane 도 즉시 따라온다 — 「다음에 뜨는」이라고
        // 말하면 거짓말이다(2026-08-17 「토스트에 다음세션부터라고 뜨는데」).
        assert!(account_switch_toast("사이오닉", false, 0, 0, false, true).contains("다음 메시지부터"));
        // 작업대 실패(금고 비었음 등)면 재시작 폴백뿐이라 옛 문장이 맞다.
        assert!(account_switch_toast("사이오닉", false, 0, 0, false, false).contains("다음에 뜨는"));
        assert!(account_switch_toast("사이오닉", true, 0, 0, false, true).contains("그대로"));
        // 되띄운 것과 기다리는 것이 한 문장에 같이 온다.
        let t = account_switch_toast("사이오닉", false, 2, 1, false, true);
        assert!(t.contains("2개") && t.contains("1개"), "{t}");
        // 보고 있는 pane 만 남았으면 「끝나면 자동」이 아니라 눌러야 한다고 말한다.
        let f = account_switch_toast("사이오닉", false, 2, 1, true, true);
        assert!(f.contains("⟳") && !f.contains("작업 중"), "{f}");
    }
}

#[cfg(test)]
mod agy_restore_tests {
    use super::{restore_agent_command, saved_agent, saved_effort, saved_model};

    /// 하네스 갈래만 보는 판 — 모델·effort 는 아래 전용 테스트가 건다.
    fn cmd(agent: Option<&str>, sid: Option<&str>, resumable: bool) -> String {
        restore_agent_command(agent, sid, resumable, None, None)
    }

    /// 복원 명령의 마지막 갈래가 claude 라, 하네스를 여기 안 적으면 **오류 없이**
    /// claude 로 되살아난다. agy 를 붙이며 실제로 그 상태였다 — 하네스가 하나 더
    /// 늘 때 같은 함정에 다시 빠지지 않게 셋을 다 건다.
    #[test]
    fn every_harness_restores_as_itself() {
        for (agent, fresh) in [("claude", "claude\r"), ("codex", "codex\r"), ("agy", "agy\r")] {
            assert_eq!(cmd(Some(agent), None, false), fresh, "{agent} 새로 띄우기");
        }
        assert_eq!(cmd(Some("claude"), Some("s1"), true), "claude --resume s1\r");
        assert_eq!(cmd(Some("codex"), Some("s2"), true), "codex resume s2\r");
        assert_eq!(cmd(Some("agy"), Some("s3"), true), "agy --conversation s3\r");
    }

    /// 이어갈 수 없는 세션(파일이 사라짐)은 **id 를 버리고** 새로 띄워야 한다 —
    /// 남의 하네스 id 를 넘기면 그 CLI 가 엉뚱한 대화를 물어온다.
    #[test]
    fn unresumable_drops_the_id() {
        assert_eq!(cmd(Some("agy"), Some("s3"), false), "agy\r");
        assert_eq!(cmd(Some("codex"), Some("s2"), false), "codex\r");
    }

    /// 모델·effort 문법이 하네스마다 다르다. 한 판에서 복붙하다 갈리기 쉬운 자리라
    /// 셋을 다 못박는다.
    #[test]
    fn each_harness_gets_its_own_model_and_effort_syntax() {
        let m = Some("claude-opus-5[1m]");
        assert_eq!(
            restore_agent_command(Some("claude"), Some("s1"), true, m, Some("xhigh")),
            "claude --resume s1 --model 'claude-opus-5[1m]' --effort 'xhigh'\r",
            "★ 작은따옴표가 빠지면 `[1m]` 이 zsh 글롭이라 명령이 통째 실패한다"
        );
        assert_eq!(
            restore_agent_command(Some("codex"), Some("s2"), true, Some("gpt-5.5"), Some("high")),
            "codex resume s2 -m 'gpt-5.5' -c model_reasoning_effort='high'\r"
        );
        assert_eq!(
            restore_agent_command(Some("agy"), Some("s3"), true, None, Some("low")),
            "agy --conversation s3 --effort 'low'\r"
        );
    }

    /// 값이 없으면 **플래그 자체가 빠져야** 한다 — 빈 문자열을 흘리면 `--model ''` 이
    /// 나가 하네스가 기본값조차 못 고른다. 옛 저장본엔 이 키가 아예 없다.
    #[test]
    fn missing_values_drop_the_flag_entirely() {
        assert_eq!(
            restore_agent_command(Some("claude"), Some("s1"), true, None, None),
            "claude --resume s1\r"
        );
        // 빈 문자열도 없음으로 친다(수집 쪽이 빈 값을 실어 보낼 수 있다).
        assert_eq!(
            restore_agent_command(Some("claude"), None, false, Some(""), Some("")),
            "claude\r"
        );
        // 한쪽만 있어도 그쪽만 붙는다 — effort 를 안 정한 세션이 흔하다.
        assert_eq!(
            restore_agent_command(Some("claude"), None, false, Some("sonnet"), None),
            "claude --model 'sonnet'\r"
        );
    }

    /// 저장본 어댑터. 옛 저장본(`{}`)이 `None` 이어야 위 "플래그를 뺀다"가 성립한다.
    #[test]
    fn saved_model_and_effort_fall_back_to_none() {
        let j = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        assert_eq!(saved_model(&j(r#"{"model":"claude-opus-5[1m]"}"#)), Some("claude-opus-5[1m]"));
        assert_eq!(saved_effort(&j(r#"{"effort":"xhigh"}"#)), Some("xhigh"));
        assert_eq!(saved_model(&j(r#"{}"#)), None, "옛 저장본");
        assert_eq!(saved_effort(&j(r#"{}"#)), None, "옛 저장본");
        // 빈 문자열이 새어 들어와도 없음으로 친다.
        assert_eq!(saved_model(&j(r#"{"model":""}"#)), None);
    }

    #[test]
    fn saved_agent_reads_agy_and_keeps_the_legacy_key() {
        let j = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        assert_eq!(saved_agent(&j(r#"{"was_agent":"agy"}"#)), Some("agy"));
        assert_eq!(saved_agent(&j(r#"{"was_agent":"codex"}"#)), Some("codex"));
        // 옛 저장본 — 이게 깨지면 판올림 한 번에 학생 pane 이 전부 셸이 된다.
        assert_eq!(saved_agent(&j(r#"{"was_claude":true}"#)), Some("claude"));
        assert_eq!(saved_agent(&j(r#"{}"#)), None);
    }
}

/// 사이드바 방 이름 판정 — 어떤 cwd 가 방을 대표하는가.
#[cfg(test)]
mod room_name_tests {
    use super::{is_temp_path, room_home_cwd};

    /// 방 이름 판정. `(cwd, 학생이 앉았나)` 를 leaf 순서대로 준다.
    fn room(panes: &[(&str, bool)]) -> Option<String> {
        let v: Vec<(std::path::PathBuf, bool)> = panes
            .iter()
            .map(|(p, a)| (std::path::PathBuf::from(p), *a))
            .collect();
        room_home_cwd(&v).map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
    }
    const SCRATCH: &str =
        "/private/tmp/claude-501/-Users-kasa-repo/0e7ab2f6/scratchpad/dogfood8-run3";

    #[test]
    fn room_name_takes_the_most_common_cwd() {
        assert_eq!(
            room(&[("/a/recall", true), ("/a/branding", true), ("/a/recall", true)]),
            Some("recall".into())
        );
        assert_eq!(room(&[]), None);
    }

    /// 동률이면 leaf 순서가 먼저인 쪽 — 순서가 고정이라 같은 방이 늘 같은 이름을 얻는다.
    #[test]
    fn room_name_breaks_a_tie_by_leaf_order() {
        assert_eq!(
            room(&[("/a/recall", true), ("/a/branding", true)]),
            Some("recall".into())
        );
        assert_eq!(
            room(&[("/a/branding", true), ("/a/recall", true)]),
            Some("branding".into())
        );
    }

    /// 2026-08-24 지적의 재현. 곁다리 셸이 임시 폴더에 앉아 있어도 방 이름은
    /// 학생이 보는 프로젝트다 — 이게 깨지면 recall 방이 `dogfood8-run3` 가 된다.
    #[test]
    fn room_name_ignores_a_stray_shell_in_a_temp_dir() {
        // leaf 순서상 셸이 **먼저**다 — 옛 규칙은 동률에서 이쪽이 이겼다.
        assert_eq!(
            room(&[(SCRATCH, false), ("/a/recall", true)]),
            Some("recall".into())
        );
    }

    /// 셸이 다수여도 학생 쪽이 이긴다 — 방의 정체는 학생이 앉은 프로젝트다.
    #[test]
    fn room_name_prefers_agent_panes_over_a_shell_majority() {
        assert_eq!(
            room(&[
                ("/a/Downloads", false),
                ("/a/Downloads", false),
                ("/a/recall", true),
            ]),
            Some("recall".into())
        );
    }

    /// 학생이 하나도 없는 방(사람이 직접 쓰는 터미널)은 셸 cwd 로 이름이 지어진다.
    #[test]
    fn room_name_falls_back_to_shells_when_no_agent_sits_there() {
        assert_eq!(
            room(&[("/a/Downloads", false), ("/a/Downloads", false), ("/a/recall", false)]),
            Some("Downloads".into())
        );
    }

    /// 방 전체가 임시 폴더면 그게 유일한 정체다 — 밀어내면 이름이 없어진다.
    #[test]
    fn room_name_keeps_a_temp_dir_when_thats_all_there_is() {
        assert_eq!(room(&[(SCRATCH, true)]), Some("dogfood8-run3".into()));
        assert_eq!(
            room(&[(SCRATCH, false), ("/tmp/scratch", false)]),
            Some("dogfood8-run3".into())
        );
    }

    /// 학생만 남긴 뒤에도 임시 제외가 걸린다 — 학생 둘이 각각 프로젝트/스크래치면
    /// 프로젝트 쪽이다.
    #[test]
    fn room_name_drops_temp_after_narrowing_to_agents() {
        assert_eq!(
            room(&[(SCRATCH, true), ("/a/recall", true)]),
            Some("recall".into())
        );
    }

    #[test]
    fn temp_paths_cover_both_macos_spellings() {
        use std::path::Path;
        assert!(is_temp_path(Path::new(SCRATCH)));
        assert!(is_temp_path(Path::new("/tmp/x")));
        assert!(is_temp_path(Path::new("/var/folders/ab/cd/T/x")));
        assert!(is_temp_path(Path::new("/private/var/folders/ab/cd/T/x")));
        assert!(!is_temp_path(Path::new("/Users/kasa/Desktop/repo")));
        // 이름이 임시 루트로 시작할 뿐인 경로는 임시가 아니다.
        assert!(!is_temp_path(Path::new("/tmpfs/repo")));
        assert!(!is_temp_path(Path::new("/Users/kasa/tmp/repo")));
    }
}

#[cfg(test)]
mod closed_pane_id_reuse_tests {
    use super::{closed_index_in, stashed_in};
    use crate::ClosedPane;

    fn rec(pane: &str, alive: bool, folder: &str) -> ClosedPane {
        ClosedPane {
            rec: serde_json::Value::Null,
            pane_id: pane.to_string(),
            character: String::new(),
            folder: folder.to_string(),
            neighbor: None,
            window: 0,
            alive,
            stashed: false,
            idle_since: None,
            preview: None,
        }
    }

    /// 죽은 기록은 번호를 안 잡으므로(`used_pane_ids`) 같은 번호가 다음 pane 에
    /// 다시 나간다. 그 새 pane 을 「닫힌 것」으로 보면 인포에서 통째로 사라지고,
    /// 그 방의 유일한 pane 이었다면 **방까지 목록에서 없어진다**(2026-08-25 실측).
    #[test]
    fn dead_record_does_not_claim_a_live_pane() {
        let list = [rec("%21", false, "nacho-neko"), rec("%21", false, "Desktop")];
        assert!(stashed_in(&list, "%21").is_none());
    }

    /// 숨긴 pane 은 실제로 그 번호를 물고 있으니 걸려야 한다.
    #[test]
    fn live_record_is_found() {
        let list = [rec("%21", false, "옛것"), rec("%21", true, "숨긴것")];
        assert_eq!(stashed_in(&list, "%21").map(|c| c.folder.as_str()), Some("숨긴것"));
    }

    /// 목록 조작(숨김 해제·끄기)은 살아 있는 것을 먼저 집는다 — 앞에 놓인 묘비
    /// 때문에 정작 자원을 문 항목을 못 건드리면 안 된다.
    #[test]
    fn index_prefers_the_live_record() {
        let list = [rec("%21", false, "옛것"), rec("%21", true, "숨긴것")];
        assert_eq!(closed_index_in(&list, "%21"), Some(1));
    }

    /// 묘비만 있으면 그건 집는다 — 목록에서 지우는 조작은 죽은 기록에도 걸려야 한다.
    #[test]
    fn index_falls_back_to_a_dead_record() {
        let list = [rec("%21", false, "옛것")];
        assert_eq!(closed_index_in(&list, "%21"), Some(0));
        assert_eq!(closed_index_in(&list, "%22"), None);
    }
}

/// 학생 교체가 **언제 묻고 언제 그냥 바꾸는가**.
///
/// 2026-08-25 지시: 「그럼 새로띄우게해 테마 바꾸면 확인버튼도 만들고 근데 말투
/// 오프돼있으면 그냥 껍데기만바뀌게」. 말투가 꺼진 채로 카드가 뜨면 되띄울 이유가
/// 없는 재시작을 묻는 셈이고, 켜진 채로 안 뜨면 대화가 말없이 끊긴다.
#[cfg(test)]
mod character_swap_plan_tests {
    use super::{character_swap_confirm_text, plan_character_swap, SwapPlan};

    #[test]
    fn persona_off_swaps_the_shell_without_asking() {
        assert_eq!(plan_character_swap(false, Some("claude"), true), SwapPlan::Now);
    }

    #[test]
    fn a_shell_pane_has_nothing_to_relaunch() {
        assert_eq!(plan_character_swap(true, None, false), SwapPlan::Now);
    }

    #[test]
    fn a_running_agent_with_persona_on_is_asked() {
        assert_eq!(
            plan_character_swap(true, Some("claude"), true),
            SwapPlan::Ask { resumable: true }
        );
    }

    /// 대화가 없어도 **묻기는 한다** — 되띄우면 지금 내용을 잃으므로 오히려 더
    /// 물어야 하는 쪽이다. 카드가 그 사실을 문구와 버튼 색으로 알린다.
    #[test]
    fn no_transcript_still_asks_but_flags_the_loss() {
        assert_eq!(
            plan_character_swap(true, Some("claude"), false),
            SwapPlan::Ask { resumable: false }
        );
        let (_, lines) = character_swap_confirm_text("은랑", false);
        assert!(
            lines.iter().any(|l| l.contains("사라집니다")),
            "대화를 잃는다는 사실이 카드에 안 적힌다"
        );
        // 이어붙일 대화가 있을 때는 반대로 「그대로」임을 말해야 한다.
        let (title, ok) = character_swap_confirm_text("은랑", true);
        assert!(title.contains("은랑으로"), "조사가 어긋난다: {title}");
        // 받침 없는 이름과 ㄹ 받침은 「로」다.
        assert!(character_swap_confirm_text("미도리", true).0.contains("미도리로"));
        assert!(character_swap_confirm_text("하치와레", true).0.contains("하치와레로"));
        assert!(character_swap_confirm_text("페이몬", true).0.contains("페이몬으로"));
        assert!(ok.iter().any(|l| l.contains("그대로")));
    }
}
