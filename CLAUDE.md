# 정체성 — 3층 작업 OS (모노레포)

이 레포는 **"AI 에이전트와 함께 일하는 1인 개발자를 위한, 터미널을 코어로 둔 작업 OS"** 다. 한 모노레포에 3층이 쌓여 있고, **나누지 않는다** — 세 층이 같이 진화 중이라 레포를 찢으면 cross-repo 오버헤드만 폭발한다(1인 개발). 분리는 경계 정리의 *결과*지 시작이 아니다.

| 층 | 코드네임 | 역할 | 상태 |
|---|---|---|---|
| ① 엔진 | **kasaterm** | 터미널 — wgpu 셀 렌더 · PTY · 한글 IME · multipane | 거의 안정기 |
| ② 작업환경 | **kasaspace** | 파일트리 · git 관리 · pane 간 에이전트 연결 | 진행 중 |
| ③ 오케스트레이션 | **blueclaudearchive** | 여러 Claude를 학생처럼 거느리는 하네스 GUI (아로나 모드) | 무게중심 |

**새 기능은 자기 층에 둔다** — 렌더/입력=①, 파일·git·에이전트 배선=②, 학생·재화·협업·경험=③. 무게중심은 ②③로 올라가는 중이라 ①은 "③를 떠받칠 만큼만" 만지면 된다. 레포 분리 트리거(미래): 터미널 엔진을 남이 임베드할 라이브러리로 배포 / ③를 터미널 없이 독립 제품으로 팔 때 — 지금은 둘 다 아님.

---

kasaterm (자체 tmux GUI 터미널 + Claude 런처). 사용자: 거노 (디자이너→개발 입문). 패키지/바이너리명 = **`kasaterm`**.

[!] 작업 시작 전 반드시 [.memory/MEMORY.md](file://./.memory/MEMORY.md) 를 먼저 읽어라 — 거노님 개발 성향·todos·피드백, 그리고 **맨 위 핸드오프 블록**(직전 세션이 어디서 멈췄는지)부터 파악. 렌더러·색·아키텍처 배경, 렌더버그 카탈로그, 코드 수정 주의점은 전부 거기 토픽 파일에 있다.

## 커밋·push 는 묻지 말고 한다 (2026-08-25 지시)

이 레포에서는 **커밋도 push 도 승인을 기다리지 않는다.** 글로벌 규칙의 「push 는 물어볼 것」을
여기서만 뒤집는 것이니, 다른 레포로 이 습관을 들고 가지 마라.

이유는 미반영분이 쌓이는 비용이다. pane 여럿이 같은 워킹트리를 쓰므로, 안 올린 커밋은 다른
기계·다른 세션에서 없는 것과 같고 남이 그 위에 덮어쓴다. 작업 하나가 끝나면 커밋하고 올리는
데까지가 한 단위다.

**그래도 물어봐야 하는 것 셋** — push 가 자유로워진 것과 뜻이 다르다.

- **브랜치 전환·checkout** — 워킹트리를 여럿이 함께 쓴다. 네가 옮기면 남의 pane 이 통째로 딸려간다.
- **force push·history 재작성**(`rebase` 뒤 강제 push, `reset --hard` 후 push) — 일반 push 는
  되돌릴 수 있지만 이건 남의 커밋을 지운다.
- **남의 브랜치·`main` 밖으로 올리는 것** — 올릴 자리가 평소와 다르면 한 번 확인한다.

## 커밋 공동저자 — 나쵸네코를 함께 단다

이 레포의 커밋은 **공동저자 줄을 둘** 단다. 기존 모델 줄은 그대로 두고, 그 아래에

```
Co-Authored-By: NachoNekoBot <322779791+NachoNekoBot@users.noreply.github.com>
```

를 덧붙인다. GitHub 은 공동저자도 기여자로 세는데, 계정에 연결된 이메일일 때만 센다 —
모델 줄의 `noreply@anthropic.com` 은 어느 계정에도 안 걸려 목록에 안 뜬다(2026-08-30 확인).
이 주소는 이 프로젝트의 기계용 계정이라 목록에 뜬다.

모델 줄을 지우지 마라. 코드를 실제로 쓴 것이 무엇인지가 기록에서 사라진다.

**공개 레포다**(`2rami/kasaterm`, public). push 가 자유로워졌다고 담는 내용까지 자유로운 게
아니다 — 키·토큰은 커밋에도 로그에도 넣지 말고, 주석·문서·커밋 메시지의 개인 호칭 금지도 그대로다.

## 자율 테스트 우선

사용자에게 "테스트 해보세요"라고 떠넘기지 말고 **너가 직접** 실행·확인·수정 사이클을 돌려라.

### ⛔ 검증용 앱을 띄우고 거두는 법 — 이 블록을 어기면 사용자 세션이 통째로 날아간다

2026-08-15 실측: 검증용 앱을 띄웠다가 `pkill -f "target/debug/kasaterm"` 으로 거뒀더니 **사용자 창의 pane 9개에서 claude 가 전부 종료**됐다(전부 "Resume this session with:" 를 남기고 셸로 돌아갔다). 원인은 둘 중 하나이고 **둘 다 아래 규칙 하나로 막힌다** — ①`pkill -f` 의 패턴이 의도보다 넓게 잡혔거나 ②새로 띄운 앱이 같은 `session.json` 을 읽어 **같은 세션 id 로 `claude --resume` 을 다시 열어** 먼저 열려 있던 쪽을 밀어냈거나. 사용자는 자기 창이 왜 비었는지 알 방법이 없다.

**띄울 때 — 세 개를 반드시 함께 준다.**

```bash
KASATERM_SESSION_FILE=/tmp/<네이름>-session.json \
KASATERM_SETTINGS_FILE=/tmp/<네이름>-settings.json \
KASATERM_AUTORESTORE=fresh \
KASATERM_STUDENTS_DIR=/tmp/<네이름>-students \
KASATERM_AUTOQUIT_MS=120000 \
./target/debug/kasaterm > /tmp/<네이름>-app.log 2>&1 &
APP=$!                     # 거둘 때는 이 PID 만: kill $APP
```

- **`KASATERM_SESSION_FILE`·`KASATERM_SETTINGS_FILE` 은 선택이 아니다.** 안 걸면 검증용 앱이 사용자의 `~/.config/kasaterm/session.json` 을 읽고, **실행 중 5초마다 자기 상태로 덮어쓴다**. 설정 파일 쪽은 사용자가 손수 적은 계정 라벨을 하네스 값으로 덮은 전례가 있다. 실데이터가 있어야 화면이 성립하면 원본을 스크래치로 **복사**해 그걸 가리켜라 — 빈 파일을 가리키면 검증하려던 UI 자체가 안 뜬다.
- **`KASATERM_AUTORESTORE=fresh`** — 저장된 세션을 복원하지 않고 빈 창으로 뜬다. 사용자 pane 의 claude 세션과 같은 id 를 다툴 경로가 사라지고, 캡처가 복원 모달만 찍는 일도 없어진다.
- **`KASATERM_STUDENTS_DIR`** 로 그림 폴더를 격리한다 — 업로드·삭제를 검증하면서 사용자가 실제로 쓰는 `~/.config/kasaterm/students/` 를 건드리지 않는다.
- **포트는 지정하지 마라.** 8765 가 사용자 앱 것이므로 새 앱은 알아서 다른 포트를 고른다. 그 번호는 로그에서 읽어라:
  `P=$(grep -o "HTTP MCP on 127.0.0.1:[0-9]*" /tmp/<네이름>-app.log | tail -1 | grep -o "[0-9]*$")`

**거둘 때 — `pkill`·`killall` 을 쓰지 마라. 이름으로 죽이는 명령 자체가 금지다.** 위에서 잡아 둔 `$APP` 만 `kill` 하거나, `KASATERM_AUTOQUIT_MS` 로 스스로 끝나게 둬라. `tmux -C` 와 `/tmp/tmux-501` 도 공유물이라 같은 규칙이다.

**그래도 사용자 세션이 죽었다면 — 되살릴 수 있다.** 대화는 안 잃는다. `~/.config/kasaterm/session.json` 에 pane 마다 `session_id`·`model`·`effort` 가 남아 있으니, 그대로 재조립해 pane 에 다시 보내면 컨텍스트까지 그대로 이어진다(실측으로 9개 복구):

```bash
# session.json 의 leaf 를 훑어 pane 별로 한 줄씩 만든 뒤(내 pane 은 제외),
kasaterm-cli send --surface "%N" "claude --resume <sid> --model '<model>' --effort '<effort>'"$'\n'
```

보내기 전에 `kasaterm-cli peek "%N"` 으로 그 pane 이 셸 프롬프트인지 확인해라 — claude 가 살아 있는 pane 에 보내면 그건 입력창에 글자를 밀어넣는 짓이 된다.

`/tmp/tmux-501` 을 지워야 할 만큼 상태가 꼬였다면, 지우기 전에 다른 pane 이 쓰는 중인지 `kasaterm-cli board` 로 먼저 확인해라.

1. **빌드/실행** — `cargo run -p kasaterm > /tmp/kasaterm-run.log 2>&1 &` (백그라운드)
2. **스크린샷** — `KASATERM_AUTOCAPTURE_MS=8000` 로 N초 후 자동 캡처. 기본 경로 `$TMPDIR/kasaterm.png` (`KASATERM_AUTOCAPTURE_PATH` 로 변경). Read tool 로 즉시 보기. macOS `screencapture` 는 권한 막혀 안 됨 — 무조건 자체 캡처.
3. **자동 입력** — `KASATERM_AUTOSEND="claude" KASATERM_AUTOSEND_MS=6000`. send_bytes 직접 주입이라 **IME 조합 경로는 재현 못 함** — 한글 조합 버그는 사용자가 직접 타이핑해야 함 (`KASATERM_IME_DEBUG=1` 로 키 코드포인트 로깅).
4. **체감(스크롤·입력 지연)은 반드시 release** — `cargo run --release -p kasaterm`. 디버그 빌드는 원래 버벅임(debug=느림, release/.app=빠름). 디버그로 "느리다" 판단 금지.
5. **시각 확인** — 스크린샷 본 후 어색한 부분 직접 짚어내고 수정. "어때보여요?" 묻지 말고 너의 판단으로 다음 액션.

## 거노 앱에 반영하기 — 굽고, 껐다 켜면 끝

거노가 쓰는 건 `~/Applications/kasaterm.app` 이고, 그건 `dist/kasaterm.app` 의 **복사본**이다. `cargo build` 도 `build-app.sh` 도 그 복사를 하지 않으니, **빌드했다고 반영된 게 아니다**(설치본 mtime 을 확인하면 바로 보인다).

너는 여기까지만 한다:

```bash
bash scripts/build-app.sh      # dist/kasaterm.app 을 새로 굽는다
```

⚠️ **다른 pane 이 이 레포의 Rust 를 고치는 중이면 이 스크립트는 거부한다** — 굽기는 워킹트리를 통째로 담으므로 남의 반쯤 만든 기능이 함께 들어가고, 운이 나쁘면 컴파일조차 안 된다(2026-08-11 지시). 누가 무엇을 만지는지 이름과 파일이 찍히니 **기다렸다가 다시 부르면 된다.** `--force` 는 그걸 알고도 강행할 때만.

그리고 **네 커밋을 반영하려고 급히 구울 필요가 없다.** 워킹트리는 공유라 나중에 누가 굽든 네 변경이 함께 실린다. 굽기는 "이제 다 됐으니 화면으로 확인하자"는 시점에 한 번이면 충분하다.

그 다음은 **거노가 앱을 껐다 켜면 끝난다.** 종료 시 `arm_self_install`(main.rs)이 도우미를 남겨, 프로세스가 완전히 사라진 뒤 `dist` 를 설치본 자리에 복사한다. 그래서 다음에 켜는 것이 새 바이너리다. 다시 띄워 주지는 않는다 — 끄려고 끈 것일 수도 있어서다. 결과는 `$TMPDIR/kasaterm-selfinstall.log`.

- **`scripts/relaunch.sh` 는 이제 선택**이다(quit→설치→재실행→inode 검증까지 한 번에 하고 싶을 때). ⚠️ **pane 안에서 돌리지 마라** — 앱을 quit 하는 순간 네 PTY 째 죽는다. 거노가 `! scripts/relaunch.sh --no-build` 로 돌린다.
- 자기 설치는 **그 설치본으로 도는 앱**에서만, **빌드 트리의 번들이 더 새로울 때만** 움직인다. `cargo run` 개발 실행과 배포된 남의 머신에서는 아무 일도 안 한다.
- ⚠️ **앱을 claude 세션 안에서 띄우지 마라**(pane 에서 `open`·relaunch). 그 앱이 claude 의 `CLAUDE_CODE_CHILD_SESSION`·`TEAMMATE_MODE`·`SESSION_ID` 를 물려받고, 그러면 그 앱이 낳는 **모든 pane** 의 claude 가 transcript 저장을 끈다. `scrub_inherited_claude_markers`(main.rs, 부팅 첫 줄)가 이제 그걸 지우지만, 애초에 안 물리는 게 낫다.

## Windows 포트 인수인계 (2026-08-14)

`windows-port` 브랜치의 Windows 핵심 기능과 x64 배포 패키징은 완료됐다. 거노가 실제 Windows 앱에서 다음 항목을 확인했다.

- 한글 IME 입력 정상
- PowerShell과 Git Bash에서 Claude 실행 정상
- Claude 실행 시 학생 배정, 캐릭터 표시, 아로나 연동 정상
- 왼쪽 패널의 추가 메뉴 정상
- UTF-8 한글 출력 정상
- 작은 pane의 최초 복원, Alt+Tab 복귀, 불필요한 위쪽 scrollback, 주기적 화면 깨짐 해결

작은 pane 복원 문제의 마지막 원인은 복원 시 모든 PTY를 전체 창 크기로 먼저 생성한 뒤 줄이던 순서였다. `restore_window_layout_at`이 split 트리의 leaf 크기를 먼저 계산해 각 PTY를 실제 pane 크기로 생성한다. Claude pane은 저장된 일반 scrollback을 다시 주입하지 않으며, resize 중 서로 다른 크기의 화면 업데이트를 합치지 않는다. 이 동작을 되돌리면 좁은 pane에서 171열 출력이 40열 화면에 찢어지는 회귀가 다시 생긴다.

Windows 배포는 레포 루트의 PowerShell에서 다음 한 명령으로 만든다.

```powershell
scripts\windows\package.ps1
```

이미 최신 release 바이너리와 `web/arona-ui/dist`가 있으면 조립만 다시 한다.

```powershell
scripts\windows\package.ps1 -SkipBuild -SkipUi
```

산출물은 `dist/kasaterm-vX.Y.Z-windows-x86_64.msi`와 portable ZIP, 각각의 SHA-256 파일이다. 패키지에는 `kasaterm.exe`, `kasaterm-cli.exe`, `WinSparkle.dll`, `arona-ui`, `collab-hooks`가 모두 들어가야 한다. 스크립트가 MSI를 다시 추출해 `characters.json`, `statusline.py`까지 검사하므로 이 검증을 제거하지 말 것. CI의 Windows 릴리스도 같은 스크립트를 호출한다.

현재 `v0.1.19` MSI는 최신 upstream `main` 위로 Windows 커밋을 rebase한 뒤 다시 만들었다. Windows Installer 관리 설치로 75개 파일을 풀어 확인했고, 설치 레이아웃의 앱을 개발용 UI·hook 환경변수 없이 실행해 `http://127.0.0.1:8765/arona-ui/`의 HTTP 200 응답까지 검증했다. `cargo check -p kasaterm`과 agent/shell scrollback 복원 회귀 테스트도 통과했다.

`fork/windows-port`는 최신 upstream보다 8커밋 앞, 0커밋 뒤인 상태로 push했고 upstream PR은 `https://github.com/2rami/kasaterm/pull/2`다. 아직 하지 않은 것은 PR 병합, 버전 태그 생성, GitHub Release 게시다. upstream에는 `v0.1.19` 릴리스가 이미 있으므로 PR 병합 뒤 다음 정식 릴리스는 `v0.1.20` 이상으로 bump한다. 로컬 패키징이나 PR 생성 완료를 정식 릴리스 완료로 오해하지 말 것.

## 함정·배경·수정 주의점은 메모리에

렌더러/색 파이프라인·아키텍처 배경, 렌더버그 디버깅 카탈로그, 코드 수정 주의점(PTY reader **`try_send` 필수** 등), 성능 히스토리는 CLAUDE.md 에 중복해 두지 않는다 — `.memory/MEMORY.md` 토픽 파일에 있다. 1순위 = [[feedback_tmuxify_rendering_pipeline]] (렌더버그 카탈로그 + try_send 트랩). 코드 만지기 전 관련 토픽을 먼저 recall 할 것.

## 코드 맵 (main.rs 12896→5187줄, App 메서드 기능별 모듈 분할 — 2026-06-05 8모듈에서 이후 확장)

`main.rs` = `struct App`/기타 struct·enum 정의 + `new` 생성자 + 자유함수(`file_icon`/`parse_markdown`/`round_rect` 등) + `fn main` + tests 만. **App 메서드는 기능별 모듈로 분리**(전부 `impl App { ... }` 확장 + `use super::*`, 타입·자유함수는 crate root 그대로 참조, cross-module 호출 메서드는 `pub(crate)`):

- `render.rs` — GPU 렌더 패스(`render_frame`/`render_frame_gpu`/`paint_gpu_overlays`/`gpu_overlay_snapshot`). 자유함수는 2026-08-15 에 아래 두 모듈로 분리(13180→8360줄), 옛 `render::…` 경로는 glob 재수출로 유지
- `screenread.rs` — claude/codex **화면 그리드 판독·재작성** 자유함수: 스피너(`find_claude_spinner`)·입력박스(`prompt_box`)·배너/픽커/앵커 감지, 팀메시지(tell/SendMessage) 색칠·프사 배치
- `sprites.rs` — 학생 스프라이트·프사 **에셋 적재와 드로잉**: 번들/override 프레임, idle GIF 캐시, `draw_student_*`
- `handler.rs` — winit `ApplicationHandler`(`window_event`/`user_event`/`new_events`/`resumed`/`exiting`/`about_to_wait`). 소켓 백엔드 위임(`SocketBytes`/`SocketSplit`/`SocketFocus`) 처리·`window.json` 저장(`exiting`)/복원(`resumed`)·header/divider drag·tab-drag move·socket 명령 드레인
- `layout.rs` — pane 조작(`split_active_pane`/`move_pane`/`close_active_pane`/`spawn_new_tab`/`swap_dir`/`focus_dir`/`drop_*`/`divider_at_px`/`toggle_pane_zoom`/`close_tab`) + `resize_backend`/`publish_pty_layout`/좌표·`target_*`
- `session.rs` — `start_pty`(로컬 pane spawn)·`start_socket_pty`(cmux 소켓 + `socket::PtyBackend`)·window/session/cwd·label·tmux/socket·`save_session_state`·`apply_screen_update`/`pump_pty_screens`
- `chrome.rs` — 치수 getter·git col·사이드바/파일트리 토글·패널·줌/폰트·toast/version
- `input.rs` — `send_bytes`·mouse(`send_mouse_sgr`)·copy/paste·`handle_wheel`·`forward_key`·claude 상태 글리프
- `markdown.rs` — `md_editor_*`·md 링크/블록
- `testkit.rs` — `schedule_auto*`·`arm_auto*`·`run_pending_auto*` (env 자동테스트 하네스)
- `gpu.rs` — `KASATERM_RENDERER=gpu` 경로. 자체 wgpu Surface + 셀 파이프라인(sugarloaf 경로와 상호배타)
- `auxwin.rs` — 자체 wgpu Surface 기반 **별도 OS 창**(chrome.rs 의 wry webview 패널들과 다름): 편집기/파일뷰 + pane undock(`AuxWindowKind::Terminal`)
- `settings.rs` — 설정 화면(타이틀바 기어 → pane 그리드 대체 전체 뷰, 좌 카테고리 nav + 우 폼)
- `socket.rs` — agent-socket ↔ TmuxSession 브리지(`PtyBackend`)·`open_preview`·`pane_record`/`window.json` IO
- `transcript.rs` — claude-code transcript(jsonl) → board 스냅샷 추출
- `bridge.rs` — bg SendMessage 브리지(teammate 플래그 유실된 detach 세션 인박스를 `claude attach` pty 로 직접 주입)
- `stream.rs` — 제거된 데몬 스트림 프로토콜에서 남은 GUI 뷰 타입(`DockedView`/`PaneStatusView`)

새 App 메서드 추가 시 도메인 맞는 모듈에. 다른 모듈/crate root 에서 호출되면 `pub(crate)`. 상세 [[reference_kasaterm_main_module_split]].

## 병렬 작업 충돌 회피 (워커 N명 동시 작업)

근본 병목 = `main.rs` 의 `struct App` — 필드가 ①②③ 3층에 걸쳐 평면으로 뭉쳐 있어, 워커 둘이 각각 다른 기능을 만져도 같은 struct 정의(2800~2990행대)를 건드려 git 충돌이 난다. **State-Sandwich 리팩토링 진행 중**(필드를 도메인 sub-struct 로 묶어 main.rs 정의는 묶음 1줄, 정의 본체는 도메인 파일로). 완료 전까지 규칙:

- **`main.rs` struct App 정의(필드 추가/수정)는 한 번에 한 워커만.** 새 필드는 오케스트레이터가 조율해 직렬화. 특히 `git_col_*`(2812행대)·`file_tree_*`(2955행대)는 인접+구조 동일 → 파일트리·git 두 작업은 한 워커가 묶거나 순차로.
- **`chrome.rs`(메서드별 분리), `collab-hooks/`(셸·py), `web/arona-ui/`(TS·React) 는 독립 작업 OK** — 특히 하네스·아로나 UI 는 Rust 코드와 물리 분리라 충돌 0, ③ 작업은 여기서 마음껏.
- **`handler.rs`·`input.rs`·`render.rs`는 거대하지만 메서드 heavy** — 다른 메서드면 충돌 드묾, 중앙 디스패치라 더 쪼개지 말 것. 화면 판독·스프라이트 자유함수는 이미 `screenread.rs`·`sprites.rs` 로 나가 있으니 그 도메인 작업은 거기서(렌더 패스와 충돌 없음).
- 층 매핑: 렌더/입력=① / 파일트리·git=②(아직 app/src) / 하네스·협업=③(분리됨). 충돌 핫스팟은 ②가 app/src 에 박혀서다 → 본격 확장 시 `kasa-workspace`·`kasa-git-badge` crate 추출 ROI 1순위. 상세 [[reference_kasaterm_parallel_work_boundaries]].

## 로컬 PTY 모드 (데몬 완전 제거 — 2026-06-05)

**데몬은 폐기됐다.** GUI(`App`)가 PTY를 직접 소유(`self.pty: HashMap<String, Arc<PtySession>>`)하고 split/focus/close/window가 **전부 로컬 즉시** 처리된다 — RPC 왕복·broadcast·통째 덮어쓰기가 없으니 옛 daemon-authoritative 불변식(부활/증식/drag먹통/덮어쓰기)은 **클래스째 사라졌다**. 구조변경 GUI 액션은 `self.pty_layout`/`ws.panes`를 직접 수정하면 된다. (데몬 detach/reattach·백그라운드 실행이 필요해지면 `archive/daemon-mode` 브랜치에 코드 전부 박제돼 있음.)

핵심 패턴:
- **초기 부팅**: `start_pty`(`session.rs`) = `spawn_session_pane`(로컬 pane spawn) + `start_socket_pty`(cmux 소켓 서버). 데몬 discover/attach 없음.
- **cmux 소켓**: claude tmux shim·kasaterm-cli·pane 협업이 쓰는 소켓 서버는 `socket::PtyBackend`. `App.pty`가 `Arc<Mutex>`가 아니라 별도 스레드서 직접 못 만져 → `send_text`/`split`/`focus`를 `EventLoopProxy`로 GUI 스레드에 위임(`UserEvent::SocketBytes`/`SocketSplit`/`SocketFocus`, `handler.rs` user_event서 처리).
- **resize**: `resize_backend`(`layout.rs`)는 **`leaf_cells` 기반**(leaf id == primary pid 직접 resize). split 직후 새 pane은 `ws.panes`에 PaneState가 아직 없어 ws.panes 순회 방식은 80×24 방치→화면 겹침이었음. 보조탭만 ws.panes로.
- **divider/window 영속**: ratio는 `save_session_state`의 layout(json)에 저장→복원. 윈도우 크기는 `~/.config/kasaterm/window.json`(`handler.rs` exiting/resumed, IO는 `socket.rs`).

**데몬 제거로 빠졌던 것은 2026-07 기준 전부 로컬 재구현 완료** (미구현이라 적혀 있던 옛 문서에 속지 말 것 — 2026-07-27 실측 확인):
- **세션 복원** — `restore_session_state`(session.rs)가 저장된 split 트리를 재생성하고 leaf 별 scrollback 을 심은 뒤, claude 를 돌던 pane 에는 `claude --resume <sid>` 를 큐잉한다. 저장된 sid 의 jsonl 이 실재할 때만 resume(없으면 셸만 뜨던 회귀 차단).
- **working bar status** — `refresh_pane_activity`(input.rs:335)가 `handler.rs` 틱에서 `App.pane_activity` 를 채우고, 렌더가 그걸 읽어 헤더 busy 바 / bg 펄스를 그린다. render.rs 의 "the daemon's transcript watcher" 주석은 낡은 문구고 동작은 로컬이다.
- **파일 미리보기·cross-window drag** — `/open-image`·`/open-markdown` → `SocketOpenPreview` → `open_file`(session.rs:1549)이 확장자로 분기해 **pane 또는 보조 탭**으로 띄운다(별도 OS 창이 필요한 편집기/파일뷰는 auxwin.rs 의 `aux_windows`). 창 간 pane 이동은 `move_pane_cross_window`(layout.rs:1435) + 헤드리스 하네스 `KASATERM_AUTOPANEMOVE`.

**undock(2026-07-24)**: 헤더 pop-out 아이콘 → `undock_pane_terminal`(auxwin.rs, aux wgpu 창이 App.ws 셀 그리드를 뷰·PTY는 App.pty에 잔존이라 세션 무중단), 창 닫기/Cmd+W = dock(활성 pane 오른쪽 split 재삽입). 헤드리스 검증은 `KASATERM_AUTOUNDOCK_MS`(+`_CAP`, testkit.rs). 상세 [[project_kasaterm_session_lifecycle]] · [[reference_kasaterm_daemon_removal]].
