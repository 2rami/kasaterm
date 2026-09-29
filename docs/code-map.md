# 코드 맵 — main.rs 와 기능 모듈들

프로젝트 지침(`CLAUDE.md`)의 「코드 맵」이 여기를 가리킨다. Rust 를 만지기 전에 읽어라.

`main.rs` = `struct App`/기타 struct·enum 정의 + `new` 생성자 + 자유함수(`file_icon`/`parse_markdown`/`round_rect` 등) + `fn main` + tests 만. **App 메서드는 기능별 모듈로 분리**(전부 `impl App { ... }` 확장 + `use super::*`, 타입·자유함수는 crate root 그대로 참조, cross-module 호출 메서드는 `pub(crate)`):

- `render.rs` — GPU 렌더 패스(`render_frame`/`render_frame_gpu`/`paint_gpu_overlays`/`gpu_overlay_snapshot`). 자유함수는 2026-08-15 에 아래 두 모듈로 분리(13180→8360줄), 옛 `render::…` 경로는 glob 재수출로 유지
- `screenread.rs` — claude/codex **화면 그리드 판독·재작성** 자유함수: 스피너(`find_claude_spinner`)·입력박스(`prompt_box`)·배너/픽커/앵커 감지, 팀메시지(tell/SendMessage) 색칠·프사 배치
- `sprites.rs` — 학생 스프라이트·프사 **에셋 적재와 드로잉**: 번들/override 프레임, idle GIF 캐시, `draw_student_*`
- `handler.rs` — winit `ApplicationHandler`(`window_event`/`user_event`/`new_events`/`resumed`/`exiting`/`about_to_wait`). 소켓 백엔드 위임(`SocketBytes`/`SocketSplit`/`SocketFocus`) 처리·`window.json` 저장(`exiting`)/복원(`resumed`)·header/divider drag·tab-drag move·socket 명령 드레인
- `layout.rs` — pane 조작(`split_active_pane`/`move_pane`/`close_active_pane`/`spawn_new_tab`/`swap_dir`/`focus_dir`/`drop_*`/`divider_at_px`/`toggle_pane_zoom`/`close_tab`) + `resize_backend`/`publish_pty_layout`/좌표·`target_*`
- `session.rs` — `start_pty`(로컬 pane spawn)·`start_socket_pty`(cmux 소켓 + `socket::PtyBackend`)·window/session/cwd·label·tmux/socket·`save_session_state`·`apply_screen_update`/`pump_pty_screens`
- `chrome.rs` — 치수 getter·git col·사이드바/파일트리 토글·패널·줌/폰트·toast/version
- `toast.rs` — 오른쪽 위 알림 한 장을 그리는 자유함수(`paint_notice`)와 배치·제목/설명 가르기. 세우는 쪽은 `set_toast`(chrome.rs)
- `git_panel.rs` — 오른쪽 Git 열의 원본 pane·기기 식별, 읽기 요청 순서·문맥 검증, 현재/로컬/원격 브랜치 표시. 원격 조회 계약은 `kasa_mcp::git_panel`의 `kasa.git-panel.v2`이며 원본 기기가 cwd를 확인한다.
- `input.rs` — `send_bytes`·mouse(`send_mouse_sgr`·호버 전달 `forward_hover`·손가락 커서 판정 `refresh_hover_pointer`)·copy/paste·`handle_wheel`·`forward_key`·claude 상태 글리프
- `markdown.rs` — `md_editor_*`·md 링크/블록
- `testkit.rs` — `schedule_auto*`·`arm_auto*`·`run_pending_auto*` (env 자동테스트 하네스)
- `gpu.rs` — `KASATERM_RENDERER=gpu` 경로. 자체 wgpu Surface + 셀 파이프라인(sugarloaf 경로와 상호배타)
- `auxwin.rs` — 자체 wgpu Surface 기반 **별도 OS 창**(chrome.rs 의 wry webview 패널들과 다름): 문서(마크다운) 창 + 별도창 공용 틀(`AuxWindows`, 이벤트 위임, documents.json)
- `auxterm.rs` — 터미널 pane 을 별도 OS 창으로 뗀다(undock/dock). 창은 `pane_id` 만 들고 셀·PTY 는 App.ws/App.pty 에 그대로 — 그리기는 본창과 같은 `compose_terminal_pane`
- `settings.rs` — 설정 화면(타이틀바 기어 → pane 그리드 대체 전체 뷰, 좌 카테고리 nav + 우 폼)
- `socket.rs` — agent-socket ↔ TmuxSession 브리지(`PtyBackend`)·`open_preview`·`pane_record`/`window.json` IO
- `transcript.rs` — claude-code transcript(jsonl) → board 스냅샷 추출
- `bridge.rs` — bg SendMessage 브리지(teammate 플래그 유실된 detach 세션 인박스를 `claude attach` pty 로 직접 주입)
- `stream.rs` — 제거된 데몬 스트림 프로토콜에서 남은 GUI 뷰 타입(`DockedView`/`PaneStatusView`)
- `agent_state.rs` — pane 상태의 **정본**: `AgentState`(Idle/Working/Compacting/Waiting/Error) 를 훅 턴 경계·기록 턴 경계·attention·명부(`agents --json`)·PTY 박동에서 `resolve` 하는 순수 함수 + `StateHub`(App.collab.hub, PtyBackend 와 Arc 공유, 250ms 메모). 헤더 바·사이드바·미니맵·보드·펫·스프라이트가 전부 이것을 읽는다. **화면은 둘째 눈**(`ScreenSigns`: 살아 있는 스피너·승인 위젯·끊김 문구) — 정본(훅·기록·명부)이 없거나 어긋날 때만 판정을 바꾼다(조용한 열린 턴 6초 조기 닫기, 훅 죽었는데 도는 스피너, 훅 없는 하네스, 승인 위젯, 끊김). 화면으로 정본을 **대체**하지 마라
- `sidebar_pulse.rs` — 사이드바 맨 위 현황 줄(모든 기기 보드의 사람 차례·작업·끝, 누르면 보드). 수는 `native_board::pulse_counts`(보드 목록과 같은 판정, 원격 거울 줄 제외)를 백그라운드로 3초마다 읽는다. 방 우클릭 메뉴로 숨기기(settings.json `sidebar_pulse`)
- `left_panel.rs` — 사이드바 옆에서 밀려 나오는 판(보드·아로나 모드). 폭 배분의 넷째 기둥, 초점(키가 판으로 가나)·폭 끌기·×·밀려 나오기. 보드는 방이 아니라 여기 뜬다
- `native_board.rs` — 운영 보드(wgpu). 첫 탭 「작업현황」(사람용, 에이전트용은 「관측」 탭 — docs/boards.md)은 자식 모듈 `native_board/work.rs`(B안: 답할 것 → 진행·검증·완료, 기기·학생, 상세의 출처·증거·연결). 오른쪽 열 「작업」 탭은 `native_board/side.rs`(정리: 지금 창의 현재 작업·변경·다음 일·막힘·검증 / 조율: 같은 할 일 목록을 좁게 / 권한 표)
- `work_mode.rs` — 작업 모드(정리·조율)와 권한 표의 나쵸 클라이언트. 정본은 나쵸(`GET/POST /api/app/work-mode`·`GET /api/app/capabilities`, 나쵸 `desk-api.md`), 카사텀 설정의 `work_mode_cache` 는 마지막으로 확인한 값뿐. 쓰기는 탭을 누를 때만, 모드는 둘뿐. 나쵸 키 없는 기기는 사람이 고른 명부 기기(`nacho_read_via`)의 `/nacho/read/*` 로 읽기만 한다 — `docs/nacho-read-relay.md`
- `version.rs` — 지금 판과 피드 최신판 견주기, 계정 메뉴 판 번호 줄의 업데이트 입구(`update_entry`: Sparkle·WinSparkle·없으면 릴리스 페이지). 여러 기기 패치 릴리스 계획·추적은 앱 밖 `tools/release/`(fastpatch=계획·CLI, backend=실제 단계, nacho=승인 소비·재개, deps=도구 고르기, devices=기기 받기 계획, proc=명령·HTTP 실행기) — `docs/fast-patch-release.md`
- `app_restart.rs` — 앱 재시작 계획용 사실을 GUI 스레드에서 잰다(바쁜 학생·미저장 편집기·자기설치 예정). 계약·도우미는 `kasa_socket::app_restart`, 절차 `docs/app-restart.md`
- `app_update.rs` — 앱 업데이트 창구의 이 기기 쪽: 수락(나쵸 승인·지금 사실), 받기·확인·준비·적용 스레드(한 번에 한 작업), 부팅 표식. 계약·검증·도우미는 `kasa_socket::app_update`, 절차 `docs/app-update.md`
- `nacho_tasks.rs` — 나쵸 작업 장부의 타입 클라이언트(`/api/app/tasks`). 같은 id 는 큰 `rev` 하나, 끊기면 마지막 목록 유지, `done` 과 검증 통과를 가른다
- `agent_transitions.rs` — 상태 전이 → 알림 이벤트(TurnDone/Waiting/Error/…) 순수 함수. 데스크톱 알림·토스트·펄스는 `chrome.rs apply_transition_event` 한 곳에서 낸다

새 App 메서드 추가 시 도메인 맞는 모듈에. 다른 모듈/crate root 에서 호출되면 `pub(crate)`. 상세 [[reference_kasaterm_main_module_split]].
