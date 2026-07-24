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
                // GUI(SCHALE OS)에서 다른 방(=다른 윈도우)의 학생을 포커스하면 그
                // 윈도우를 앞으로 가져와야 한다. active_pane 만 바꾸면 보이지 않는
                // 윈도우의 pane 이 선택돼 화면은 그대로라 "방전환→윈도우전환"이 안 됐다.
                // switch_window 가 leaves[0] 로 active_pane 을 덮으니 그 뒤에 원하는
                // pane 으로 다시 지정한다.
                // board id 가 실제 leaf 인 윈도우가 있을 때만 포커스한다 — 캐릭터/작업명/async
                // 같은 비-leaf 집계 id 로 active_pane 을 덮으면 다음 /layout 폴에서 그 타일이
                // 빠져 "pane 이 닫힌 것처럼" 보였다(거노: 캐릭터 클릭→학생 선택하면 닫힘).
                if let Some(wi) = self.window_of_pane(id) {
                    if wi != self.active_window {
                        self.switch_window(wi);
                    }
                    self.ws.lock().unwrap().active_pane = Some(id.clone());
                    self.chrome_dirty = true;
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
            UserEvent::SocketSpawnStudent(character) => {
                self.spawn_student(character);
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
                if self.pane_claude_sid.get(pane.as_str()) == Some(&sid) {
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
            UserEvent::ResumeSession { id, cwd, newroom, attach } => {
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
                            format!("claude attach {id}\r")
                        } else {
                            match cwd {
                                Some(c) if !c.is_empty() => {
                                    let q = c.replace('\'', "'\\''");
                                    format!("cd '{q}' && claude --resume {id}\r")
                                }
                                _ => format!("claude --resume {id}\r"),
                            }
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
            UserEvent::BgAgentsChanged => {
                // agents/attach 뷰 pane 재바인딩을 board 폴링에만 맡기지 않는다 — 웹뷰/
                // CLI 가 board 를 안 부르는 세션에선 rebind 가 영영 안 돌아 pane 이 스폰
                // 로컬 랜덤에 머물렀다(거노: 이번엔 유우카로 떠). 3s 폴러에 편승해 항상
                // 돈다. bind_transcript 는 proxy 이벤트(SocketSessionBound)라 다음 루프에
                // apply_session_character 로 이어진다.
                if let Some(be) = self.socket_backend.clone() {
                    let live: std::collections::HashSet<String> =
                        self.ws.lock().unwrap().panes.keys().cloned().collect();
                    be.rebind_agents_panes(&live);
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
        // Restore the saved window position (physical px). Only when the saved
        // point still lands on a live monitor — the monitor may have been
        // unplugged since, and an off-screen restore would strand the window.
        if let Some((px, py)) = crate::socket::read_window_pos() {
            let on_screen = window.available_monitors().any(|m| {
                let mp = m.position();
                let ms = m.size();
                px >= mp.x as f64
                    && px < (mp.x as f64 + ms.width as f64 - 60.0)
                    && py >= mp.y as f64
                    && py < (mp.y as f64 + ms.height as f64 - 60.0)
            });
            if on_screen {
                window.set_outer_position(winit::dpi::PhysicalPosition::new(px, py));
            }
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
                if crate::render::STUDENT_SPRITE_ANIMATING
                    .load(std::sync::atomic::Ordering::Relaxed)
                    && anim_proxy.send_event(UserEvent::Redraw).is_err()
                {
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
        // claude 5시간 사용량 폴러 — 로컬 /claude-usage(oauth/usage 프록시)를 60초마다
        // 조회해 5시간 창 사용률(%)을 채운다. 타이틀바 우상단 pill 의 소스(거노: 웹뷰
        // TitleBar UsagePill 을 웹뷰 안 봐서 터미널에도). curl 로 로컬 엔드포인트만 쳐
        // 토큰은 서버(키체인)가 읽는다 — argv 유출 없음. 값이 바뀔 때만 redraw.
        {
            let usage_proxy = self.proxy.clone();
            let usage_cache = self.claude_usage.clone();
            std::thread::spawn(move || loop {
                let next = fetch_claude_five_hour(&crate::mcp_panel_port());
                // 일시적 fetch 실패(None)면 마지막 유효값을 유지 — pill 깜빡임/사라짐 방지.
                // (git col 폴러와 동일 정책. 5시간 창은 항상 존재해 참 None 은 사실상 없음.)
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
                std::thread::sleep(std::time::Duration::from_secs(60));
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
                    let out = std::process::Command::new("ps")
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
                    if let Ok(out) = std::process::Command::new(&bin)
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
        #[cfg(unix)]
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
        if !want_tmux {
            if let Some(state) = crate::socket::read_session_state() {
                if App::count_claude_panes(&state) > 0 {
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
        self.arm_autotoggle();
        self.arm_autoarona();
        // 온보딩 제거(거노) — 강제 ModePicker 자동오픈 안 함. 터미널이 기본,
        // SCHALE OS 는 타이틀바 ✨ 버튼/단축키(Cmd+Shift+A)로 켠다(progressive disclosure).
        self.arm_autotabs();
        self.arm_autodrag();
        self.arm_autopanemove();
        self.arm_force_drag();
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
        // 별도 wgpu 편집기/파일뷰 창(auxwin.rs). 자체 GpuRenderer 를 가지므로 메인
        // 창의 surface·터미널 로직과 완전히 격리 — 이벤트를 kind 별 라우팅에 위임한다.
        if let Some(pos) = self.aux_windows.iter().position(|a| a.window.id() == id) {
            self.aux_window_event(pos, event, event_loop);
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
            // 트랙패드 핀치(macOS magnification) → 커서 아래 이미지 pane 줌.
            WindowEvent::PinchGesture { delta, .. } => {
                self.handle_pinch(delta);
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
                // 학생 프사 hover — 큰 bust 확대 팝업을 뜨고 지우려면 진입/이탈에
                // 재페인트(이벤트 기반 루프라 커서 이동만으론 프레임이 안 돈다).
                {
                    let (cx, cy) = self.cursor_px;
                    let new_face = self.face_hit_rects.iter().any(|(_, r)| {
                        cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                    });
                    if new_face != self.face_hover {
                        self.face_hover = new_face;
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
                        // 단일탭 pane 드래그면 실제 레이아웃을 라이브로 재배치
                        // (멀티탭은 탭 추출이라 update_live_drag 가 알아서 건너뜀).
                        self.update_live_drag();
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
                    if active {
                        window.set_cursor(CursorIcon::Grabbing);
                        // 프리뷰 박스가 아니라 실제 레이아웃을 라이브로 재배치.
                        self.update_live_drag();
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
                    // 학생 프사 위 → pointer(클릭하면 학생 설정이 열린다는 암시).
                    let icon = if matches!(icon, CursorIcon::Default)
                        && self.face_hit_rects.iter().any(|(_, r)| {
                            cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                        })
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
                    if hit(self.settings_btn_rect) {
                        // 사이드바 "Settings" 항목 — 설정 별도창을 열거나(이미
                        // 열려 있으면) 그 창을 포커스한다.
                        self.open_settings_window(event_loop, None, None);
                        window.request_redraw();
                        return;
                    }
                    // 학생 프사(statusline) 클릭 → 학생 설정 별도창을 Students
                    // 카테고리 + 그 학생 선택 상태로 연다(딥링크). pane 포커스
                    // 클릭보다 먼저 잡아 프사를 눌러도 pane 이 안 튀게.
                    if let Some((name, _)) =
                        self.face_hit_rects.iter().find(|(_, r)| hit(*r)).cloned()
                    {
                        self.open_settings_window(
                            event_loop,
                            Some(SettingsCat::Students),
                            Some(name),
                        );
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
                        if self.zoomed_pane.is_some() {
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
                    // Sidebar-toggle button in the title strip (right of the
                    // traffic lights). Caught before the title-bar drag path
                    // so the click toggles instead of moving the window. Not
                    // painted with tabs on top, so don't eat the click either.
                    if !self.tabs_on_top {
                        let (bx, by, bw, bh) = Self::sidebar_toggle_rect();
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
                    // SCHALE OS(아로나) ✨ 버튼 — 터미널↔SCHALE OS 토글(메뉴 대신).
                    if let Some((bx, by, bw, bh)) = self.arona_btn_rect() {
                        if cx >= bx && cx <= bx + bw && cy >= by && cy <= by + bh {
                            self.toggle_arona_panel(event_loop);
                            window.request_redraw();
                            return;
                        }
                    }
                    // Settings gear — opens the settings window (or focuses it if
                    // already open). Closing is via the window's own controls.
                    if let Some((bx, by, bw, bh)) = self.settings_toggle_rect() {
                        if cx >= bx && cx <= bx + bw && cy >= by && cy <= by + bh {
                            self.open_settings_window(event_loop, None, None);
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
                    // cell grid. Hit order lives in `window_strip_click`.
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
                                ActionKind::NewTab => {
                                    let _ = self.spawn_new_tab(&menu_pid);
                                }
                                ActionKind::Close => self.close_pane(&menu_pid),
                                _ => {}
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
                    // Pop-out icon (file tabs): tear the tab's editor into its
                    // own wgpu window. Checked before × since it sits left of it.
                    if let Some((pid, idx)) = self
                        .pane_tab_popout_rects
                        .iter()
                        .find(|(_, _, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                        .map(|(id, i, _)| (id.clone(), *i))
                    {
                        // 파일 탭 → 에디터 팝아웃 창. 터미널 탭 → PTY pane undock(별도
                        // 터미널 창). 같은 pop-out 아이콘을 content 종류로 분기한다.
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
                            self.undock_pane_terminal(&pid, event_loop);
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
                                if !self.try_copy_md_block() {
                                    if self.md_body_rects.contains_key(&pane_id) {
                                        self.md_click_caret(
                                            &pane_id,
                                            self.cursor_px.0,
                                            self.cursor_px.1,
                                        );
                                        // Arm a selection drag: anchor at the
                                        // pressed caret; the drag moves the
                                        // caret while the anchor stays. A
                                        // plain click resolves on Released
                                        // (anchor == caret → cleared).
                                        if let Ok(mut ws) = self.ws.lock() {
                                            if let Some(m) = ws
                                                .panes
                                                .get_mut(&pane_id)
                                                .and_then(|p| p.markdown_mut())
                                            {
                                                m.sel_anchor =
                                                    Some((m.cur_line, m.cur_col));
                                            }
                                        }
                                        self.md_select_drag = Some(pane_id.clone());
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
                            // Phase 3 tear-off: 파일 탭을 창 밖에서 놓으면 별도
                            // 편집기 창으로 뜯어낸다. 창 안(패널 body 포함)에
                            // 놓으면 아래 split/dock 경로가 그대로 처리 —
                            // 여기선 커서가 창 밖으로 나갔을 때만 가로챈다.
                            if td.active {
                                let (win_w, win_h) = self.logical_win_size();
                                if Self::drag_left_window(
                                    self.cursor_px.0,
                                    self.cursor_px.1,
                                    win_w,
                                    win_h,
                                ) && self.tab_is_file(&td.pane, td.from)
                                {
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
        // Dock badge tracks unread notifications: opening a pane clears it,
        // a background notify raises it.
        self.sync_dock_badge();
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
        self.run_pending_autowheel();
        self.run_pending_sticky_seek();
        self.run_pending_autotoggle();
        self.run_pending_autoarona(event_loop);
        self.run_pending_autotabs();
        self.run_pending_autoopen();
        self.run_pending_autoconfirm();
        self.run_pending_autosettings(event_loop);
        self.run_pending_autoshellmenu();
        self.run_pending_automdselect();
        self.run_pending_auxpopout(event_loop);
        self.run_pending_autoundock(event_loop);
        self.drain_aux_captures();
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
            || !self.pending_capture.is_empty()
            // 별도창 캡처가 대기 중이면 그 deadline 까지 깨어 있어야 arming 이 발화한다.
            || self.aux_windows.iter().any(|a| a.pending_capture.is_some())
            || self.pending_autogit.is_some()
            || self.autoquit_at.is_some()
            // sticky 클릭 seek 이 도는 동안엔 스크롤이 목표 프롬프트에 닿을 때까지
            // 프레임을 펌프해야 노치가 계속 나가고 화면 관찰이 이어진다.
            || crate::render::sticky_seek_active()
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
/// 로컬 `/claude-usage`(oauth/usage 프록시)에서 5시간 창 사용률(%)만 뽑는다. curl 로
/// 로컬 엔드포인트만 쳐 토큰은 서버(키체인)가 읽는다 — argv 유출 없음. 실패/토큰
/// 없음/형식밖이면 None. utilization 은 이미 0..100 퍼센트(웹뷰 UsagePill 과 동일).
fn fetch_claude_five_hour(port: &str) -> Option<f32> {
    let out = std::process::Command::new("curl")
        .args(["-s", "--max-time", "5", &format!("http://127.0.0.1:{port}/claude-usage")])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    if v.get("ok").and_then(|b| b.as_bool()) != Some(true) {
        return None;
    }
    v.get("usage")?
        .get("five_hour")?
        .get("utilization")?
        .as_f64()
        .map(|x| x as f32)
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
