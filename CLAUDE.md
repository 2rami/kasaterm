kasaterm (자체 tmux GUI 터미널 + Claude 런처). 사용자: 거노 (디자이너→개발 입문). 패키지/바이너리명 = **`kasaterm`**.

[!] 작업 시작 전 반드시 [.memory/MEMORY.md](file://./.memory/MEMORY.md) 를 먼저 읽어라 — 거노님 개발 성향·todos·피드백, 그리고 **맨 위 핸드오프 블록**(직전 세션이 어디서 멈췄는지)부터 파악. 렌더러·색·아키텍처 배경, 렌더버그 카탈로그, 코드 수정 주의점은 전부 거기 토픽 파일에 있다.

## 자율 테스트 우선

사용자에게 "테스트 해보세요"라고 떠넘기지 말고 **너가 직접** 실행·확인·수정 사이클을 돌려라.

반드시 먼저 정리: `pkill -f "target/debug/kasaterm"; pkill -f "target/release/kasaterm"; pkill -f "tmux -C"; sleep 1; rm -rf /tmp/tmux-501`

1. **빌드/실행** — `cargo run -p kasaterm > /tmp/kasaterm-run.log 2>&1 &` (백그라운드)
2. **스크린샷** — `KASATERM_AUTOCAPTURE_MS=8000` 로 N초 후 자동 캡처. 기본 경로 `$TMPDIR/kasaterm.png` (`KASATERM_AUTOCAPTURE_PATH` 로 변경). Read tool 로 즉시 보기. macOS `screencapture` 는 권한 막혀 안 됨 — 무조건 자체 캡처.
3. **자동 입력** — `KASATERM_AUTOSEND="claude" KASATERM_AUTOSEND_MS=6000`. send_bytes 직접 주입이라 **IME 조합 경로는 재현 못 함** — 한글 조합 버그는 사용자가 직접 타이핑해야 함 (`KASATERM_IME_DEBUG=1` 로 키 코드포인트 로깅).
4. **체감(스크롤·입력 지연)은 반드시 release** — `cargo run --release -p kasaterm`. 디버그 빌드는 원래 버벅임(debug=느림, release/.app=빠름). 디버그로 "느리다" 판단 금지.
5. **시각 확인** — 스크린샷 본 후 어색한 부분 직접 짚어내고 수정. "어때보여요?" 묻지 말고 너의 판단으로 다음 액션.

## 함정·배경·수정 주의점은 메모리에

렌더러/색 파이프라인·아키텍처 배경, 렌더버그 디버깅 카탈로그, 코드 수정 주의점(PTY reader **`try_send` 필수** 등), 성능 히스토리는 CLAUDE.md 에 중복해 두지 않는다 — `.memory/MEMORY.md` 토픽 파일에 있다. 1순위 = [[feedback_tmuxify_rendering_pipeline]] (렌더버그 카탈로그 + try_send 트랩). 코드 만지기 전 관련 토픽을 먼저 recall 할 것.

## 코드 맵 (main.rs 12896→3515줄, App 메서드 8모듈 분할 — 2026-06-05)

`main.rs` = `struct App`/기타 struct·enum 정의 + `new` 생성자 + 자유함수(`file_icon`/`parse_markdown`/`round_rect` 등) + `fn main` + tests 만. **App 메서드는 기능별 모듈로 분리**(전부 `impl App { ... }` 확장 + `use super::*`, 타입·자유함수는 crate root 그대로 참조, cross-module 호출 메서드는 `pub(crate)`):

- `render.rs` — GPU 렌더 패스(`render_frame`/`render_frame_gpu`/`paint_gpu_overlays`/`gpu_overlay_snapshot`)
- `handler.rs` — winit `ApplicationHandler`(`window_event`/`user_event`/`new_events`/`resumed`/`exiting`/`about_to_wait`). 소켓 백엔드 위임(`SocketBytes`/`SocketSplit`/`SocketFocus`) 처리·`window.json` 저장(`exiting`)/복원(`resumed`)·header/divider drag·tab-drag move·socket 명령 드레인
- `layout.rs` — pane 조작(`split_active_pane`/`move_pane`/`close_active_pane`/`spawn_new_tab`/`swap_dir`/`focus_dir`/`drop_*`/`divider_at_px`/`toggle_pane_zoom`/`close_tab`) + `resize_backend`/`publish_pty_layout`/좌표·`target_*`
- `session.rs` — `start_pty`(로컬 pane spawn)·`start_socket_pty`(cmux 소켓 + `socket::PtyBackend`)·window/session/cwd·label·tmux/socket·`save_session_state`·`apply_screen_update`/`pump_pty_screens`
- `chrome.rs` — 치수 getter·git col·사이드바/파일트리 토글·패널·줌/폰트·toast/version
- `input.rs` — `send_bytes`·mouse(`send_mouse_sgr`)·copy/paste·`handle_wheel`·`forward_key`·claude 상태 글리프
- `markdown.rs` — `md_editor_*`·md 링크/블록
- `testkit.rs` — `schedule_auto*`·`arm_auto*`·`run_pending_auto*` (env 자동테스트 하네스)

새 App 메서드 추가 시 도메인 맞는 모듈에. 다른 모듈/crate root 에서 호출되면 `pub(crate)`. 상세 [[reference_kasaterm_main_module_split]].

## 로컬 PTY 모드 (데몬 완전 제거 — 2026-06-05)

**데몬은 폐기됐다.** GUI(`App`)가 PTY를 직접 소유(`self.pty: HashMap<String, Arc<PtySession>>`)하고 split/focus/close/window가 **전부 로컬 즉시** 처리된다 — RPC 왕복·broadcast·통째 덮어쓰기가 없으니 옛 daemon-authoritative 불변식(부활/증식/drag먹통/덮어쓰기)은 **클래스째 사라졌다**. 구조변경 GUI 액션은 `self.pty_layout`/`ws.panes`를 직접 수정하면 된다. (데몬 detach/reattach·백그라운드 실행이 필요해지면 `archive/daemon-mode` 브랜치에 코드 전부 박제돼 있음.)

핵심 패턴:
- **초기 부팅**: `start_pty`(`session.rs`) = `spawn_session_pane`(로컬 pane spawn) + `start_socket_pty`(cmux 소켓 서버). 데몬 discover/attach 없음.
- **cmux 소켓**: claude tmux shim·kasaterm-cli·pane 협업이 쓰는 소켓 서버는 `socket::PtyBackend`. `App.pty`가 `Arc<Mutex>`가 아니라 별도 스레드서 직접 못 만져 → `send_text`/`split`/`focus`를 `EventLoopProxy`로 GUI 스레드에 위임(`UserEvent::SocketBytes`/`SocketSplit`/`SocketFocus`, `handler.rs` user_event서 처리).
- **resize**: `resize_backend`(`layout.rs`)는 **`leaf_cells` 기반**(leaf id == primary pid 직접 resize). split 직후 새 pane은 `ws.panes`에 PaneState가 아직 없어 ws.panes 순회 방식은 80×24 방치→화면 겹침이었음. 보조탭만 ws.panes로.
- **divider/window 영속**: ratio는 `save_session_state`의 layout(json)에 저장→복원. 윈도우 크기는 `~/.config/kasaterm/window.json`(`handler.rs` exiting/resumed, IO는 `socket.rs`).

**후속 미구현(데몬 제거로 빠진 것 — 로컬 재구현):** ① `claude --resume` 세션 복원("열면 이어가기" 핵심, `start_pty`가 아직 복원 안 함, `pane_record`는 저장 중) ② working bar status(transcript watcher가 데몬 연결이라 dead) ③ dock/undock·파일 미리보기(`open_preview`)·cross-window drag. 상세 [[project_kasaterm_session_lifecycle]] · [[reference_kasaterm_daemon_removal]].
