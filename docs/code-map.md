# 코드 맵 — main.rs 와 기능 모듈들

프로젝트 지침(`CLAUDE.md`)의 「코드 맵」이 여기를 가리킨다. Rust 를 만지기 전에 읽어라.

`main.rs` = `struct App`/기타 struct·enum 정의 + `new` 생성자 + 자유함수(`file_icon`/`parse_markdown`/`round_rect` 등) + `fn main` + tests 만. **App 메서드는 기능별 모듈로 분리**(전부 `impl App { ... }` 확장 + `use super::*`, 타입·자유함수는 crate root 그대로 참조, cross-module 호출 메서드는 `pub(crate)`):

- `render.rs` — GPU 렌더 패스(`render_frame`/`render_frame_gpu`/`paint_gpu_overlays`/`gpu_overlay_snapshot`). 자유함수는 2026-08-15 에 아래 두 모듈로 분리(13180→8360줄), 옛 `render::…` 경로는 glob 재수출로 유지
- `screenread.rs` — claude/codex **화면 그리드 판독·재작성** 자유함수: 스피너(`find_claude_spinner`)·입력박스(`prompt_box`)·배너/픽커/앵커 감지, 팀메시지(tell/SendMessage) 색칠·프사 배치·상태줄 mod 가 막 지은 줄 덧칠(`paint_status_line`)
- `sprites.rs` — 학생 스프라이트·프사 **에셋 적재와 드로잉**: 번들/override 프레임, idle GIF 캐시, `draw_student_*`
- `handler.rs` — winit `ApplicationHandler`(`window_event`/`user_event`/`new_events`/`resumed`/`exiting`/`about_to_wait`). 소켓 백엔드 위임(`SocketBytes`/`SocketSplit`/`SocketFocus`) 처리·`window.json` 저장(`exiting`)/복원(`resumed`)·header/divider drag·tab-drag move·socket 명령 드레인
- `layout.rs` — pane 조작(`split_active_pane`/`move_pane`/`close_active_pane`/`spawn_new_tab`/`swap_dir`/`focus_dir`/`drop_*`/`divider_at_px`/`toggle_pane_zoom`/`close_tab`) + `resize_backend`/`publish_pty_layout`/좌표·`target_*`
- `session.rs` — `start_pty`(로컬 pane spawn)·`start_socket_pty`(cmux 소켓 + `socket::PtyBackend`)·window/session/cwd·label·tmux/socket·`save_session_state`·`apply_screen_update`/`pump_pty_screens`
- `chrome.rs` — 치수 getter·git col·사이드바/파일트리 토글·패널·줌/폰트·toast/version
- `toast.rs` — 오른쪽 위 알림 한 장을 그리는 자유함수(`paint_notice`)와 배치·제목/설명 가르기. 세우는 쪽은 `set_toast`(chrome.rs)
- `info.rs` — 오른쪽 Info 열. 카드 아래 「모든 방」 목록(학생 줄·실행 상세·토큰·스킬·MCP)과 그 수집 워커(`ps`+`lsof`, 펼쳤을 때만 자주)
- `info_focus.rs` — Info 열 맨 위 「지금 보는 칸」 카드. 사실은 `observe` 한 곳(칸 종류·폴더·모델·mod 사실·그 칸 포트)이고, 이 기기 칸은 감시 스레드가 mod 신호(`claude_mod::set_focus_listener`)·프로세스 지문·느린 주기로, 다른 기기 칸은 원본의 `/term/pane-info`(`kasa_mcp::pane_info`, 같은 `observe` 를 부른다) 롱폴로 받는다. 포트(lsof)는 뒤에서 읽어 mod 반영을 막지 않는다
- `git_panel.rs` — 오른쪽 Git 열의 원본 pane·기기 식별, 읽기 요청 순서·문맥 검증, 현재/로컬/원격 브랜치 표시. 원격 조회 계약은 `kasa_mcp::git_panel`의 `kasa.git-panel.v2`이며 원본 기기가 cwd를 확인한다. 일꾼(`spawn_poller`)은 칸이 바뀌면 바로, 작업 트리 파일이 바뀌면(`kasa_mcp::git_watch`, 누가 고쳤든) 합쳐 한 번, 그 밖에는 git 지문이 바뀔 때만 읽고, 다른 기기 칸은 `spawn_remote_watcher` 가 원본의 `/term/gitcol/wait` 에 매달린다. 칸 폴더가 여러 레포를 담은 부모면 그 아래 레포를 최근에 만진 순으로 골라 보이고 머리 메뉴에서 칸마다 고른다(`kasa_mcp::git_panel::panel_view`)
- `input.rs` — `send_bytes`·mouse(`send_mouse_sgr`·호버 전달 `forward_hover`·손가락 커서 판정 `refresh_hover_pointer`)·copy/paste·`handle_wheel`·`forward_key`·claude 상태 글리프
- `markdown.rs` — `md_editor_*`·md 링크/블록
- `testkit.rs` — `schedule_auto*`·`arm_auto*`·`run_pending_auto*` (env 자동테스트 하네스)
- `gpu.rs` — `KASATERM_RENDERER=gpu` 경로. 자체 wgpu Surface + 셀 파이프라인(sugarloaf 경로와 상호배타)
- `auxwin.rs` — 자체 wgpu Surface 기반 **별도 OS 창**(chrome.rs 의 wry webview 패널들과 다름): 문서(마크다운) 창 + 별도창 공용 틀(`AuxWindows`, 이벤트 위임, documents.json)
- `auxterm.rs` — 터미널 pane 을 별도 OS 창으로 뗀다(undock/dock). 창은 `pane_id` 만 들고 셀·PTY 는 App.ws/App.pty 에 그대로 — 그리기는 본창과 같은 `compose_terminal_pane`
- `settings.rs` — 설정 화면(타이틀바 기어 → pane 그리드 대체 전체 뷰, 좌 카테고리 nav + 우 폼)
- `socket.rs` — agent-socket ↔ TmuxSession 브리지(`PtyBackend`)·`open_preview`·`pane_record`/`window.json` IO
- `transcript` — claude-code transcript(jsonl) → board 스냅샷 추출. 본체는 `crates/kasa-agents/src/transcript.rs`(main.rs 가 `use kasa_agents::transcript` 로 옛 경로 유지)
- `chat_view.rs` — 학생 pane 「대화로 보기」(⋮·Info 학생 줄). pane 마다의 보기 상태·기록 읽기(1초, `socket::read_incremental`)·입력칸 키/IME/붙여넣기·휠·클릭. 자식 `chat_view/parse.rs` 는 jsonl → 대화 칸(폰 `mobile/lib/conversation.dart` 와 같은 규칙, codex rollout 포함)과 화면 선택지 판독, `chat_view/paint.rs` 는 배치(기록이 바뀔 때만)·그리기. 렌더는 대화 pane 의 격자를 비우고 `paint_chat_views` 로 본문 자리에 그린다
- `chat_view/live.rs` — 대화 보기 위의 연결 mod 지금(일 상태·도는 도구·승인 요청). 이 기기 칸은 `claude_mod::mirror_view` 를
  바로, 거울은 원본 `/term/mod-live` 에 매달린다. 승인 창 「Yes」·「No」의 mod 결정(`/term/mod-decide`)도 여기
- `bridge.rs` — bg SendMessage 브리지(teammate 플래그 유실된 detach 세션 인박스를 `claude attach` pty 로 직접 주입)
- `stream.rs` — 제거된 데몬 스트림 프로토콜에서 남은 GUI 뷰 타입(`DockedView`/`PaneStatusView`)
- 엔진·협업 크레이트(카사라이트·`kasa tui` 가 git rev 로 함께 쓴다, 설계 `docs/terminal-engine.md`):
  `kasa-screen`(셀·행·ScreenUpdate·ANSI·리플로우·칸 배치 — kasa-bridge 가 재수출) ·
  `kasa-collab`(보드 수집기 `board_service`·tell 장부 `tell_service`·칸 열쇠 `surface_keys`·기계 id `identity`·GUI 없는
  호스트용 tell 전달 `delivery`·claude 훅 설치 `hooks`. 기기 명부·관문·원격 칸은 호스트가 `env::CollabEnv` 로 꽂는다 —
  본판은 `kasa_mcp::install_collab_env`, kasa-mcp 가 옛 경로를 재수출) · `kasa-agents`(대화 기록 읽기) ·
  `kasa-socket::cli`(`kasaterm-cli` 본체 — 바이너리와 `kasa tui` 멀티콜이 부른다)
- `kasa-mcp/src/claude_mod.rs` — claude 안에 실린 연결 mod(`collab-hooks/claude-mods/kasaterm-bridge`)가 loopback HTTP(`/claude-mod/*`)로 알린 사실의 저장소: 칸별 턴·압축·승인·질문·사용량·백그라운드·활동, 승인 요청 브로커(원격 결정·감사 기록), tell 받은편지함, 바뀐 순간의 상태줄(`status_overlay` — 엔진이 다시 그릴 때까지만). 사실은 그 칸에 지금 도는 claude 가 hello 를 보낸 그 pid 일 때만(`live`) 정본이다. 계약 `docs/claude-mod-bridge.md`
- `tell_delivery.rs` — 안전한 tell 의 이 기기 배달: 대기열에서 칸을 골라 신원을 증명한 뒤 빈 입력창을 확인해 붙여넣고 Enter(mod 칸도 같다 — 일하는 칸은 진행 중인 턴에 들어간다). 계약 `docs/tell-protocol.md`
- `agent_state.rs` — pane 상태의 **정본**: `AgentState`(Idle/Working/Compacting/Waiting/Error) 를 훅 턴 경계·기록 턴 경계·attention·명부(`agents --json`)·PTY 박동에서 `resolve` 하는 순수 함수 + `StateHub`(App.collab.hub, PtyBackend 와 Arc 공유, 250ms 메모). 헤더 바·사이드바·미니맵·보드·펫·스프라이트가 전부 이것을 읽는다. mod 칸은 `claude_mod::live` 가 정본이라 `resolve_module` 이 바로 판정하고 화면·기록 턴·명부·Enter 다리는 쉰다(`input.rs` 화면 스캔도 그 칸은 건너뛴다). **화면은 둘째 눈**(`ScreenSigns`: 살아 있는 스피너·승인 위젯·끊김 문구) — 정본(훅·기록·명부)이 없거나 어긋날 때만 판정을 바꾼다(조용한 열린 턴 6초 조기 닫기, 훅 죽었는데 도는 스피너, 훅 없는 하네스, 승인 위젯, 끊김). 화면으로 정본을 **대체**하지 마라
- `board_digest.rs` — 모든 기기 보드 스냅샷(`collab.snapshot`)의 요약: 사람 차례·하는 중·끝 수와 사람을 기다리는 학생. 사이드바 학생 줄·펫 현황판이 쓴다. 보드 판은 걷었다(나쵸 대화·작업은 나쵸 독립 앱)
- `sidebar_pulse.rs` — 사이드바 맨 위 「목록 | 배치도」 전환과 학생 줄 사정(모든 기기 보드의 사람 차례·작업·끝). 수는 `board_digest.rs`(원격 거울 줄 제외)를 백그라운드로 3초마다 읽고 펫 현황판에도 적는다. 방 우클릭 메뉴로 숨기기(settings.json `sidebar_pulse`)
- `version.rs` — 지금 판과 피드 최신판 견주기, 계정 메뉴 판 번호 줄의 업데이트 입구(`update_entry`: Sparkle·WinSparkle·없으면 릴리스 페이지). 여러 기기 패치 릴리스 계획·추적은 앱 밖 `tools/release/`(fastpatch=계획·CLI, backend=실제 단계, nacho=승인 소비·재개, deps=도구 고르기, devices=기기 받기 계획, proc=명령·HTTP 실행기) — `docs/fast-patch-release.md`
- `native_device_account.rs` — 설정 「계정」의 KASA 계정(로그인·Google·GitHub 연결·선택). 자식 `native_account_profile.rs` 는 얼굴 줄(프사·닉네임)·「로그인 방법」·아이디·비밀번호 바꾸기, `native_work_permissions.rs` 는 일 권한. 관문 쪽은 `kasa_mcp` 의 `gateway_profile.rs`(`/relay/profile`)·`device_profile.rs`(이 기기 호출) — `docs/account-oauth.md` 「Profile」
- `remote_approval.rs` — 원격 승인의 데스크톱 화면: 같은 계정 **다른 기기** 학생의 권한 요청을 관문 긴 폴링으로 받아 오른쪽 위 결정 알림([보기][나중에], `collab.toast_action` 센티널)과 원문 시트([허락][거절], Return=거절)로 묻고 관문에 보낸다. 이 기기 요청은 `kasa_mcp::approval_bridge` 가 브로커(`claude_mod`)↔관문을 잇는다 — `docs/remote-approval.md`
- `native_op_approval.rs` — 설정 「계정」의 1Password: 서비스 계정 토큰 넣기(NSSecureTextField 시트 → `kasa_mcp::op_approval`, 키체인)·지우기, 폰 Face ID 승인 열쇠 믿기(지문 시트, Return=취소)·거두기, 새 열쇠 알림. 학생 요청 길은 소켓 `op.secret`(socket.rs, 칸은 소켓 상대 pid 로) — `docs/op-faceid-approval.md`
- `update_notice.rs` — 새 판 알림: 업데이터(맥 preview Sparkle 확인·WinSparkle)가 찾은 판을 오른쪽 위 결정 알림으로 세우고, [업데이트]면 끊길 일을 한 번 묻고 설치를 맡긴다, [닫기]면 그 판을 기기 설정에 적는다. Sparkle 쪽 확인·받기·즉시 설치는 `macos_sparkle.rs` — `docs/automatic-preview-updates.md`
- `app_restart.rs` — 앱 재시작 계획용 사실을 GUI 스레드에서 잰다(바쁜 학생·미저장 편집기·자기설치 예정). 계약·도우미는 `kasa_socket::app_restart`, 절차 `docs/app-restart.md`
- `app_update.rs` — 앱 업데이트 창구의 이 기기 쪽: 수락(나쵸 승인·지금 사실), 받기·확인·준비·적용 스레드(한 번에 한 작업), 부팅 표식. 계약·검증·도우미는 `kasa_socket::app_update`, 절차 `docs/app-update.md`
- `mirror_follow.rs` — 원본 격자는 마지막으로 만진 쪽을 따른다(tmux `window-size latest`). 사람 손(키·IME·왼클릭·SGR 누름·확대)이 닿은 칸을 판정해, 거울이면 `remote::touch_source` 로 원본을 그 칸 크기로 잡고 원본이면 `reclaim_viewer_sizes` 로 되찾는다. 만지기 전 거울은 `mirror_view` 가 뷰어 쪽에서 다시 접는다. 호스트가 `viewport_latest` 를 모르면(옛 판) 확대 때만 키우는 옛 규칙(`layout.rs fit_zoomed_mirror`). 폰까지 묶은 칸 크기 규칙은 `docs/webterm-handoff.md` 「원본 크기는 쓰는 쪽이 쥔다」
- `mirror_render.rs` — 거울 칸을 어떤 보기로 그릴지 한 곳의 판정(`MirrorKind` Chat·Shell·Grid). Grid(옛 원본)만 만지면 원본 크기를 빌린다(`touch_surface_size` 가 이걸 본다). 보이는 Chat 칸은 대화로 연다(`open_mirror_chats`, 사람이 터미널로 돌린 칸은 그대로). `docs/mirror-render.md`
- `shell_view.rs` — 거울 셸 칸의 「명령 + 결과」 카드. 원본 `/term/blocks`(긴 폴링, `kasa_mcp::shell_blocks`)를 칸마다 일꾼이 받아 합치고, 자식 `shell_view/paint.rs` 가 칸 폭으로 다시 접어 그린다. 키는 원본 PTY 로(터미널과 같다), 클릭·휠은 카드가 먹는다. 셸 통합 없음·대체 화면이면 격자를 줄여 그린다(`layout.rs pane_display_scale`). 원본 쪽 줄 해석은 `kasa_pty::block_lines`
- `prompt_nav.rs` — claude 칸의 스크롤바·프롬프트 눈금·프롬프트 이동(Option·Ctrl+↑↓). 그림·누름은 하나, 정본은 둘(`NavSource`): classic 은 이 터미널의 스크롤백(`scrollback_state`, 앵커는 `turnjump.rs` 캐시의 `❯` 줄)이라 `scroll_to_abs` 로 곧장 옮기고 키도 여기서 받는다(`prompt_nav_key`). 풀스크린(대체 화면)은 칸 안 mod(`collab-hooks/claude-mods/prompt-nav`, claude shim 이 `--plugin-dir` 로 싣는다)가 쓴 `<shim>/prompt-nav/<pane>.json` 을 읽어 그리며, 요청 파일 + 장전 화음(`ctrl+x b`) → `armed` 확인 → `ctrl+↑` 로 mod 에 스크롤을 시킨다. classic 칸의 `/prompt-nav` 는 mod 가 상태 파일 `ask` 로 맡긴다. 턴 띠 ↑↓↡ 도 mod 가 있으면 이 길로 간다
- `trust_prompt.rs` — claude 폴더 신뢰 화면 자동 통과. 화면 펌프(`pump_pty_screens`)가 후보 pane 을 적고 GUI 틱이 판정한다: claude pane·입력 조용·한글 조합 아님일 때만, 초점이 No 면 아래 화살표 한 번, Yes 면 Enter 한 번(같은 화면에 반복 없음). 신뢰 선탑재는 `kasa_socket::claude_trust`(claude shim 이 `kasaterm-cli claude-trust "$PWD"` 로 부름)
- `own_room.rs` — 이 기기 방 지키기: 현지에 세울 칸(셸·학생·연결 칸)의 기준이 다른 기기 방의 보기 창이면 이 기기 방으로 옮기고(`own_spawn_host` — 섞이면 그 방이 통째로 「이 기기」 절로 넘어가 저쪽 학생이 이쪽 학생처럼 보인다), 자기 칸이 0 이 되면(닫기·이사·미러만 복원) 이 기기 셸 방을 하나 세운다(`keep_own_room`, 1초 틱)
- `agent_transitions.rs` — 상태 전이 → 알림 이벤트(TurnDone/Waiting/Error/…) 순수 함수. 데스크톱 알림·토스트·펄스는 `chrome.rs apply_transition_event` 한 곳에서 낸다

새 App 메서드 추가 시 도메인 맞는 모듈에. 다른 모듈/crate root 에서 호출되면 `pub(crate)`. 상세 [[reference_kasaterm_main_module_split]].
