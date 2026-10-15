# 코드 맵 — main.rs 와 기능 모듈들

프로젝트 지침(`CLAUDE.md`)의 「코드 맵」이 여기를 가리킨다. Rust 를 만지기 전에 읽어라.
크레이트 사이 층과 의존 방향은 [`workspace.md`](workspace.md)(검사 `scripts/check-workspace.py`).

`main.rs` = `struct App`/기타 struct·enum 정의 + `new` 생성자 + 그리기·문서 자유함수(`file_icon`/`parse_markdown`/`round_rect` 등) + `fn main`(부팅 순서 조립) + tests 만. 부팅 자유함수는 아래 세 모듈에 있고 main.rs 가 옛 경로(`crate::verification_run` 등)로 재수출한다. **App 메서드는 기능별 모듈로 분리**(전부 `impl App { ... }` 확장 + `use super::*`, 타입·자유함수는 crate root 그대로 참조, cross-module 호출 메서드는 `pub(crate)`):

- `bootstrap.rs` — `main()` 이 창을 띄우기 전에 부르는 것: 패닉·stderr 로그, 물려받은 claude 표식 지우기(`CLAUDE_MARKER_ENV`), 라이트(`lite_mode`·`apply_lite_env`)·캡처 env·검증 실행 판정(`verification_run`), 엔진 호스트 정책(`install_pty_host_policy`), 자기설치(`install_pending`·`arm_self_install`), 끝난 태스크·빈 인박스 청소, 살아 있는 인스턴스 pid
- `pane_shims.rs` — 칸 셸 PATH 앞에 놓는 shim 설치(`install_pane_shims`, 원자 교체 `write_shim`)·미리보기/`open`/tmux/rust-analyzer shim·칸 env(`proxy_env`·`agent_name_suffix`·신원 도우미)·학생 shim(`install_student_shims`). 하네스별 래퍼는 자식: `pane_shims/claude.rs`(claude 래퍼·협업 훅·연결 mod·계정 줄) · `pane_shims/codex.rs`(codex 래퍼·계정 파일·`codex_binary`) · `pane_shims/agy.rs`(agy 훅 래퍼)
- `spawn_env.rs` — 칸이 뜰 자리: 시작 cwd(`resolve_spawn_cwd`)·기본 셸·훅용 셸(`hook_shell_program`)·고를 수 있는 셸, 이 인스턴스의 소켓 경로(`resolve_kasaterm_socket_path`)와 HTTP 포트(`mcp_panel_port`)

- `render.rs` — GPU 렌더 패스(`render_frame`/`render_frame_gpu`/`paint_gpu_overlays`/`gpu_overlay_snapshot`). 자유함수는 2026-08-15 에 아래 두 모듈로 분리(13180→8360줄), 옛 `render::…` 경로는 glob 재수출로 유지.
  `render_frame_gpu` 는 그리는 **차례**를 보여 주고, 화면 구역별 패스는 자식 모듈이 그린다: `render/sidebar.rs`(방 카드·배치도/별도창 띠·pane 줄·목록 가장자리·줄 우클릭 메뉴) · `render/file_tree_column.rs`(파일트리 칼럼) · `render/side_column.rs`(오른쪽 Git·Info·세션·MCP 칼럼) · `render/status_bar.rs`(하단 상태줄) · `account_popover.rs`(계정 드롭다운 배치와 그리기). 자식 패스는 `g` 와 이 프레임에 읽는 작은 `Frame`, 고쳐 쓸 상태 `&mut` 만 받는다 — `g` 가 `self.gpu` 를 잡은 안쪽이라 `&self` 메서드로 구할 값은 부르는 쪽이 미리 풀어 넘긴다. 클립·히트렉트·그리는 순서는 옮기기 전과 같다
- `screenread.rs` — claude/codex **화면 그리드 판독·재작성** 자유함수: 스피너(`find_claude_spinner`)·입력박스(`prompt_box`)·배너/픽커/앵커 감지, 팀메시지(tell/SendMessage) 색칠·프사 배치·상태줄 mod 가 막 지은 줄 덧칠(`paint_status_line`)
- `sprites.rs` — 학생 스프라이트·프사 **에셋 적재와 드로잉**: 번들/override 프레임, idle GIF 캐시, `draw_student_*`
- `handler.rs` — winit `ApplicationHandler`(`window_event`/`user_event`/`new_events`/`resumed`/`exiting`/`about_to_wait`). 소켓 백엔드 위임(`SocketBytes`/`SocketSplit`/`SocketFocus`) 처리·`window.json` 저장(`exiting`)/복원(`resumed`)·header/divider drag·tab-drag move·socket 명령 드레인
- `layout.rs` — pane 조작(`split_active_pane`/`move_pane`/`close_active_pane`/`spawn_new_tab`/`swap_dir`/`focus_dir`/`drop_*`/`divider_at_px`/`toggle_pane_zoom`/`close_tab`) + `resize_backend`/`publish_pty_layout`/좌표·`target_*`
- `session.rs` — 칸 번호(`used_pane_ids`·`alloc_pane_id`)·새 칸 세우기(`spawn_session_pane`·`bring_pane_home`)·PTY 등록(`insert_pty`)·칸 판정(`window_leaves`·`pane_bypass_on`·`pane_agent_working`). 기능별 몸통은 `session/` 자식(옛 `session::…` 경로는 재수출):
  `boot.rs`(첫 칸 `start_pty`·옛 tmux·소켓 서버 `start_socket_pty`) · `screen_pump.rs`(`apply_screen_update`·`pump_pty_screens`) ·
  `agent_launch.rs`(칸에 학생 env 심기·학생/페르소나 바꾸기·`restart_pane_agent`) · `accounts.rs`(claude·codex 계정 전환과 재시작 대기) ·
  `save.rs`(`session_state_json`·`save_session_state`) · `restore.rs`(`restore_session_state`·복원 명령·복원 전 수 세기) ·
  `local_restore.rs`(저장·옛 저장본 복원에서 다른 기기 미러 제외, 섞인 로컬 탭·방 번호 보존) ·
  `titles.rs`(대화 기록 꼬리 → 칸 제목·effort) · `rooms.rs`(방 만들기·고르기·순서·닫기·이름, 칸·탭 초점) ·
  `closed_panes.rs`(닫은 칸 기록·되살리기·직접 숨김/자동 보관 사유) · `verify.rs`(격리 복원 표시·첫 탭 닫기 회귀 리그) · `file_tree.rs`(cwd·파일 트리·파일 열기) · `sidebar.rs`(방 카드 높이·스크롤) ·
  `remote_rooms.rs`(원격 셸·거울 칸·원격 방 보기 창) · `migrate.rs`(칸 이사·되돌리기)
- `chrome.rs` — 치수 getter·git col·사이드바/파일트리 토글·패널·줌/폰트·toast/version
- `toast.rs` — 오른쪽 위 알림 한 장을 그리는 자유함수(`paint_notice`)와 배치·제목/설명 가르기. 세우는 쪽은 `set_toast`(chrome.rs)
- `account_brand.rs` — 실제 이메일 도메인으로 계정의 Gmail·NAVER·Sionic 표지를 고르는 공통 판정과 네이티브 그리기. 설정·계정 팝오버·상태줄이 같은 폭과 자산을 쓴다.
- `share_panel.rs` + `share_panel/{input,files,paint}.rs` — 공유 파일의 최근/폴더 탐색·검색·입력 소유권·읽기 전용 미리보기. 목록·이미지·바로가기 작업은 워커에서 읽고, 이전 선택의 늦은 응답은 세대로 거른다. 설정은 `native_settings/share.rs`, 공통 색인과 안전한 로컬 경로는 `kasa-mcp/share/catalog.rs`, 바로가기 위치는 `share/shortcut.rs`가 맡는다. 계약은 `docs/share-sync.md`.
- `session/accounts/sync.rs` — 다른 기기의 Claude 계정 선택: 검증 신원 일치·중복 별명 판정, 최신 선택 직렬 전파와 기기별 적용 확인. 계약은 `docs/agent-account-selection.md`.
- `info.rs` — 오른쪽 Info 열. 카드 아래 「모든 방」 목록(학생 줄·실행 상세·토큰·스킬·MCP)과 그 수집 워커(`ps`+`lsof`, 펼쳤을 때만 자주)
- `info_focus.rs` — Info 열 맨 위 「지금 보는 칸」 카드. 사실은 `observe` 한 곳(칸 종류·폴더·모델·mod 사실·그 칸 포트)이고, 이 기기 칸은 감시 스레드가 mod 신호(`claude_mod::set_focus_listener`)·프로세스 지문·느린 주기로, 다른 기기 칸은 원본의 `/term/pane-info`(`kasa_mcp::pane_info`, 같은 `observe` 를 부른다) 롱폴로 받는다. 포트(lsof)는 뒤에서 읽어 mod 반영을 막지 않는다
- `info_prompts.rs` — Info 카드 아래 「내 프롬프트」. 지금 보는 칸에 묶인 기록(claude transcript·codex rollout, 거울은 원본 `/transcript-raw`)을 처음부터 읽어 `chat_view::parse` 규칙으로 사람이 친 말과 묻는 말에 고른 답만 남기고(`relayed` 로 쪽지·보고를 뺀다), 줄마다 체크칸(`prompt-checks.json`)을 두며, 누르면 대화 보기를 켠 칸은 그 보기 맨 위로(`chat_view_jump`), 풀스크린 claude 는 `prompt_nav` mod 행, 거울은 받은 화면 앵커에서 글로 맞춘 번호로 옮긴다(`place` — 차례를 지키는 최장 일치 + 사이 틈 차례 메우기). mod 없는 풀스크린 codex 와 풀스크린 원본의 거울은 화면의 `❯`·`›` 줄을 보며 휠로 찾아간다(`Seek`, 거울은 원본 화면이 돌아올 때까지 기다리며 성큼 걷는다)
- `git_panel.rs` — 오른쪽 Git 열의 원본 pane·기기 식별, 읽기 요청 순서·문맥 검증, 현재/로컬/원격 브랜치 표시. 원격 조회 계약은 `kasa_mcp::git_panel`의 `kasa.git-panel.v2`이며 원본 기기가 cwd를 확인한다. 일꾼(`spawn_poller`)은 칸이 바뀌면 바로, 작업 트리 파일이 바뀌면(`kasa_mcp::git_watch`, 누가 고쳤든) 합쳐 한 번, 그 밖에는 git 지문이 바뀔 때만 읽고, 다른 기기 칸은 `spawn_remote_watcher` 가 원본의 `/term/gitcol/wait` 에 매달린다. 칸 폴더가 여러 레포를 담은 부모면 그 아래 레포를 최근에 만진 순으로 골라 보이고 머리 메뉴에서 칸마다 고른다(`kasa_mcp::git_panel::panel_view`)
- `nl_command.rs` — 셸 입력줄의 `# 자연어 요청` + 맨 Enter 를 명령 한 줄로 바꿔 **넣기만** 한다(실행은 사람이 Enter). 셸 프롬프트 판정은 OSC 133 `B` 마크 + 마지막 명령 블록이 닫힘(`C` 뒤 `D` 전이면 claude·vim 같은 그 명령이 키를 받는 중이라 안 가로챈다). 모델은 OpenGateway(`themegen::og_key`·`curl_post`), settings `nl_command`·`nl_command_model`. 시험 `KASATERM_AUTONLCMD`
- `input.rs` — `send_bytes`·mouse(`send_mouse_sgr`·호버 전달 `forward_hover`·손가락 커서 판정 `refresh_hover_pointer`)·copy/paste·`handle_wheel`·`forward_key`·claude 상태 글리프
- `markdown.rs` — `md_editor_*`·md 링크/블록
- `testkit.rs` — `schedule_auto*`·`arm_auto*`·`run_pending_auto*` (env 자동테스트 하네스)
- `gpu.rs` — `KASATERM_RENDERER=gpu` 경로. 자체 wgpu Surface + 셀 파이프라인(sugarloaf 경로와 상호배타)
- `auxwin.rs` — 자체 wgpu Surface 기반 **별도 OS 창**(chrome.rs 의 wry webview 패널들과 다름): 문서(마크다운) 창 + 별도창 공용 틀(`AuxWindows`, 이벤트 위임, documents.json)
- `auxterm.rs` — 터미널 pane 을 별도 OS 창으로 뗀다(undock/dock). 창은 `pane_id` 만 들고 셀·PTY 는 App.ws/App.pty 에 그대로 — 그리기는 본창과 같은 `compose_terminal_pane`
- `settings.rs` — 설정 변경 적용·계정 로그인·연결 초안 저장. `native_settings.rs`가 검색·네이티브 폼·입력 상태를 그리고, `settings_room.rs`가 방 수명·검색어·스크롤·키보드 초점을 유지한다.
- `native_settings/devices.rs` — 이 기기/다른 기기 목록과 연결 추가·수정·삭제 폼. 저장 전 초안, 입력 검증, 원래 항목 대조와 메타데이터 보존을 같은 모듈에서 검사한다.
- `settings/phone_login.rs` — 폰 Claude 재로그인의 요청별 상태·CLI 점유·확인·취소. 자격 반영은 `claude_auth/phone_credentials.rs`, 폰 화면은 `mobile/lib/screens/claude_accounts.dart`; 계약은 `docs/mobile-claude-login.md`.
- `socket.rs` — agent-socket ↔ TmuxSession 브리지(`PtyBackend`)·`open_preview`·`pane_record`/`window.json` IO
- `transcript` — Claude/Codex 기록(jsonl) → board 스냅샷 추출. 본체는 `crates/kasa-agents/src/transcript.rs`(main.rs 가 `use kasa_agents::transcript` 로 옛 경로 유지), `transcript/signals.rs`는 정상 종료·중단 구분과 Codex 질문 호출/접수/답변 추적
- `chat_view.rs` — 학생 pane 「대화로 보기」(⋮·Info 학생 줄, PC). 위는 기록에서 편 claude classic 꼴 대화, 아래는 진짜 화면 바닥(스피너·입력칸·상태줄·선택지 창, `bottom_region`)을 붙인 격자(`chat_frame`)를 지어 터미널 합성기에 넘긴다 — 키는 학생 TUI 로 가고 휠·PgUp·접힌 줄 누름·선택 복사만 이 보기가 받는다. 바닥을 못 찾는 판은 진짜 터미널. 자식: `chat_view/classic.rs`(대화 줄 짓기 — classic 렌더러의 요소·글리프·색·들여쓰기 그대로, 덩이마다 칸 폭으로 펴 두고 바뀐 것만 다시) · `chat_view/history.rs`(기록 처음부터 읽기와 무게 걷기, 거울은 원본 `/transcript-raw?history=1`) · `chat_view/parse.rs`(jsonl·rollout → 대화 칸, 폰 `mobile/lib/conversation.dart` 와 같은 규칙, 화면 선택지 판독)
- `bridge.rs` — bg SendMessage 브리지(teammate 플래그 유실된 detach 세션 인박스를 `claude attach` pty 로 직접 주입)
- `stream.rs` — 제거된 데몬 스트림 프로토콜에서 남은 GUI 뷰 타입(`DockedView`/`PaneStatusView`)
- 엔진·협업 크레이트(카사라이트·`kasa tui` 가 git rev 로 함께 쓴다, 설계 `docs/terminal-engine.md`):
  `kasa-screen`(셀·행·ScreenUpdate·ANSI·리플로우·칸 배치 — kasa-bridge 가 재수출) ·
  `kasa-collab`(보드 수집기 `board_service`·tell 장부 `tell_service`·칸 열쇠 `surface_keys`·기계 id `identity`·GUI 없는
  호스트용 tell 전달 `delivery`·claude 훅 설치 `hooks`. 기기 명부·관문·원격 칸은 호스트가 `env::CollabEnv` 로 꽂는다 —
  본판은 `kasa_mcp::install_collab_env`, kasa-mcp 가 옛 경로를 재수출) · `kasa-agents`(대화 기록 읽기) ·
  `kasa-socket::cli`(`kasaterm-cli` 본체 — 바이너리와 `kasa tui` 멀티콜이 부른다)
- `kasa-pty/src/state.rs` — `PtySession` 의 필드 한 곳, 책임별 `impl` 은 `state/` 자식: `lifecycle`(띄우기·입양·닫기) ·
  `reader`(읽기 스레드) · `vt`(파서·답장) · `grid`(격자 → 행·ANSI) · `screen`(읽기·구독) · `viewport`(크기의 주인) ·
  `input`(쓰기 관문) · `blocks`(OSC 133) · `inline`(OSC 1337·kitty) · `process`·`agents`(프로세스·에이전트 감지) ·
  `registry`(세션 등록부). 상세 지도는 `state.rs` 머리말이 정본
- `kasa-mcp/src/http.rs` — 호스트 HTTP 서버. `http/router.rs` 가 모듈마다의 `routes()` 를 한 표로 합치고 공통 레이어
  (`auth`)를 두른다. 자식은 기능별: `socket`(`/term/ws`) · `term_*`(웹 터미널) · `sessions`·`panes`·`pane_read`(세션·칸) ·
  `collab`(보드·tell) · `migrate` · `claude_account` · `schedule`·`tasks` · `files` · `git_api` · `settings` · `phone` ·
  `machine_proxy` 등. 상세 지도는 `http.rs` 머리말이 정본
- `kasa-mcp/src/claude_mod.rs` — claude 안에 실린 연결 mod(`collab-hooks/claude-mods/kasaterm-bridge`)가 loopback HTTP(`/claude-mod/*`)로 알린 사실의 저장소: 칸별 턴·압축·승인·질문·사용량·백그라운드·활동, 승인 요청 브로커(원격 결정·감사 기록), tell 받은편지함, 바뀐 순간의 상태줄(`status_overlay` — 엔진이 다시 그릴 때까지만). 사실은 그 칸에 지금 도는 claude 가 hello 를 보낸 그 pid 일 때만(`live`) 정본이다. 계약 `docs/claude-mod-bridge.md`
- `tell_delivery.rs` — 안전한 tell 의 이 기기 배달: 대기열에서 칸을 골라 신원을 증명한 뒤 빈 입력창을 확인해 붙여넣고 Enter(mod 칸도 같다 — 일하는 칸은 진행 중인 턴에 들어간다). 계약 `docs/tell-protocol.md`
- `agent_state.rs` — pane 상태의 **정본**: `AgentState`(Idle/Working/Compacting/Waiting/Error) 를 훅 턴 경계·기록 턴 경계·attention·명부(`agents --json`)·PTY 박동에서 `resolve` 하는 순수 함수 + `StateHub`(App.collab.hub, PtyBackend 와 Arc 공유, 250ms 메모). 헤더 바·사이드바·미니맵·보드·펫·스프라이트가 전부 이것을 읽는다. mod 칸은 `claude_mod::live` 가 정본이라 `resolve_module` 이 바로 판정하고 화면·기록 턴·명부·Enter 다리는 쉰다(`input.rs` 화면 스캔도 그 칸은 건너뛴다). **화면은 둘째 눈**(`ScreenSigns`: 살아 있는 스피너·승인 위젯·끊김 문구) — 정본(훅·기록·명부)이 없거나 어긋날 때만 판정을 바꾼다(조용한 열린 턴 6초 조기 닫기, 훅 죽었는데 도는 스피너, 훅 없는 하네스, 승인 위젯, 끊김). 화면으로 정본을 **대체**하지 마라
- `board_digest.rs` — 모든 기기 보드 스냅샷(`collab.snapshot`)의 요약: 사람 차례·하는 중·끝 수와 사람을 기다리는 학생. 사이드바 학생 줄·펫 현황판이 쓴다. 보드 판은 걷었다(나쵸 대화·작업은 나쵸 독립 앱)
- `sidebar_pulse.rs` — 사이드바 맨 위 「목록 | 배치도」 전환과 학생 줄 사정(모든 기기 보드의 사람 차례·작업·끝). 수는 `board_digest.rs`(원격 거울 줄 제외)를 백그라운드로 3초마다 읽고 펫 현황판에도 적는다. 방 우클릭 메뉴로 숨기기(settings.json `sidebar_pulse`)
- `version.rs` — 지금 판과 피드 최신판 견주기, 계정 메뉴 판 번호 줄의 업데이트 입구(`update_entry`: Sparkle·WinSparkle·없으면 릴리스 페이지). 여러 기기 패치 릴리스 계획·추적은 앱 밖 `tools/release/`(fastpatch=계획·CLI, backend=실제 단계, nacho=승인 소비·재개, deps=도구 고르기, devices=기기 받기 계획, proc=명령·HTTP 실행기) — `docs/fast-patch-release.md`
- `native_device_account.rs` — 설정 「계정」의 KASA 계정(로그인·Google·GitHub 연결·선택). 자식 `native_account_profile.rs` 는 얼굴 줄(프사·닉네임)·「로그인 방법」·아이디·비밀번호 바꾸기, `native_work_permissions.rs` 는 일 권한. 관문 쪽은 `kasa_mcp` 의 `gateway_profile.rs`(`/relay/profile`)·`device_profile.rs`(이 기기 호출) — `docs/account-oauth.md` 「Profile」
- `remote_approval.rs` — 원격 승인의 데스크톱 화면: 같은 계정 **다른 기기** 학생의 권한 요청을 관문 긴 폴링으로 받아 오른쪽 위 결정 알림([보기][나중에], `collab.toast_action` 센티널)과 원문 시트([허락][거절], Return=거절)로 묻고 관문에 보낸다. 이 기기 요청은 `kasa_mcp::approval_bridge` 가 브로커(`claude_mod`)↔관문을 잇는다 — `docs/remote-approval.md`
- `native_op_approval.rs` — 설정 「계정」의 1Password: 「1Password 연결」(`kasa_mcp::op_connect` 가 op CLI 로 금고·서비스 계정을 만들어 토큰을 키체인에, 계정이 여럿이면 고르기 시트)·토큰 넣기(NSSecureTextField 시트)·지우기, 폰 열쇠 자동 믿기(`op_approval::auto_pick`) 뒤 남은 열쇠만 지문 시트(Return=취소)·거두기, 새 열쇠·믿게 된 폰 알림. 폰이 부르는 `op-link`(socket.rs 설정 동작)는 자동 믿기를 지금 돌린다. 학생 요청 길은 소켓 `op.secret`(socket.rs, 칸은 소켓 상대 pid 로) — `docs/op-faceid-approval.md`
- `update_notice.rs` — 새 판 알림: 업데이터(맥 preview Sparkle 확인·WinSparkle)가 찾은 판을 오른쪽 위 결정 알림으로 세우고, [업데이트]면 끊길 일을 한 번 묻고 설치를 맡긴다, [닫기]면 그 판을 기기 설정에 적는다. Sparkle 쪽 확인·받기·즉시 설치는 `macos_sparkle.rs` — `docs/automatic-preview-updates.md`
- `app_restart.rs` — 앱 재시작 계획용 사실을 GUI 스레드에서 잰다(바쁜 학생·미저장 편집기·자기설치 예정). 계약·도우미는 `kasa_socket::app_restart`, 절차 `docs/app-restart.md`
- `app_update.rs` — 앱 업데이트 창구의 이 기기 쪽: 수락(나쵸 승인·지금 사실), 받기·확인·준비·적용 스레드(한 번에 한 작업), 부팅 표식. 계약·검증·도우미는 `kasa_socket::app_update`, 절차 `docs/app-update.md`
- `mirror_follow.rs` — 원본 격자는 마지막으로 만진 쪽을 따른다(tmux `window-size latest`). 사람 손(키·IME·왼클릭·SGR 누름·확대)이 닿은 칸을 판정해, 거울이면 `remote::touch_source` 로 원본을 그 칸 크기로 잡고 원본이면 `reclaim_viewer_sizes` 로 되찾는다. 만지기 전 거울은 `mirror_view` 가 뷰어 쪽에서 다시 접는다. 호스트가 `viewport_latest` 를 모르면(옛 판) 확대 때만 키우는 옛 규칙(`layout.rs fit_zoomed_mirror`). 폰까지 묶은 칸 크기 규칙은 `docs/webterm-handoff.md` 「원본 크기는 쓰는 쪽이 쥔다」
- `mirror_render.rs` — 거울 칸을 어떤 보기로 그릴지 한 곳의 판정(`MirrorKind` Chat·Shell·Grid). Grid(옛 원본)만 만지면 원본 크기를 빌린다(`touch_surface_size` 가 이걸 본다). 거울은 처음엔 터미널이고 대화·명령 묶음은 ⋮ 로 칸마다 켠다. `docs/mirror-render.md`
- `shell_view.rs` — 거울 셸 칸과 ⋮ 「명령으로 보기」를 켠 이 기기 셸 칸의 「명령 + 결과」 카드(이 기기 칸은 제 HTTP 창구로 읽고, 학생이 뜨면 터미널로 돌아간다). 원본 `/term/blocks`(긴 폴링, `kasa_mcp::shell_blocks`)를 칸마다 일꾼이 받아 합치고, 자식 `shell_view/paint.rs` 가 칸 폭으로 다시 접어 그린다. 키는 원본 PTY 로(터미널과 같다), 클릭·휠은 카드가 먹는다. 셸 통합 없음·대체 화면이면 격자를 줄여 그린다(`layout.rs pane_display_scale`). 원본 쪽 줄 해석은 `kasa_pty::block_lines`
- `prompt_nav.rs` — 풀스크린 claude 칸의 스크롤바·프롬프트 눈금·프롬프트 이동. 칸 안 mod(`collab-hooks/claude-mods/prompt-nav`, claude shim 이 `--plugin-dir` 로 싣는다)가 쓴 `<shim>/prompt-nav/<pane>.json` 을 읽어 그리며, 요청 파일 + 장전 화음(`ctrl+x b`) → `armed` 확인 → `ctrl+↑` 로 mod 에 스크롤을 시킨다. Option·Ctrl+↑↓ 는 claude 로 가 mod 단추가 받는다. classic 렌더러(스크롤백 막대)는 받지 않는다
- `trust_prompt.rs` — claude 폴더 신뢰 화면 자동 통과. 화면 펌프(`pump_pty_screens`)가 후보 pane 을 적고 GUI 틱이 판정한다: claude pane·입력 조용·한글 조합 아님일 때만, 초점이 No 면 아래 화살표 한 번, Yes 면 Enter 한 번(같은 화면에 반복 없음). 신뢰 선탑재는 `kasa_socket::claude_trust`(claude shim 이 `kasaterm-cli claude-trust "$PWD"` 로 부름)
- `own_room.rs` — 이 기기 방 지키기: 현지에 세울 칸(셸·학생·연결 칸)의 기준이 다른 기기 방의 보기 창이면 이 기기 방으로 옮기고(`own_spawn_host` — 섞이면 그 방이 통째로 「이 기기」 절로 넘어가 저쪽 학생이 이쪽 학생처럼 보인다), 자기 칸이 0 이 되면(닫기·이사·미러만 복원) 이 기기 셸 방을 하나 세운다(`keep_own_room`, 1초 틱)
- `lid_away.rs` — 덮으면 이사(맥 전용): 학생이 일하는 동안 `pmset disablesleep` 로 잠자기를 막고, 덮이면 일하는 학생을 예약 이사(`migrate_pane`)한 뒤 재운다. 정책은 순수 함수 `step`, 덮개·외부 모니터·전원·열 판독은 `probe`, 설치물·명령은 `kasa_socket::lid_away` — `docs/lid-away.md`
- `frame_pace.rs` — 몰려오는 그리기(PTY 출력 끝·`RedrawRequested`, `render_frame_paced`)를 화면 주사율 이상은 한 장으로 합친다. 미룬 그림은 `frame-pace` 스레드가 주기 끝에 `UserEvent::Redraw` 로 깨워 반드시 그린다. 사람 동작 직후·캡처·하네스는 `render_frame` 을 그대로 불러 미루지 않는다
- `turn_probe.rs` — 이벤트 루프 한 바퀴의 메인 스레드 시간. 1초 넘는 바퀴는 늘 `[stall]`, `KASATERM_PROFILE` 이면 클릭 바퀴·50ms 넘는 바퀴도, `KASATERM_LAG_PROBE` 면 이벤트가 처리되기까지 줄 선 시간을 `[lag]` 로 찍는다
- `agent_transitions.rs` — 실제 PTY·세션별 알림 관찰자와 상태 전이 순수 함수. 완료는 새 정상 종료 신호가 2초 유지된 경우만, 질문은 호출 ID별 한 번이다. 탭 선택이나 조용한 idle 전이는 완료가 아니다. 데스크톱 알림·토스트·펄스는 `chrome.rs apply_transition_event` 한 곳에서 낸다

새 App 메서드 추가 시 도메인 맞는 모듈에. 다른 모듈/crate root 에서 호출되면 `pub(crate)`. 상세 [[reference_kasaterm_main_module_split]].
