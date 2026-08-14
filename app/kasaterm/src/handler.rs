//! winit ApplicationHandler — App 의 이벤트 루프(window_event/new_events/user_event/resumed/about_to_wait).
//! main.rs 에서 분리. impl App 메서드·타입은 crate root 그대로 참조.
use super::*;

impl ApplicationHandler<UserEvent> for App {
    /// A background thread (PTY snapshot, socket) asked us to repaint.
    /// Delivered even while a WaitUntil is parked, so this is what makes
    /// committed-Hangul echo / backspace / space show up without lag.
    // event_loop 는 SocketOpenWeb(자식 창 생성) 한 곳만 쓴다.
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        // window_event 와 같은 이유 — 소켓/백그라운드에서 온 변경도 자동 저장
        // 대상이다(pane 분할·세션 바인딩이 여기로 들어온다).
        self.session_touched = true;
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
            UserEvent::SocketNewWindow => self.new_window(),
            UserEvent::SocketNewTab(from, reply) => {
                let outer = from
                    .clone()
                    .or_else(|| self.ws.lock().unwrap().active_pane.clone());
                let outcome = match outer {
                    // split 과 같은 규칙: 지정한 pane 이 없으면 **거절한다**. 조용히
                    // 포커스로 떨어지면 엉뚱한 창에 탭이 생기고도 성공으로 답한다.
                    Some(o) if self.window_of_pane(&o).is_none() => {
                        Err(format!("탭을 열 pane {o} 이 없다"))
                    }
                    Some(o) => self.spawn_new_tab(&o).map_err(|e| format!("{e:#}")),
                    None => Err("활성 pane 이 없다".to_string()),
                };
                let _ = reply.send(outcome);
            }
            UserEvent::SocketMovePane(moving, target, zone, reply) => {
                let outcome = if self.window_of_pane(moving).is_none() {
                    Err(format!("옮길 pane {moving} 이 없다"))
                } else if self.window_of_pane(target).is_none() {
                    Err(format!("놓을 자리 {target} 이 없다"))
                } else if moving == target {
                    Err("자기 자신 옆으로는 못 옮긴다".to_string())
                } else {
                    self.move_pane(moving, target, *zone);
                    // `move_pane` 은 성공/실패를 안 돌려준다(드래그 경로라 실패해도
                    // 화면이 그대로면 사용자가 안다). 소켓은 그럴 수 없으니 **옮겨진
                    // 자리로 판정한다** — 대상과 같은 창에 있으면 붙은 것이다.
                    match self.window_of_pane(moving) {
                        Some(w) if Some(w) == self.window_of_pane(target) => Ok(moving.clone()),
                        _ => Err(format!(
                            "{moving} 이 {target} 옆으로 안 붙었다 — 트리에서 떨어졌는지 확인해라"
                        )),
                    }
                };
                let _ = reply.send(outcome);
            }
            UserEvent::SocketClosedPanes(discard, reply) => {
                // 지목이 있으면 **먼저** 끈다 — 그 뒤 남은 목록을 실어 보내므로 호출자는
                // 한 왕복으로 "무엇을 껐고 무엇이 남았는지"를 같이 받는다.
                let mut killed = serde_json::Value::Null;
                if let Some(want) = discard {
                    match self.closed_panes.iter().position(|c| &c.pane_id == want) {
                        Some(i) => {
                            let c = &self.closed_panes[i];
                            killed = serde_json::json!({
                                "pane": c.pane_id, "character": c.character,
                                "folder": c.folder, "was_alive": c.alive,
                            });
                            self.discard_closed_pane_at(i);
                        }
                        None => {
                            let _ = reply.send(Err(format!(
                                "되살리기 목록에 {want} 이 없다 — 이미 끄거나 되살렸다"
                            )));
                            return;
                        }
                    }
                }
                let list: Vec<serde_json::Value> = self
                    .closed_panes
                    .iter()
                    .rev() // 최근에 닫은 것이 위 — Info 패널·⌘⇧T 와 같은 순서
                    .map(|c| {
                        serde_json::json!({
                            "pane": c.pane_id,
                            "character": c.character,
                            "folder": c.folder,
                            // 살아 있는 항목이 진짜 비용이다 — 셸·claude 를 그대로 물고 있다.
                            "alive": c.alive,
                            "window": c.window,
                        })
                    })
                    .collect();
                let _ = reply.send(Ok(serde_json::json!({
                    "closed": list, "killed": killed, "keep": crate::CLOSED_PANE_KEEP,
                })));
                self.render_frame();
                return;
            }
            UserEvent::SocketSplit(dir, focus, from, reply) => {
                // `split_active_pane` always sets the new pane active (correct
                // for the GUI's keyboard split). The socket path defaults to
                // no-focus so a scripted split doesn't yank the user's focus
                // (like `tell`) — restore the prior active pane unless the
                // caller opted in with `--focus`.
                let prev = self.ws.lock().unwrap().active_pane.clone();
                // 부른 쪽이 pane 을 지정했으면 그 pane 을 쪼갠다. split_active_pane 이
                // active_pane 하나만 보고 cwd·방·트리 위치를 전부 거기서 가져오므로,
                // 잠깐 갈아끼웠다 아래에서 되돌리는 것으로 충분하다. 없는 pane 이면
                // 무시하고 포커스 기준으로 — 죽은 id 로 갈아끼우면 split 이 통째로
                // 실패한다.
                // 지정한 pane 이 없으면 **거절한다**. 예전엔 조용히 포커스 기준으로
                // 떨어졌는데, 그러면 「%999 를 쪼개 달라」가 엉뚱한 창을 쪼개고도
                // 성공으로 답한다 — 부른 쪽은 자기 대상이 무시된 걸 모른다.
                // 존재 판정은 `ws.panes` 가 아니라 **레이아웃 트리**로 한다:
                // split 직후 새 pane 은 `ws.panes` 에 PaneState 가 아직 없어서,
                // 방금 만든 pane 을 다음 대상으로 넘기는 연속 split(`--count`)이
                // 「없는 pane」으로 거절당한다. 트리는 `split_leaf` 가 동기로 갱신한다.
                let missing = from.as_ref().and_then(|f| {
                    self.window_of_pane(f).is_none().then(|| f.clone())
                });
                let outcome = if let Some(f) = missing {
                    Err(format!("쪼갤 pane {f} 이 없다 — 종료·재시작으로 사라졌는지 확인해라"))
                } else {
                    if let Some(from) = from {
                        self.ws.lock().unwrap().active_pane = Some(from.clone());
                    }
                    self.split_pane_auto(*dir).map_err(|e| format!("{e:#}"))
                };
                if !*focus {
                    if let Some(prev) = prev {
                        self.ws.lock().unwrap().active_pane = Some(prev);
                    }
                }
                if let Err(ref why) = outcome {
                    eprintln!("[kasaterm] socket split 실패: {why}");
                }
                let _ = reply.send(outcome);
                self.chrome_dirty = true;
                self.render_frame();
                return;
            }
            UserEvent::SocketSplitFleet(count, from, host_ratio, reply) => {
                // 배치는 포커스를 옮기지 않는다 — 오케스트레이터가 배경에서 하는
                // 일이라, 사람이 보고 있는 자리를 빼앗으면 안 된다(`tell` 과 같은
                // 규칙). `spawn_split_session` 은 트리만 건드리고 active_pane 을
                // 안 만지므로 따로 되돌릴 것도 없다.
                let outcome = self
                    .split_fleet(*count, from.as_deref(), *host_ratio)
                    .map_err(|e| format!("{e:#}"));
                if let Err(ref why) = outcome {
                    eprintln!("[kasaterm] socket split_fleet 실패: {why}");
                }
                let _ = reply.send(outcome);
                self.chrome_dirty = true;
                self.render_frame();
                return;
            }
            UserEvent::NotifyFocus { pane, sid } => {
                // 알림을 쏜 시점의 세션과 지금 그 pane 의 세션이 같을 때만 옮긴다.
                // surface id 는 재사용되므로, 그 사이 pane 이 닫히고 번호가 새 셸에
                // 넘어갔으면 엉뚱한 자리로 끌려간다 — 그때는 아무 데도 안 가는 게 맞다.
                let same = match sid.as_deref() {
                    Some(s) => self.pane_claude_sid.get(pane).is_some_and(|c| c == s),
                    // 세션을 못 실은 알림(순정 셸 pane)은 존재 확인까지만.
                    None => self.ws.lock().unwrap().panes.contains_key(pane),
                };
                if !same {
                    return;
                }
                // 배너를 눌렀는데 창이 뒤에 남아 있으면 누른 뜻이 없다. 방 전환은
                // `SocketFocus` 가 하므로 여기서는 창만 앞으로 올리고 넘긴다.
                if let Some(w) = self.window.as_ref() {
                    w.focus_window();
                }
                let _ = self.proxy.send_event(UserEvent::SocketFocus(pane.clone()));
                return;
            }
            UserEvent::SocketFocus(id) => {
                // GUI(SCHALE OS)에서 다른 방(=다른 윈도우)의 학생을 포커스하면 그
                // 윈도우를 앞으로 가져와야 한다. active_pane 만 바꾸면 보이지 않는
                // 윈도우의 pane 이 선택돼 화면은 그대로라 "방전환→윈도우전환"이 안 됐다.
                // switch_window 가 leaves[0] 로 active_pane 을 덮으니 그 뒤에 원하는
                // pane 으로 다시 지정한다.
                // board id 가 실제 leaf 인 윈도우가 있을 때만 포커스한다 — 캐릭터/작업명/async
                // 같은 비-leaf 집계 id 로 active_pane 을 덮으면 다음 /layout 폴에서 그 타일이
                // 빠져 "pane 이 닫힌 것처럼" 보였다(거노: 캐릭터 클릭→학생 선택하면 닫힘).
                if self.focus_pane(id) {
                    self.render_frame();
                }
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
                    // 거노: "터미널 보기"는 화면 2분할(터미널 좌·아로나 우).
                    // 아로나 창이 떠 있으면 둘을 모니터 좌/우 절반으로 타일링.
                    self.tile_terminal_arona_split();
                    if let Some(id) = focus_pane {
                        self.ws.lock().unwrap().active_pane = Some(id.clone());
                        self.chrome_dirty = true;
                        self.render_frame();
                    }
                }
                return;
            }
            UserEvent::SocketToggleGit => {
                // 아로나 타이틀바 버튼 → 터미널 GUI git 소스컨트롤 패널. 메인 창을 띄우고
                // (숨겨져 있으면) 둘을 타일링한 뒤 git 컬럼 토글(거노).
                if let Some(w) = &self.window {
                    w.set_visible(true);
                    w.focus_window();
                    w.request_redraw();
                }
                self.tile_terminal_arona_split();
                self.toggle_git_col();
                self.chrome_dirty = true;
                self.render_frame();
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
            UserEvent::SocketQuerySessions(reply) => {
                // 방=윈도우 목록. 라벨(name, cwd)은 refresh_window_labels 가 채운다.
                self.refresh_window_labels();
                let _ = reply.send((
                    self.windows.len(),
                    self.active_window,
                    self.window_labels.clone(),
                ));
                return;
            }
            UserEvent::SocketSwitchSession(idx) => {
                self.switch_window(*idx);
                return;
            }
            UserEvent::SocketNewRoom(character) => {
                self.new_room_with_character(character);
                return;
            }
            UserEvent::SocketSpawnStudent(character, reply) => {
                let id = self.spawn_student(character);
                let _ = reply.send(id);
                return;
            }
            UserEvent::SocketSwapCharacter(pane, character) => {
                self.swap_character(pane, character);
                return;
            }
            UserEvent::SocketRepersona(pane, character) => {
                self.repersona_pane(&pane, &character);
                return;
            }
            UserEvent::SocketSessionBound(pane, sid) => {
                // 배지 판정용: pane → claude 실제 sessionId(fork 시 갈라진 진짜 세션).
                // report-cwd 가 매 렌더 이 이벤트를 재발화해 bg 세션 pane_claude_sid 를
                // 보강하므로(F/H), 이미 같은 sid 면 no-op — 무한 relabel·render 를 막는다.
                let already_labeled = self
                    .ws
                    .lock()
                    .ok()
                    .and_then(|ws| ws.pane_character.get(pane.as_str()).cloned())
                    .is_some_and(|name| !name.is_empty());
                if self.pane_claude_sid.get(pane.as_str()) == Some(&sid) && already_labeled {
                    return;
                }
                self.pane_claude_sid.insert(pane.clone(), sid.clone());
                self.apply_session_character(pane, sid);
                // 즉시 redraw — 없으면 idle 세션 attach 는 화면 업데이트가 안 흘러
                // 다음 리드로우가 영영 없고, 교정된 학생(테두리·명찰·프사)이 사용자가
                // 스크롤 등으로 리드로우를 강제할 때까지 옛 모습으로 남았다(거노:
                // 스크롤 살짝 올렸다 내려야 바뀜 — 바인딩은 즉시, 픽셀만 지연).
                self.chrome_dirty = true;
                self.render_frame();
                return;
            }
            UserEvent::SocketViewCwd(pane, cwd) => {
                // 파일트리 루트 오버라이드 — "pane 이 보는 경로"(statusline report /
                // transcript bind). 활성 pane 이면 다음 프레임 refresh_file_tree 가
                // 새 루트를 집도록 리드로우만 청한다.
                let is_active = self
                    .ws
                    .lock()
                    .ok()
                    .and_then(|w| w.active_pane.clone())
                    .as_deref()
                    == Some(pane.as_str());
                let prev = self.pane_view_cwd.insert(pane.clone(), cwd.clone());
                if is_active && prev.as_ref() != Some(cwd) {
                    self.chrome_dirty = true;
                    if let Some(w) = self.window.as_ref() {
                        w.request_redraw();
                    }
                }
                return;
            }
            UserEvent::NudgePaneResize(pane) => {
                // 1행 지글 — 줄이고 120ms 뒤 원복(pending_unjiggle drain). 크기가
                // 변해야 SIGWINCH 재레이아웃이 돌아 statusline 이 재실행된다(같은
                // 크기 resize 는 no-op). 원 크기는 그리드 스냅샷에서 취한다.
                let size = {
                    let ws = self.ws.lock().unwrap();
                    ws.panes.get(pane).and_then(|p| p.term()).map(|t| (t.cols, t.rows))
                };
                if let (Some((cols, rows)), Some(sess)) = (size, self.pty.get(pane)) {
                    if rows > 4 {
                        let _ = sess.resize(cols, rows - 1);
                        self.pending_unjiggle.push((
                            pane.clone(),
                            cols,
                            rows,
                            std::time::Instant::now() + std::time::Duration::from_millis(120),
                        ));
                    }
                }
                return;
            }
            UserEvent::ResumeSession { id, cwd, newroom, attach, harness } => {
                // 새 pane 을 띄우고, 그 셸 프롬프트가 뜰 즈음 `claude --resume <id>` 를
                // 주입한다(주입 자체는 pending_restores drain 이 시간 기반으로 처리).
                // 세션 cwd 가 있으면 cd 를 앞에 붙여 어느 방에서 열어도 올바른 프로젝트
                // 세션을 잇는다(claude --resume 는 cwd 의 프로젝트 기준).
                // 세션→캐릭터 매핑이 있으면 스폰 전에 pending 배정(거노 ④) — 랜덤 둔갑을
                // 시점부터 차단하고 persona 까지 그 캐릭터로 맞춘다. 없으면 기존 랜덤.
                // background 세션은 detach 때 fork 로 id 가 갈려(id=fork sessionId) 직접
                // 매핑이 없을 수 있어, bg_agents 의 부모 체인을 따라 원본 학생을 찾는다.
                // 진입 시점에 확정해야 attach 로 foreground 가 되며 폴러(kind=background)에서
                // 빠져 상속이 끊기기 전에 고정된다(거노: 백그라운드 재진입 학생 바뀜).
                let direct = kasa_mcp::character::session_character(id);
                let resolved = direct.clone().or_else(|| {
                    let mut cur = id.to_string();
                    for _ in 0..8 {
                        let parent = self
                            .bg_agents
                            .lock()
                            .ok()
                            .and_then(|m| m.get(&cur).cloned())
                            .flatten();
                        match parent {
                            Some(p) => {
                                if let Some(c) = kasa_mcp::character::session_character(&p) {
                                    return Some(c);
                                }
                                cur = p;
                            }
                            None => break,
                        }
                    }
                    None
                });
                // 바인딩도 부모도 없는 세션(포크 parentSessionId 미상) — pending 을 비워두면
                // split 상속(layout.rs, 같은 맥락 이어보기용)이 소스 pane 의 학생을 물려줘
                // 무관한 세션이 그 학생으로 둔갑했다(거노: 왼쪽 pane 둘 다 프라나). 여기서
                // 빈 슬롯 학생을 뽑아 확정한다 — 매핑 없는 pane 없게, 이후엔 파싱만(거노).
                let resolved = resolved.or_else(|| {
                    let members = kasa_mcp::character::characters_json()
                        .map(|c| kasa_mcp::character::member_names(&c))
                        .unwrap_or_default();
                    let taken: std::collections::HashSet<String> = {
                        let ws = self.ws.lock().unwrap();
                        let mut t: std::collections::HashSet<String> =
                            ws.pane_character.values().cloned().collect();
                        t.extend(kasa_mcp::character::assigned_global());
                        t
                    };
                    let free: Vec<String> = members
                        .iter()
                        .filter(|n| !taken.contains(n.as_str()))
                        .cloned()
                        .collect();
                    kasa_mcp::character::pick_random(&free, id)
                        .or_else(|| kasa_mcp::character::pick_random(&members, id))
                });
                if let Some(ch) = resolved {
                    // 부모 체인/신선 배정으로 온 학생은 세션 id 에 영속 — 재진입·board
                    // retained 가 체인 재탐색 없이(부모가 bg_agents 에서 밀려나도) 같은
                    // 학생을 파싱한다. direct 면 이미 같은 값이라 무쓰기.
                    if direct.is_none() {
                        let _ = kasa_mcp::character::bind_session_character(id, &ch);
                    }
                    self.pending_character = Some(ch);
                }
                let new_id = if *newroom {
                    self.new_window();
                    self.ws.lock().unwrap().active_pane.clone()
                } else {
                    self.split_active_pane(kasa_pty::SplitDir::Horizontal).ok()
                };
                if let Some(new_id) = new_id {
                    // pane↔세션 transcript 즉석 확정(파싱 우선): attach 뷰는 bind hook 이
                    // 안 떠서 board discovery 의 recent-jsonl 추측이 같은 cwd 의 남의 활성
                    // 세션에 오귀속됐다(거노: 왼쪽 pane 둘 다 프라나 + board 내용 뒤섞임).
                    // 세션 id 를 아는 유일한 시점인 여기서 bind_transcript 한 호출로
                    // bound(board)·SocketSessionBound(render pane_claude_sid + 캐릭터)를
                    // 정렬한다. jsonl 미존재(막 포크돼 첫 기록 전)면 기존 discovery 폴백.
                    if let (Some(be), Some(tp)) = (
                        self.socket_backend.clone(),
                        socket::transcript_path_for_session(id),
                    ) {
                        let _ = kasa_socket::Backend::bind_transcript(
                            be.as_ref(),
                            &new_id,
                            &tp.to_string_lossy(),
                        );
                    }
                    if let Some(sess) = self.pty.get(&new_id).cloned() {
                        let cmd = if *attach {
                            // daemon background 세션 연결(claude attach) — 세션은 background
                            // 유지, detach 해도 안 죽음. id 로 daemon 직접이라 cwd 불필요.
                            // claude 의 daemon 개념이라 harness 와 무관하게 claude 다.
                            format!("claude attach {id}\r")
                        } else {
                            // 하네스별 조립은 sessions::resume_command 한 곳에만 둔다 —
                            // 예전에 CLI 와 GUI 가 각자 만들다 한쪽만 고쳐진 적이 있다.
                            let line = kasa_socket::sessions::resume_command(
                                harness,
                                id,
                                cwd.as_deref().unwrap_or(""),
                            );
                            format!("{line}\r")
                        };
                        let at = std::time::Instant::now()
                            + std::time::Duration::from_millis(900);
                        self.pending_restores.push((sess, cmd, at));
                    }
                }
                self.chrome_dirty = true;
                self.render_frame();
                return;
            }
            UserEvent::SaveSession { surface } => {
                // foreground claude 를 background daemon 으로 detach: 입력칸 비우고(Ctrl-U)
                // ←←(agents view = "bg-detach") 를 gap 두고 주입한다. claude TUI 의
                // leftArrowOpensAgents(기본 ON) confirm-on-second-press 를 태운다 — 첫 ←
                // 무장("← again for agents"), 두 번째 ← 가 실제 detach. 입력칸이 비어야
                // ← 가 커서이동이 아니라 agents 로 먹힌다(그래서 Ctrl-U 선행).
                let pane = surface
                    .clone()
                    .or_else(|| self.ws.lock().unwrap().active_pane.clone());
                if let Some(pane) = pane {
                    if let Some(sess) = self.pty.get(&pane).cloned() {
                        let now = std::time::Instant::now();
                        let at = |n| now + std::time::Duration::from_millis(n);
                        self.pending_restores.push((sess.clone(), "\u{15}".to_string(), at(0)));
                        self.pending_restores.push((sess.clone(), "\u{1b}[D".to_string(), at(80)));
                        self.pending_restores.push((sess, "\u{1b}[D".to_string(), at(160)));
                    }
                }
                return;
            }
            UserEvent::SocketCloseRoom(idx) => {
                if let Err(e) = self.close_window(*idx) {
                    eprintln!("[room] close {idx} failed: {e}");
                }
                return;
            }
            UserEvent::SocketClose(id) => {
                self.close_pane(id);
                self.chrome_dirty = true;
                self.render_frame();
                return;
            }
            UserEvent::SocketSaveCharacter(name, persona, new_name, reply) => {
                // 네이티브가 사람 손으로 하는 3단을 그대로 태운다. 이유는
                // `SocketSaveCharacter` 주석에 있다 — 요약하면 순서와 뒤처리가
                // 이 두 액션에 묶여 있어서다.
                // 거부는 **토스트로만** 알려진다(`flush_student_name` 은 버퍼를
                // 조용히 되돌리고 반환값이 없다). 그래서 저장 전후의 토스트를
                // 비교해 판정하고, 그 문구를 그대로 회신에 실어 웹뷰가 같은 말을
                // 보여 주게 한다 — 네이티브 토스트는 웹뷰 창에서 안 보인다.
                //
                // 로스터 캐시(`character_slugs`)로 판정하지 않는 이유: 캐시는 이름이
                // **실제로 바뀔 때만** 무효화되므로, 캐시와 파일이 어긋난 순간
                // 거부를 성공으로 읽는다(실측으로 거짓 성공을 냈다).
                let toast_before = self.collab.toast.clone();
                self.settings_apply(SettingsAction::SelectStudent(name.clone()));
                if let Some(p) = persona {
                    self.students_caret = p.chars().count();
                    self.students_persona = p.clone();
                }
                if let Some(n) = new_name {
                    self.students_name = n.clone();
                }
                self.settings_apply(SettingsAction::CloseStudent);
                let rejected = match (&toast_before, &self.collab.toast) {
                    (_, None) => None,
                    (None, Some((m, _))) => Some(m.clone()),
                    // 시각까지 보는 건 같은 문구가 연달아 떴을 때를 가르기
                    // 위해서다(같은 이름으로 두 번 시도하면 문구가 같다).
                    (Some((bm, bt)), Some((m, t))) => (t != bt || m != bm).then(|| m.clone()),
                };
                let want = new_name.clone().unwrap_or_else(|| name.clone());
                let _ = reply.send(Ok(serde_json::json!({
                    "ok": rejected.is_none(),
                    "name": if rejected.is_none() { want } else { name.clone() },
                    "error": rejected,
                })));
                return;
            }
            UserEvent::SocketSettingsAction(action, id, label, reply) => {
                // 성공 판정을 토스트로 하지 않는다 — Step 5 의 캐릭터 저장과 다른
                // 점이다. 거기선 토스트가 뜨는 것 자체가 거부였지만, 여기선
                // `select_theme`·`delete_theme` 이 **성공에도** 토스트를 띄운다.
                // 그래서 판정은 액션마다 그 액션이 남긴 상태로 하고, 토스트 문구는
                // 판정과 무관하게 그대로 실어 보낸다(웹뷰 창에선 네이티브 토스트가
                // 안 보이니, 네이티브가 하려던 말은 여기서만 전달된다).
                let toast_before = self.collab.toast.clone();
                let arg = id.clone().unwrap_or_default();
                // 네이티브에는 이 검사가 필요 없다 — 카드가 있는 것만 눌리기
                // 때문이다. HTTP 는 아무 문자열이나 보낼 수 있고, 없는 id 로
                // `select-theme` 을 태우면 `character_theme` 가 없는 폴더를 가리킨
                // 채 굳는다. 그러면 로스터는 번들로 떨어지는데 「쓰는 중」 배지는
                // **어느 카드에도 안 붙어서**, 사용자는 무엇이 켜져 있는지 화면에서
                // 알 수 없게 된다(실측으로 그 상태를 만들었다).
                let theme_exists =
                    |id: &str| kasa_mcp::character::list_themes().iter().any(|(t, _)| t.as_str() == id);
                let ok = match action.as_str() {
                    "select-theme" if !arg.is_empty() && !theme_exists(&arg) => {
                        Err(format!("'{arg}' 테마가 없어요"))
                    }
                    "select-theme" => {
                        self.settings_apply(SettingsAction::SelectTheme(arg.clone()));
                        Ok(socket::read_character_theme() == arg)
                    }
                    // 만들기는 성공하면 새 테마의 이름 칸을 포커스한다(사용자가
                    // 곧바로 이름을 짓게) — 그 버퍼가 섰는지가 곧 성공 신호다.
                    // 웹뷰에는 포커스 개념이 없으니 확인한 뒤 걷어낸다.
                    "new-theme" => {
                        self.settings_apply(SettingsAction::ExportTheme);
                        let made = self.theme_label_edit.take().map(|(t, _)| t);
                        self.settings_input = None;
                        Ok(made.is_some())
                    }
                    // 네이티브의 3단(포커스 → 버퍼 → 커밋)을 그대로 태운다. 키
                    // 이벤트 경로가 없어 버퍼를 직접 심는 건 testkit 하네스와 같다.
                    "rename-theme" => {
                        let next = label.clone().unwrap_or_default();
                        let next = next.trim().to_string();
                        if next.is_empty() {
                            // 네이티브에는 이 상태가 없다 — 포커스하면 지금 이름이
                            // 버퍼에 실려 있어서다. 막지 않으면 `theme.json` 의
                            // label 이 빈 문자열로 굳어 이름 없는 카드가 선다.
                            Err("이름은 비울 수 없어요".to_string())
                        } else {
                            self.settings_apply(SettingsAction::FocusThemeLabel(arg));
                            if let Some((_, buf)) = self.theme_label_edit.as_mut() {
                                *buf = next;
                            }
                            self.flush_theme_label();
                            // 실패하면 `flush_theme_label` 이 버퍼를 남긴 채
                            // 돌아온다(네이티브에선 사용자가 계속 고치는 자리다).
                            // 웹뷰 요청엔 이어서 고칠 사람이 없으니 여기서 걷어야,
                            // 다음 액션이 이 찌꺼기를 엉뚱한 테마에 흘려보내지 않는다.
                            let failed = self.theme_label_edit.take().is_some();
                            self.settings_input = None;
                            Ok(!failed)
                        }
                    }
                    // 번들은 폴더가 없어 치울 것도 없다 — 네이티브도 그 카드엔
                    // 버튼을 안 그린다. 없는 테마를 「치웠다」고 답하지 않으려면
                    // **있었는지**를 먼저 봐야 한다: 사라졌는지만 보면 처음부터
                    // 없던 것도 성공으로 읽힌다(실측).
                    "delete-theme" if arg.is_empty() => {
                        Err("기본 테마는 치울 수 없어요".to_string())
                    }
                    "delete-theme" if !theme_exists(&arg) => {
                        Err(format!("'{arg}' 테마가 없어요"))
                    }
                    "delete-theme" => {
                        self.settings_apply(SettingsAction::DeleteTheme(arg.clone()));
                        Ok(!theme_exists(&arg))
                    }
                    // 폴더 열기는 실패를 알 창구가 없다(`open_path` 는 OS 에
                    // 던지고 끝). 번들은 열 폴더가 없다는 걸 네이티브가 토스트로
                    // 말해 주고, 그 문구가 회신에 실린다 — 그래서 빈 id 는 통과.
                    "open-theme-dir" if !arg.is_empty() && !theme_exists(&arg) => {
                        Err(format!("'{arg}' 테마가 없어요"))
                    }
                    "open-theme-dir" => {
                        self.settings_apply(SettingsAction::OpenThemeDir(arg));
                        Ok(true)
                    }
                    "open-students-dir" => {
                        self.settings_apply(SettingsAction::OpenStudentsDir);
                        Ok(true)
                    }
                    "open-roster" => {
                        self.settings_apply(SettingsAction::OpenCharactersJson);
                        Ok(true)
                    }
                    "refresh-assets" => {
                        self.settings_apply(SettingsAction::RefreshStudentAssets);
                        Ok(true)
                    }
                    // 스위치는 켠 값이 파일에 남았는지로 판정한다 — 토글이라
                    // 「눌렀다」만으로는 반영됐는지 알 수 없다.
                    "toggle-persona" => {
                        self.settings_apply(SettingsAction::ToggleClaudePersona);
                        Ok(socket::read_claude_persona() == self.set_claude_persona)
                    }
                    // Theme 탭 밖의 컨트롤은 전부 여기로 — 판정 규칙이 위와 같아
                    // (액션이 남긴 상태로 본다) 한 함수에 모아 뒀다.
                    other => self.settings_web_action(other, &arg, label.as_deref()),
                };
                let message = match (&toast_before, &self.collab.toast) {
                    (_, None) => None,
                    (None, Some((m, _))) => Some(m.clone()),
                    (Some((bm, bt)), Some((m, t))) => (t != bt || m != bm).then(|| m.clone()),
                };
                let _ = reply.send(match ok {
                    Ok(ok) => Ok(serde_json::json!({ "ok": ok, "message": message })),
                    Err(e) => Err(e),
                });
                // 네이티브 설정 화면이 열려 있으면 같은 변경을 곧바로 보여야 한다.
                // `refresh_student_assets` 를 지나는 액션은 스스로 repaint 하지만
                // 나머지(만들기·치우기·이름)는 아니라, 이 한 줄이 없으면 다음 입력이
                // 올 때까지 옛 목록이 남는다.
                self.chrome_dirty = true;
                self.render_frame();
                return;
            }
            UserEvent::SocketCapture(pane, path, max_w, reply) => {
                self.arm_pane_capture(pane, path.clone(), *max_w, reply.clone());
                // 무장만으로는 부족하다 — 여기서 한 프레임을 직접 그려야 리드백이
                // 돌고 회신이 나간다. request_redraw 만 걸면 창이 가려져 있을 때
                // OS 가 그 그리기를 미뤄, 부른 쪽이 타임아웃까지 매달린다.
                self.render_frame();
                return;
            }
            UserEvent::SocketPasteImage(surface, bytes) => {
                // 아로나 프롬프트 입력창 이미지 드롭(webview) → 그 pane claude 에 첨부.
                // 시스템 클립보드에 비트맵으로 싣고 그 pane 에 Ctrl+V(0x16) — claude 가
                // paste 시 osascript 로 클립보드 PNG 를 읽어 [Image] 칩으로 단다(터미널
                // DroppedFile 과 같은 경로, 포커스 무관: 클립보드는 시스템 전역).
                if let Ok(img) = image::load_from_memory(&bytes) {
                    let rgba = img.to_rgba8();
                    let (w, h) = rgba.dimensions();
                    let data = arboard::ImageData {
                        width: w as usize,
                        height: h as usize,
                        bytes: std::borrow::Cow::Owned(rgba.into_raw()),
                    };
                    if let Ok(mut cb) = arboard::Clipboard::new() {
                        if cb.set_image(data).is_ok() {
                            if let Some(p) = self.pty_for_pane(&surface) {
                                let _ = p.send_bytes(&[0x16]);
                            }
                        }
                    }
                }
                return;
            }
            UserEvent::SocketOpenPreview(path, target) => {
                // imgopen/mdopen·SendUserFile 훅 → 요청 pane 의 보조 탭으로(크롬 탭).
                // target = 요청자 pid($KASATERM_PANE_ID); open_file 이 그 pane 을 찾아
                // 탭으로 붙인다(못 찾으면 active split 폴백).
                self.open_file(std::path::PathBuf::from(path), target.clone(), true);
                self.chrome_dirty = true;
                self.render_frame();
                return;
            }
            UserEvent::SocketOpenWeb(url, target) => {
                // `kasaterm-cli web <url>` → 요청 pane 옆에 웹 pane split.
                // 파일 미리보기와 달리 여기서 처리하는 이유: 자식 창 생성에
                // ActiveEventLoop 가 필요하다.
                self.open_web_pane(event_loop, url, target.as_deref());
                self.chrome_dirty = true;
                self.render_frame();
                return;
            }
            UserEvent::OpenMarkdownWindow(path) => {
                // macOS `.md` 더블클릭(odoc)/argv → 새 워크스페이스에 마크다운 풀.
                // cold-launch(앱 꺼진 채)면 window·pty_layout 이 아직 없어 디퍼했다가
                // start_pty 직후 flush, 켜진 채면 즉시 연다.
                let p = std::path::PathBuf::from(path);
                if self.window.is_none() || self.pty_layout.is_none() {
                    self.pending_open_md.push(p);
                } else {
                    self.open_markdown_window(p);
                }
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
                // so the sidebar session reads the rename override even when this
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
            UserEvent::ClaudeAccountAutoswitch { to, cooldown_until, pct } => {
                // 떠나는 계정을 먼저 잠근다 — 저장 순서가 반대면 그 사이 폴러가
                // 한 번 더 판정해 방금 소진한 계정으로 되돌아갈 수 있다.
                if let Some(until) = *cooldown_until {
                    socket::write_account_cooldown(&self.set_claude_account, until);
                }
                // 어느 계정으로 갈아탔는지가 이 토스트의 전부다 — 이름을 안 붙인
                // 슬롯이면 이메일로 부른다("계정 3 으로 전환" 은 아무 말도 아니다).
                let label = |id: &str| -> String {
                    match self.set_claude_accounts.iter().position(|a| a.id == id) {
                        Some(i) => crate::settings::account_display(
                            id,
                            &self.set_claude_accounts[i].label,
                            &format!("계정 {}", i + 2),
                        ),
                        None => crate::settings::account_display("", "", "기본 계정"),
                    }
                };
                let (from_label, to_label) = (label(&self.set_claude_account), label(to));
                // 지금 **에이전트가 도는** pane 은 옛 계정 토큰을 문 채로 남는다 —
                // 계정은 프로세스 env 라 뜰 때 박힌다. 표시해 두고 헤더에서 재시작을
                // 권한다(셸만 있는 pane 은 되띄울 게 없으니 뺀다).
                //
                // A→B→A 로 돌아온 pane 은 지운다. 옛 표시를 남기면 이미 맞는 계정으로
                // 도는 pane 에 「재시작할까요」가 떠 있게 된다(Orca 도 이 collapse 를
                // 따로 다룬다).
                let running: Vec<String> = self
                    .pty
                    .iter()
                    .filter(|(_, p)| p.active_agent().is_some())
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in running {
                    match self.pane_account_stale.get(&id) {
                        // 떠났던 계정으로 되돌아왔다 — 이 pane 은 다시 맞는 계정이다.
                        Some((orig, _)) if orig == &to_label => {
                            self.pane_account_stale.remove(&id);
                        }
                        // 이미 표시된 pane 은 **최초의** 계정을 유지한다. 갱신하면
                        // A→B→C 에서 "B → C" 가 되어, 실제로 도는 A 를 잃는다.
                        Some(_) => {}
                        None => {
                            self.pane_account_stale
                                .insert(id, (from_label.clone(), to_label.clone()));
                        }
                    }
                }
                self.set_claude_account = to.clone();
                // shim 을 다시 깔아 이미 열려 있는 pane 도 다음 claude 부터 새 계정으로.
                self.settings_save();
                self.set_toast(format!(
                    "{from_label} 사용량 {pct:.0}% — {to_label} 로 전환했어요 (다음에 뜨는 claude 부터)"
                ));
                self.chrome_dirty = true;
                self.render_frame();
                return;
            }
            UserEvent::ClaudeAccountExhausted { pct, resets_at } => {
                // 갈 곳이 없다. 토스트만 띄우면 창을 안 보고 있을 때 놓치므로 데스크톱
                // 알림까지 쏜다 — 이건 「일하다 막혔다」라서 지금 알아야 하는 종류다.
                //
                // 폴러가 60초마다 보낸다. `dedup` 키를 **풀리는 시각**으로 잡아 같은
                // 한도 창에서는 한 번만 울리게 한다(시각을 모르면 pct 로라도 묶는다 —
                // 키가 매번 달라지면 매분 알림이 뜬다).
                let when = resets_at
                    .map(|t| {
                        let left = t.saturating_sub(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map_or(0, |d| d.as_secs()),
                        );
                        format!("{}분 뒤 풀려요", left.div_ceil(60))
                    })
                    .unwrap_or_else(|| "언제 풀리는지는 모르겠어요".to_string());
                let body =
                    format!("사용량 {pct:.0}% — 옮겨갈 계정이 없어요. {when}");
                self.set_toast(body.clone());
                crate::chrome::notify_desktop(
                    "계정 한도",
                    &body,
                    None,
                    Some(&format!(
                        "acct-exhausted:{}",
                        resets_at.map_or_else(|| format!("pct{pct:.0}"), |t| t.to_string())
                    )),
                    None,
                );
                self.chrome_dirty = true;
                self.render_frame();
                return;
            }
            UserEvent::BgAgentsChanged => {
                // agents/attach 뷰 pane 재바인딩을 board 폴링에만 맡기지 않는다 — 웹뷰/
                // CLI 가 board 를 안 부르는 세션에선 rebind 가 영영 안 돌아 pane 이 스폰
                // 로컬 랜덤에 머물렀다(거노: 이번엔 유우카로 떠). 3s 폴러에 편승해 항상
                // 돈다. bind_transcript 는 proxy 이벤트(SocketSessionBound)라 다음 루프에
                // apply_session_character 로 이어진다.
                if let Some(be) = self.socket_backend.clone() {
                    let live: std::collections::HashSet<String> =
                        self.ws.lock().unwrap().panes.keys().cloned().collect();
                    // 반드시 GUI 스레드 **밖에서**. 이 안의 `query_pane_pids` 는 GUI
                    // 스레드에 이벤트를 보내고 답을 기다리는 구조라, GUI 스레드가
                    // 직접 부르면 자기가 처리해야 할 이벤트를 자기가 기다리는 꼴이
                    // 된다 — 300ms 를 꽉 채워 타임아웃하고 빈 목록을 받으므로 창은
                    // 3초마다 300ms 멈추고 재바인딩은 한 번도 성공하지 못한다
                    // (실측: 메인 스레드 대기 시간의 10%가 여기였다). pane argv 를
                    // 읽는 ps 포크가 프레임 밖으로 나가는 건 덤이다.
                    std::thread::spawn(move || be.rebind_agents_panes(&live));
                }
                // 포크 사전 바인딩: 새 bg 세션이 미바인딩이고 부모(argv --resume 계보)
                // 가 바인딩돼 있으면 즉시 영속 — 포크 SessionStart 의 persona 조회
                // (/persona)·board retained·attach 해석이 다음 폴부터 부모 학생을 잡는다.
                let fork_pairs: Vec<(String, String)> = self
                    .bg_agents
                    .lock()
                    .map(|m| {
                        m.iter()
                            .filter_map(|(s, p)| p.clone().map(|p| (s.clone(), p)))
                            .collect()
                    })
                    .unwrap_or_default();
                for (sid, parent) in fork_pairs {
                    if kasa_mcp::character::session_character(&sid).is_none() {
                        if let Some(c) = kasa_mcp::character::session_character(&parent) {
                            let _ = kasa_mcp::character::bind_session_character(&sid, &c);
                        }
                    }
                }
                // 폴러가 부모 맵을 갱신 → 이미 바인딩된 pane 들에 부모 학생 상속
                // 재적용(바인딩 시점엔 맵이 비어 놓쳤을 수 있음). 배지 판정도 이 맵을
                // 읽으므로 재적용 후 render 로 타이틀바 배지·이름을 함께 갱신한다.
                let pairs: Vec<(String, String)> = self
                    .pane_claude_sid
                    .iter()
                    .map(|(p, s)| (p.clone(), s.clone()))
                    .collect();
                for (pane, sid) in pairs {
                    self.apply_session_character(&pane, &sid);
                }
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
        // A focused pop-out editor window blinks its caret off the same wake
        // (blink thread / timer). request_redraw coalesces, and only the
        // focused aux window repaints — unfocused ones stay idle (no GPU burn).
        // Terminal undock 창은 예외: 이 wake 를 만든 PTY echo 가 그 창이 뷰하는
        // pane 을 갱신했을 수 있어, 포커스 여부와 무관하게 매 wake redraw 해야 셸
        // 출력이 라이브로 반영된다(idle 엔 Wait 라 wake 자체가 안 나 CPU 0).
        for a in &self.aux_windows {
            if a.focused || matches!(a.kind, crate::auxwin::AuxWindowKind::Terminal { .. }) {
                a.window.request_redraw();
            }
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Persist every session's layout + pane cwds + claude sessions so the
        // next launch restores the full workspace (A3).
        self.save_session_state();
        // Persist the window size + position so the next launch restores the
        // frame instead of the hardcoded default (껐던 크기·위치 복원).
        self.save_window_frame();
        crate::arm_self_install();
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        // macOS `.md` 더블클릭(odoc) 핸들러 등록 — resumed = applicationDidFinishLaunching
        // 시점이라, AppKit 이 launch 초기에 건 기본 odoc→`application:openURLs:` 라우팅
        // (winit 미구현 → "못 연다" 에러)을 여기서 덮어쓴다. NSAppleEventManager 는 같은
        // (class,id)에 마지막 등록이 이기므로 AppKit '후'인 여기가 정답 — main() 1차는
        // AppKit 보다 일러 되덮여 무효였다. 큐된 cold-launch odoc 디스패치보다 먼저 걸려야
        // 첫 파일도 잡으므로 resumed 최상단(window/GPU 생성 전).
        #[cfg(target_os = "macos")]
        crate::macos_open::install_open_doc_handler(self.proxy.clone());
        // Ask for desktop-notification permission up front so the prompt
        // appears at launch rather than mid-work on the first completion.
        crate::chrome::ensure_notification_authorization();
        // 배너 클릭을 받을 delegate. **알림이 배달되기 전에** 걸려야 한다 — delegate 가
        // 없는 동안 눌린 알림은 앱만 깨우고 어디로 갈지 없이 사라진다.
        #[cfg(target_os = "macos")]
        crate::macos_notify::install_notification_click_handler(self.proxy.clone());
        // Windows 자동 업데이트 시작 — WinSparkle.dll 이 있을 때만(없으면 no-op).
        // mac 메뉴 블록은 cfg(macos) 라, 여기(메뉴 밖)서 별도로 건다.
        #[cfg(windows)]
        crate::win_sparkle::init();
        // macOS menu bar: app submenu (About/Quit) + a "보기" submenu with
        // the "Git 패널" toggle. Built once (NSApp exists by resumed). Clicks
        // arrive on muda's global channel, drained in about_to_wait. Stored
        // on self so the menu outlives this function.
        #[cfg(target_os = "macos")]
        if self.menu.is_none() {
            use muda::accelerator::Accelerator;
            use muda::{Menu, MenuItem, PredefinedMenuItem, Submenu};
            let menu = Menu::new();
            let app_m = Submenu::new("kasaterm", true);
            let update_item = MenuItem::new("업데이트 확인…", true, None);
            // ⌘Q 를 가로채 종료 확인(ghostty 식)을 띄우려면 PredefinedMenuItem::quit(OS 가
            // 직접 terminate 라 가로채기 불가) 대신 커스텀 항목으로 — MenuEvent 로 받아 NSAlert.
            let quit_item = MenuItem::new("kasaterm 종료", true, "CmdOrCtrl+Q".parse::<Accelerator>().ok());
            let _ = app_m.append_items(&[
                &PredefinedMenuItem::about(None, None),
                &update_item,
                &PredefinedMenuItem::separator(),
                &quit_item,
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
            // 편집 메뉴 — macOS 는 이 메뉴(Cmd+V/Cmd+C 단축키)가 있어야 아로나
            // webview 입력창 붙여넣기가 먹는다. 다만 PredefinedMenuItem::paste/copy 는
            // 그 단축키 keyDown 을 NSMenu 가 가로채 winit 까지 안 내려보내 터미널
            // paste/copy 가 먹통이었다(거노). 그래서 Copy/Paste 만 *커스텀* 항목으로
            // 만들어 MenuEvent 로 받고, webview 우선 위임(send_*_action) 후 안 먹으면
            // 직접 클립보드를 처리한다. Cut/SelectAll(Cmd+X/A)은 터미널이 안 쓰니 predefined 유지.
            let copy_item = MenuItem::new("복사", true, "CmdOrCtrl+C".parse::<Accelerator>().ok());
            let paste_item = MenuItem::new("붙여넣기", true, "CmdOrCtrl+V".parse::<Accelerator>().ok());
            let edit_m = Submenu::new("편집", true);
            let _ = edit_m.append_items(&[
                &PredefinedMenuItem::undo(None),
                &PredefinedMenuItem::redo(None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::cut(None),
                &copy_item,
                &paste_item,
                &PredefinedMenuItem::select_all(None),
            ]);
            let _ = menu.append(&app_m);
            let _ = menu.append(&edit_m);
            let _ = menu.append(&view_m);
            menu.init_for_nsapp();
            self.git_menu_item = Some(git_item);
            self.session_menu_item = Some(session_item);
            self.board_menu_item = Some(board_item);
            self.arona_menu_item = Some(arona_item);
            self.copy_menu_item = Some(copy_item);
            self.paste_menu_item = Some(paste_item);
            self.update_menu_item = Some(update_item);
            self.quit_menu_item = Some(quit_item);
            self.menu = Some(menu);
            // Sparkle 자동 업데이트 시작(.app 빌드에서만 — dev 는 framework 없어 no-op).
            self.sparkle_updater = crate::macos_sparkle::init();
        }
        // WaitUntil so the cursor blink ticks even when no terminal output
        // is arriving — the redraw inside RedrawRequested re-arms the
        // schedule. Pure Wait would freeze the blink mid-phase, Poll would
        // burn CPU on idle.
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + std::time::Duration::from_millis(BLINK_HALF_PERIOD_MS),
        ));
        // Restore the last window size; fall back to the default on first run.
        // `KASATERM_WINDOW_SIZE="w,h"` — 저장된 크기를 무시한다. 폭에 딸린 버그
        // (학생 테마가 좁은 pane 에서 안 붙는 것 같은)는 **그 폭으로 띄워야만** 재현되는데,
        // 검증 인스턴스는 저장된 크기를 물려받아 늘 넓게 떴다. `KASATERM_WINDOW_POS` 와 같은
        // 목적·같은 형식.
        let forced_size = std::env::var("KASATERM_WINDOW_SIZE").ok().and_then(|s| {
            let (a, b) = s.split_once(',')?;
            Some((a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?))
        });
        let (init_w, init_h) = forced_size
            .or_else(crate::socket::read_window_size)
            .unwrap_or((1100.0, 860.0));
        // 저장된 자리가 아직 살아 있는 화면이면 **창을 만들 때부터** 그 자리에
        // 띄운다. 만든 뒤에 옮기면 세 가지를 잃는다: ① 저장된 크기가 만들어진
        // 화면에 맞춰 깎이고(큰 모니터용 3840 이 내장에서 1512 로 잘렸다) ②
        // 레이어 backing scale 이 만들어진 화면 값으로 박히며 ③ 옮긴 뒤에야
        // 되잡는 한 박자가 생긴다. 판정 기준은 좌상단 점이 아니라 창 중심이다 —
        // 점으로 보면 창을 화면 가장자리에 붙이는 흔한 습관에 1픽셀만 어긋나도
        // 위치 기억이 통째로 버려진다(실측 x=1510, 화면 시작 1512).
        let restore_pos = crate::socket::read_window_pos().filter(|&(px, py)| {
            event_loop.available_monitors().any(|m| {
                let mp = m.position();
                let ms = m.size();
                let sf = m.scale_factor();
                let (cx, cy) = (px + init_w * sf / 2.0, py + init_h * sf / 2.0);
                cx >= mp.x as f64
                    && cx < (mp.x as f64 + ms.width as f64)
                    && cy >= mp.y as f64
                    && cy < (mp.y as f64 + ms.height as f64)
            })
        });
        let attrs = WindowAttributes::default()
            .with_title("kasaterm")
            // Force dark appearance so the system titlebar paints its
            // text in light gray. Default is "follow OS", which would
            // give black text on our dark content view and make the
            // process-name label nearly invisible in light mode.
            .with_theme(Some(Theme::Dark))
            .with_inner_size(LogicalSize::new(init_w, init_h))
            // 배경 실행(검증 캡처)일 땐 뜨면서 키 포커스를 가져가지 않는다.
            .with_active(!crate::background_launch());
        // `KASATERM_WINDOW_POS="x,y"` — 저장된 위치를 무시하고 거기 띄운다. 헤드리스
        // 검증용이다: 그냥 두면 테스트 인스턴스가 **저장된 자리**(=쓰던 모니터의
        // 그 자리)에 떠서 작업 화면을 덮는다. `KASATERM_NO_FOCUS` 가 키 포커스는
        // 막아 주지만 가리는 것까지는 못 막는다(거노: "포커스 안 뺏어가게 맥북에
        // 띄워서 해봐"). 기본 디스플레이 좌표가 (0,0) 이라 `100,100` 이면 맥북 화면이다.
        let forced_pos = std::env::var("KASATERM_WINDOW_POS").ok().and_then(|s| {
            let (a, b) = s.split_once(',')?;
            Some((a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?))
        });
        let attrs = match forced_pos.or(restore_pos) {
            Some((px, py)) => attrs.with_position(winit::dpi::PhysicalPosition::new(px, py)),
            None => attrs,
        };
        let restore_pos = forced_pos.or(restore_pos);
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
            crate::auxwin::create_untabbed(event_loop, attrs).expect("create window"),
        );
        // `with_position` 을 무시하는 플랫폼(일부 Wayland 컴포지터)을 위한 폴백.
        // 이미 그 자리에 떴으면 no-op 이라 mac/Windows 에선 값이 없다.
        if let Some((px, py)) = restore_pos {
            window.set_outer_position(winit::dpi::PhysicalPosition::new(px, py));
        }
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
        // 학생 도트 배너 애니 타이머. Clawd 배너 자리에 학생 도트가 보이는
        // 동안만 프레임 주기로 redraw를 깨운다(blink 스레드와 같은 proxy
        // 패턴). 배너가 없으면 sleep+load만 도는 무비용 루프.
        {
            let anim_proxy = self.proxy.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(
                    crate::render::STUDENT_ANIM_FRAME_MS,
                ));
                let animating = crate::render::STUDENT_SPRITE_ANIMATING
                    .load(std::sync::atomic::Ordering::Relaxed);
                if animating && anim_proxy.send_event(UserEvent::Redraw).is_err() {
                    break;
                }
            });
        }
        // ultracode 혜성 타이머. 도트 배너 스레드와 같은 패턴이지만 주기가 다르다
        // (66ms) — 혜성은 픽셀 이동이라 200ms 론 순간이동으로 보인다. ultracode
        // pane 이 없으면 sleep+load 만 도는 무비용 루프.
        {
            let comet_proxy = self.proxy.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(
                    crate::render::ULTRA_COMET_FRAME_MS,
                ));
                let animating = crate::render::ULTRA_COMET_ANIMATING
                    .load(std::sync::atomic::Ordering::Relaxed);
                if animating && comet_proxy.send_event(UserEvent::Redraw).is_err() {
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
        // claude 한도 폴러 — 로컬 /claude-usage(oauth/usage 프록시)를 조회해 **가장 먼저
        // 닫히는 창**의 사용률을 채운다. Info 탭 머리 계정 행의 소스. curl 로 로컬
        // 엔드포인트만 쳐 토큰은 서버(키체인)가 읽는다 — argv 유출 없음. 값이 바뀔 때만
        // redraw.
        //
        // 주기는 60초지만 **5초 단위로 쪼개 자며** 계정이 바뀌었는지 본다. 전에는 통째로
        // 60초를 자서, 계정을 눌러도 숫자가 최대 1분(+서버 캐시 1분) 동안 옛 계정 것으로
        // 남았다 — 거노: "누를때마다 바뀐다는 표시가 없고".
        {
            let usage_proxy = self.proxy.clone();
            let usage_cache = self.claude_usage.clone();
            let usage_all = self.claude_usage_all.clone();
            std::thread::spawn(move || {
                // 방금 전환했으면 잠시 **자동 전환 판정만** 쉰다(표시는 계속 갱신).
                let mut last_switch: Option<std::time::Instant> = None;
                let mut seen_account = socket::read_claude_account();
                // 비활성 계정 조회까지 남은 사이클 수(0 이면 이번에 친다). 60초 주기라
                // 5 = 5분. 첫 바퀴는 0 이라 창을 열자마자 표가 찬다.
                let mut others_due = 0u8;
                loop {
                    // 조회할 슬롯을 **매 사이클 설정에서 다시 읽어 URL 에 못 박는다.**
                    //
                    // 전에는 dir 을 안 넘겼는데, 그 경우 프록시(kasa-mcp/http.rs)는
                    // 「활성 슬롯」이 아니라 **자기 프로세스의**
                    // `KASATERM_CLAUDE_ACCOUNT_DIR` env 로 떨어진다. 그 env 를 세우는
                    // 곳은 shim 을 굽는 자리 하나뿐이라 이 앱의 설정과 갈릴 수 있고,
                    // 특히 `mcp_panel_port()` 의 최후 폴백이 8765 라 **다른 인스턴스의**
                    // 서버에 물리면 남의 계정 숫자로 판정한다(2026-08-13 실측: 설정은
                    // 기본 슬롯인데 조회는 acct-2 를 보고 있었다).
                    //
                    // 그래서 자동전환이 「떠날 계정이 아닌 계정」의 사용률을 보고,
                    // 100% 인 슬롯을 쓰면서 47% 를 읽어 95% 게이트가 영영 안 열렸다.
                    // 슬롯을 URL 로 말하면 어느 인스턴스가 답하든 답이 맞는다.
                    //
                    // `id` 와 `dir` 은 **한 스냅샷**이다(dir 은 id 에서 순수 파생) —
                    // 아래 판정의 `current` 가 같은 값을 봐야 「어느 계정의 숫자인가」와
                    // 「어느 계정에서 떠나는가」가 안 갈린다. 기본 슬롯은 경로가 없고
                    // (`claude_account_dir("") == None`) 프록시에서 빈 문자열이 곧 기본
                    // 로그인이라, 빈 문자열로 눌러 넘기면 의미가 정확히 맞는다.
                    let active_id = socket::read_claude_account();
                    let active_dir = socket::claude_account_dir(&active_id)
                        .map_or(String::new(), |p| p.to_string_lossy().into_owned());
                    let fetched = fetch_claude_usage(&crate::mcp_panel_port(), &active_dir);
                    let usage = fetched.as_ref().map(|(u, _, _)| u);
                    let next = fetched.as_ref().and_then(|(u, stale, dir)| {
                        socket::usage_pressure(u).map(|p| crate::UsageBadge {
                            pct: p.pct,
                            label: p.label,
                            stale: *stale,
                            account_dir: dir.clone(),
                            resets_at: p.resets_at,
                        })
                    });
                    // 아래 계정별 표에서 다시 쓴다 — 활성 계정을 두 번 조회하지 않게.
                    let active_badge = next.clone();
                    // 일시적 fetch 실패(None)면 마지막 유효값을 유지 — 깜빡임/사라짐 방지.
                    // (git col 폴러와 동일 정책.)
                    if next.is_some() {
                        match usage_cache.lock() {
                            Ok(mut g) => {
                                if *g != next {
                                    *g = next;
                                    drop(g);
                                    if usage_proxy.send_event(UserEvent::Redraw).is_err() {
                                        break;
                                    }
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    // 등록된 **모든** 계정의 한도 — 드롭다운이 누르기 전에 보여줘야 하는
                    // 값이다(거노: "누르면 전환되버리잖아"). 활성 계정은 방금 받은 값을
                    // 그대로 재사용하고, 나머지만 슬롯을 지정해 추가로 조회한다.
                    // 프록시가 슬롯별 토큰을 직접 읽어 **전환 없이** 답한다.
                    //
                    // ⚠️ 오래 안 쓴 계정은 못 읽는다 — OAuth 토큰이 8시간쯤에 만료되고
                    // 갱신은 그 계정으로 claude 를 돌릴 때 일어난다(실측 2026-08-05:
                    // 세 슬롯 중 둘이 3시간 전 만료라 usage 가 거부됐다). 그런 계정은
                    // 표에 안 들어가고 화면이 `—`(모름)를 그린다.
                    //
                    // 비활성 슬롯은 **5분마다**만 친다. 매 사이클 치면 만료된 슬롯에
                    // 헛 curl 을 계속 띄우고, 살아 있는 슬롯은 upstream 레이트리밋을
                    // 활성 계정과 나눠 쓰게 된다. 6시간짜리 디스크 스냅샷이 사이를 메운다.
                    // 지금 쓰는 계정 값은 **매 사이클** 표에도 넣는다. 표 전체를 5분마다만
                    // 갱신하던 동안, 드롭다운·계정 행의 숫자는 활성 계정 것마저 5분 동안
                    // 굳어 있었다 — 계정을 눌러 전환할 때만 움직이는 것처럼 보인 이유다
                    // (거노 2026-08-07: "전환해야만 사용량 갱신되는데"). 비활성 슬롯은
                    // 아래 5분 주기 그대로다(만료 토큰에 헛 curl, upstream 레이트리밋 공유).
                    if others_due != 0 {
                        if let Some(b) = active_badge.clone() {
                            if let Ok(mut g) = usage_all.lock() {
                                if g.get(&b.account_dir) != Some(&b) {
                                    g.insert(b.account_dir.clone(), b);
                                    drop(g);
                                    if usage_proxy.send_event(UserEvent::Redraw).is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    if others_due == 0 {
                        let mut all: HashMap<String, crate::UsageBadge> = HashMap::new();
                        if let Some(b) = active_badge {
                            all.insert(b.account_dir.clone(), b);
                        }
                        let mut dirs: Vec<String> = vec![String::new()];
                        dirs.extend(socket::read_claude_accounts().iter().filter_map(|a| {
                            socket::claude_account_dir(&a.id)
                                .map(|p| p.to_string_lossy().into_owned())
                        }));
                        for d in dirs {
                            if all.contains_key(&d) {
                                continue;
                            }
                            let Some((u, stale, dir)) =
                                fetch_claude_usage(&crate::mcp_panel_port(), &d)
                            else {
                                continue;
                            };
                            if let Some(p) = socket::usage_pressure(&u) {
                                all.insert(
                                    dir.clone(),
                                    crate::UsageBadge {
                                        pct: p.pct,
                                        label: p.label,
                                        stale,
                                        account_dir: dir,
                                        resets_at: p.resets_at,
                                    },
                                );
                            }
                        }
                        // 조회가 통째로 실패한 사이클엔 옛 표를 지우지 않는다 — 빈칸은
                        // "한도 여유"로 읽혀서, 낡은 숫자보다 나쁘다.
                        if !all.is_empty() {
                            match usage_all.lock() {
                                Ok(mut g) => {
                                    if *g != all {
                                        *g = all;
                                        drop(g);
                                        if usage_proxy.send_event(UserEvent::Redraw).is_err() {
                                            break;
                                        }
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        others_due = 5;
                    } else {
                        others_due -= 1;
                    }
                    // 한도 자동 계정 전환. 판정은 여기(폴러)서 하고 실제 전환은 GUI
                    // 스레드가 한다 — 설정 저장이 shim 을 다시 까는 일이라 App 이 필요하다.
                    //
                    // **stale 값으로는 안 옮긴다**: upstream 이 막혀 재사용된 숫자로
                    // 계정을 떠나면 멀쩡한 자리를 옛 기록 때문에 버리는 셈이다.
                    let rested = last_switch.is_none_or(|t| t.elapsed().as_secs() >= 300);
                    let fresh = fetched.as_ref().is_some_and(|(_, stale, _)| !*stale);
                    // 소진된 슬롯을 **전환과 무관하게** 쿨다운에 적는다.
                    //
                    // 전에는 쿨다운이 전환이 실제로 일어날 때만(`ClaudeAccountAutoswitch`
                    // 처리부) 찍혔다. 그래서 「전환 못 하고 그냥 터진」 슬롯은 기록이 없고,
                    // `pick_next_account` 는 쿨다운만 보므로 그 슬롯을 멀쩡한 후보로 골라
                    // 옮겨가자마자 또 막혔다(2026-08-13 실측: acct-3 이 100% critical 인데
                    // account-cooldown.json 에 키 자체가 없었다).
                    //
                    // 표(`usage_all`)는 모든 슬롯의 압력을 이미 알고 있으니 여기서 적는다.
                    // 기준은 자동전환 임계와 같은 값이다 — 「옮겨갈 수 없다고 판단하는 선」과
                    // 「가면 안 된다고 적는 선」이 다르면 그 사이 값에서 왕복이 생긴다.
                    // `write_account_cooldown` 은 더 뒤를 가리키는 기록을 덮지 않으므로
                    // 매 사이클 불러도 안전하고, resets_at 이 없는 슬롯은 언제 풀릴지 모르니
                    // 적지 않는다(모르는 값으로 잠그면 영영 안 풀린다).
                    {
                        let limit = socket::read_account_autoswitch_pct();
                        let known: Vec<(String, std::path::PathBuf)> = socket::read_claude_accounts()
                            .iter()
                            .filter_map(|a| socket::claude_account_dir(&a.id).map(|d| (a.id.clone(), d)))
                            .collect();
                        if let Ok(g) = usage_all.lock() {
                            for (id, dir) in &known {
                                let key = dir.to_string_lossy();
                                if let Some(b) = g.get(key.as_ref()) {
                                    if !b.stale && b.pct >= limit {
                                        if let Some(until) = b.resets_at {
                                            socket::write_account_cooldown(id, until);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if let (Some(u), true, true, true) =
                        (usage, socket::read_account_autoswitch(), rested, fresh)
                    {
                        if let Some(p) = socket::usage_pressure(u) {
                            if p.pct >= socket::read_account_autoswitch_pct() {
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map_or(0, |d| d.as_secs());
                                // `active_id` 재사용 — 위 조회와 **같은 스냅샷**이어야
                                // 「어느 계정의 숫자로 판정했나」와 「어느 계정에서
                                // 떠나나」가 안 갈린다. 그 사이에 비활성 슬롯 curl 이
                                // 끼므로(슬롯당 최대 5초) 다시 읽으면 값이 바뀔 수 있다.
                                let to = socket::pick_next_account(
                                    &active_id,
                                    &socket::read_claude_accounts(),
                                    &socket::read_account_cooldowns(),
                                    now,
                                );
                                match to {
                                    Some(to) => {
                                        last_switch = Some(std::time::Instant::now());
                                        let ev = UserEvent::ClaudeAccountAutoswitch {
                                            to,
                                            cooldown_until: p.resets_at,
                                            pct: p.pct,
                                        };
                                        if usage_proxy.send_event(ev).is_err() {
                                            break;
                                        }
                                    }
                                    None => {
                                        // 갈 곳이 없다 — 남은 계정이 전부 쿨다운이거나
                                        // 등록된 게 하나뿐이다. 전에는 여기서 **조용히**
                                        // 아무 일도 안 일어나, 리밋에 걸린 줄 모르고 손으로
                                        // 계정마다 로그인하는 일이 벌어졌다(거노 2026-08-13:
                                        // "방금도 리밋걸린거 하나씩 로그인함").
                                        //
                                        // 폴러는 60초마다 도는 백그라운드 스레드라 알림을
                                        // 직접 쏘면 매분 뜬다. GUI 로 넘겨 dedup 을 태운다.
                                        if usage_proxy
                                            .send_event(UserEvent::ClaudeAccountExhausted {
                                                pct: p.pct,
                                                resets_at: p.resets_at,
                                            })
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // 60초를 5초씩 쪼개 자며 활성 계정이 바뀌었는지 본다. 바뀌면 즉시
                    // 다시 조회한다 — 서버 캐시도 계정별로 갈렸으니 그 조회는 캐시
                    // 미스라 새 계정 값을 곧바로 물어 온다. 설정 파일을 보는 것은
                    // 폴러가 GUI 상태를 못 만지기 때문이고, `settings_save` 가 파일과
                    // shim env 를 함께 갱신하므로 이 신호로 충분하다.
                    for _ in 0..12 {
                        std::thread::sleep(std::time::Duration::from_secs(5));
                        let now = socket::read_claude_account();
                        if now != seen_account {
                            seen_account = now;
                            break;
                        }
                    }
                }
            });
        }
        // bg-agents 폴러. `claude agents --json --all` 로 claude 자체 supervisor 의
        // background 세션(←← detach 로 fork 된 것)을 3초마다 조회해 sessionId→
        // parentSessionId 맵을 채운다. 타이틀바 배지·학생 유지(부모 캐릭터 상속)가
        // 이 맵을 읽는다. raw 출력엔 parentSessionId 가 없어(kind/pid 만) pid argv 의
        // `--resume` 로 부모를 잇는다(background_agents_handler 와 동일 방식).
        {
            let bg_proxy = self.proxy.clone();
            let bg_cache = self.bg_agents.clone();
            std::thread::spawn(move || {
                let bin = kasa_mcp::claude_bin();
                // pid argv 의 `--resume <path>` basename = 부모 세션 uuid. ←← detach 는
                // 부모 대화를 fork 해 새 sessionId 로 잇는데 raw json 엔 부모가 없어,
                // 이 argv 가 원본→background 를 잇는 유일한 끈이다(macOS/Linux ps).
                let parent_of = |pid: u64| -> Option<String> {
                    let out = crate::proc::command("ps")
                        .args(["-ww", "-o", "command=", "-p", &pid.to_string()])
                        .output()
                        .ok()?;
                    if !out.status.success() {
                        return None;
                    }
                    let cmd = String::from_utf8_lossy(&out.stdout);
                    let mut it = cmd.split_whitespace();
                    while let Some(tok) = it.next() {
                        if tok == "--resume" {
                            return std::path::Path::new(it.next()?)
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .map(str::to_string);
                        }
                    }
                    None
                };
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    let mut next: HashMap<String, Option<String>> = HashMap::new();
                    if let Ok(out) = crate::proc::command(&bin)
                        .args(["agents", "--json", "--all"])
                        .output()
                    {
                        if out.status.success() {
                            if let Ok(v) =
                                serde_json::from_slice::<serde_json::Value>(&out.stdout)
                            {
                                let arr = v
                                    .as_array()
                                    .cloned()
                                    .or_else(|| v.get("agents").and_then(|a| a.as_array().cloned()))
                                    .unwrap_or_default();
                                for a in &arr {
                                    if a.get("kind").and_then(|k| k.as_str()) != Some("background") {
                                        continue;
                                    }
                                    let Some(sid) = a.get("sessionId").and_then(|s| s.as_str())
                                    else {
                                        continue;
                                    };
                                    let parent = a
                                        .get("parentSessionId")
                                        .and_then(|s| s.as_str())
                                        .map(str::to_string)
                                        .or_else(|| {
                                            a.get("pid").and_then(|p| p.as_u64()).and_then(parent_of)
                                        });
                                    next.insert(sid.to_string(), parent);
                                }
                            }
                        }
                    }
                    let mut guard = match bg_cache.lock() {
                        Ok(g) => g,
                        Err(_) => break,
                    };
                    if *guard != next {
                        *guard = next;
                        drop(guard);
                        // 배지 redraw + 부모 학생 상속 재적용(BgAgentsChanged 가 둘 다).
                        if bg_proxy.send_event(UserEvent::BgAgentsChanged).is_err() {
                            break;
                        }
                    }
                }
            });
        }
        // bg SendMessage 브리지 — detach 포크(teammate 플래그 유실)에게 kt-* 팀
        // 인박스 미읽음을 attach pty 주입으로 배달. bg-agents 폴러가 채우는 맵을
        // 읽기 전용으로 공유받는다.
        crate::bridge::spawn_inbox_bridge(self.bg_agents.clone());
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
        // Chrome-style session restore: if the last run left a saved layout with
        // at least one claude pane, offer to reopen it (크롬 "복원하시겠습니까?")
        // instead of silently starting fresh. The blank session start_pty just
        // spawned is the "새로 시작" fallback that stays if the user declines;
        // 복원 tears it down and rebuilds. Tmux backend manages its own restore,
        // so only the direct-PTY path prompts.
        // 검증 실행은 거노의 저장 세션을 안 읽는다 — 복원 대화상자가 캡처를 통째로
        // 덮어서 정작 봐야 할 화면이 안 보였고(실측 2026-08-06 좁은 창 촬영), 실수로
        // "복원"이 눌리면 그 인스턴스가 14 pane 을 열어 셸을 무더기로 띄운다.
        // 저장하지 않는 실행이 읽지도 않는 것이 짝이 맞다(`save_window_frame` 가드).
        if !want_tmux && !crate::verification_run() {
            if let Some(state) = crate::socket::read_session_state() {
                // 기준은 claude 수가 아니라 전체 pane 수 — 셸만 쓰던 창도 레이아웃과
                // 스크롤백은 되살릴 값이 있다(claude 기준이면 아무것도 못 되살린다).
                if App::count_panes(&state) > 0 {
                    self.restore_prompt = Some(state);
                }
            }
        }
        // cold-launch 로 디퍼됐던 `.md` 오픈을 연다 — 이제 window·pty_layout(%0)
        // 둘 다 준비됨. 빈손이면 무비용.
        for p in std::mem::take(&mut self.pending_open_md) {
            self.open_markdown_window(p);
        }
        self.schedule_autosend();
        self.schedule_autocapture();
        self.arm_autosplit();
        self.arm_autowindows();
        self.arm_autoexpand();
        self.arm_autoalert();
        self.arm_autotoggle();
        self.arm_autoarona();
        // 온보딩 제거(거노) — 강제 ModePicker 자동오픈 안 함. 터미널이 기본,
        // SCHALE OS 는 타이틀바 ✨ 버튼/단축키(Cmd+Shift+A)로 켠다(progressive disclosure).
        self.arm_autotabs();
        self.arm_autodrag();
        self.arm_autopanemove();
        self.arm_force_drag();
        self.arm_auto_pane_merge();
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
        // 실제 이벤트로 깨어났다 = 저장할 거리가 생겼을 수 있다. 우리가 건
        // 자동 저장 타이머(new_events 의 ResumeTimeReached)로는 세우지 않는다 —
        // 그러면 idle 상태에서도 5초마다 wake→touched→wake 가 영구히 돈다.
        self.session_touched = true;
        // Child panel windows (session/board) drive their own wry webviews.
        // Their events must never reach the terminal logic below: without this
        // guard a panel's Resized/ScaleFactorChanged falls through and calls
        // gpu.resize() with the panel's tiny size, shrinking the main wgpu
        // viewport uniform → everything renders ~2x zoomed; a CloseRequested
        // would exit the whole app instead of just closing the panel.
        // 웹 pane 자식 창 — 패널들과 같은 가드(아래 주석 참조). Cmd+W 는 그
        // 웹 pane 닫기로 해석한다(webpane.rs).
        if self.web_host_window_event(id, &event) {
            return;
        }
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
        // 설정 웹뷰 창. 위 패널들과 같은 격리가 **반드시** 필요하다 — 이 가드가
        // 없으면 이 창의 Resized 가 아래로 흘러 `g.resize()` 를 이 창 크기로 불러
        // 메인 뷰포트가 통째로 줄고(모든 게 2배 확대로 보인다), CloseRequested 는
        // 패널 하나가 아니라 **앱 전체**를 종료시킨다.
        if self.settings_web_window.as_ref().map(|w| w.id()) == Some(id) {
            match &event {
                WindowEvent::CloseRequested => self.close_settings_web_window(),
                WindowEvent::Resized(size) => {
                    if let Some(wv) = self.settings_web_webview.as_ref() {
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
        // 별도 wgpu 편집기/파일뷰 창(auxwin.rs). 자체 GpuRenderer 를 가지므로 메인
        // 창의 surface·터미널 로직과 완전히 격리 — 이벤트를 kind 별 라우팅에 위임한다.
        if let Some(pos) = self.aux_windows.iter().position(|a| a.window.id() == id) {
            self.aux_window_event(pos, event, event_loop);
            return;
        }
        let Some(window) = self.window.clone() else { return; };
        // 위 가드를 다 통과했어도 **메인 창이 아닌 id** 는 여기로 오면 안 된다.
        // 패널을 닫는 순간 필드를 먼저 비우므로, 같은 배치에 남아 있던 그 창의
        // Resized 가 이 아래로 흘러 `g.resize()` 를 남의 크기로 부른다 — 위
        // 1050행대 주석이 기록한 바로 그 사고(메인 뷰포트가 통째로 줄어듦)다.
        if id != window.id() {
            return;
        }
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
            WindowEvent::ScaleFactorChanged { .. } => {
                // 모니터가 바뀌면 뷰가 창보다 작게 남는 일이 있다 — 그 상태는
                // 앱 내부에선 아무 모순도 안 보이므로(레이어·inner_size·스왑체인이
                // 전부 같이 작아진다) 창 기준으로 먼저 되잡는다.
                gpu::ensure_view_fills_window(&window);
                // 레이어 backing scale 은 AppKit 이 갱신해 주지 않는다(실측: 내장↔외부를
                // 세 번 오가도 창을 만들 때 값 그대로). 새 drawable 을 잡기 **전**에
                // 맞춰야 둘의 짝이 같은 프레임에 성립한다.
                gpu::ensure_layer_scale_matches(&window);
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
            // 창 이동은 렌더에 영향 없음 — 프레임 저장 디바운스만 재장전.
            WindowEvent::Moved(_) => {
                self.window_frame_save_due =
                    Some(Instant::now() + std::time::Duration::from_millis(1000));
                // Track the last un-zoomed frame so a green-button zoom (which
                // bypasses toggle_maximize_no_anim) still restores on the next
                // title double-click.
                gpu::remember_unzoomed_frame(&window, &mut self.saved_window_frame);
            }
            WindowEvent::Resized(size) => {
                self.window_frame_save_due =
                    Some(Instant::now() + std::time::Duration::from_millis(1000));
                gpu::remember_unzoomed_frame(&window, &mut self.saved_window_frame);
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
                if focused {
                    // A restored pane can acquire its footer after the first
                    // PTY snapshot, making its initially assigned grid a few
                    // rows too tall. Focus return is a reliable reconciliation
                    // point, and same-size PTY resizes are no-ops.
                    let (cols, rows) = self.window_cells();
                    self.resize_backend(cols, rows);
                    for pane in self.pty.values() {
                        pane.publish_full_snapshot();
                    }
                }
                self.repaint_all();
                window.request_redraw();
            }
            WindowEvent::Occluded(false) => {
                self.repaint_all();
                window.request_redraw();
            }
            // 비-macOS 에서 OS 의 밝게/어둡게를 아는 유일한 창구. macOS 는 창
            // 장식을 다크로 고정해 둬 이 값이 시스템을 안 나타내므로, 그쪽은
            // theme.rs 가 시스템 설정을 직접 읽고 이 이벤트는 흘려보낸다.
            WindowEvent::ThemeChanged(t) => {
                theme::note_window_theme(t == winit::window::Theme::Light);
                if theme::poll_system_theme() {
                    self.begin_theme_fx();
                    self.repaint_all();
                }
                window.request_redraw();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_wheel(delta);
            }
            // 트랙패드 핀치(macOS magnification) → 커서 아래 이미지 pane 줌.
            WindowEvent::PinchGesture { delta, .. } => {
                self.handle_pinch(delta);
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = self.effective_scale();
                if self.autohover.is_none() {
                    self.cursor_px = (position.x as f32 / scale, position.y as f32 / scale);
                }
                // 호버 툴팁 시계를 다시 잰다. 셀 하나를 넘게 움직였을 때만 —
                // 손 떨림 수준의 1px 이동으로 시계가 매번 리셋되면 툴팁이 영원히
                // 안 뜬다. 이미 떠 있던 툴팁은 자리를 벗어나는 순간 접는다.
                {
                    let (cx, cy) = self.cursor_px;
                    let moved = self
                        .hover
                        .as_ref()
                        .is_none_or(|h| (h.at.0 - cx).abs() > 4.0 || (h.at.1 - cy).abs() > 4.0);
                    if moved {
                        if self.hover.as_ref().is_some_and(|h| h.text.is_some()) {
                            self.chrome_dirty = true;
                        }
                        self.hover = Some(crate::HoverState {
                            at: (cx, cy),
                            since: std::time::Instant::now(),
                            req: None,
                            text: None,
                        });
                    }
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
                // Commit modal is a full-window overlay over the pane grid —
                // drive its cursor here (I-beam over the message field, default
                // elsewhere) and skip the pane/column hover below so it can't
                // override the cursor. (Settings is now its own window.)
                if self.git.commit_modal_open {
                    let (cx, cy) = self.cursor_px;
                    let hit = |r: (f32, f32, f32, f32)| {
                        cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                    };
                    let want_text = self.git.commit_input_rect.map(hit).unwrap_or(false);
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
                    // 터미널 셀 위에서도 I-beam 을 쓸지는 설정이 정한다(기본 화살표).
                    // 글자를 고르는 자리라 I-beam 이 맞다는 쪽과, 화살표여야 클릭 대상이
                    // 보인다는 쪽이 갈려서 고르는 몫을 사람에게 넘긴다. 위 입력칸 판정은
                    // 설정과 무관하게 그대로다 — 거긴 정말 글자를 치는 자리다.
                    let over_cells = self.mouse_cursor == "ibeam"
                        && self.px_to_pane_cell(cx, cy).is_some();
                    let want_text = over_cells
                        || (self.file_tree.visible && hit(self.file_tree.search_rect))
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
                // ghostty ⋮ 핸들: pane 상단 띠(top_zone) 진입/이탈 시 redraw해
                // ⋮ 등장·소멸을, ⋮ rect 위(on_handle)면 손모양+진함을 갱신한다
                // (render는 cursor_px를 live로 읽으니 redraw만 트리거하면 됨).
                {
                    let (cx, cy) = self.cursor_px;
                    let hit = |r: &(f32, f32, f32, f32)| {
                        cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                    };
                    let in_zone = self.pane_top_zones.iter().any(|(_, r)| hit(r));
                    if in_zone != self.handle_zone_hovered {
                        self.handle_zone_hovered = in_zone;
                        self.chrome_dirty = true;
                        window.request_redraw();
                    }
                    let on_handle = self.pane_handle_rects.iter().any(|(_, r)| hit(r));
                    if on_handle != self.handle_hovered {
                        self.handle_hovered = on_handle;
                        // 커서 모양은 아래 hover-feedback 블록(매 move 최종 set_cursor)
                        // 이 ⋮ 까지 보고 결정한다 — 여기서 set 하면 곧 덮어써짐.
                        self.chrome_dirty = true;
                        window.request_redraw();
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
                        // Ctrl+드래그 중 하단 세로선(split_htov_at 이 만든 [..,1])이 상단
                        // 세로선과 ratio 정렬되면 관통 세로선으로 재병합 — 그때부턴 상하가
                        // 같이 움직인다(거노: 위에랑 맞춰지면 같이). resize_drag 를 관통
                        // divider 로 전환해 이후 드래그가 상하 함께 이동한다. merge_vtoh_at
                        // 이 구조(V split + 상하 H)까지 검증하므로 일반 divider 엔 무해.
                        if self.modifiers.control_key()
                            && dir == kasa_pty::SplitDir::Horizontal
                            && path.last() == Some(&1)
                        {
                            let v_path = path[..path.len() - 1].to_vec();
                            let snap = 1.5 / cols.max(1) as f32;
                            if let Some(merged) = self
                                .pty_layout
                                .as_mut()
                                .and_then(|t| t.merge_vtoh_at(&v_path, snap))
                            {
                                self.resize_drag =
                                    Some((merged, kasa_pty::SplitDir::Horizontal));
                                self.last_divider_pos = None;
                            }
                        }
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
                // Raw-editor selection drag: the caret follows the cursor
                // while the anchor stays at the press. Caret-follow scrolling
                // makes dragging past the edge extend the selection.
                if let Some(id) = self.md_select_drag.clone() {
                    self.md_click_caret(&id, self.cursor_px.0, self.cursor_px.1);
                    self.md_ensure_caret_visible();
                    window.request_redraw();
                    return;
                }
                // 렌더 뷰 선택 드래그: 끝점만 따라온다(앵커는 누른 자리에 고정).
                if self.md_render_sel.as_ref().is_some_and(|s| s.dragging) {
                    let scroll = self
                        .md_render_sel
                        .as_ref()
                        .and_then(|s| {
                            let pane = s.pane.clone();
                            self.ws.lock().ok().and_then(|ws| {
                                ws.panes
                                    .get(&pane)
                                    .and_then(|p| p.markdown())
                                    .map(|m| m.scroll)
                            })
                        })
                        .unwrap_or(0.0);
                    if let Some(s) = self.md_render_sel.as_mut() {
                        s.end = (self.cursor_px.0, self.cursor_px.1 + scroll);
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
                // 사이드바 pane 줄 드래그. 떨어질 자리는 **커서가 얹힌 줄의 위/아래
                // 절반**이라, 같은 방 안 재배치와 다른 방으로 옮기기가 한 규칙으로
                // 처리된다(목록이 방 경계를 넘어 이어져 있어서다).
                if self.sidebar_row_drag.is_some() {
                    let (px, py) = self.cursor_px;
                    let start = self.sidebar_row_drag.as_ref().unwrap().start;
                    let (dx, dy) = (px - start.0, py - start.1);
                    let src = self.sidebar_row_drag.as_ref().unwrap().pane.clone();
                    let target = self
                        .sidebar_row_rects
                        .iter()
                        .find(|(_, id, r)| {
                            *id != src
                                && px >= r.0
                                && px <= r.0 + r.2
                                && py >= r.1
                                && py <= r.1 + r.3
                        })
                        .map(|(_, id, r)| (id.clone(), py < r.1 + r.3 / 2.0))
                        // 줄이 아니라 **방 카드**에 떨어뜨려도 받는다. pane 이 하나뿐인
                        // 방은 목록을 펴지 않으므로 줄만 받으면 그런 방으로는 영영 못
                        // 옮긴다 — 정작 옮길 이유가 가장 큰 쪽이 막히는 셈이다.
                        .or_else(|| {
                            let wi = self
                                .window_tab_rects
                                .iter()
                                .find(|(_, r)| {
                                    px >= r.0 && px <= r.0 + r.2 && py >= r.1 && py <= r.1 + r.3
                                })
                                .map(|(i, _)| *i)?;
                            let last = self.window_leaves(wi).into_iter().last()?;
                            (last != src).then_some((last, false))
                        });
                    if let Some(d) = self.sidebar_row_drag.as_mut() {
                        if !d.active && dx * dx + dy * dy > 9.0 {
                            d.active = true;
                        }
                        d.target = target;
                    }
                    if self.sidebar_row_drag.as_ref().map(|d| d.active).unwrap_or(false) {
                        window.set_cursor(CursorIcon::Grabbing);
                        self.chrome_dirty = true;
                        window.request_redraw();
                    }
                    return;
                }
                // 방(윈도우) 탭 재배치 드래그. 문턱을 넘으면 active 로 바꾸고,
                // 커서가 지나친 탭 개수로 삽입 자리를 다시 읽는다.
                if self.win_tab_drag.is_some() {
                    let (px, py) = self.cursor_px;
                    let start = self.win_tab_drag.as_ref().unwrap().start;
                    let (dx, dy) = (px - start.0, py - start.1);
                    // 세로 사이드바는 y, 상단 탭 모드는 x 로 읽는다.
                    let horizontal = self.tabs_on_top;
                    // 오버플로로 앞쪽 탭이 접혀 있으면 그 앞으로는 못 꽂는다 —
                    // 보이는 첫 탭을 하한으로 두어야 안 보이는 자리에 떨어지지
                    // 않는다(rects 는 보이는 탭만 담는다).
                    let mut target =
                        self.window_tab_rects.first().map(|(i, _)| *i).unwrap_or(0);
                    for (i, (rx, ry, rw, rh)) in &self.window_tab_rects {
                        let past = if horizontal {
                            px > rx + rw / 2.0
                        } else {
                            py > ry + rh / 2.0
                        };
                        if past {
                            target = i + 1;
                        }
                    }
                    let mut from = 0usize;
                    if let Some(d) = self.win_tab_drag.as_mut() {
                        if !d.active && dx * dx + dy * dy > 9.0 {
                            d.active = true;
                        }
                        d.target = target;
                        from = d.from;
                    }
                    if self.win_tab_drag.as_ref().map(|d| d.active).unwrap_or(false) {
                        window.set_cursor(CursorIcon::Grabbing);
                        // 꺼내기 자리에 들어서는 **순간** 방이 별도창으로 떨어지고,
                        // 그 창이 커서를 따라온다. 놓을 때까지 기다리면 드래그 내내
                        // 이게 나갈지 말지가 화면에 안 보인다(거노: "아직도 드래그
                        // 뗄 때 분리된다"). 판정은 놓을 때 쓰던 `room_drag_tears`
                        // 그대로라 두 순간이 갈리지 않는다.
                        let tears = self.room_drag_tears();
                        let torn = self.torn_aux_room(from).is_some();
                        self.drag_trace("방탭", tears, torn);
                        if torn || tears {
                            self.drag_tear_follow_room(from, event_loop);
                        }
                        self.chrome_dirty = true;
                        window.request_redraw();
                    }
                    return;
                }
                // Tab reorder drag: flip to active past the threshold, then
                // re-derive the drop index from the cursor's x over this
                // pane's tab pills. The insertion bar is painted from
                // `tab_drag.target`.
                if self.tab_drag.is_some() {
                    let (px, py) = self.cursor_px;
                    let (start, src_pane, src_tab) = {
                        let d = self.tab_drag.as_ref().unwrap();
                        (d.start, d.pane.clone(), d.from)
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
                        // 창 밖으로 나가는 순간 뜯어낸다 — 놓을 때까지 기다리면
                        // 드래그 내내 "빠질지 말지"가 화면에 안 보인다. 한 번
                        // 뜯긴 뒤엔 커서가 창 안으로 돌아와도 계속 따라오게 두고
                        // (되돌리기는 창 닫기 = dock), 라이브 재배치는 멈춘다 —
                        // 레이아웃에 없는 pane 을 옮기려 들면 안 되기 때문이다.
                        let (win_w, win_h) = self.logical_win_size();
                        let out = Self::drag_left_window(px, py, win_w, win_h);
                        self.drag_trace("pane탭", out, self.torn_aux_window(&src_pane).is_some());
                        let torn = out || self.torn_aux_window(&src_pane).is_some();
                        let followed = torn
                            && self.drag_tear_follow(&src_pane, Some(src_tab), event_loop);
                        if !followed {
                            // 단일탭 pane 드래그면 실제 레이아웃을 라이브로 재배치
                            // (멀티탭은 탭 추출이라 update_live_drag 가 알아서 건너뜀).
                            self.update_live_drag();
                        }
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
                    let active = hd.active;
                    let pane = hd.pane.clone();
                    if active {
                        window.set_cursor(CursorIcon::Grabbing);
                        // 탭 pill 과 같은 규칙 — 창 밖으로 나가면 놓기 전에 뜯긴다.
                        let (win_w, win_h) = self.logical_win_size();
                        let out = Self::drag_left_window(
                            self.cursor_px.0,
                            self.cursor_px.1,
                            win_w,
                            win_h,
                        );
                        self.drag_trace("pane헤더", out, self.torn_aux_window(&pane).is_some());
                        let torn = out || self.torn_aux_window(&pane).is_some();
                        let followed = torn && self.drag_tear_follow(&pane, None, event_loop);
                        if !followed {
                            // 프리뷰 박스가 아니라 실제 레이아웃을 라이브로 재배치.
                            self.update_live_drag();
                        }
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
                    // ⋮ 핸들 위 → pointer(손모양). 위 단계가 커서를 안 가져갔을 때만.
                    let icon = if matches!(icon, CursorIcon::Default)
                        && self.pane_handle_rects.iter().any(|(_, r)| {
                            cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                        })
                    {
                        CursorIcon::Pointer
                    } else {
                        icon
                    };
                    // 그 밖의 모든 누를 수 있는 표면 — 버튼·탭·메뉴 항목·목록 행.
                    // 히트렉트를 다시 훑지 않고 직전 프레임이 세운 플래그를 읽는다.
                    // 들림을 그린 자리가 곧 손가락이 뜨는 자리라, 새 버튼을 만들어도
                    // 커서를 따로 챙길 일이 없다.
                    let icon = if matches!(icon, CursorIcon::Default)
                        && self.gpu.as_ref().is_some_and(|g| g.hover_pointer)
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
            // Right-click → file-tree context menu. Over a row: single-select it
            // (unless it's already in the selection, so a right-click on one of
            // several keeps the whole batch) and open the menu at the cursor.
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                let (cx, cy) = self.cursor_px;
                // 사이드바 pane 행 → 숨기기 메뉴. 이 띠는 좌클릭을 통째로 삼키므로
                // (아래 `window_strip_click` 게이트) 우클릭도 여기서 끝낸다.
                if self.sidebar_visible && !self.tabs_on_top && cx < self.sidebar_w_logical {
                    if self.sidebar_row_right_click(cx, cy) {
                        window.request_redraw();
                    }
                    return;
                }
                if self.file_tree.visible
                    && cy > TITLE_HEIGHT
                    && cx >= self.file_tree_col_x()
                    && cx < self.file_tree_col_x() + self.file_tree.w_logical
                {
                    let inside = |r: &(f32, f32, f32, f32)| {
                        cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                    };
                    if let Some(path) = self
                        .file_tree
                        .rects
                        .iter()
                        .find(|(_, r)| inside(r))
                        .map(|(p, _)| p.clone())
                    {
                        let in_sel = self.file_tree.selected.as_deref() == Some(path.as_path())
                            || self.file_tree.selected_more.contains(&path);
                        if !in_sel {
                            self.file_tree.selected = Some(path.clone());
                            self.file_tree.selected_more.clear();
                        }
                        self.file_tree.ctx_menu = Some((cx, cy));
                        self.chrome_dirty = true;
                        window.request_redraw();
                    }
                    return;
                }
                // Info 탭 프로세스·포트 행 → 종료·복사 메뉴. 행 rect 는 Info 본문이
                // 그릴 때만 갱신되므로 탭까지 확인한다(Git 탭에선 낡은 좌표).
                if self.git.col_visible
                    && self.info.tab == state::SideTab::Info
                    && cy > TITLE_HEIGHT
                    && cx >= self.git_col_x()
                {
                    let inside = |r: &(f32, f32, f32, f32)| {
                        cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                    };
                    let target = self
                        .info
                        .proc_rects
                        .iter()
                        .find(|(_, r)| inside(r))
                        .map(|(p, _)| state::InfoTarget::Proc(*p))
                        .or_else(|| {
                            self.info
                                .port_rects
                                .iter()
                                .find(|(_, _, r)| inside(r))
                                .map(|(port, pid, _)| state::InfoTarget::Port(*port, *pid))
                        });
                    if let Some(target) = target {
                        self.info.ctx_menu = Some((cx, cy, target));
                        self.chrome_dirty = true;
                        window.request_redraw();
                    }
                    return;
                }
                // 헤더 띠 우클릭 → ⋮ 메뉴. 헤더가 생기면 hover ⋮ 핸들이 사라져
                // (중복 진입점 제거) 상단바 토글로 되돌아갈 입구가 통째로 없었다
                // (2026-08-13 지적) — 헤더 자신이 그 메뉴의 진입점이 된다.
                // 헤더 없는 pane 은 기존 hover ⋮ 가 있으니 여기서 안 잡는다.
                // `header_at_px` 를 안 쓰는 이유: 분할이 아니면 무조건 None 이라
                // 홀로 있는 pane 에 ⋮ 로 켠 헤더가 안 잡힌다 — has_header() 로
                // 실제 그려진 헤더만 가른다.
                let hdr_hit = {
                    let (cols, rows) = self.window_cells();
                    let pad = WINDOW_PADDING + self.effective_sidebar_w();
                    self.effective_leaf_rects(cols, rows)
                        .into_iter()
                        .find(|(_, rx, ry, rw, _)| {
                            let bx = pad + *rx as f32 * self.cell.w;
                            let by = TITLE_HEIGHT + *ry as f32 * self.cell.h;
                            let bw = *rw as f32 * self.cell.w;
                            cx >= bx && cx <= bx + bw
                                && cy >= by && cy <= by + PANE_HEADER_HEIGHT
                        })
                        .map(|(id, ..)| id)
                };
                if let Some(pid) = hdr_hit {
                    let headered = self
                        .ws
                        .lock()
                        .unwrap()
                        .panes
                        .get(&pid)
                        .is_some_and(|p| p.has_header());
                    if headered {
                        self.ws.lock().unwrap().active_pane = Some(pid.clone());
                        self.handle_menu = Some(pid);
                        self.chrome_dirty = true;
                        window.request_redraw();
                        return;
                    }
                }
            }
            // Middle-click → close the tab under the cursor: in-pane tab pill
            // first, then a window tab (side sidebar or top title strip). The
            // browser/터미널들 공통 관례라 kasaterm 도 따른다.
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Middle,
                ..
            } => {
                let (cx, cy) = self.cursor_px;
                let inside = |r: &(f32, f32, f32, f32)| {
                    cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                };
                let pane_tab = self
                    .pane_tab_rects
                    .iter()
                    .find(|(_, _, r)| inside(r))
                    .map(|(id, idx, _)| (id.clone(), *idx));
                if let Some((id, idx)) = pane_tab {
                    self.close_tab(&id, idx);
                    return;
                }
                let in_strip = (self.sidebar_visible && cx < self.sidebar_w_logical)
                    || (self.tabs_on_top && cy < TITLE_HEIGHT);
                if in_strip {
                    if let Some(idx) = self
                        .window_tab_rects
                        .iter()
                        .find(|(_, r)| inside(r))
                        .map(|(i, _)| *i)
                    {
                        self.confirm_or_close_session(idx);
                    }
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
                // File-tree context menu open: a press resolves a menu item or
                // dismisses the menu. Above every other hit-test so the overlay
                // wins the click.
                if matches!(state, ElementState::Pressed) && self.file_tree.ctx_menu.is_some() {
                    let (cx, cy) = self.cursor_px;
                    let action = self
                        .file_tree
                        .ctx_menu_rects
                        .iter()
                        .find(|(_, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                        .map(|(a, _)| *a);
                    self.file_tree.ctx_menu = None;
                    self.chrome_dirty = true;
                    if let Some(action) = action {
                        self.run_ft_menu_action(action);
                    }
                    window.request_redraw();
                    return;
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
                // Chrome-style restore prompt: same swallow-every-click modal as
                // confirm-close. [복원] rebuilds the saved workspace, [새로 시작]
                // discards it; the scrim ignores stray clicks. Checked early so
                // the fresh session behind the dim never leaks a press.
                if self.restore_prompt.is_some() {
                    if matches!(state, ElementState::Pressed) {
                        let (cx, cy) = self.cursor_px;
                        if let Some(btn) = self
                            .restore_btn_rects
                            .iter()
                            .find(|(_, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                            .map(|(b, _)| *b)
                        {
                            self.restore_dialog_pick(btn);
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
                    if hit(self.feedback_btn_rect) {
                        // 트레이 말풍선 — 설정 창을 Feedback 페이지로 바로 연다.
                        // 쓰다 만 본문은 App 에 남아 있어 다시 열면 그대로다.
                        self.open_settings_window(event_loop, Some(SettingsCat::Feedback), None);
                        window.request_redraw();
                        return;
                    }
                    if hit(self.settings_btn_rect) {
                        // 사이드바 "Settings" 항목 — 설정 별도창을 열거나(이미
                        // 열려 있으면) 그 창을 포커스한다.
                        self.open_settings_window(event_loop, None, None);
                        window.request_redraw();
                        return;
                    }
                    // Claude Code 스크롤 sticky prompt pill 클릭 → 그 프롬프트가
                    // 화면에 들어올 때까지 위로 스크롤. mouse-tracking TUI 라 정확한
                    // 오프셋은 모르지만, 화면에 프롬프트 행이 나타났는지는 kasaterm 이
                    // 직접 본다 — 클릭 텍스트를 target 으로 잡고 about_to_wait 가 매 틱
                    // wheel-up 한 노치씩 보내며 관찰(begin_sticky_seek → sticky_seek_step).
                    // pane_takes_mouse SGR 전달보다 먼저 잡아 클릭이 Claude Code 로
                    // 새지 않게 한다.
                    if let Some((pane_id, target)) = crate::render::STICKY_PILLS.with(|s| {
                        s.borrow()
                            .iter()
                            .find(|(_, r, _)| hit(*r))
                            .map(|(id, _, text)| (id.clone(), text.clone()))
                    }) {
                        // wheel 을 쏠 pane-local 셀 = 클릭 지점(그 pane 안이므로 안전).
                        let cell = self
                            .px_to_pane_cell(cx, cy)
                            .map(|(_, c, r)| (c, r))
                            .unwrap_or((1, 1));
                        crate::render::begin_sticky_seek(pane_id, target, cell);
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
                        // `aux:<i>` = 접어 둔 별도창. pane id 와 한 목록에 서므로
                        // 접두사로 가른다 — 그 id 로 toggle_pane_zoom 을 부르면
                        // 레이아웃에 없는 pane 으로 zoom 이 들어가 화면이 빈다.
                        if let Some(i) = id.strip_prefix("aux:").and_then(|n| n.parse().ok()) {
                            self.unhide_aux(i, event_loop);
                        } else if self.zoomed_pane.is_some() {
                            self.toggle_pane_zoom(&id);
                        }
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
                            if target == crate::win_sparkle::UPDATE_TOAST_ACTION {
                                // 업데이트 토스트: [설치] → WinSparkle 다운로드·
                                // 설치 위임, [나중에] → 그냥 접음(다음 실행 재체크).
                                if ok {
                                    crate::win_sparkle::install();
                                }
                            } else {
                                self.respond_approval(&target, ok);
                            }
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
                                // 업데이트 토스트 본문 클릭은 dismiss 만 — 센티널을
                                // active_pane 에 넣으면 존재하지 않는 pane 을 가리킨다.
                                if target != crate::win_sparkle::UPDATE_TOAST_ACTION {
                                    self.ws.lock().unwrap().active_pane = Some(target);
                                }
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
                    // 사용량 pill = Claude 계정 스위처. 드롭다운은 타이틀바 아래 pane
                    // 위로 걸치므로 pane 라우팅보다 먼저 잡아야 한다. rect 는 직전
                    // 프레임의 render 가 채운 것(⋮ 핸들 메뉴와 같은 짝).
                    let inside = |r: &(f32, f32, f32, f32)| {
                        cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                    };
                    // 손잡이가 둘이다 — Info 탭의 계정 행과, 늘 보이는 상태줄 세그먼트.
                    // 어느 쪽으로 열었는지 기억해 두고 메뉴를 그 자리에 붙인다.
                    let chip_hit = self.account_chip_rect.as_ref().is_some_and(&inside);
                    let status_hit = self.status_account_rect.as_ref().is_some_and(&inside);
                    if chip_hit {
                        self.account_menu_anchor = self.account_chip_rect;
                    } else if status_hit {
                        self.account_menu_anchor = self.status_account_rect;
                    }
                    let chip_hit = chip_hit || status_hit;
                    if self.account_menu {
                        let pick = self
                            .account_menu_hits
                            .iter()
                            .find(|(_, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                            .map(|(item, _)| item.clone());
                        self.account_menu = false;
                        self.chrome_dirty = true;
                        window.request_redraw();
                        match pick {
                            // 제공자 행은 **메뉴를 닫지 않는다** — 계정 목록을 옆으로
                            // 여는 손잡이라, 열자마자 닫히면 아무 데도 못 간다. 같은
                            // 행을 다시 누르면 접힌다.
                            Some(AccountMenuItem::Provider(p)) => {
                                self.account_menu_provider =
                                    (self.account_menu_provider != Some(p)).then_some(p);
                                self.account_menu = true;
                                return;
                            }
                            // 밀도도 그 자리에서 바뀌는 것을 봐야 하므로 메뉴를 유지한다.
                            Some(AccountMenuItem::Density(c)) => {
                                self.set_usage_compact = c;
                                self.settings_save();
                                self.account_menu = true;
                                return;
                            }
                            // claude 는 settings_save 가 끝에서 shim 을 재생성하고,
                            // codex 는 활성 슬롯 경로 파일만 갈아 끼운다 — 어느 쪽이든
                            // 이미 떠 있는 pane 도 다음 실행부터 이 계정으로 뜬다.
                            Some(AccountMenuItem::Select(p, id)) => {
                                match p {
                                    AccountProvider::Claude => self.set_claude_account = id,
                                    AccountProvider::Codex => self.set_codex_account = id,
                                }
                                self.account_menu_provider = None;
                                self.settings_save();
                                return;
                            }
                            Some(AccountMenuItem::UsageDetails)
                            | Some(AccountMenuItem::ManageAccounts) => {
                                self.account_menu_provider = None;
                                self.open_settings_window(
                                    event_loop,
                                    Some(SettingsCat::Claude),
                                    None,
                                );
                                return;
                            }
                            None => self.account_menu_provider = None,
                        }
                        // 메뉴 밖 클릭은 **닫기만 하고 소비한다.** 예전엔 pane focus 를
                        // 위해 흘려보냈는데, 메뉴가 창 하단에 뜨는 데다 pane 하단바를
                        // 여는 손잡이가 바로 그 아래라 「닫으려고 눌렀는데 하단바가
                        // 열리는」 꼴이었다(거노 2026-08-13 지적). 팝오버 밖 클릭을
                        // 삼키는 것이 데스크톱 관례고 Orca(radix Popover)도 그렇다.
                        return;
                    } else if chip_hit {
                        self.account_menu = true;
                        self.chrome_dirty = true;
                        window.request_redraw();
                        return;
                    }
                    // Sidebar-toggle button in the title strip (right of the
                    // traffic lights). Caught before the title-bar drag path
                    // so the click toggles instead of moving the window. Not
                    // painted with tabs on top, so don't eat the click either.
                    if !self.tabs_on_top {
                        let (bx, by, bw, bh) = self.sidebar_toggle_rect();
                        if cx >= bx && cx <= bx + bw && cy >= by && cy <= by + bh {
                            self.toggle_sidebar();
                            return;
                        }
                    }
                    // File-tree toggle, just right of the sidebar toggle.
                    {
                        let (bx, by, bw, bh) = self.file_tree_toggle_rect();
                        if cx >= bx && cx <= bx + bw && cy >= by && cy <= by + bh {
                            self.toggle_file_tree();
                            return;
                        }
                    }
                    // Side-panel toggle, parked at the right end of the strip.
                    // It's the only chrome button left up here — the account
                    // pill, the arona ✨ and the settings gear all moved into
                    // the panel's Info tab, which this button opens.
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
                    // cell grid. Hit order lives in `window_strip_click`.
                    // 사이드바 pane 메뉴가 떠 있으면 그게 최상단이다. **아래 게이트보다
                    // 앞이어야 한다** — 그 게이트가 왼쪽 띠의 클릭을 통째로 삼켜서,
                    // 뒤에 두면 메뉴가 영영 안 닫힌다.
                    if self.sidebar_menu_click(cx, cy) {
                        window.request_redraw();
                        return;
                    }
                    if self.sidebar_visible && cx < self.sidebar_w_logical {
                        if self.window_strip_click(cx, cy) {
                            return;
                        }
                        // Empty sidebar space — swallow the click.
                        return;
                    }
                    // Top-tabs mode: the window tabs live in the title strip,
                    // not the sidebar, so they need their own gate — without it
                    // the click fell through to the title-bar drag below and
                    // the tabs were dead (switch/close/+ all no-ops). A miss is
                    // NOT swallowed: empty strip space still drags the window.
                    if self.tabs_on_top && cy < TITLE_HEIGHT && self.window_strip_click(cx, cy) {
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
                        // "빠른 파일" 고정 섹션 행: 클릭=보조탭, Opt+클릭=별도 편집기 창.
                        if let Some(path) = self
                            .file_tree.quick_rects
                            .iter()
                            .find(|(_, r)| inside(r))
                            .map(|(p, _)| p.clone())
                        {
                            if self.modifiers.alt_key() {
                                self.popout_file_window(path, event_loop);
                            } else {
                                self.open_file(path, None, true);
                            }
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
                            if std::env::var_os("KASATERM_KEY_DEBUG").is_some() {
                                eprintln!(
                                    "[fttree] row-click super={} shift={} sel_some={} more={}",
                                    self.host_mod(),
                                    self.modifiers.shift_key(),
                                    self.file_tree.selected.is_some(),
                                    self.file_tree.selected_more.len()
                                );
                            }
                            // Cmd-click: toggle this row in/out of the selection
                            // (no expand/preview/drag — pure multi-select, VSCode).
                            if self.host_mod() {
                                if self.file_tree.selected.as_deref() == Some(path.as_path()) {
                                    // Deselecting the primary — promote one of the
                                    // extras so a selection still has an anchor.
                                    let next = self.file_tree.selected_more.iter().next().cloned();
                                    if let Some(n) = &next {
                                        self.file_tree.selected_more.remove(n);
                                    }
                                    self.file_tree.selected = next;
                                } else if !self.file_tree.selected_more.remove(&path) {
                                    // New row: it becomes the primary; the old
                                    // primary (if any) demotes into the extras.
                                    if let Some(prev) = self.file_tree.selected.replace(path.clone()) {
                                        self.file_tree.selected_more.insert(prev);
                                    }
                                }
                                self.chrome_dirty = true;
                                window.request_redraw();
                                return;
                            }
                            // Shift-click: select the contiguous run from the
                            // anchor (primary) to this row, by visible order.
                            if self.modifiers.shift_key() {
                                if let Some(anchor) = self.file_tree.selected.clone() {
                                    let ai = self.file_tree.nodes.iter().position(|n| n.path == anchor);
                                    let pi = self.file_tree.nodes.iter().position(|n| n.path == path);
                                    if let (Some(ai), Some(pi)) = (ai, pi) {
                                        let (lo, hi) = if ai <= pi { (ai, pi) } else { (pi, ai) };
                                        let run: Vec<std::path::PathBuf> = self.file_tree.nodes[lo..=hi]
                                            .iter()
                                            .map(|n| n.path.clone())
                                            .collect();
                                        self.file_tree.selected_more.clear();
                                        for p in run {
                                            if p != anchor {
                                                self.file_tree.selected_more.insert(p);
                                            }
                                        }
                                    }
                                } else {
                                    self.file_tree.selected = Some(path.clone());
                                }
                                self.chrome_dirty = true;
                                window.request_redraw();
                                return;
                            }
                            // Plain click: single-select (drops any multi-select),
                            // also the Cmd+Delete target.
                            self.file_tree.selected = Some(path.clone());
                            self.file_tree.selected_more.clear();
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
                                    // Opt+더블클릭 = 별도 편집기 창으로 바로 열기.
                                    if self.modifiers.alt_key() {
                                        self.popout_file_window(path.clone(), event_loop);
                                    } else {
                                        self.open_file_split(path.clone());
                                    }
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
                        // 프로세스·포트 우클릭 메뉴가 떠 있으면 그게 최상단이다.
                        // 밖을 눌렀으면 닫기만 하고 클릭을 삼킨다 — 메뉴를 닫는
                        // 클릭이 밑의 행까지 누르면 놀란다.
                        if let Some((_, _, target)) = self.info.ctx_menu {
                            let picked = self
                                .info
                                .ctx_menu_rects
                                .iter()
                                .find(|(_, r)| inside(r))
                                .map(|(a, _)| *a);
                            self.info.ctx_menu = None;
                            if let Some(action) = picked {
                                self.run_info_menu_action(action, target);
                            }
                            window.request_redraw();
                            return;
                        }
                        // 칼럼 탭(Git / Info / 세션). 닫기·확장보다 먼저 봐야
                        // 한다 — 셋 다 같은 머리 줄에 있고 탭이 가장 왼쪽이다.
                        if let Some((tab, _)) = self
                            .info
                            .tab_rects
                            .iter()
                            .find(|(_, r)| inside(r))
                            .map(|(t, r)| (*t, *r))
                        {
                            if self.info.tab != tab {
                                self.info.tab = tab;
                                // Info 로 막 넘어왔으면 목록이 비어 있다 — 다음
                                // 프레임의 pump_info 가 즉시 채우도록 놓아둔다.
                                self.info.scroll = 0.0;
                                // Git 탭에선 Info 본문이 안 그려져 메뉴도 안 뜨는데
                                // 열린 채로 두면 그 뒤 클릭을 계속 삼킨다.
                                self.info.ctx_menu = None;
                            }
                            window.request_redraw();
                            return;
                        }
                        // 세션 기록 행 / 범위 칩 / 새로고침. Info 와 같은 이유로
                        // 탭을 먼저 확인한다 — 다른 탭에선 낡은 좌표가 남는다.
                        if self.info.tab == state::SideTab::Mcp && self.mcp_col_click(cx, cy) {
                            self.chrome_dirty = true;
                            window.request_redraw();
                            return;
                        }
                        if self.info.tab == state::SideTab::Sessions
                            && self.sessions_col_click(cx, cy)
                        {
                            self.chrome_dirty = true;
                            window.request_redraw();
                            return;
                        }
                        // 아래 hit rect 들은 Info 본문이 그릴 때만 갱신되므로,
                        // Git 탭에서는 낡은 좌표가 남아 있다 — 탭을 확인하지
                        // 않으면 git 목록 클릭을 Info 행이 가로챈다.
                        if self.info.tab == state::SideTab::Info {
                            // 머리의 전역 진입점(아로나·설정). 계정 행은 여기가 아니라
                            // 타이틀바 시절과 같은 `account_chip_rect` 경로로 잡힌다 —
                            // 드롭다운을 여는 클릭이라 pane 라우팅보다 앞서야 한다.
                            if let Some(act) = self
                                .info
                                .action_rects
                                .iter()
                                .find(|(_, r)| inside(r))
                                .map(|(a, _)| *a)
                            {
                                match act {
                                    state::InfoAction::Arona => self.toggle_arona_panel(event_loop),
                                    state::InfoAction::Settings => {
                                        self.open_settings_window(event_loop, None, None)
                                    }
                                    state::InfoAction::Feedback => self.open_settings_window(
                                        event_loop,
                                        Some(SettingsCat::Feedback),
                                        None,
                                    ),
                                }
                                window.request_redraw();
                                return;
                            }
                            if self.info.refresh_rect.map(|r| inside(&r)).unwrap_or(false) {
                                self.info.last_refresh = None;
                                window.request_redraw();
                                return;
                            }
                            if let Some(sec) = self
                                .info
                                .sec_rects
                                .iter()
                                .find(|(_, r)| inside(r))
                                .map(|(s, _)| *s)
                            {
                                let flag = match sec {
                                    state::InfoSection::Dir => &mut self.info.dir_collapsed,
                                    state::InfoSection::Procs => &mut self.info.procs_collapsed,
                                    state::InfoSection::Closed => &mut self.info.closed_collapsed,
                                    state::InfoSection::Ports => &mut self.info.ports_collapsed,
                                };
                                *flag = !*flag;
                                window.request_redraw();
                                return;
                            }
                            if let Some(btn) = self
                                .info
                                .dir_btn_rects
                                .iter()
                                .find(|(_, r)| inside(r))
                                .map(|(b, _)| *b)
                            {
                                if let Some(path) = self.info.root.clone() {
                                    match btn {
                                        state::InfoDirBtn::Reveal => self.reveal_in_file_manager(&path),
                                        state::InfoDirBtn::Editor => {
                                            if let Some((_, target)) = crate::proc::open_with_apps().first() {
                                                crate::proc::open_path_with(target, &path);
                                            }
                                        }
                                        state::InfoDirBtn::CopyPath => {
                                            let s = path.to_string_lossy().into_owned();
                                            self.copy_to_clipboard(s, "경로 복사됨");
                                        }
                                    }
                                }
                                window.request_redraw();
                                return;
                            }
                            // 되살리기 대기 줄의 × → 목록에서 지우고 프로세스도 끈다.
                            // 줄 자체보다 먼저 본다 — × 는 그 줄 위에 얹혀 있어,
                            // 순서가 반대면 끄려던 것이 되살아난다.
                            if let Some(idx) = self
                                .info
                                .closed_kill_rects
                                .iter()
                                .find(|(_, r)| inside(r))
                                .map(|(i, _)| *i)
                            {
                                self.discard_closed_pane_at(idx);
                                window.request_redraw();
                                return;
                            }
                            // 되살리기 대기 줄 → 그 pane 만 되살린다. 그룹 머리보다
                            // 먼저 본다 — 목록 끝에 붙어 있어 서로 겹치진 않지만,
                            // 되살리기는 되돌릴 수 있는 동작이라 우선해도 안전하다.
                            if let Some(idx) = self
                                .info
                                .closed_rects
                                .iter()
                                .find(|(_, r)| inside(r))
                                .map(|(i, _)| *i)
                            {
                                self.reopen_closed_pane_at(idx);
                                window.request_redraw();
                                return;
                            }
                            // pane 그룹 머리 → 그 그룹만 접기.
                            if let Some(pane) = self
                                .info
                                .group_rects
                                .iter()
                                .find(|(_, r)| inside(r))
                                .map(|(p, _)| p.clone())
                            {
                                // 한 번 = 접기/펴기, 두 번 = 그 학생으로 포커스.
                                // 더블클릭이면 접기 토글이 두 번 걸려 제자리로
                                // 돌아오므로, 여기선 포커스만 얹으면 된다.
                                let now = Instant::now();
                                let is_double = matches!(
                                    self.info.last_group_click.as_ref(),
                                    Some((t, k))
                                        if *k == pane
                                            && now.duration_since(*t).as_millis() < 400
                                );
                                // 방은 「접어둔 것」을, 학생은 「펴 둔 것」을 기억한다
                                // — 기본값이 서로 반대(방=열림, 학생=닫힘)라 한 집합
                                // 으로는 표현할 수 없다.
                                if pane.starts_with("win:") {
                                    if !self.info.group_collapsed.remove(&pane) {
                                        self.info.group_collapsed.insert(pane.clone());
                                    }
                                } else if !self.info.pane_expanded.remove(&pane) {
                                    self.info.pane_expanded.insert(pane.clone());
                                }
                                if is_double {
                                    self.info.last_group_click = None;
                                    // 방 머리는 그 방으로, pane 머리는 그 pane 으로.
                                    match pane.strip_prefix("win:").and_then(|n| n.parse().ok()) {
                                        Some(wi) => self.switch_window(wi),
                                        None => {
                                            self.focus_pane(&pane);
                                        }
                                    }
                                } else {
                                    self.info.last_group_click = Some((now, pane));
                                }
                                self.chrome_dirty = true;
                                window.request_redraw();
                                return;
                            }
                            // 종료(×)는 행보다 먼저 — 행 안에 겹쳐 있다.
                            if let Some(pid) = self
                                .info
                                .kill_rects
                                .iter()
                                .find(|(_, r)| inside(r))
                                .map(|(p, _)| *p)
                            {
                                self.kill_process(pid, false);
                                window.request_redraw();
                                return;
                            }
                            // 포트의 종료(×)도 결국 쥔 프로세스를 죽이는 것 —
                            // 열기(행 클릭)보다 먼저 봐야 행에 삼켜지지 않는다.
                            if let Some(pid) = self
                                .info
                                .port_kill_rects
                                .iter()
                                .find(|(_, _, r)| inside(r))
                                .map(|(_, pid, _)| *pid)
                            {
                                self.kill_process(pid, false);
                                window.request_redraw();
                                return;
                            }
                            // 포트 행 → 브라우저로 localhost 열기. dev 서버를 띄운
                            // 직후 "몇 번 포트였지"를 확인하러 스크롤백을 뒤지는
                            // 일이 이 클릭 하나로 끝난다.
                            if let Some(port) = self
                                .info
                                .port_rects
                                .iter()
                                .find(|(_, _, r)| inside(r))
                                .map(|(p, _, _)| *p)
                            {
                                self.open_localhost(port);
                                return;
                            }
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
                        // Commit-detail file row (inside an expanded commit) →
                        // toggle that file's diff.
                        if let Some((hash, path)) = self
                            .git.col_commit_file_rects
                            .iter()
                            .find(|(_, _, r)| inside(r))
                            .map(|(h, p, _)| (h.clone(), p.clone()))
                        {
                            self.toggle_git_commit_file(hash, path);
                            window.request_redraw();
                            return;
                        }
                        // Recent-commit row → double-click expands its file list
                        // (single click is just the preview, no action).
                        if let Some(hash) = self
                            .git.col_commit_rects
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
                            window.request_redraw();
                            return;
                        }
                        // Empty column space — swallow the click.
                        return;
                    }
                    // ── ghostty ⋮ 핸들 메뉴 ─────────────────────────────
                    // ⋮ 클릭 → 메뉴 토글. 메뉴 열림 상태: 버튼=액션, ⋮ 자기=닫기,
                    // 밖=닫고 클릭은 흘려보냄. (드래그 이동은 Phase 4)
                    let handle_hit = self
                        .pane_handle_rects
                        .iter()
                        .find(|(_, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                        .map(|(id, _)| id.clone());
                    if let Some(menu_pid) = self.handle_menu.clone() {
                        if let Some(action) = self
                            .handle_menu_hits
                            .iter()
                            .find(|(_, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                            .map(|(a, _)| *a)
                        {
                            self.ws.lock().unwrap().active_pane = Some(menu_pid.clone());
                            match action {
                                ActionKind::SplitV => {
                                    let _ = self.split_active_pane(kasa_pty::SplitDir::Vertical);
                                }
                                ActionKind::SplitH => {
                                    let _ = self.split_active_pane(kasa_pty::SplitDir::Horizontal);
                                }
                                ActionKind::ToggleStatusbar => self.toggle_statusbar(&menu_pid),
                                ActionKind::ToggleHeader => self.toggle_pane_header(&menu_pid),
                                ActionKind::ToggleZoom => self.toggle_pane_zoom(&menu_pid),
                                ActionKind::NewTab => {
                                    let _ = self.spawn_new_tab(&menu_pid);
                                }
                                ActionKind::Close => self.close_pane(&menu_pid),
                                ActionKind::RefreshRenderer => self.refresh_renderer(),
                                ActionKind::Undock => {
                                    self.undock_pane_terminal(&menu_pid, event_loop, None)
                                }
                                // md 토글은 헤더 세그먼트 전용이라 ⋮ 메뉴엔 없다.
                                // 와일드카드로 두지 않는 이유: ⋮ 항목을 늘렸는데
                                // 여기 arm 을 빠뜨리면 클릭이 조용히 아무것도 안
                                // 하고 끝난다(ToggleHeader·ToggleZoom 이 실제로
                                // 그랬다) — 컴파일 에러로 잡히게 남김없이 적는다.
                                ActionKind::MdRender | ActionKind::MdRaw => {}
                            }
                            self.handle_menu = None;
                            self.chrome_dirty = true;
                            window.request_redraw();
                            return;
                        }
                        // ⋮ 자기 또는 메뉴 밖 클릭 → 닫기.
                        self.handle_menu = None;
                        self.chrome_dirty = true;
                        window.request_redraw();
                        if handle_hit.is_some() {
                            return; // ⋮ 자기 클릭은 토글로 소비.
                        }
                        // 밖 클릭은 닫기만 하고 계속 흘러간다(pane focus 등).
                    } else if let Some(pid) = handle_hit {
                        // ⋮ press → arm a drag instead of opening the menu
                        // immediately. A release under the threshold toggles
                        // the menu (release path); past it, the pane relocates
                        // exactly like a header-bar drag. This is how a
                        // header-less pane gets dragged at all.
                        self.ws.lock().unwrap().active_pane = Some(pid.clone());
                        self.header_drag = Some(HeaderDrag {
                            pane: pid,
                            start: self.cursor_px,
                            active: false,
                            from_handle: true,
                        });
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
                            ActionKind::Close => {
                                self.close_pane(&pid);
                            }
                            ActionKind::NewTab => {
                                let _ = self.spawn_new_tab(&pid);
                            }
                            ActionKind::ToggleHeader => {
                                self.toggle_pane_header(&pid);
                            }
                            ActionKind::ToggleZoom => {
                                self.toggle_pane_zoom(&pid);
                            }
                            ActionKind::RefreshRenderer => {
                                self.refresh_renderer();
                            }
                            ActionKind::Undock => {
                                self.undock_pane_terminal(&pid, event_loop, None);
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
                    // 바깥주소(터널) 칩 — 전역이라 pane id 가 없다. 결과는 낙관
                    // 반영하고(끄기 TERM 은 소멸이 한 박자 늦어 즉시 pgrep 하면
                    // 아직 살아 보인다) 5초 뒤 폴이 확정한다.
                    if self.statusbar.tunnel_rect.is_some_and(|r| sb_hit(&r)) {
                        let want = !self.statusbar.tunnel_on.unwrap_or(false);
                        let msg = match kasa_mcp::tunnel::set(want) {
                            Ok(on) => {
                                self.statusbar.tunnel_on = Some(on);
                                if on {
                                    match kasa_mcp::tunnel::host() {
                                        Some(h) => format!("바깥주소 열림 — https://{h}"),
                                        None => "바깥주소 열림".to_string(),
                                    }
                                } else {
                                    "바깥주소 닫힘".to_string()
                                }
                            }
                            Err(e) => e,
                        };
                        self.statusbar.tunnel_checked = Some(std::time::Instant::now());
                        self.collab.toast = Some((msg, std::time::Instant::now()));
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
                        // 줌은 공용 헬퍼(클램프+pan 재클램프)로, 회전/리셋은 view-state
                        // 직접. 버튼은 pane 중심 줌이라 anchor 없음.
                        match kind {
                            ImageBtn::ZoomIn => {
                                self.image_zoom_by(&pid, 1.25, None);
                            }
                            ImageBtn::ZoomOut => {
                                self.image_zoom_by(&pid, 1.0 / 1.25, None);
                            }
                            ImageBtn::Rotate => {
                                if let Ok(mut ws) = self.ws.lock() {
                                    if let Some(pane) = ws.panes.get_mut(&pid) {
                                        pane.image_rot = (pane.image_rot + 1) % 4;
                                        // Pan is in screen space; rotating the
                                        // texture invalidates it.
                                        pane.image_pan_x = 0.0;
                                        pane.image_pan_y = 0.0;
                                        pane.dirty = true;
                                    }
                                }
                            }
                            ImageBtn::Reset => {
                                if let Ok(mut ws) = self.ws.lock() {
                                    if let Some(pane) = ws.panes.get_mut(&pid) {
                                        pane.image_zoom = 1.0;
                                        pane.image_rot = 0;
                                        pane.image_pan_x = 0.0;
                                        pane.image_pan_y = 0.0;
                                        pane.dirty = true;
                                    }
                                }
                            }
                        }
                        if let Ok(mut ws) = self.ws.lock() {
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
                    // 계정 재시작 칩 — 옛 계정으로 도는 pane 을 새 계정으로 되띄운다.
                    // 오른쪽 버튼 무리보다 왼쪽에 있어 순서는 상관없지만, 눌렀을 때
                    // 뒤의 헤더 드래그가 같이 먹지 않게 여기서 먼저 가로챈다.
                    if let Some(pid) = self
                        .pane_restart_chip_rects
                        .iter()
                        .find(|(_, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                        .map(|(id, _)| id.clone())
                    {
                        // 표시는 **성공했을 때만** 지운다. 실패했는데 지우면 옛 계정으로
                        // 도는 pane 이 새 계정인 것처럼 보인다(Orca 의
                        // `reopenCodexRestartPrompt` 와 같은 이유).
                        if self.restart_pane_agent(&pid) {
                            self.pane_account_stale.remove(&pid);
                        } else {
                            self.set_toast("되띄우지 못했어요 — 그 pane 에 도는 에이전트가 없어요".into());
                        }
                        self.chrome_dirty = true;
                        window.request_redraw();
                        return;
                    }
                    // Pop-out icon (file tabs): tear the tab's editor into its
                    // own wgpu window. Checked before × since it sits left of it.
                    if let Some((pid, idx)) = self
                        .pane_tab_popout_rects
                        .iter()
                        .find(|(_, _, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                        .map(|(id, i, _)| (id.clone(), *i))
                    {
                        // 파일 탭 → 에디터 팝아웃 창. 터미널 탭 → 그 탭만 undock
                        // (다중탭이면 탭 승격, 아니면 pane 통째 — undock_pane_tab
                        // 이 가른다). 같은 pop-out 아이콘을 content 종류로 분기한다.
                        let is_term = self
                            .ws
                            .lock()
                            .unwrap()
                            .panes
                            .get(&pid)
                            .and_then(|p| p.tabs.get(idx))
                            .map(|t| matches!(t.content, PaneContent::Terminal(_)))
                            .unwrap_or(false);
                        if is_term {
                            self.undock_pane_tab(&pid, idx, event_loop, None);
                        } else {
                            self.popout_pane_tab(&pid, idx, event_loop, None);
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
                        // Ctrl+세로선 드래그: 상하 관통하던 세로선을 상/하 독립
                        // 경계로 영구 분리(트리를 상하먼저로 재구조화)한 뒤, 새
                        // 하단 경계만 드래그한다. 정렬 안 됨/단일 pane 이면
                        // split_htov_at 이 None → 아래 일반 드래그로 폴백.
                        if self.modifiers.control_key()
                            && dir == kasa_pty::SplitDir::Horizontal
                        {
                            if let Some(bot) = self
                                .pty_layout
                                .as_mut()
                                .and_then(|t| t.split_htov_at(&path))
                            {
                                self.resize_drag =
                                    Some((bot, kasa_pty::SplitDir::Horizontal));
                                self.publish_pty_layout();
                                let (cols, rows) = self.window_cells();
                                self.resize_backend(cols, rows);
                                window.request_redraw();
                                return;
                            }
                        }
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
                            from_handle: false,
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
                                // 찾기 바는 본문 위에 떠 있다 — 바를 눌렀는데
                                // 밑에 있는 글자로 캐럿까지 옮겨 가면 안 된다.
                                if self.md_find_click(&pane_id) {
                                    window.request_redraw();
                                    return;
                                }
                                if !self.try_copy_md_block() {
                                    if self.md_body_rects.contains_key(&pane_id) {
                                        // 거터의 접기 삼각형은 캐럿 배치보다 먼저
                                        // 본다 — 삼각형 위에서 손을 떼는 순간
                                        // 본문에 엉뚱한 선택이 생기면 안 된다.
                                        if self.md_fold_click(
                                            &pane_id,
                                            self.cursor_px.0,
                                            self.cursor_px.1,
                                        ) {
                                            window.request_redraw();
                                            return;
                                        }
                                        // 캐럿 이동 + 드래그 앵커 + 연타 선택
                                        // (더블 = 단어, 트리플 = 줄). 순서가
                                        // 계약이라 한 함수에 묶여 있고, 헤드리스
                                        // 하네스도 같은 함수를 쓴다. 평범한
                                        // 클릭은 Released 에서 앵커 == 캐럿으로
                                        // 판정돼 선택이 접힌다.
                                        self.md_press_caret(
                                            &pane_id,
                                            self.cursor_px.0,
                                            self.cursor_px.1,
                                        );
                                        // Cmd+클릭 = 정의로 뛴다. 캐럿은 먼저
                                        // 옮겨 어디를 눌렀는지 보이게 하고, 드래그
                                        // 앵커는 세우지 않는다 — 뛰고 나서 원래
                                        // 파일에 선택이 남아 있으면 어리둥절하다.
                                        if self.modifiers.super_key() {
                                            self.lsp_goto_request(&pane_id);
                                            window.request_redraw();
                                            return;
                                        }
                                        self.md_select_drag = Some(pane_id.clone());
                                    } else {
                                        // 렌더 뷰: 문서 좌표로 선택 앵커를 잡는다.
                                        // 링크 열기는 Released 로 미룬다 — 링크 위에서
                                        // 드래그를 시작할 수도 있어서, 누르는 순간
                                        // 브라우저가 뜨면 선택이 불가능해진다.
                                        let scroll = self
                                            .ws
                                            .lock()
                                            .ok()
                                            .and_then(|ws| {
                                                ws.panes
                                                    .get(&pane_id)
                                                    .and_then(|p| p.markdown())
                                                    .map(|m| m.scroll)
                                            })
                                            .unwrap_or(0.0);
                                        let at =
                                            (self.cursor_px.0, self.cursor_px.1 + scroll);
                                        self.md_render_sel = Some(crate::MdRenderSel {
                                            pane: pane_id.clone(),
                                            anchor: at,
                                            end: at,
                                            dragging: true,
                                        });
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
                                let _ = crate::proc::command("open")
                                    .arg(&url)
                                    .spawn();
                                window.request_redraw();
                                return;
                            }
                        }
                        // End a raw-editor selection drag; a plain click (no
                        // movement) leaves anchor == caret, which reads as
                        // "no selection" — drop the anchor entirely.
                        if let Some(id) = self.md_select_drag.take() {
                            if let Ok(mut ws) = self.ws.lock() {
                                if let Some(pane) = ws.panes.get_mut(&id) {
                                    let empty = pane.markdown().map_or(false, |m| {
                                        m.sel_anchor == Some((m.cur_line, m.cur_col))
                                    });
                                    if empty {
                                        if let Some(m) = pane.markdown_mut() {
                                            m.sel_anchor = None;
                                        }
                                    }
                                    pane.dirty = true;
                                }
                            }
                            window.request_redraw();
                            return;
                        }
                        // 렌더 뷰 선택 드래그 종료. 거의 안 움직였으면 그건 선택이
                        // 아니라 클릭이다 — 선택을 지우고 그때서야 링크를 연다.
                        if self.md_render_sel.as_ref().is_some_and(|s| s.dragging) {
                            let moved = self.md_render_sel.as_ref().is_some_and(|s| {
                                (s.end.0 - s.anchor.0).abs() > 3.0
                                    || (s.end.1 - s.anchor.1).abs() > 3.0
                            });
                            if moved {
                                if let Some(s) = self.md_render_sel.as_mut() {
                                    s.dragging = false;
                                }
                            } else {
                                self.md_render_sel = None;
                                self.try_open_md_link();
                            }
                            window.request_redraw();
                            return;
                        }
                        // 사이드바 pane 줄 드래그 종료. 문턱을 넘고 떨어질 줄이
                        // 있을 때만 옮긴다 — 포커스는 press 가 이미 했다.
                        if let Some(d) = self.sidebar_row_drag.take() {
                            window.set_cursor(CursorIcon::Default);
                            if let (true, Some((target, before))) = (d.active, d.target) {
                                let zone = if before {
                                    crate::DropZone::Up
                                } else {
                                    crate::DropZone::Down
                                };
                                self.move_pane(&d.pane, &target, zone);
                                self.chrome_dirty = true;
                            }
                            window.request_redraw();
                            return;
                        }
                        // 방 탭 재배치 종료. 문턱을 넘었을 때만 옮긴다 — 전환은
                        // press 가 이미 했으므로, 안 넘었으면 버릴 것뿐이다.
                        if let Some(d) = self.win_tab_drag.take() {
                            if d.active {
                                window.set_cursor(CursorIcon::Default);
                                // 끄는 도중에 이미 나갔으면 놓는 순간 할 일이 없다 —
                                // 여기서 재배치를 태우면 방금 꺼낸 방을 다시 줄 세운다.
                                // 판정(`room_drag_tears`)은 CursorMoved 와 한 벌을
                                // 쓴다: 두 벌이면 「끌 땐 안 나가는데 놓으면 나가는」
                                // 어긋남이 생기고 화면만 봐선 원인을 못 짚는다.
                                if self.torn_aux_room(d.from).is_some() {
                                    window.request_redraw();
                                    return;
                                }
                                if self.room_drag_tears() {
                                    let near = self.cursor_screen_phys();
                                    self.undock_window_room(d.from, event_loop, near);
                                } else {
                                    self.reorder_window(d.from, d.target);
                                }
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
                            // 드래그 중 이미 뜯겨 별도창이 됐다 — 놓는 순간 할 일이
                            // 없다. 아래 split/dock 경로를 그대로 태우면 레이아웃에
                            // 없는 pane 을 다시 꽂으려 든다.
                            if td.active && self.torn_aux_window(&td.pane).is_some() {
                                self.chrome_dirty = true;
                                window.request_redraw();
                                return;
                            }
                            // Phase 3 tear-off: 탭을 창 밖에서 놓으면 별도 창으로
                            // 뜯어낸다 — 파일 탭=편집기 창, 터미널 탭=undock 터미널
                            // 창(헤더 pop-out 아이콘과 동일 경로, 커서 자리에 스폰).
                            // 창 안(패널 body 포함)에 놓으면 아래 split/dock 경로가
                            // 그대로 처리 — 여기선 창 밖으로 나갔을 때만 가로챈다.
                            if td.active {
                                let (win_w, win_h) = self.logical_win_size();
                                if Self::drag_left_window(
                                    self.cursor_px.0,
                                    self.cursor_px.1,
                                    win_w,
                                    win_h,
                                ) {
                                    if self.tab_is_file(&td.pane, td.from) {
                                        // 단일탭 파일 pane 이면 라이브 백업이 남아
                                        // 있을 수 있으니 먼저 정리(원위치 복귀 상태).
                                        self.finish_live_drag();
                                        let near = self.cursor_screen_phys();
                                        self.popout_pane_tab(
                                            &td.pane,
                                            td.from,
                                            event_loop,
                                            near,
                                        );
                                        self.chrome_dirty = true;
                                        window.request_redraw();
                                        return;
                                    }
                                    let is_term = self
                                        .ws
                                        .lock()
                                        .unwrap()
                                        .panes
                                        .get(&td.pane)
                                        .and_then(|p| p.tabs.get(td.from))
                                        .map(|t| {
                                            matches!(t.content, PaneContent::Terminal(_))
                                        })
                                        .unwrap_or(false);
                                    if is_term {
                                        self.finish_live_drag();
                                        let near = self.cursor_screen_phys();
                                        self.undock_pane_tab(
                                            &td.pane, td.from, event_loop, near,
                                        );
                                        self.chrome_dirty = true;
                                        window.request_redraw();
                                        return;
                                    }
                                }
                            }
                            // 라이브로 옮기던 단일탭 pane 을 타깃 중앙에 놓았다 —
                            // split 이 아니라 그 pane 의 탭으로 들어간다. 라이브가
                            // 걸린 드래그(drag_orig_layout 이 있는 경우)는 여기서
                            // 판정해야 한다: 아래 body_drop 은 **원본** 트리로
                            // 재판정하는데, 화면엔 소스가 빠져 형제가 벌어진 모습이
                            // 떠 있어 커서 밑 과녁이 서로 다르다.
                            if self.drag_orig_layout.is_some() && self.take_center_drop(&td.pane) {
                                self.chrome_dirty = true;
                                window.request_redraw();
                                return;
                            }
                            // 단일탭 pane 을 라이브로 통째 옮긴 경우: 이미 실제
                            // 재배치가 끝났으니 백업만 정리하고 확정한다. 아래의
                            // split/move 경로를 또 타면 이중 적용된다.
                            if self.finish_live_drag() {
                                self.chrome_dirty = true;
                                window.request_redraw();
                                return;
                            }
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
                            // 이미 뜯긴 pane — 탭 드래그와 같은 이유로 여기서 끝낸다.
                            if hd.active && self.torn_aux_window(&hd.pane).is_some() {
                                self.chrome_dirty = true;
                                window.request_redraw();
                                return;
                            }
                            if hd.active {
                                // 커서가 사이드바의 다른 윈도우 탭 위면 cross-window
                                // 이동이 최우선이다. 라이브 재배치가 pane 가장자리에
                                // 매핑돼 drag_live_applied=Some 이 돼 있어도(거의 항상
                                // 그렇다) 그걸 먼저 확정하면 사이드바 드롭이 통째로
                                // 스킵된다 — 그게 "윈도우 간 pane 이동이 안 되던" 버그.
                                // 라이브로 옮겨진 자리를 원본으로 되돌린 뒤 옮긴다.
                                if let Some(target) = self
                                    .sidebar_window_drop_target(self.cursor_px.0, self.cursor_px.1)
                                {
                                    if let Some(orig) = self.drag_orig_layout.take() {
                                        self.pty_layout = Some(orig);
                                    }
                                    self.drag_live_applied = None;
                                    self.move_pane(&hd.pane, &target, DropZone::Right);
                                } else if self.take_center_drop(&hd.pane) {
                                    // 타깃 중앙(헤더 띠 또는 본문 가운데)에 놓았다 —
                                    // 화면을 더 쪼개는 게 아니라 그 pane 의 탭으로
                                    // 들어간다. 병합이 resize·publish 까지 마친다.
                                } else {
                                    // 같은 창 안 — 라이브로 이미 재배치된 현재
                                    // pty_layout 이 최종. 백업/throttle 만 정리한다.
                                    self.finish_live_drag();
                                }
                                window.request_redraw();
                            } else if hd.from_handle {
                                // ⋮ click (no drag past the threshold): toggle
                                // that pane's handle menu — the press deferred
                                // it so a drag could win instead.
                                self.handle_menu =
                                    if self.handle_menu.as_deref() == Some(hd.pane.as_str()) {
                                        None
                                    } else {
                                        Some(hd.pane.clone())
                                    };
                                self.chrome_dirty = true;
                                window.request_redraw();
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
                // Confirm-close modal: Enter = 기본 버튼(저장 안 한 게 있으면
                // 저장, 아니면 닫기), Esc = 취소. Swallow all other keys so
                // nothing reaches the PTY behind the dim.
                if self.confirm_close.is_some() {
                    if matches!(event.state, ElementState::Pressed) {
                        use winit::keyboard::{Key, NamedKey};
                        let go = match &self.confirm_close {
                            Some(d) if matches!(d.why, CloseWhy::Dirty(_)) => ConfirmBtn::Save,
                            _ => ConfirmBtn::Close,
                        };
                        match event.logical_key {
                            Key::Named(NamedKey::Enter) => {
                                self.confirm_dialog_pick(go, event_loop);
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
                // Restore prompt: Enter = 복원, Esc = 새로 시작. Swallow all
                // other keys so nothing reaches the fresh session behind the dim.
                if self.restore_prompt.is_some() {
                    if matches!(event.state, ElementState::Pressed) {
                        use winit::keyboard::{Key, NamedKey};
                        match event.logical_key {
                            Key::Named(NamedKey::Enter) => {
                                self.restore_dialog_pick(RestoreBtn::Restore);
                            }
                            Key::Named(NamedKey::Escape) => {
                                self.restore_dialog_pick(RestoreBtn::Fresh);
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
                // Cmd+W (macOS) / Ctrl+Shift+W: 활성 pane/탭 닫기. close_active_tab 이
                // tab-vs-pane 판정 + job 실행 중 확인 모달까지 처리(거노: 커맨드 W 로도 닫기).
                if matches!(event.state, ElementState::Pressed)
                    && !event.repeat
                    && self.host_mod()
                    && matches!(
                        event.physical_key,
                        winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyW)
                    )
                {
                    self.close_active_tab();
                    window.request_redraw();
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
        // 창 이동/리사이즈 1초 뒤 프레임 저장(디바운스) — exit 훅에만 맡기면
        // 크래시·강제종료 때 크기·위치가 유실된다. about_to_wait 는 블링크
        // 타이머(WaitUntil)로 주기 호출되니 별도 타이머가 필요 없다.
        if let Some(due) = self.window_frame_save_due {
            if Instant::now() >= due {
                self.window_frame_save_due = None;
                self.save_window_frame();
            }
        }
        // 창 크기와 같은 이유로 세션 스냅샷도 주기 저장한다 — exiting() 은 Cmd+Q
        // 때만 돌아, 강제 종료·크래시면 복원 창이 직전 정상 종료 시점의 낡은
        // 상태를 띄운다. 실제 이벤트가 있었을 때만(touched) 주기마다 한 번.
        if self.session_touched && self.session_saved_at.elapsed() >= crate::SESSION_AUTOSAVE_PERIOD
        {
            self.autosave_session();
        }
        // Dock badge tracks unread notifications: opening a pane clears it,
        // a background notify raises it.
        self.sync_dock_badge();
        // 웹 pane 자식 창을 pane 프레임에 맞춘다 — split/리사이즈/탭 전환/줌이
        // 어디서 일어났든 다음 턴에 여기서 따라잡는다(호스트 없으면 즉시 반환).
        self.sync_web_hosts();
        // Windows 업데이트 체커 결과 → sticky 토스트([설치][나중에] 칩).
        // 승인 토스트 배관을 센티널 action 으로 재사용(win_sparkle.rs 참고).
        // 승인 토스트가 점유 중이면 take 하지 않고 다음 틱으로 미룬다.
        if self.collab.toast_action.is_none() {
            if let Some(v) = crate::win_sparkle::take_found() {
                self.collab.toast =
                    Some((format!("↑ 새 버전 v{v}"), std::time::Instant::now()));
                self.collab.toast_action =
                    Some(crate::win_sparkle::UPDATE_TOAST_ACTION.to_string());
                self.collab.toast_rect = None;
                self.chrome_dirty = true;
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
        }
        // gif 애니: 멀티프레임 이미지 pane 의 현재 프레임이 delay 를 넘겼으면 다음 프레임으로
        // 넘기고 redraw. gif 가 있을 때만 WaitUntil(다음 전환 시각)로 타이머를 잡아 부드럽게
        // 돈다(거노: 이미지 pane gif 도 재생). 정지 이미지(frames==1)엔 영향 없음.
        {
            let now = std::time::Instant::now();
            let mut gif_advanced = false;
            let mut gif_next: Option<std::time::Instant> = None;
            {
                let ws = self.ws.lock().unwrap();
                for pane in ws.panes.values() {
                    if let Some(img) = pane.image() {
                        if img.frames.len() < 2 {
                            continue;
                        }
                        if img.tick(now) {
                            gif_advanced = true;
                        }
                        if let Some(dl) = img.next_deadline() {
                            gif_next = Some(gif_next.map_or(dl, |n| n.min(dl)));
                        }
                    }
                }
            }
            if gif_advanced {
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            if let Some(dl) = gif_next {
                event_loop.set_control_flow(ControlFlow::WaitUntil(dl));
            }
        }
        // 마크다운 노치 스크롤 관성 — gif 와 같은 방식(상태 tick → dirty → 아래
        // 펌프 조건이 redraw). 애니가 없으면 첫 줄에서 바로 돌아온다.
        self.tick_md_scroll();
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
            } else if self.paste_menu_item.as_ref().map(|m| m.id()) == Some(&ev.id) {
                // Cmd+V: key 창 first responder(아로나 webview)에 paste: 위임 →
                // 안 먹으면(터미널 창) 직접 클립보드 붙여넣기.
                #[cfg(target_os = "macos")]
                let to_webview = crate::macos_open::send_paste_action();
                #[cfg(not(target_os = "macos"))]
                let to_webview = false;
                if !to_webview {
                    self.input_buf.clear();
                    self.current_suggestion = None;
                    self.paste_clipboard();
                }
            } else if self.copy_menu_item.as_ref().map(|m| m.id()) == Some(&ev.id) {
                // Cmd+C: webview 우선 copy: 위임 → 안 먹으면 터미널 선택영역 복사.
                #[cfg(target_os = "macos")]
                let to_webview = crate::macos_open::send_copy_action();
                #[cfg(not(target_os = "macos"))]
                let to_webview = false;
                if !to_webview && self.selection.is_some() {
                    self.copy_selection();
                }
            } else if self.update_menu_item.as_ref().map(|m| m.id()) == Some(&ev.id) {
                // "업데이트 확인" → Sparkle 표준 다이얼로그(.app 빌드에서만 active).
                #[cfg(target_os = "macos")]
                if let Some(c) = self.sparkle_updater.as_ref() {
                    crate::macos_sparkle::check_for_updates(c);
                }
            } else if self.quit_menu_item.as_ref().map(|m| m.id()) == Some(&ev.id) {
                // ⌘Q → 종료 확인(ghostty 식). 확인 시 event_loop.exit() 로 정상 종료
                // (exiting() 콜백이 window.json·세션 저장). 취소면 무시.
                #[cfg(target_os = "macos")]
                let ok = crate::macos_open::confirm_quit();
                #[cfg(not(target_os = "macos"))]
                let ok = true;
                if ok {
                    // exiting() 이 저장하는 건 세션 스냅샷이지 편집기 버퍼가
                    // 아니다 — 저장 안 한 문서가 있으면 여기서 먼저 묻는다.
                    if self.guard_dirty(&PendingClose::Window) {
                        return;
                    }
                    event_loop.exit();
                    return;
                }
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
        // capture_next 는 프레임당 하나만 받으므로 due 인 것 중 첫 건만 —
        // 나머지는 다음 tick 에서 이어서 처리된다.
        if let Some(pos) = self
            .pending_capture
            .iter()
            .position(|(at, _)| std::time::Instant::now() >= *at)
        {
            let (_, path) = self.pending_capture.remove(pos);
            if let Some(g) = self.gpu.as_mut() {
                g.capture_next = Some(path);
            }
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
        // Headless verification: auto-resolve the launch restore prompt so a
        // capture can exercise the full 복원 rebuild (or 새로 시작 discard)
        // without a click. KASATERM_AUTORESTORE=restore|fresh. No-op unless a
        // prompt is actually up, so it never fires on a normal (no saved state)
        // launch.
        if self.restore_prompt.is_some() {
            match std::env::var("KASATERM_AUTORESTORE").as_deref() {
                Ok("restore") => {
                    self.restore_dialog_pick(RestoreBtn::Restore);
                    if let Some(w) = self.window.as_ref() {
                        w.request_redraw();
                    }
                }
                Ok("fresh") => {
                    self.restore_dialog_pick(RestoreBtn::Fresh);
                    if let Some(w) = self.window.as_ref() {
                        w.request_redraw();
                    }
                }
                _ => {}
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
            if self.pending_restores.iter().any(|(_, _, at)| now >= *at) {
                // The first shell frame has populated pane chrome by now, so
                // size once more before starting a full-screen program. This
                // prevents Claude from laying out against the pre-footer grid.
                let (cols, rows) = self.window_cells();
                self.resize_backend(cols, rows);
            }
            self.pending_restores.retain(|(sess, cmd, at)| {
                if now >= *at {
                    let _ = sess.send_bytes(cmd.as_bytes());
                    false
                } else {
                    true
                }
            });
        }
        // 지글 원복 — NudgePaneResize 가 1행 줄인 pane 을 원 크기로 되돌린다.
        if !self.pending_unjiggle.is_empty() {
            let now = std::time::Instant::now();
            let due: Vec<(String, u16, u16)> = self
                .pending_unjiggle
                .iter()
                .filter(|(_, _, _, at)| now >= *at)
                .map(|(p, c, r, _)| (p.clone(), *c, *r))
                .collect();
            self.pending_unjiggle.retain(|(_, _, _, at)| now < *at);
            for (pane, cols, rows) in due {
                if let Some(sess) = self.pty.get(&pane) {
                    let _ = sess.resize(cols, rows);
                }
            }
        }
        // Reap dead pty sessions before anything else — a closed shell
        // should disappear from the layout on the very next loop turn
        // so the user sees the gap collapse immediately.
        self.reap_dead_panes(event_loop);
        // Refresh per-pane busy state (Claude's working spinner → header bar +
        // completion toast). Self-throttled, so this is cheap per loop turn.
        self.refresh_pane_activity();
        self.note_claude_panes();
        self.apply_autowait();
        self.apply_autounread();
        // rust 파일이 편집기로 열려 있으면 rust-analyzer 를 붙인다. 여기서 하는
        // 이유는 편집기가 열리는 경로가 여러 개라서다(사이드바·소켓·복원·팝아웃)
        // — 한 자리에 두면 어느 경로로 열려도 같은 순간에 붙는다. 이미 알린
        // 파일이면 `lsp_attach` 가 경로만 보고 즉시 나간다.
        if let Some(id) = self.ws.lock().ok().and_then(|w| w.active_pane.clone()) {
            self.lsp_attach(&id);
            // 자동완성 응답은 왕복이라 키 경로에서 못 기다린다 — 도착한 것을
            // 여기서 받아 팝업에 얹는다.
            self.lsp_complete_pump(&id);
            // 편집으로 어긋난 접힘을 걷어낸다. 렌더는 검증된 목록을 보게 된다.
            if let Ok(mut ws) = self.ws.lock() {
                if let Some(m) = ws.panes.get_mut(&id).and_then(|p| p.markdown_mut()) {
                    m.folds_valid();
                }
            }
        }
        // 정의 이동 응답도 왕복이라 여기서 받는다 — 온 순간 그 파일을 연다.
        self.lsp_goto_pump();
        // 마우스가 멎었으면 그 자리를 묻고, 답이 왔으면 툴팁에 담는다.
        self.lsp_hover_tick();
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
        self.run_pending_force_drag();
        self.run_pending_auto_pane_merge();
        self.run_pending_autowheel();
        self.run_pending_sticky_seek();
        self.run_pending_autotoggle();
        self.run_pending_autoarona(event_loop);
        self.run_pending_autotabs();
        self.run_pending_autoopen();
        self.run_pending_autoconfirm();
        self.run_pending_autowinclose();
        self.run_pending_autolastclose();
        self.run_pending_autowinreorder();
        self.run_pending_autoroomrename();
        self.run_pending_autoftrename();
        self.run_pending_autopathsearch();
        self.run_pending_autowinundock(event_loop);
        self.run_pending_autoclosereopen();
        self.run_pending_autostash();
        self.run_pending_autoview();
        self.run_pending_autoinfo();
        // 커서 배치보다 **앞**이다 — 스크롤이 정해진 뒤라야 AUTOCURSOR 가 놓은
        // 자리가 「잘려 안 보이는 행」위인지가 의미를 갖는다.
        self.run_pending_autocolscroll();
        self.run_pending_autocursor();
        self.run_pending_autoexpandclick();
        self.run_pending_autorowdrag();
        self.run_pending_autotheme();
        // OS 의 밝게/어둡게가 바뀌었나 — `theme: system` 일 때만 실제로 조회한다
        // (게이트는 poll 안에 있다). 바뀐 순간에만 참이라 평소엔 아무 일도 없다.
        if theme::poll_system_theme() {
            self.begin_theme_fx();
            self.repaint_all();
        }
        // 라이트↔다크 플립 → 떠 있는 claude 재테마(리포트 + /theme 피커 주입).
        // poll_system_theme 바로 뒤: OS 전환으로 방금 바뀐 팔레트도 같은 틱에 잡힌다.
        self.poll_claude_retheme();
        self.run_pending_autozoomprobe();
        self.run_pending_autoheader();
        self.resolve_force_handle_menu();
        self.run_pending_automovescreen();
        self.run_pending_forcesurfacehalf();
        self.run_pending_layergeom();
        self.run_pending_automenuclick(event_loop);
        self.run_pending_autohdrmenu(event_loop);
        self.run_pending_autopillclick(event_loop);
        self.run_pending_autoinfodbl(event_loop);
        self.run_pending_autosettings(event_loop);
        self.run_pending_autoshellmenu();
        self.run_pending_autoftmenu();
        self.run_pending_automdselect();
        self.run_pending_automdscript(event_loop);
        self.run_pending_auxpopout(event_loop);
        self.run_pending_autoundock(event_loop);
        self.run_pending_autoauxmd(event_loop);
        self.run_pending_autoundock_scroll();
        self.run_pending_autoundock_dock();
        self.run_pending_autoauxtree();
        self.run_pending_autoteardrag(event_loop);
        self.run_pending_autotearroom(event_loop);
        self.run_pending_autostudent(event_loop);
        self.run_pending_autoboxlabel();
        self.run_pending_autoroomsplit();
        self.run_pending_autoforeignsplit();
        self.run_pending_autofacehover(event_loop);
        self.run_pending_autotreeclick(event_loop);
        self.drain_aux_captures();
        // 편집기 자동 저장 — 타자가 멎은 지 설정 시간이 지난 버퍼를 쓴다.
        // 반환된 다음 만기는 아래 control flow 에 넣는다. 실측하면 이게 없어도
        // 저장은 되는데, 커서 블링크 스레드가 530ms 마다 무조건 루프를 깨워
        // about_to_wait 이 계속 돌기 때문이다 — 즉 지금은 무관한 장식용 타이머에
        // 얹혀 가는 셈이다. 블링크가 언젠가 조건부가 되면(창이 비활성일 때 안
        // 깜빡이는 건 충분히 있을 법하다) 조용한 편집기가 소리 없이 안 써진다.
        // 대기 중인 게 없으면 None 이라 유휴 비용은 0.
        let autosave_due = self.run_editor_autosave();
        // 유휴인데 프레임이 계속 나가는 원인을 잡는 계측(`KASATERM_PUMP_DEBUG=1`).
        // 아래 펌프 조건이 17개라 어느 것이 참인지 눈으로 가릴 수가 없다. 값이
        // **바뀔 때만** 한 줄 찍는다 — 매 프레임 찍으면 그 출력이 프레임을 먹는다.
        // 아래 `if` 를 그대로 두고 따로 재평가하는 이유: 조건을 Vec 수집으로
        // 합치면 `||` 단락이 사라져 모든 조건이 매번 평가된다.
        if std::env::var_os("KASATERM_PUMP_DEBUG").is_some() {
            let mut why: Vec<&'static str> = Vec::new();
            if self.version_alpha() > 0.0 {
                why.push("version");
            }
            if self.copy_toast_alpha() > 0.0 {
                why.push("copy_toast");
            }
            if self.collab_toast_alpha() > 0.0 && self.collab.toast_action.is_none() {
                why.push("collab_toast");
            }
            if self.any_notify_flash() {
                why.push("notify_flash");
            }
            // 아래 실제 조건과 같게 **보이는** pane 만 센다 — 좁힌 쪽과 어긋나면
            // 로그가 「pane_working 이라 펌프한다」고 하는데 실제론 안 걸린다.
            if {
                let visible = self.visible_pane_ids();
                self.pane_activity
                    .iter()
                    .any(|(id, a)| {
                        matches!(a.status.as_str(), "working" | "compacting")
                            && visible.contains(id)
                    })
            } {
                why.push("pane_working");
            }
            if !self.pending_capture.is_empty() {
                why.push("pending_capture");
            }
            if self.aux_windows.iter().any(|a| a.pending_capture.is_some()) {
                why.push("aux_capture");
            }
            if self.pending_autogit.is_some() {
                why.push("autogit");
            }
            if self.autoquit_at.is_some() {
                why.push("autoquit");
            }
            if crate::testkit::mdscript_pending() {
                why.push("mdscript");
            }
            if crate::render::sticky_seek_active() {
                why.push("sticky_seek");
            }
            if !self.window_alert.is_empty() {
                why.push("window_alert");
            }
            if self
                .pane_activity
                .values()
                .any(|a| crate::chrome::status_needs_you(&a.status))
            {
                why.push("needs_you");
            }
            if !self.md_scroll_anim.is_empty() {
                why.push("md_scroll");
            }
            if self.theme_fx.is_some() {
                why.push("theme_fx");
            }
            if self.expand_anim.is_some() {
                why.push("expand_anim");
            }
            if self.sidebar_visible && !self.tabs_on_top && !self.expanded_windows.is_empty() {
                why.push("sidebar_gif");
            }
            let line = why.join(",");
            thread_local! {
                static PUMP_PREV: std::cell::RefCell<String> =
                    std::cell::RefCell::new(String::from("\0"));
            }
            PUMP_PREV.with(|p| {
                let mut prev = p.borrow_mut();
                if *prev != line {
                    eprintln!(
                        "[pump] {}",
                        if line.is_empty() {
                            "(none → Wait, 여기서 프레임이 계속 나가면 원인은 UserEvent 쪽)"
                        } else {
                            line.as_str()
                        }
                    );
                    *prev = line;
                }
            });
        }
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
            //
            // **보이는** pane 만 센다. 그 바는 pane 헤더에 있어 다른 방의 pane 은 화면에
            // 없는데, 좁히기 전에는 방마다 claude 를 띄운 것만으로 이 조건이 늘 참이 되어
            // 앱이 유휴에도 30fps 를 갈았다(2026-08-13 실측). working→idle 전환을 놓치는
            // 것도 아니다 — `refresh_pane_activity` 는 300ms 주기로 따로 돌고, 방을
            // 전환하면 `chrome_dirty` 가 서서 그 방의 바가 곧바로 다시 움직인다.
            || {
                let visible = self.visible_pane_ids();
                self.pane_activity
                    .iter()
                    .any(|(id, a)| {
                        matches!(a.status.as_str(), "working" | "compacting")
                            && visible.contains(id)
                    })
            }
            || !self.pending_capture.is_empty()
            // 별도창 캡처가 대기 중이면 그 deadline 까지 깨어 있어야 arming 이 발화한다.
            || self.aux_windows.iter().any(|a| a.pending_capture.is_some())
            || self.pending_autogit.is_some()
            || self.autoquit_at.is_some()
            // 남은 md 스크립트 단계는 about_to_wait 이 다시 돌아야 발화한다.
            || crate::testkit::mdscript_pending()
            // sticky 클릭 seek 이 도는 동안엔 스크롤이 목표 프롬프트에 닿을 때까지
            // 프레임을 펌프해야 노치가 계속 나가고 화면 관찰이 이어진다.
            || crate::render::sticky_seek_active()
            // 못 본 알림이 있는 방 탭은 숨쉰다 — 그 호흡이 이어지려면 프레임이
            // 계속 나가야 한다(커서 블링크에 얹혀 있던 시절엔 공짜였다).
            || !self.window_alert.is_empty()
            // 손을 기다리는 pane 의 핑크 깜빡임(사이드바 줄 + pane 테두리)도 같은 이유로.
            || self
                .pane_activity
                .values()
                .any(|a| crate::chrome::status_needs_you(&a.status))
            // 노치 스크롤 관성이 목표에 붙을 때까지 프레임을 펌프한다.
            || !self.md_scroll_anim.is_empty()
            // 테마 전환 디졸브가 걷히는 동안.
            || self.theme_fx.is_some()
            // 사이드바 방이 펴지거나 접히는 동안.
            || self.expand_anim.is_some()
            // 펼친 목록의 학생 얼굴은 idle gif 라 프레임을 계속 넘겨야 한다.
            // 접으면 멈춘다 — 안 보이는 그림에 프레임을 태우지 않는다.
            || (self.sidebar_visible && !self.tabs_on_top && !self.expanded_windows.is_empty())
        {
            // 관성은 33ms(≈30fps)로 굴리면 그 자체가 계단으로 보인다 — 도는 동안만
            // 8ms 로 촘촘히. 테마 디졸브도 같은 이유로 촘촘한 쪽에 붙인다 —
            // 0.4초짜리라 30fps 면 열두 장뿐이고, 그러면 블록이 퍼지는 게 아니라
            // 뚝뚝 끊겨 보인다. 다른 펌프 사유(블링크·펄스)엔 33ms 로 충분하다.
            let period = if self.md_scroll_anim.is_empty()
                && self.theme_fx.is_none()
                && self.expand_anim.is_none()
            {
                33
            } else {
                8
            };
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + std::time::Duration::from_millis(period),
            ));
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        } else {
            // 지연 flush 두 종(세션 스냅샷·편집기 자동 저장)은 "조용해진 직후"가
            // 가장 중요하다. dirty 인 동안만 이른 쪽 만기를 걸어 그때 한 번
            // flush 하고, 끝나면 다시 완전한 Wait 로 돌아간다(idle 상시 폴링
            // 아님). 이 만기에만 기대야 하는 이유는 위 `autosave_due` 주석 참고.
            let deadline = self
                .session_touched
                .then(|| self.session_saved_at + crate::SESSION_AUTOSAVE_PERIOD)
                .into_iter()
                .chain(autosave_due)
                .min();
            event_loop.set_control_flow(match deadline {
                Some(at) => ControlFlow::WaitUntil(at),
                None => ControlFlow::Wait,
            });
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
    ///
    /// The unsaved-changes flavour resolves the *files* here and then re-enters
    /// the same close path instead of closing directly. Those editors are clean
    /// now, so the guard can't fire twice, and a close that also had a job
    /// running still gets its "실행 중이에요" question — which it wouldn't if we
    /// jumped straight to `do_close`.
    pub(crate) fn confirm_dialog_pick(&mut self, btn: ConfirmBtn, event_loop: &ActiveEventLoop) {
        let Some(dlg) = self.confirm_close.take() else { return };
        self.chrome_dirty = true;
        if btn == ConfirmBtn::Cancel {
            return;
        }
        if let CloseWhy::Dirty(docs) = &dlg.why {
            if btn == ConfirmBtn::Save {
                // 쓰기가 실패했으면 닫지 않는다 — 여기서 밀고 나가면 저장하려던
                // 편집분을 그대로 버리는 셈이다(토스트가 이유를 띄운다).
                if !self.save_dirty_docs(docs) {
                    return;
                }
            } else {
                self.discard_dirty_docs(docs);
            }
            match dlg.action {
                PendingClose::Window => {
                    if !self.confirm_or_close_window() {
                        event_loop.exit();
                    }
                }
                PendingClose::Tab { pane, idx } => self.confirm_or_close_tab(&pane, idx),
                // 단일 탭 pane 은 confirm_or_close_tab 이 Pane 으로 승격시킨다.
                PendingClose::Pane { pane } => self.confirm_or_close_tab(&pane, 0),
                PendingClose::Session(i) => self.confirm_or_close_session(i),
                other @ PendingClose::AuxEditor(_) => self.do_close(other),
            }
            return;
        }
        match dlg.action {
            PendingClose::Window => event_loop.exit(),
            other => self.do_close(other),
        }
    }
    /// Resolve the Chrome-style restore prompt: 복원 rebuilds the saved
    /// workspace, 새로 시작 discards the saved state and keeps the fresh session
    /// start_pty already spawned. Either way the prompt clears.
    pub(crate) fn restore_dialog_pick(&mut self, btn: RestoreBtn) {
        let Some(state) = self.restore_prompt.take() else {
            return;
        };
        self.chrome_dirty = true;
        match btn {
            RestoreBtn::Restore => self.restore_session_state(&state),
            RestoreBtn::Fresh => crate::socket::clear_session_state(),
        }
    }
}

/// Build a git-column snapshot from `git_status`, split into Staged Changes /
/// Changes (VSCode model; no dedup, so a partially-staged file shows in both).
/// Returns `None` on a transient git failure so the caller keeps the last good
/// snapshot. Shared by the 1.2s poller and the per-click stage/unstage refresh
/// so a + / − press reflects immediately instead of waiting for the next tick.
/// 로컬 `/claude-usage`(oauth/usage 프록시) 응답 — `usage` 본문에 `stale` 과
/// `account_dir` 을 함께 돌려준다. curl 로 로컬 엔드포인트만 쳐 토큰은 서버(키체인)가
/// 읽는다 — argv 유출 없음. 실패/토큰 없음/형식밖이면 None.
///
/// `stale`·`account_dir` 이 필요한 이유: 화면이 "지금 값인지"와 "어느 계정 값인지"를
/// 말해야 한다. 전에는 `usage` 만 떠서, upstream 이 막힌 옛 숫자와 방금 조회한 숫자가
/// 화면에서 똑같이 보였고 계정을 바꿔도 표시가 안 바뀌었다(거노 2026-08-05).
///
/// `dir` 은 조회할 계정 저장소 — `None` 이면 활성 계정. 프록시가 슬롯별 토큰을 직접
/// 읽으므로 **전환하지 않고** 남의 계정 한도를 볼 수 있다(계정 드롭다운이 그걸 쓴다).
///
/// 쿼리 값 퍼센트 인코딩. 슬롯 경로는 홈 아래 절대경로라 공백·한글·`&` 가 들어올 수
/// 있고, 그대로 붙이면 거기서 쿼리가 잘려 **엉뚱한 슬롯**(대개 기본 계정)을 조회한다.
/// `/` 는 쿼리 값에서 합법이라 남겨 로그를 읽을 수 있게 둔다.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `dir` 은 조회할 **계정 저장소 경로** — 빈 문자열이 기본 로그인이다.
///
/// 호출자가 반드시 명시한다. 안 넘기는 길을 두면 프록시가 자기 프로세스의
/// `KASATERM_CLAUDE_ACCOUNT_DIR` env 로 떨어져 엉뚱한(또는 **다른 인스턴스의**)
/// 계정을 조회하는데, 그게 자동전환을 통째로 멈춰 세운 버그였다(2026-08-13).
/// `Option` 을 없애 그 실수를 타입으로 막는다.
fn fetch_claude_usage(port: &str, dir: &str) -> Option<(serde_json::Value, bool, String)> {
    // 슬롯 경로엔 공백·한글이 들어올 수 있어 쿼리로 넘기기 전에 퍼센트 인코딩한다.
    let url = format!("http://127.0.0.1:{port}/claude-usage?dir={}", urlencode(dir));
    let out = crate::proc::command("curl")
        .args(["-s", "--max-time", "5", &url])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    if v.get("ok").and_then(|b| b.as_bool()) != Some(true) {
        return None;
    }
    let stale = v.get("stale").and_then(|b| b.as_bool()).unwrap_or(false);
    let dir = v.get("account_dir").and_then(|s| s.as_str()).unwrap_or_default().to_string();
    Some((v.get("usage")?.clone(), stale, dir))
}

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
