## 커밋·push 는 묻지 말고 한다 

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
KASATERM_WINDOW_FILE=/tmp/<네이름>-window.json \
KASATERM_AUTORESTORE=fresh \
KASATERM_STUDENTS_DIR=/tmp/<네이름>-students \
KASATERM_AUTOQUIT_MS=120000 \
./target/debug/kasaterm > /tmp/<네이름>-app.log 2>&1 &
APP=$!                     # 거둘 때는 이 PID 만: kill $APP
```

- **`KASATERM_SESSION_FILE`·`KASATERM_SETTINGS_FILE` 은 선택이 아니다.** 안 걸면 검증용 앱이 사용자의 `~/.config/kasaterm/session.json` 을 읽고, **실행 중 5초마다 자기 상태로 덮어쓴다**. 설정 파일 쪽은 사용자가 손수 적은 계정 라벨을 하네스 값으로 덮은 전례가 있다. 실데이터가 있어야 화면이 성립하면 원본을 스크래치로 **복사**해 그걸 가리켜라 — 빈 파일을 가리키면 검증하려던 UI 자체가 안 뜬다.
- **`KASATERM_AUTORESTORE=fresh`** — 저장된 세션을 복원하지 않고 빈 창으로 뜬다. 사용자 pane 의 claude 세션과 같은 id 를 다툴 경로가 사라지고, 캡처가 복원 모달만 찍는 일도 없어진다.
- **`KASATERM_STUDENTS_DIR`** 로 그림 폴더를 격리한다 — 업로드·삭제를 검증하면서 사용자가 실제로 쓰는 `~/.config/kasaterm/students/` 를 건드리지 않는다.
- **`KASATERM_WINDOW_FILE`** 도 빠뜨리지 마라. 안 걸면 검증용 앱이 자기 창 크기를 사용자의 `window.json` 에 적어 두고, 다음에 사람이 앱을 열 때 엉뚱한 크기로 뜬다(2026-08-31 실제로 덮었다 — 세션은 격리해 무사했는데 이 하나가 목록에 없었다).
- **격리 창구가 없는 것도 안다.** `session_characters.json`(세션↔학생 바인딩)과 `caps.json` 은 언제나 `~/.config/kasaterm/` 을 쓴다. 다만 전자는 **읽고 병합해** 쓰므로 검증 세션의 학생이 항목으로 늘 뿐 사람의 바인딩을 지우지는 않고, 후자는 같은 터미널이면 같은 값이라 무해하다. 그래도 깨끗하게 두고 싶으면 검증 전에 복사해 두고 끝나면 되돌려라.
- ⚠️ **`KASATERM_SOCKET_PATH` 를 띄울 때도 주고, `kasaterm-cli` 를 부를 때도 줘라.** CLI 는 `$KASATERM_SOCKET_PATH > $CMUX_SOCKET_PATH > /tmp/cmux.sock` 순으로 붙고 **포트는 안 본다** — 리그를 다른 포트로 띄웠어도 CLI 에 이 변수를 안 주면 그 명령이 **사용자 앱으로 간다**(2026-09-03 실측: `split right` 이 사용자 창에 pane 을 만들었고, `send` 가 도는 학생의 입력창에 글자를 밀어넣었다). 앱 부팅 줄과 CLI 호출 양쪽에 같은 경로를 걸어라:
  `export KASATERM_SOCKET_PATH=/tmp/<네이름>/rig.sock` 를 먼저 하고 그 셸에서 둘 다 부르는 것이 가장 안전하다.
- **화면을 판정할 때 `kasaterm-cli capture <pane>` 을 믿지 마라 — 원본 글자판이다.** 그건 `capture_pane_offscreen` 이라 `t.cells` 를 그대로 찍어, 렌더러가 화면을 만들며 하는 일(스크롤백 당김·입력창 붙잡기 같은 재구성)이 **안 보인다**. 렌더 변경을 눈으로 확인하려면 `kasaterm-cli capture --window <path>`(창 프레임) 이나 `KASATERM_AUTOCAPTURE_MS`/`_PATH` 를 써라. 2026-09-03 에 이걸 몰라 "코드가 안 돈다"고 한동안 오판했다(계측을 심어 보니 값은 맞게 돌고 있었다).
- **포트는 지정하지 마라.** 8765 가 사용자 앱 것이므로 새 앱은 알아서 다른 포트를 고른다. 그 번호는 로그에서 읽어라:
  `P=$(grep -o "HTTP MCP on 127.0.0.1:[0-9]*" /tmp/<네이름>-app.log | tail -1 | grep -o "[0-9]*$")`
- **색을 검증한다면 `NO_COLOR` 를 먼저 걷어내라.** claude code 의 셸 도구는 이 변수를 켜 두는데, 거기서 앱을 띄우면 앱을 거쳐 **pane 의 셸까지** 물려간다. 그러면 셸이 스스로 색을 끄고(PowerShell 7 은 `$PSStyle.OutputRendering` 이 `PlainText` 로 내려간다) 화면이 죄다 흑백으로 나온다 — 렌더러는 멀쩡한데 없는 버그를 쫓게 된다(2026-08-31 실제로 한 번 속았다). 의심되면 pane 안에서 `$PSStyle.OutputRendering` 과 `$env:NO_COLOR` 를 찍어 봐라: `Host` 와 빈 값이면 정상이다. claude 마커(`CLAUDE_MARKER_ENV`)와 달리 앱이 지워 주지 않는다 — 사용자가 일부러 켰을 수도 있는 값이라 터미널이 함부로 뺏으면 안 된다.

**거둘 때 — `pkill`·`killall` 을 쓰지 마라. 이름으로 죽이는 명령 자체가 금지다.** 위에서 잡아 둔 `$APP` 만 `kill` 하거나, `KASATERM_AUTOQUIT_MS` 로 스스로 끝나게 둬라. `tmux -C` 와 `/tmp/tmux-501` 도 공유물이라 같은 규칙이다.

**그래도 사용자 세션이 죽었다면 — 되살릴 수 있다.** 대화는 안 잃는다. `~/.config/kasaterm/session.json` 에 pane 마다 `session_id`·`model`·`effort` 가 남아 있으니, 그대로 재조립해 pane 에 다시 보내면 컨텍스트까지 그대로 이어진다(실측으로 9개 복구):

```bash
# session.json 의 leaf 를 훑어 pane 별로 한 줄씩 만든 뒤(내 pane 은 제외),
kasaterm-cli send --surface "%N" "claude --resume <sid> --model '<model>' --effort '<effort>'"$'\n'
```

보내기 전에 `kasaterm-cli peek "%N"` 으로 그 pane 이 셸 프롬프트인지 확인해라 — claude 가 살아 있는 pane 에 보내면 그건 입력창에 글자를 밀어넣는 짓이 된다.

`/tmp/tmux-501` 을 지워야 할 만큼 상태가 꼬였다면, 지우기 전에 다른 pane 이 쓰는 중인지 `kasaterm-cli board` 로 먼저 확인해라.

1. **빌드/실행** — `cargo run -p kasaterm > /tmp/kasaterm-run.log 2>&1 &` (백그라운드)
2. **스크린샷** — `KASATERM_AUTOCAPTURE_MS=8000` 로 N초 후 자동 캡처. 기본 경로 `$TMPDIR/kasaterm.png` (`KASATERM_AUTOCAPTURE_PATH` 로 변경). **판정은 `askimg <png> "질문"` 으로** — `Read` 로 열면 그 이미지가 대화에 박혀 매 요청마다 다시 전송되고 빼는 수단이 없다(2026-09-05: 이 줄을 따른 세션에 47장 8.5MB 가 쌓여 32MB 벽에 걸렸다). macOS `screencapture` 는 권한 막혀 안 됨 — 무조건 자체 캡처.
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

### 다른 기기(맥미니)에서 작업 중이어도 굽는다 — 「맥북에서 구워 주세요」로 넘기지 마라

이사로 미니에 와 있든 처음부터 미니에서 시작했든, 굽기는 네 몫이다. 맥북에는 나쵸 역터널의 관문
(`kasaterm-remote`)이 있고, 그 `bake` 동사가 맥북 레포의 `scripts/remote-bake.sh` 를 돌린다(2026-09-17).

```bash
scripts/macbook-bake.sh status    # 맥북 판: HEAD·미커밋·dist/설치본 시각·펫·서비스
scripts/macbook-bake.sh pet       # pull → 펫만 갈아 끼우고 다시 띄움 — 즉시 반영
scripts/macbook-bake.sh journal   # pull → request-journal(펫의 뇌) 재시작 — 즉시 반영
scripts/macbook-bake.sh app       # pull → build-app.sh (다른 pane 이 Rust 를 만지면 거부한다, --force 로 강행)
```

- **먼저 push 해라.** 맥북은 `git pull --ff-only origin main` 으로 받으므로 안 올린 커밋은 안 구워진다.
- **앱 껐다 켜기(자기설치)는 네가 하지 않는다** — 거노나 나쵸가 한다. `app` 을 구웠으면 「구웠다, 껐다 켜면 반영」까지만 보고한다.
- 셋 중 어느 것이 필요한지는 고친 자리로 정한다: 펫 그림·말풍선·메뉴 → `pet`, 펫의 뇌·나쵸 말투·집컴 전원(`tools/request_journal`) → `journal`, 앱 본체 → `app`.
- 맥북에 있을 때의 펫 전용 길은 `scripts/pet-reload.sh` 다(앱 굽기·앱 재시작 없이 펫만).

그 다음은 **거노가 앱을 껐다 켜면 끝난다.** 종료 시 `arm_self_install`(main.rs)이 도우미를 남겨, 프로세스가 완전히 사라진 뒤 `dist` 를 설치본 자리에 복사한다. 그래서 다음에 켜는 것이 새 바이너리다. 다시 띄워 주지는 않는다 — 끄려고 끈 것일 수도 있어서다. 결과는 `$TMPDIR/kasaterm-selfinstall.log`.

- **`scripts/relaunch.sh` 는 이제 선택**이다(quit→설치→재실행→inode 검증까지 한 번에 하고 싶을 때). ⚠️ **pane 안에서 돌리지 마라** — 앱을 quit 하는 순간 네 PTY 째 죽는다. 거노가 `! scripts/relaunch.sh --no-build` 로 돌린다.
- 자기 설치는 **그 설치본으로 도는 앱**에서만, **빌드 트리의 번들이 더 새로울 때만** 움직인다. `cargo run` 개발 실행과 배포된 남의 머신에서는 아무 일도 안 한다.
- ⚠️ **앱을 claude 세션 안에서 띄우지 마라**(pane 에서 `open`·relaunch). 그 앱이 claude 의 `CLAUDE_CODE_CHILD_SESSION`·`TEAMMATE_MODE`·`SESSION_ID` 를 물려받고, 그러면 그 앱이 낳는 **모든 pane** 의 claude 가 transcript 저장을 끈다. `scrub_inherited_claude_markers`(main.rs, 부팅 첫 줄)가 이제 그걸 지우지만, 애초에 안 물리는 게 낫다.



## 코드 맵 

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
- `auxwin.rs` — 자체 wgpu Surface 기반 **별도 OS 창**(chrome.rs 의 wry webview 패널들과 다름): 문서(마크다운) 창 + 별도창 공용 틀(`AuxWindows`, 이벤트 위임, documents.json)
- `auxterm.rs` — 터미널 pane 을 별도 OS 창으로 뗀다(undock/dock). 창은 `pane_id` 만 들고 셀·PTY 는 App.ws/App.pty 에 그대로 — 그리기는 본창과 같은 `compose_terminal_pane`
- `settings.rs` — 설정 화면(타이틀바 기어 → pane 그리드 대체 전체 뷰, 좌 카테고리 nav + 우 폼)
- `socket.rs` — agent-socket ↔ TmuxSession 브리지(`PtyBackend`)·`open_preview`·`pane_record`/`window.json` IO
- `transcript.rs` — claude-code transcript(jsonl) → board 스냅샷 추출
- `bridge.rs` — bg SendMessage 브리지(teammate 플래그 유실된 detach 세션 인박스를 `claude attach` pty 로 직접 주입)
- `stream.rs` — 제거된 데몬 스트림 프로토콜에서 남은 GUI 뷰 타입(`DockedView`/`PaneStatusView`)
- `agent_state.rs` — pane 상태의 **정본**: `AgentState`(Idle/Working/Compacting/Waiting/Error) 를 훅 턴 경계·기록 턴 경계·attention·명부(`agents --json`)·PTY 박동에서 `resolve` 하는 순수 함수 + `StateHub`(App.collab.hub, PtyBackend 와 Arc 공유, 250ms 메모). 헤더 바·사이드바·미니맵·보드·펫·스프라이트가 전부 이것을 읽는다. **화면 글리프로 상태를 정하지 않는다** — 화면은 스프라이트 자리와 압축 % 장식에만
- `agent_transitions.rs` — 상태 전이 → 알림 이벤트(TurnDone/Waiting/Error/…) 순수 함수. 데스크톱 알림·토스트·펄스는 `chrome.rs apply_transition_event` 한 곳에서 낸다

새 App 메서드 추가 시 도메인 맞는 모듈에. 다른 모듈/crate root 에서 호출되면 `pub(crate)`. 상세 [[reference_kasaterm_main_module_split]].

