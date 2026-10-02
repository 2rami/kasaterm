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
- **`KASATERM_WINDOW_FILE`** 도 빠뜨리지 마라. 안 걸면 검증용 앱이 자기 창 크기를 사용자의 `window.json` 에 적어 두고, 다음에 사람이 앱을 열 때 엉뚱한 크기로 뜬다(2026-08-31 실제로 덮었다 — 세션은 격리해 무사했는데 이 하나가 목록에 없었다).
- **`KASATERM_DEVICE_FILE=/tmp/<네이름>/device.json`** — 관문 계정 로그인 파일. 안 걸면 리그가 사용자의 `device.json` 을 읽어
  설정 「계정」이 사용자 계정으로 로그인된 화면으로 뜬다(2026-10-02). 로그인 전 화면을 보려면 반드시 건다.
- **`KASATERM_MACHINES='[]'`** — 사용자 명부(`machines.json`)를 안 읽게 한다. 안 걸면 리그가 명부의 기기마다 ssh 터널을 열고,
  저쪽에서 이쪽으로 오는 되돌아오는 길(`-R`)을 **사용자 앱과 같은 원격 포트**로 열어 다툰다(2026-09-29 실측: 한 기기는 거부됐지만
  다른 기기는 리그 쪽이 열렸다). 기기 칸(직통·빌드 다름)은 그래서 리그로 못 보고, 실기기에서 확인한다.
- **`KASATERM_COLLAB_ROOT`** 로 `session_characters.json`(세션↔학생 바인딩)·bind 마커·`agent-roster/` 를 가른다. 안 걸면 본판 폴더를 **읽고 병합해** 쓰므로 검증 세션의 학생이 항목으로 늘 뿐 사람의 바인딩을 지우지는 않지만, 같은 cwd 의 같은 pane 번호(`%2`)가 본판 세션에 결합해 글리프가 남의 것을 보는 일이 생긴다.
- ⚠️ **`KASATERM_SOCKET_PATH` 를 띄울 때도 주고, `kasaterm-cli` 를 부를 때도 줘라.** CLI 는 `$KASATERM_SOCKET_PATH > $CMUX_SOCKET_PATH > /tmp/cmux.sock` 순으로 붙고 **포트는 안 본다** — 리그를 다른 포트로 띄웠어도 CLI 에 이 변수를 안 주면 그 명령이 **사용자 앱으로 간다**(2026-09-03 실측: `split right` 이 사용자 창에 pane 을 만들었고, `send` 가 도는 학생의 입력창에 글자를 밀어넣었다). 앱 부팅 줄과 CLI 호출 양쪽에 같은 경로를 걸어라:
  `export KASATERM_SOCKET_PATH=/tmp/<네이름>/rig.sock` 를 먼저 하고 그 셸에서 둘 다 부르는 것이 가장 안전하다.
- **화면을 판정할 때 `kasaterm-cli capture <pane>` 을 믿지 마라 — 원본 글자판이다.** 그건 `capture_pane_offscreen` 이라 `t.cells` 를 그대로 찍어, 렌더러가 화면을 만들며 하는 일(스크롤백 당김·입력창 붙잡기 같은 재구성)이 **안 보인다**. 렌더 변경을 눈으로 확인하려면 `kasaterm-cli capture --window <path>`(창 프레임) 이나 `KASATERM_AUTOCAPTURE_MS`/`_PATH` 를 써라. 2026-09-03 에 이걸 몰라 "코드가 안 돈다"고 한동안 오판했다(계측을 심어 보니 값은 맞게 돌고 있었다).
- **`TMPDIR` 도 리그 폴더로.** 앱은 stderr 가 tty 가 아니면 `$TMPDIR/kasaterm-app.log` 로 돌린다 — 안 가르면 리그 로그가
  사용자 앱 로그에 섞이고, `> app.log` 로 받은 파일은 비어 있다(2026-09-25). 소켓 경로는 104바이트 한도라 스크래치 폴더
  깊숙이 두면 `path must be shorter than SUN_LEN` 으로 소켓 서버가 안 선다 — `/tmp/<네이름>/` 처럼 짧게.
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
2. **누르기** — `KASATERM_AUTOCLICKS="x,y;x,y"`(논리 좌표) · `_MS`(첫 클릭, 기본 6000) · `_GAP_MS`(사이, 기본 1200). 팝오버·묶음 알약을
   연 채로 `kasaterm-cli capture --window` 로 찍는다. 좌표는 창의 논리 크기(물리 폭 ÷ 로그의 `scale=`) 기준이다 — 캡처 PNG 는 줄여 저장되니 비율로 옮긴다. 표시용 픽스처(`KASATERM_AUTOINFO=execution…`)는
   검증 실행(`KASATERM_WINDOW_SIZE="w,h"`)에서만 선다.
3. **스크린샷** — `KASATERM_AUTOCAPTURE_MS=8000` 로 N초 후 자동 캡처. 기본 경로 `$TMPDIR/kasaterm.png` (`KASATERM_AUTOCAPTURE_PATH` 로 변경). **판정은 `Read` 로 직접 본다** — 먼저 `sips -s format jpeg -s formatOptions 60 -Z 1200 <png> --out <jpg>` 로 줄여서 연다. 이미지는 대화에 박혀 매 요청마다 다시 전송되고 빼는 수단이 없다(2026-09-05: 한 세션에 47장 8.5MB 가 쌓여 32MB 벽에 걸렸다) — 볼 것을 정한 뒤 한 장씩. macOS `screencapture` 는 권한 막혀 안 됨 — 무조건 자체 캡처.
4. **자동 입력** — `KASATERM_AUTOSEND="claude" KASATERM_AUTOSEND_MS=6000`. send_bytes 직접 주입이라 **IME 조합 경로는 재현 못 함** — 한글 조합 버그는 사용자가 직접 타이핑해야 함 (`KASATERM_IME_DEBUG=1` 로 키 코드포인트 로깅).
5. **체감(스크롤·입력 지연)은 반드시 release** — `cargo run --release -p kasaterm`. 디버그 빌드는 원래 버벅임(debug=느림, release/.app=빠름). 디버그로 "느리다" 판단 금지.
6. **시각 확인** — 스크린샷 본 후 어색한 부분 직접 짚어내고 수정. "어때보여요?" 묻지 말고 너의 판단으로 다음 액션.

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
