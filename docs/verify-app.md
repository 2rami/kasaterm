# 검증용 앱을 띄우고 거두는 법

프로젝트 지침(`CLAUDE.md`)의 「자율 테스트 우선」이 여기를 가리킨다. 검증용 앱을 띄우기 전에 읽어라.

## ⛔ 이 블록을 어기면 사용자 세션이 통째로 날아간다

2026-08-15 실측: 검증용 앱을 띄웠다가 `pkill -f "target/debug/kasaterm"` 으로 거뒀더니 **사용자 창의 pane 9개에서 claude 가 전부 종료**됐다(전부 "Resume this session with:" 를 남기고 셸로 돌아갔다). 원인은 둘 중 하나이고 **둘 다 아래 규칙 하나로 막힌다** — ①`pkill -f` 의 패턴이 의도보다 넓게 잡혔거나 ②새로 띄운 앱이 같은 `session.json` 을 읽어 **같은 세션 id 로 `claude --resume` 을 다시 열어** 먼저 열려 있던 쪽을 밀어냈거나. 사용자는 자기 창이 왜 비었는지 알 방법이 없다.

**띄울 때 — 세 개를 반드시 함께 준다.**

```bash
KASATERM_SESSION_FILE=/tmp/<네이름>-session.json \
KASATERM_SETTINGS_FILE=/tmp/<네이름>-settings.json \
KASATERM_WINDOW_FILE=/tmp/<네이름>-window.json \
KASATERM_AUTORESTORE=fresh \
KASATERM_STUDENTS_DIR=/tmp/<네이름>-students \
KASATERM_KASANET_KEY=/tmp/<네이름>-kasanet.key \
KASATERM_MACHINES='[]' \
KASATERM_AUTOQUIT_MS=120000 \
./target/debug/kasaterm > /tmp/<네이름>-app.log 2>&1 &
APP=$!                     # 거둘 때는 이 PID 만: kill $APP
```

- **`KASATERM_KASANET_KEY`** — 카사넷 기기 키. 본판 키(`~/.config/kasaterm/kasanet.key`)로 뜨면 다른 기기가 리그와 본판을
  한 기기로 본다. 빠뜨려도 위 격리 env 가 하나라도 걸려 있으면 이번 실행 전용 키로 뜨지만, 리그를 다시 띄워도 같은 id 여야
  하는 검증이면 이 줄로 고정해라. 카사넷을 아예 끄려면 `KASATERM_KASANET=off`.
- **리그 둘 사이 카사넷을 볼 때는 `KASATERM_KASANET_BIND=127.0.0.1:0` 도 걸어라.** 서명 안 된 디버그 앱이 `0.0.0.0` UDP 를
  열면 macOS 방화벽이 사람 화면에 묻기 창을 띄우고, 답하기 전까지 들어오는 UDP 를 막아 직통 대신 중계만 잡힌다
  (2026-09-29). 루프백에만 열면 창도 안 뜨고 직통이 선다.

- **`KASATERM_SESSION_FILE`·`KASATERM_SETTINGS_FILE` 은 선택이 아니다.** 안 걸면 검증용 앱이 사용자의 `~/.config/kasaterm/session.json` 을 읽고, **실행 중 5초마다 자기 상태로 덮어쓴다**. 설정 파일 쪽은 사용자가 손수 적은 계정 라벨을 하네스 값으로 덮은 전례가 있다. 실데이터가 있어야 화면이 성립하면 원본을 스크래치로 **복사**해 그걸 가리켜라 — 빈 파일을 가리키면 검증하려던 UI 자체가 안 뜬다.
- **`KASATERM_AUTORESTORE=fresh`** — 저장된 세션을 복원하지 않고 빈 창으로 뜬다. 사용자 pane 의 claude 세션과 같은 id 를 다툴 경로가 사라지고, 캡처가 복원 모달만 찍는 일도 없어진다.
- **`KASATERM_STUDENTS_DIR`** 로 그림 폴더를 격리한다 — 업로드·삭제를 검증하면서 사용자가 실제로 쓰는 `~/.config/kasaterm/students/` 를 건드리지 않는다.
- **`KASATERM_PET_DIR=/tmp/<검증>/pet`** 은 상태 피드(`board.json`·`overlay.json`·`inbox.jsonl`)와 읽는 앱의 `pet.pid` 자리를 가른다.
- **`KASATERM_WINDOW_FILE`** 도 빠뜨리지 마라. 안 걸면 검증용 앱이 자기 창 크기를 사용자의 `window.json` 에 적어 두고, 다음에 사람이 앱을 열 때 엉뚱한 크기로 뜬다(2026-08-31 실제로 덮었다 — 세션은 격리해 무사했는데 이 하나가 목록에 없었다).
- **`KASATERM_DEVICE_FILE=/tmp/<네이름>/device.json`** — 관문 계정 로그인 파일. 안 걸면 리그가 사용자의 `device.json` 을 읽어
  설정 「계정」이 사용자 계정으로 로그인된 화면으로 뜬다(2026-10-02). 로그인 전 화면을 보려면 반드시 건다.
- **`KASATERM_MACHINES='[]'`** — 사용자 명부(`machines.json`)를 안 읽게 한다. 안 걸면 리그가 명부의 기기마다 ssh 터널을 열고,
  저쪽에서 이쪽으로 오는 되돌아오는 길(`-R`)을 **사용자 앱과 같은 원격 포트**로 열어 다툰다(2026-09-29 실측: 한 기기는 거부됐지만
  다른 기기는 리그 쪽이 열렸다). 기기 칸(직통·빌드 다름)은 그래서 리그로 못 보고, 실기기에서 확인한다.
- **`KASATERM_COLLAB_ROOT`** 로 네이티브 앱의 `session_characters.json`(세션↔학생 바인딩)·bind 마커·`agent-roster/` 조회를 가른다. 안 걸면 본판 폴더를 **읽고 병합해** 쓰므로 검증 세션의 학생이 항목으로 늘 뿐 사람의 바인딩을 지우지는 않지만, 같은 cwd 의 같은 pane 번호(`%2`)가 본판 세션에 결합해 글리프가 남의 것을 보는 일이 생긴다. **수동 학생 선택 파일과 원본 셸 훅의 쓰기까지 격리하는 설정은 아니다** — 아래 「학생·대화 픽스처」를 따른다.
- ⚠️ **`KASATERM_SOCKET_PATH` 를 띄울 때도 주고, `kasaterm-cli` 를 부를 때도 줘라.** CLI 는 `$KASATERM_SOCKET_PATH > $CMUX_SOCKET_PATH > /tmp/cmux.sock` 순으로 붙고 **포트는 안 본다** — 리그를 다른 포트로 띄웠어도 CLI 에 이 변수를 안 주면 그 명령이 **사용자 앱으로 간다**(2026-09-03 실측: `split right` 이 사용자 창에 pane 을 만들었고, `send` 가 도는 학생의 입력창에 글자를 밀어넣었다). 앱 부팅 줄과 CLI 호출 양쪽에 같은 경로를 걸어라:
  `export KASATERM_SOCKET_PATH=/tmp/<네이름>/rig.sock` 를 먼저 하고 그 셸에서 둘 다 부르는 것이 가장 안전하다.
  ⚠️ **Windows 는 이 경로의 마지막 조각만 named pipe 이름(`\\.\pipe\rig.sock`)으로 쓴다** — 폴더가 달라도 이름이 같으면
  옆 학생 리그와 한 파이프다. 이름이 이미 있으면 앱은 `kasaterm-<pid>.sock` 으로 물러나고, CLI 는 남의 리그에 붙는다
  (2026-10-23: `rig.sock` 으로 친 한 줄이 옆 리그 칸에 들어갔다). 이름에 자기 이름을 넣고(`win-migrate-a.sock`),
  `[System.IO.Directory]::GetFiles("\\.\pipe\")` 로 내 리그가 그 이름을 잡았는지 확인한다.
- Windows 리그의 설정 파일은 BOM 없이 써라 — Windows PowerShell 5.1 `Set-Content -Encoding utf8` 은 BOM 을 붙이고, 앱은
  그 파일을 못 읽어 기본값(셸·셰임)으로 뜬 뒤 덮어쓴다. `[IO.File]::WriteAllText` 를 쓴다.
- **칸 안에서 claude·codex 를 따로 띄워 화면을 재 볼 때(격리 tmux 등)는 `env -i` 로 칸 환경을 걷어라.** 물려받은
  `KASATERM_PANE_ID` 로 그 실행이 내 칸 이름으로 학생·대화를 묻는다. 앱은 칸 터미널 밖 실행에 자리를 주지 않고 앉은
  학생을 빌려 줄 뿐이지만(`agent_identity.rs`), 그 실행의 훅이 알리는 대화 번호는 여전히 내 칸으로 간다. 2026-10-14 에는
  tmux 표본 넷이 칸 자리를 차례로 받았다 놓아 그 칸 학생 이름표가 비고 미니맵이 로고로 떨어졌다. 칸 tty 를 따지는 판정은
  macOS 만이다. 예:
  `env -i HOME="$HOME" USER="$USER" PATH=… TERM=xterm-256color KASATERM_SOCKET_PATH=/tmp/<네이름>/none.sock tmux -L <소켓> -f /dev/null new-session -d '<명령>'`
- 격리한 두 앱 사이의 실제 보드 주소·조건부 닫기를 검증할 때만 보는 쪽에 `KASATERM_TEST_BOARD_REMOTE=1`을 더한다. `KASATERM_MACHINES`는 직접 띄운 원본 리그의 루프백 주소만 명시하며, 본판 명부나 기기를 쓰지 않는다. 기본 격리 실행은 원격 보드 수집을 계속 끈다.
- 실제 Claude CLI의 `--restricted`·`--settings`는 대화형 `/config` 저장 위치를 격리하지 않는다. 별도 설정 프로필을 검증한 경우에만 옵션을 변경하고, 기본 인증 프로필을 쓰는 출력 시험에서는 설정 선택창을 보기만 한다.
- **화면을 판정할 때 `kasaterm-cli capture <pane>` 을 믿지 마라 — 원본 글자판이다.** 지금 보이는 방의 칸이면 창 프레임을 잘라 주지만, 안 보이는 방의 칸이면 `capture_pane_offscreen` 이라 `t.cells` 를 그대로 찍어(응답에 `"offscreen":true`), 렌더러가 화면을 만들며 하는 일(스크롤백 당김·입력창 붙잡기 같은 재구성)이 **안 보인다**. 칸 안 그림(kitty·OSC 1337)은 거기도 그린다 — 예전엔 빠져서 학생 얼굴 자리가 빈칸으로 찍혀 「그 칸만 그림이 안 나온다」로 오판했다(2026-10-06). 렌더 변경을 눈으로 확인하려면 `kasaterm-cli capture --window <path>`(창 프레임) 이나 `KASATERM_AUTOCAPTURE_MS`/`_PATH` 를 써라. 2026-09-03 에 이걸 몰라 "코드가 안 돈다"고 한동안 오판했다(계측을 심어 보니 값은 맞게 돌고 있었다).
- **리그 창이 다른 창에 가려져 있으면 `peek`·`capture` 가 몇 초 전 화면을 준다.** 격자는 프레임이 돌 때 PTY 에서 옮겨지는데, 가려진 창은
  다음 입력이 올 때까지 프레임을 거의 안 돈다(2026-10-02: Claude Code 는 클릭에 18ms 만에 다시 그렸는데 1.5초 뒤 `peek` 은 옛 화면이었다).
  앱이 무엇을 받고 무엇을 그렸는지는 pane 명령 앞에 입출력을 적는 pty 중계를 끼워 보고, 화면 판정은 다음 입력 뒤에 한다.
- **알림은 안 뜬다.** 번들 없는 맥 리그는 데스크톱 알림·자체 배너를 내지 않는다(사람 화면에 리그 배너가 뜨던 것, 2026-10-07). 배너를 검증할 때만 `KASATERM_NOTIFY_BANNER=1`.
  Windows 는 검증 실행·격리 env·cargo `target` 폴더의 exe 가 같은 조용한 실행이다. 토스트를 검증할 때만
  `KASATERM_NOTIFY_TOAST=1` — 설치본과 갈린 리그 신원(`com.kasa.kasaterm.rig`, 꺼진 앱을 띄우는 `LocalServer32` 없음)으로 낸다.
  끝나면 `HKCU\Software\Classes\AppUserModelId\com.kasa.kasaterm.rig` 를 지운다. 클릭은 사람 화면의 토스트를 누르지 말고
  등록된 활성화기(`{d79515d3-0e66-43c1-b6a6-aff9f85d594d}`)를 COM 으로 만들어 `Activate(aumid, launch 인자)` 로 흉내 낸다.
- **`TMPDIR` 도 리그 폴더로.** 앱은 stderr 가 tty 가 아니면 `$TMPDIR/kasaterm-app.log` 로 돌린다 — 안 가르면 리그 로그가
  사용자 앱 로그에 섞이고, `> app.log` 로 받은 파일은 비어 있다(2026-09-25). 소켓 경로는 104바이트 한도라 스크래치 폴더
  깊숙이 두면 `path must be shorter than SUN_LEN` 으로 소켓 서버가 안 선다 — `/tmp/<네이름>/` 처럼 짧게.
  Windows 는 `TMP`·`TEMP` 를 바꿔도 Git Bash 의 `/tmp` 가 사용자 TEMP 에 고정이라, 리그 칸에서 claude 를 띄우면 묶기 훅
  표식 `/tmp/kasaterm-bound-_<칸 번호>` 가 본판 같은 번호 칸의 표식을 덮는다(2026-10-23). 리그 뒤 그 파일을 지워라.
- **포트는 지정하지 마라.** 8765 가 사용자 앱 것이므로 새 앱은 알아서 다른 포트를 고른다. 그 번호는 로그에서 읽어라:
  `P=$(grep -o "HTTP on 127.0.0.1:[0-9]*" /tmp/<네이름>-app.log | tail -1 | grep -o "[0-9]*$")`
- **색을 검증한다면 `NO_COLOR` 를 먼저 걷어내라.** claude code 의 셸 도구는 이 변수를 켜 두는데, 거기서 앱을 띄우면 앱을 거쳐 **pane 의 셸까지** 물려간다. 그러면 셸이 스스로 색을 끄고(PowerShell 7 은 `$PSStyle.OutputRendering` 이 `PlainText` 로 내려간다) 화면이 죄다 흑백으로 나온다 — 렌더러는 멀쩡한데 없는 버그를 쫓게 된다(2026-08-31 실제로 한 번 속았다). 의심되면 pane 안에서 `$PSStyle.OutputRendering` 과 `$env:NO_COLOR` 를 찍어 봐라: `Host` 와 빈 값이면 정상이다. claude 마커(`CLAUDE_MARKER_ENV`)와 달리 앱이 지워 주지 않는다 — 사용자가 일부러 켰을 수도 있는 값이라 터미널이 함부로 뺏으면 안 된다.

**거둘 때 — `pkill`·`killall` 을 쓰지 마라. 이름으로 죽이는 명령 자체가 금지다.** 위에서 잡아 둔 `$APP` 만 `kill` 하거나, `KASATERM_AUTOQUIT_MS` 로 스스로 끝나게 둬라. `tmux -C` 와 `/tmp/tmux-501` 도 공유물이라 같은 규칙이다.

**그래도 사용자 세션이 죽었다면 — 되살릴 수 있다.** 대화는 안 잃는다. `~/.config/kasaterm/session.json` 에 pane 마다 `session_id`·`model`·`effort` 가 남아 있으니, 그대로 재조립해 pane 에 다시 보내면 컨텍스트까지 그대로 이어진다(실측으로 9개 복구):

```bash
# session.json 의 leaf 를 훑어 pane 별로 한 줄씩 만든 뒤(내 pane 은 제외),
kasaterm-cli tell --raw "%N" "claude --resume <sid> --model '<model>' --effort '<effort>'"$'\n'
```

보내기 전에 `kasaterm-cli peek "%N"` 으로 그 pane 이 셸 프롬프트인지 확인해라 — claude 가 살아 있는 pane 에 보내면 그건 입력창에 글자를 밀어넣는 짓이 된다.

`/tmp/tmux-501` 을 지워야 할 만큼 상태가 꼬였다면, 지우기 전에 다른 pane 이 쓰는 중인지 `kasaterm-cli board` 로 먼저 확인해라.

1. **빌드/실행** — `cargo run -p kasaterm > /tmp/kasaterm-run.log 2>&1 &` (백그라운드)
2. **누르기** — `KASATERM_AUTOCLICKS="x,y;x,y"`(논리 좌표) · `_MS`(첫 클릭, 기본 6000) · `_GAP_MS`(사이, 기본 1200). `text:<글>` 항목은
   pane 격자에서 그 글자를 찾아 사람 손처럼 옮겨(호버) 누르고 90ms 뒤에 뗀다 — 창 배율이 실행마다 달라 px 를 미리 못 정하는 TUI 단추용
   (`KASATERM_AUTOCLICKS="text:학생 현황 ];text:메모 ]"`). `native:<글>` 은 같은 자리를 macOS 에서 **AppKit NSEvent 로 winit 뷰에**
   넘긴다 — winit 이 누름·뗌마다 먼저 내는 CursorMoved(그래서 뗌 앞에 같은 칸 끌기 `32` 가 붙는다)와 수정키 갱신까지 사람 손과
   같고, 창 서버를 안 거쳐 사람 화면의 초점은 안 뺏는다. `x,y`·`text:` 는 `window_event` 를 직접 불러 그 단계를 건너뛴다.
   `opt:<글>`·`shift:<글>` 은 그 수정키를 쥔 `native:`, `drag:<글>><글>` 은 앞 글에서 눌러 뒷 글까지 끌고 뗀다(뒷 글이
   `top`·`bottom` 이면 대화 칸 본문 밖으로 끌어 0.9초 쥔다). 대화로 보는 칸은 원본 격자가 아니라 그 보기가 그린 격자에서 찾는다.
   글 대신 `@x,y`(논리 좌표)를 쓰면 그 자리에서 잡거나 놓는다(사이드바 방 카드에 놓기). `native:@x,y` 도 같아서, 올려야 생기는
   단추(파일트리 폴더 줄의 「셸을 이 폴더로」)를 사람 손처럼 옮겨 누른다. `handle:%N` 은 그 칸의 ⋮ 손잡이라
   머리 없는 칸을 통째로 끈다(`drag:handle:%1>@700,600`).
   `right:@x,y`는 같은 AppKit 경로로 우클릭 메뉴를 연다. 방 이름 변경 등 메뉴 진입은 실제 창 캡처에서 잰 좌표로 검사한다.
   **키**는 `KASATERM_AUTOKEYS="9000:text:hello;9500:alt+left;10000:cmd+c"`(앱 시작 기준 ms, `shift`·`ctrl`·`alt`·`cmd` +
   `left`·`right`·`up`·`down`·`home`·`end`·`backspace`·`enter`·`esc`·글자) — 같은 AppKit 길로 `forward_key` 까지 탄다.
   `tell --raw` 는 PTY 에 바로 써서 키 처리를 못 본다. 입력칸 ⌘C·⌘V 는 검증 실행에서 사람 클립보드 대신
   `KASATERM_TEST_COPY_PATH` 파일에 쓰고 `KASATERM_TEST_PASTE_TEXT` 를 붙인다(`lineedit::SystemClip`). ⚠️ 한가한 리그는 프레임을 드물게 돌아 예약 시각이 몇 초씩 밀리고 뒤
   걸음끼리 한 틱에 몰린다 — 옆에서 `kasaterm-cli peek` 을 0.25초마다 불러 깨워 두고, 걸음 사이를 넉넉히(수 초) 띄워라.
   ⚠️ `text:`·`native:` 의 px 는 클릭 판정 함수(`px_to_pane_cell`)로 거꾸로 구한 것이라 **그린 자리와 판정 자리가 어긋나는 버그는
   원리상 못 잡는다** — 그건 캡처에서 글자 위치를 재서 `x,y` 로 눌러 확인한다. 그리고 **사람이 누르는 배치를 그대로 세워라**:
   좁은 리그에선 Claude Code 옆 창이 아래에 붙어 단추가 pane 위쪽에 안 오니, pane 위 34px 를 헤더로 잡아먹던 판정(2026-10-06)이
   통과로 보였다 — 칸 폭 115 이상이면 오른쪽에 붙는다(`KASATERM_WINDOW_SIZE=2300,900` 두 칸). 팝오버·묶음 알약을
   연 채로 `kasaterm-cli capture --window` 로 찍는다. 좌표는 창의 논리 크기(물리 폭 ÷ 로그의 `scale=`) 기준이다 — 캡처 PNG 는 줄여 저장되니 비율로 옮긴다. 표시용 픽스처(`KASATERM_AUTOINFO=execution…`)는
   검증 실행(`KASATERM_WINDOW_SIZE="w,h"`)에서만 선다.
3. **스크린샷** — `KASATERM_AUTOCAPTURE_MS=8000` 로 N초 후 자동 캡처. 기본 경로 `$TMPDIR/kasaterm.png` (`KASATERM_AUTOCAPTURE_PATH` 로 변경). **판정은 `Read` 로 직접 본다** — 먼저 `sips -s format jpeg -s formatOptions 60 -Z 1200 <png> --out <jpg>` 로 줄여서 연다. 이미지는 대화에 박혀 매 요청마다 다시 전송되고 빼는 수단이 없다(2026-09-05: 한 세션에 47장 8.5MB 가 쌓여 32MB 벽에 걸렸다) — 볼 것을 정한 뒤 한 장씩. macOS `screencapture` 는 권한 막혀 안 됨 — 무조건 자체 캡처.
4. **자동 입력** — `KASATERM_AUTOSEND="claude" KASATERM_AUTOSEND_MS=6000`. send_bytes 직접 주입이라 **IME 조합 경로는 재현 못 함** — 한글 조합 버그는 사용자가 직접 타이핑해야 함 (`KASATERM_IME_DEBUG=1` 로 키 코드포인트 로깅).
5. **체감(스크롤·입력 지연)은 반드시 release** — `cargo run --release -p kasaterm`. 디버그 빌드는 원래 버벅임(debug=느림, release/.app=빠름). 디버그로 "느리다" 판단 금지.
6. **시각 확인** — 스크린샷 본 후 어색한 부분 직접 짚어내고 수정. "어때보여요?" 묻지 말고 너의 판단으로 다음 액션.

## 학생·대화 픽스처

**리그에서 학생 얼굴을 붙이려고 `/repersona`·`kasaterm-cli repersona`·학생 이름 런처를 호출하지 마라.**
2026-10-16 검증에서 `/repersona?sid=<검증 UUID>`가 본판 `~/.config/kasaterm/manual_characters.json`에
검증 UUID를 추가했다. `character.rs::manual_pick_path`는 HOME 아래 경로를 직접 쓰며
`KASATERM_COLLAB_ROOT`·`KASATERM_STUDENTS_DIR`·`KASATERM_SETTINGS_FILE`을 읽지 않는다.
세션·소켓·협업 루트가 전부 격리됐어도 이 쓰기는 본판으로 간다. `sid`만 빼도 안전하지 않다 —
GUI의 `repersona_pane`이 그 칸의 기존 sid를 수동 선택으로 기록한다. 학생 이름 런처도 내부에서 `/repersona`를 부른다.

가짜 하네스의 이름·그림·대화 상태는 다음 경로로 맞춘다. 실제 사용자 UUID나 프로세스는 빌리지 않는다.

1. 위 격리 부팅 환경을 유지하고 그림은 리그의 `KASATERM_STUDENTS_DIR`에 복사한다. 가짜 하네스를 **리그 칸 안에서** 실행하고
   그 자식의 실제 PID와 새 검증 UUID를 기록한다.
2. 그 리그 소켓에 짝인 `.mcp_port`에서 읽은 포트로 `POST /agent-identity`를 보낸다.
   쿼리는 `surface=<리그 칸>`, `sid=<새 검증 UUID>`, `character=<픽스처 학생>`, `pid=<가짜 하네스 PID>`다.
   **`resumed=1`은 넣지 않는다.** 다른 기기에서 이어 열 학생을 찾는 분기는 수동 선택 표식을 기록할 수 있다.
   새 대화 경로의 `prepare_agent_identity`는 수동 선택 표식을 쓰지 않고, 세션 바인딩을 격리 협업 루트에 쓴다.
   새 루트의 바인딩 위치는 `<KASATERM_COLLAB_ROOT>/sessions/session_characters.json`이다.
3. 화면 외에 보드·미러의 대화 신원도 필요하면 같은 UUID의 합성 transcript를 리그 폴더에 만들고,
   **같은 리그 소켓·협업 루트·칸 id를 준 CLI**로 `kasaterm-cli bind-transcript <합성 transcript 경로>`를 직접 부른다.
   Codex 파일명은 `rollout-<시각>-<검증 UUID>.jsonl` 형식이다. `/repersona`로 보완하지 않는다.
4. 응답 이름과 `/term/panes`의 신원을 확인하고, 검증 전후 본판 `manual_characters.json`이 바뀌지 않았는지 확인한다.
   이름이 안 보이면 원본의 바인딩·하네스 판정을 먼저 조사한다. 미러에 따로 학생을 재배정하지 않는다.

일반 `claude`·`codex` 래퍼의 정체성 요청도 `/agent-identity`를 쓰지만, 이것이 실행되는 모든 훅의 격리를 보증하지는 않는다.
특히 저장소 원본 `collab-hooks/kasaterm-bind-transcript.sh`는 HOME의 `agent-roster/`와 `/tmp`의 bind 마커에 직접 쓴다.
위 가짜 하네스 픽스처에는 그 셸 훅 전체를 실행하지 말고 3번의 CLI 바인딩만 쓴다.

## 배경 방 배치 동기화 리그

사용량의 개별 셸 종료는 격리 부팅에 `KASATERM_AUTOUSAGE_LIFECYCLE=1`을 더한다. 실제 셸을 새로 만들어
숨긴 기본 탭·보조 탭 각각의 종료와 복원, 문서를 함께 보관한 칸의 「열기」, 마지막 탭의 명시 종료를 확인한다.
`[usage-lifecycle] PASS case=0..3` 네 줄이 모두 필요하다. 세션 Arc를 붙든 채 실행이 끝나는지와 옛 EOF가
되살린 셸을 지우지 않는지도 대조한다. 사용량 상세 화면은 `KASATERM_AUTOPOPOVER=usage-cpu-owned`로
이 리그 소유 셸만 펼칠 수 있다. `devices`는 합쳐진 기기·업데이트 판을 연다.

배경 방 분할은 위 격리 부팅에 `KASATERM_AUTOWINDOWS=1 KASATERM_AUTOWINDOWS_MS=1000
KASATERM_AUTOFOREIGNSPLIT_MS=4000`을 더해 검사한다. `KASATERM_AUTOFOREIGNSPLIT=fleet`은 두 칸 일괄 배치다.
`[autoforeignsplit] PASS`는 트리뿐 아니라 공개 pane 소속·방 배치·변경 알림과 기존 방 초점·줌 보존을 확인한다.
원본 방을 전환하기 전에 `/term/panes`·`/windows`와 실제 미러 목록도 대조한다. 방을 한 번 열면 publication 누락이 가려진다.
미러 리그는 `KASATERM_MACHINES`에 직접 띄운 원본의 루프백 주소만 넣고,
`KASATERM_TEST_MIRROR_LABEL=<원본 이름> KASATERM_TEST_MIRROR_PANE=<배경 방 칸> KASATERM_TEST_MIRROR_ROOM=1`로
방 전체를 연다. 기기 목록 수집을 위해 사이드바를 켜 둔다(`sidebar_visible=false`로 부팅했다면
`KASATERM_AUTOTOGGLE_SIDEBAR_MS=500`). `KASATERM_TEST_MIRROR_ROOM`을 빼면 기존 단일 칸 미러 검증이다.

같은 원본 칸을 여러 방에서 비추는 구성원 동기는 `KASATERM_TEST_MIRROR_EXTRA_PANE=<다른 원본 방의 기준 칸>`으로
두 번째 보기 방도 연다. 원본에서 첫 방의 칸 하나를 두 번째 방으로 옮긴 뒤, 도착 방에 새 거울이 생겨도 출발 방의
옛 거울은 5초 유예 뒤 사라지는지 확인한다. 원본 칸·대화와 도착 방의 거울은 살아 있어야 한다.

미러 닫기는 위 리그에 `KASATERM_TEST_MIRROR_CLOSE_MS=4500`, `KASATERM_TEST_MIRROR_CLOSE_KIND=tab|pane|session|remote-room|remote-pane`을 더한다.
macOS 시트는 `KASATERM_UPDATE_RIG_SHOTS=<기존 폴더>`로 자기 앱의 시트만 캡처한다. `KASATERM_UPDATE_RIG_PRESS_MS=1800`은 기본 단추(취소),
`KASATERM_UPDATE_RIG_BUTTON=닫기`를 더하면 그 제목의 실제 NSButton을 누른다(격리 실행에서만). `KASATERM_TEST_MD_SCRIPT`의 `reopen:`은 되살리기 경로를 실행한다.
GPU 확인 카드는 `KASATERM_TEST_CLOSE_FALLBACK=1`로 별도 검사한다. 원본 첫 탭·다중 탭 칸·방 전체, 취소·원본 완료 응답·신원 변경 거절,
혼합 로컬/미러 칸과 대기 중 새 탭 추가, 유예 시간 전후 되살리기를 모두 확인한다. 본판 세션으로 닫기·되살리기를 시험하지 않는다.

## 기기 간 드래그 이사 리그

위 격리 환경의 앱 둘을 루프백 기기로 연결하고 `KASATERM_SHELL=/bin/zsh`로 시작한다. 가짜 Claude·Codex는 위 학생 픽스처 규약을 지키고, `surface.turn`의 `end`와 실제 입력창(Claude 보더·❯, Codex 명시 배경·›, bracketed paste·입력 커서)을 출력한다. SID 바인딩만 먼저 알리고 입력창은 늦게 그리는 경우도 만든다. 이 지연 중에는 `/transfer/launch`·이사 결과가 성공하면 안 된다.

`/transfer/spawn`이 돌려준 `SessionIdentity`와 `input_revision`으로 `/transfer/launch`를 부른다. 빈 셸, 예약 후 초안 입력, 세대 교체, CLI 시작 직후 종료, 늦은 입력창을 확인한다. 전체 이사는 종료 시 마지막 JSONL 행을 flush하는 픽스처로 검사한다. 출발·도착 SID·방, 파일 마지막 행, 이사하지 않은 형제 탭의 동일성과 생존을 대조한다. 첫 탭·보조 탭 각각 이동과 도착 재개 중 출발 칸 닫기를 포함한다. 실제 CLI 재개 시간과 가짜 하네스 시간은 별도로 기록한다.

Windows 끼리(같은 PC 두 리그)는 `KASATERM_MACHINE_ID` 를 리그마다 다르게 주고, 출발 리그 명부(`KASATERM_MACHINES`)에 도착 리그의 루프백 주소와 `roots` 를 적는다. 도착 폴더는 이미 신뢰한 폴더의 하위로 고른다(새 폴더면 claude 가 신뢰 확인 창에서 멈춘다). 리그 셰임 폴더(`<리그 TEMP>\kasaterm-shim-<pid>\claude.cmd`)를 `--setting-sources project --strict-mcp-config --tools ""` 로 진짜 claude 를 부르는 래퍼로 덮으면 원본 훅·사용자 플러그인 없이 재개 줄이 그 래퍼를 탄다. 셰임 훅이 하던 신원 알리기·기록 묶기는 위 픽스처 2·3번(`/agent-identity`, `bind-transcript`)으로 대신한다. 훅이 없어도 끝난 턴의 기록을 묶으면 상태는 「쉼」이 된다(2026-10-23 꼬리 창 수정 뒤).

훅까지 재야 하면 래퍼에 `--settings <리그 셰임>\claude-hooks-settings.json` 을 더한다(사용자 설정은 여전히 `--setting-sources project` 로 뺀다). 이때 훅은 리그 밖에도 쓴다: agent-roster 는 홈(`~/.config/kasaterm/agent-roster/<cwd 이름>.json`)에, 묶기 표식은 Git sh 의 `/tmp`(리그 TEMP 가 아니라 **사용자 TEMP**)에 칸 번호 이름(`kasaterm-bound-_0`)으로 남아 같은 번호의 사용자 칸 표식을 덮는다. cwd 는 사용자 칸이 안 쓰는 리그 전용 폴더(신뢰한 폴더 하위, 예: worktree 의 `targetigcwd`)로 하고, 끝나면 그 roster 파일과 내 대화 경로가 든 표식만 지운다.

대화 운반이 홈의 정본 경로를 요구하면 **이번 리그에서 만든 새 UUID 파일만** 만들고 정리한다. 사용자 세션을 복사해 이사시키거나 같은 UUID로 띄우지 않는다. 실제 Claude 측정은 `--safe-mode --restricted --strict-mcp-config --tools ""`로 훅·도구를 끈 새 대화로 하며, 별도 리그 소켓으로 바인딩한다. 원본 셸 훅·학생 이름 런처는 실행하지 않는다.

## KasaLite(터미널 고정판) 리그

`scripts/build-lite-app.sh` 가 굽는 `dist/KasaLite.app` 은 같은 바이너리를 `kasaterm-lite` 로 이름만 바꾼 것이다. 부팅에서
`apply_lite_env`(main.rs)가 위 격리 env 를 **전부 스스로** 건다 — 뿌리는 `KASATERM_LITE_ROOT`, 없으면 `~/.config/kasaterm-lite`.
그래서 리그는 뿌리 하나만 주면 된다:

```bash
KASATERM_LITE_ROOT=/tmp/<네이름>-lite KASATERM_NO_FOCUS=1 dist/KasaLite.app/Contents/MacOS/kasaterm-lite \
  > /tmp/<네이름>-lite.log 2>&1 &
APP=$!
export KASATERM_SOCKET_PATH=/tmp/<네이름>-lite/lite.sock     # CLI 도 같은 소켓으로
```

- 본판 pane 안에서 띄워도 된다 — `apply_lite_env` 가 `KASATERM_SOCKET_PATH`·`KASATERM_TMUX_SHIM_DIR` 상속을 **조건 없이** 덮는다.
- lite 는 HTTP 서버(8765)·자기설치·마커 청소·온보딩·계정 복구를 타지 않는다. 사후에 `~/.config/kasaterm/` 해시가 그대로인지,
  `$TMPDIR/kasaterm-selfinstall.log` 가 안 생겼는지로 확인한다. 로그는 `$TMPDIR/kasaterm-lite-app.log`.
- shim 은 최소(rc 셋 + `kasaterm-cli`)라 pane 에서 `which claude` 가 **진짜** claude 여야 하고 `which kasaterm-cli` 는
  `$TMPDIR/kasaterm-lite-shim-<pid>/` 여야 한다.

## 업데이트 리그(Sparkle 실제 흐름)

새 판 창·진행 막대·다시 켜기를 바꿨으면 실제 Sparkle 로 돌려 본다. `scripts/update-rig.py` 가 설치본을 뼈대로 격리 번들
두 판(0.0.1·0.0.2, 번들 id `com.kasa.kasaterm.updaterig`, 시험 EdDSA 키)과 그 키로 서명한 로컬 피드를 만든다. 앱은
`KASATERM_UPDATE_RIG_FEED`(루프백만)가 있으면 설치 위치 검사 없이 Sparkle 을 켜고 그 피드를 본다.

```bash
python3 scripts/update-rig.py --root /tmp/<네이름>-up --binary target/debug/kasaterm   # port·피드 주소를 찍는다
(cd /tmp/<네이름>-up/serve && python3 -m http.server <port> --bind 127.0.0.1 > ../http.log 2>&1 &)
cd /tmp/<네이름>-up && source rig.env && export KASATERM_UPDATE_RIG_SHOTS=$PWD/shots KASATERM_UPDATE_RIG_PRESS_MS=3000 \
  KASATERM_AUTOSEND="sleep 600" KASATERM_AUTOQUIT_MS=60000 && mkdir -p shots
app/kasaterm-rig.app/Contents/MacOS/kasaterm > run.out 2>&1 &
APP=$!
```

- **캡처** — Sparkle 창은 AppKit 이라 `capture --window`(wgpu 프레임)에 안 잡히고, 이 세션엔 화면 녹화 권한도 없다.
  `KASATERM_UPDATE_RIG_SHOTS` 면 앱이 제 창(시트 포함)을 창 서버에서 떠 달라질 때마다 `NNN-w<창번호>.png` 로 남긴다.
- **누르기** — `KASATERM_UPDATE_RIG_PRESS_MS` 면 그만큼 그대로인 창의 기본 단추(Return)를 누른다. 업데이트 설치 →
  설치 후 다시 시작 → 끊김 시트의 [나중에] 순이다. `KASATERM_AUTOSEND="sleep 600"` 으로 일하는 창을 만들면 시트가 선다.
- **판정** — `[update-rig] … 기본 단추 누름` 로그, `app/kasaterm-rig.app` 의 `CFBundleVersion` 이 0.0.2 로 바뀌었는지,
  다시 켜졌다면 부모가 launchd(ppid 1)인 새 프로세스. [나중에] 길은 다시 켜지 않고 `AUTOQUIT` 종료 때 설치된다.
- ⚠️ **다시 켜는 길은 사람 화면에 OS 창을 띄울 수 있다.** 다시 켜진 리그는 LaunchServices 가 띄워 TCC 책임자가 리그
  자신이다. 셸이 `~/Desktop` 같은 보호 폴더에서 뜨면 「kasaterm-rig 의 데스크톱 접근」 창이 뜨고, 리그는 굽을 때마다
  ad-hoc 서명이 바뀌어 매번 다시 묻는다(2026-10-02 세 번 띄웠다). 리그 설정에 `default_cwd` 를 박아 두었지만, 같은 경로에
  다시 구운 번들은 LaunchServices 가 옛 Info.plist 를 기억해 `LSEnvironment` 가 어긋날 수 있다. 다시 켜기까지 볼 일이
  아니면 [나중에] 길로 끝내라.
- 다시 켜진 리그의 `TMPDIR` 은 LaunchServices 가 사용자 값으로 덮는다 — 그 로그는 본판 `$TMPDIR/kasaterm-app.log` 에
  이어 붙는다(이어쓰기라 본판 기록은 안 깨진다). 리그 `AUTOQUIT` 으로 오래 남지 않게 한다.
- 리그는 배경(Accessory) 앱이라 스스로 앞에 오지 않는다. 그래서 「앱이 앞에 있을 때 표준 창」 조건은 리그에서 건너뛴다 —
  표준 창이 뜨는 순간 리그가 앞으로 나와 키 창을 가져가니, 사람이 타자 중이면 끝난 뒤 돌려라.

## 사이드바 폭 검증

위 격리 부팅 환경을 모두 유지한 release 앱에 다음 옵션을 더한다. `KASATERM_WINDOW_SIZE` 검증 모드는 저장된 창 UI 폭을 읽지 않으므로, 폭 변경은 아래 하네스가 실제 경계 드래그 이벤트로 수행한다.

```bash
KASATERM_WINDOW_SIZE=1600,1000 \
KASATERM_AUTOSPLIT=hhhhh KASATERM_AUTOSPLIT_MS=1000 \
KASATERM_SIDEBAR_FIXTURE=1 \
KASATERM_AUTOINFO=rooms KASATERM_AUTOINFO_MS=3500 \
KASATERM_AUTOSIDEBAR_DIR=/tmp/<네이름>/sidebar-shots
```

- 목록·배치도 각각 140→240→360→520→140px로 끌고, 첫 칸을 실제 클릭한다. 6.5초부터 시작해 약 21초에 끝나므로 `KASATERM_AUTOQUIT_MS`는 30000 이상으로 준다.
- 단계별 PNG와 JSON의 실제 폭·행/지도/별도창 사각형·스크롤 범위·`hits_within_width`·`clicked_expected_pane`을 확인한다. PNG는 위 규칙대로 줄여 한 장씩 본다.
- `KASATERM_AUTOSIDEBAR_EXTRAS=1`이면 검증 앱의 마지막 칸 하나를 보관하고 둘을 별도창으로 떼어 같은 사이클을 반복한다. 실제 사용자 세션에는 적용하지 않는다.
- 학생·원격 기기는 표시용 픽스처다. 실기기 연결 성공이나 원격 실행 성공의 증거로 쓰지 않는다.
