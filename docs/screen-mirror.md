# 기기 화면 거울 — 폰·다른 PC 에서 기기 화면 보기·조작

각 기기의 카사텀이 자기 화면을 JPEG 로 떠서 흘리고, 폰과 다른 기기의 카사텀이 그것을 본다. 보는 쪽이 「조작」을 켜면 누르기·휠·키·
글자를 그 기기에 넣는다(스웜 화면 에이전트 `sionic-ai/swarm agent/main.swift` 의 CGEvent 길과 같은 결).

## 왜

- PC 의 「화면 보기」는 macOS 화면공유(`vnc://`)다. 같은 망의 맥끼리만 되고, 폰과 Windows 에서는 못 쓴다. 굳은 화면공유 앱·
  유령 연결을 걷어내는 손질도 따로 든다(`machinescol.rs open_screen_share`).
- 폰과 기기 사이에는 이미 카사텀 길(카사넷 직통·관문)이 있고, PC 와 다른 기기 사이에는 `/m/<기기>/` 문(터널·카사넷)이 있다.
  화면을 그 길에 실으면 따로 포트를 열지 않고 어디서든 같다.

## 흐름

```
폰 ScreenMirrorScreen ──(직통이면 카사넷, 아니면 관문)──┐
PC 웹 칸 /m/<기기>/screen ──(이 기기 HTTP → 그 기기 길)──┤
                                                     ▼
                         그 기기 카사텀  GET /screen/ws  (kasa-mcp http/screen.rs → screen_mirror.rs)
                                                     │  Source::capture (앱이 꽂는다)
                                                     ▼
                         app/kasaterm/src/screen_capture.rs  macOS CGDisplayCreateImage · Windows StretchBlt
```

- 화면을 뜨는 법은 OS 마다 달라 앱이 부팅 때 `kasa_mcp::screen_mirror::install` 로 꽂는다. 관문·`kasa-device`·lite 처럼 꽂지
  않은 곳은 `unavailable` 로 답한다.
- PC 는 Info 「다른 기계」 줄 메뉴의 **「화면 거울」**로 연다(연결된 기기만). 그 기기 화면이 이 창의 웹 칸(`/screen` 보기
  페이지)으로 뜬다. 화면공유(「화면 보기」)·「KVM 보기」는 그대로 둔다.
- 폰은 허브의 기기 머리글 **모니터 단추**로 연다(연결된 기기만).

## 계약

### `GET /screen/ws?display=&w=&h=&q=&fps=`

- 주인만(`guest_denied` — 화면에는 메일·비밀번호 창까지 보인다). 남의 Origin 은 거절(`ws_origin_ok`).
- 쿼리와 `tune` 은 같은 이름을 쓴다. `w`·`h` 160–3840(기본 1600) 상자 안으로 비율을 지켜 줄인다(키우지 않는다).
  `q` 20–90(기본 60), `fps` 1–15(기본 8). `display` 가 없거나 모르는 번호면 주 화면.
- 서버 → 보는 쪽
  - 텍스트 `{"t":"hello","displays":[{id,width,height,main}],"display":id}` — 붙자마자, 그리고 화면을 바꾼 뒤. 크기는 화면
    좌표(포인트)다.
  - 바이너리 `[u32 seq BE][u16 meta 길이 BE][meta JSON {w,h,d}][JPEG]`. seq 는 1부터.
  - 텍스트 `{"t":"denied","reason":"permission"|"unavailable"}` — 사정이 바뀔 때 한 번. 소켓은 닫지 않고 3초마다 다시 떠 본다.
- 보는 쪽 → 서버
  - `{"t":"ack","seq":n}` — 그 장을 **다 그린 뒤**. 보낸 적 없는 번호·옛 번호는 버린다.
  - `{"t":"tune",display?,w?,h?,q?,fps?}` — 바꾸면 같은 그림이라도 한 장 다시 보낸다.
  - 조작(아래 「조작」). 좌표 `x`·`y` 는 지금 보는 화면 안의 비율 0–1 이라 받은 그림 크기와 상관없다. `m` 은 1 shift·2 control·
    4 option(alt)·8 command(win).
    - `{"t":"pointer",x,y,b,m?}` — `b` 는 단추 상태 통째(1 왼쪽·2 오른쪽·4 가운데). 서버가 연결마다 직전 상태를 쥐고 견줘
      누름·뗌·끌기를 가른다. 클릭 = `b:1` 뒤 `b:0`.
    - `{"t":"scroll",x,y,dx?,dy?}` — 그 기기 화면 포인트 단위, 양수 dy 는 위로 굴림(내용이 내려온다). 먼저 그 자리로 커서를 옮긴다.
    - `{"t":"key",k,down?,m?}` — `k` 는 웹 `KeyboardEvent.code`(`Enter`·`Tab`·`Backspace`·`Escape`·`Delete`·화살표·`Home`·`End`·
      `PageUp`·`PageDown`·`KeyA`…·`Digit0`…·`F1`–`F12`·문장부호). `down` 이 없으면 눌렀다 뗀다. 모르는 이름은 버린다.
    - `{"t":"text",s}` — 글자째 넣기(4096 바이트까지). 폰 자판·보는 쪽 입력기가 조합을 끝낸 글이다.
  - 서버 → 보는 쪽 `{"t":"input","ok":false,"reason":"permission"|"unavailable"}` — 조작을 못 넣을 때, 사정이 바뀔 때만 한 번.
    다시 넣히면 `{"t":"input","ok":true}`.

### 역압·박자

- ack 안 받은 장은 **2장까지**. 막힌 동안은 화면을 뜨지도 않는다. 느린 폰이 관문 흐름 큐(256 프레임)를 채워 끊기지 않고,
  아무도 안 보는 화면을 뜨느라 CPU 를 쓰지 않는다.
- 줄인 픽셀의 지문이 지난 장과 같으면 인코딩도 전송도 안 한다. 2초 동안 안 바뀌면 0.5초마다만 떠 본다.
- 20초마다 핑, 60초 동안 아무 말(ack·핑·퐁)이 없으면 닫는다.
- 보는 쪽마다 따로 뜬다(크기·품질이 제각각이라). 프레임은 어디에도 남기지 않는다.

### 보는 쪽

- 폰(`mobile/lib/screen_session.dart`·`screens/screen_mirror.dart`): 직통이면 1920·q60, 관문이면 1280·q50, 8fps. 수명은 터미널과
  같다 — 앱이 뒤로 가면 닫고, 돌아오면 관문부터 다시 붙고, 길이 바뀌면(`routeChanges`) 옮겨 붙는다. 화면은 가운데 맞춤,
  핀치(최대 8배)·두 번 탭(그 자리 3배 ↔ 전체). 두 장을 함께 풀어 늦게 끝난 옛 장이 새 장을 덮지 않게 seq 로 거른다.
- 웹(`crates/kasa-mcp/assets/screen/index.html`): 창 픽셀(×devicePixelRatio, 160 단위)만큼 받고 창 크기가 바뀌면 `tune`.
  `?name=` 이 탭 제목(「화면 · 이름」)이 된다. 「실제 크기」로 1:1 스크롤 보기.

## 조작

- **차례**: 조작은 받은 차례대로 하나씩 넣고 끝날 때까지 기다린다(누름·뗌이 뒤바뀌지 않게). 소켓이 끊길 때 단추가 눌린 채면 그 자리에서
  뗀다 — 그 기기에 왼쪽 단추가 눌린 채 남으면 사람이 그 앞에서 마우스를 못 쓴다.
- **글자는 글자째**: 한글을 키 위치로 풀어 보내면 폰 자판은 만들던 음절을 고쳐 쓰고 그 기기 입력기는 백스페이스를 자모 하나로 받아
  어긋난다(스웜 앱 KVM 이 같은 까닭으로 「글자 보내기」를 뒀다). 그래서 글은 유니코드 그대로 넣는다 — macOS
  `CGEventKeyboardSetUnicodeString`(20자씩), Windows `KEYEVENTF_UNICODE`. macOS 두벌식 입력기가 켜진 채로도 「안녕 hello 한글」이
  그대로 들어간다(2026-10-29 실측, 숨긴 TextEdit). 그래서 한/영 키는 두지 않는다.
- **두 번 누름**: 맥은 시각이 아니라 이벤트의 누름 수를 읽는다. 0.4초 안·5포인트 안의 누름을 2·3 으로 잇는다. Windows 는 OS 가 시각으로 가른다.
- **권한**: macOS 는 손쉬운 사용 권한이 없으면 넣은 이벤트를 소리 없이 버린다. 그래서 넣기 전에 `AXIsProcessTrusted` 로 묻고, 없으면
  `permission` 을 알리고 연결마다 한 번 `AXIsProcessTrustedWithOptions(prompt)` 로 그 기기 앞에 창을 띄운다. 켜면 다시 열 필요 없이
  바로 먹는다. 주인만 여는 문(`guest_denied`)이라 따로 「조작 허용」 단추는 두지 않았다 — 그 기기 앞의 권한 켜기가 그 동의다.
- **폰**(`screens/screen_mirror.dart`): 보기가 기본, 앱 바 손 단추로 조작. 조작에서는 보기에서 키운 배율·자리를 그대로 쥐고(확대 손을
  뗀다) 탭 = 클릭, 길게 = 오른쪽 클릭, 한 손가락 끌기 = 휠(손가락 아래 내용이 따라온다 — 문턱 전 거리도 굴린다)이다. 아래에 키 줄
  (esc·tab·화살표·⌫·⏎)과 「그 기기에 글 넣기」 입력줄이 붙고, 본문을 2px 강조 테로 두른다. 권한이 없으면 위에 안내 띠.
  둘로 나누는 까닭은 크롬 거울과 같다 — 핀치·끌기가 확대·이동과 같은 손짓이고, 보다가 잘못 누르면 그 기기에서 일이 일어난다.
- **웹**(`assets/screen/index.html`): 「조작」 단추. 마우스는 `e.buttons` 그대로·움직임은 화면 틀마다 하나, 휠은 브라우저와 부호가
  반대, 이름 붙은 키와 단축키(ctrl·⌘·alt)만 키로, 나머지 글은 숨은 입력칸에 이 기기 입력기가 조합을 끝낸 글째 보낸다. 조합 끝과
  `input` 차례가 엔진마다 달라(웹킷은 끝 뒤에 input 이 한 번 더 온다) 칸에 남은 글을 보내고 비운다.

## OS 별

- **macOS** — `CGPreflightScreenCaptureAccess` 가 거짓이면 `permission`. 권한 없이도 한 장이 뜨이지만 남의 창이 빠진
  배경화면이라 거짓 화면이 된다. 처음 거절될 때 연결마다 한 번 `CGRequestScreenCaptureAccess` 로 그 기기 앞에 권한 창을 띄운다.
  **권한을 켠 뒤 그 기기의 카사텀을 다시 열어야** 풀린다(macOS 가 프로세스마다 판정을 붙든다). `CGDisplayCreateImage` 는 SDK
  헤더에서 빠져 `dlsym` 으로 찾는다. 화면 제 색 공간 그대로 비트맵에 그린다 — sRGB 로 바꾸면 한 장에 16ms 가 더 든다.
- **Windows** — 모니터 번호는 `EnumDisplayMonitors` 차례. `StretchBlt(HALFTONE, CAPTUREBLT)` 로 줄여 받는다. 잠금 화면·UAC 같은
  보안 데스크톱에서는 실패해 그 동안 지난 장이 남는다. 조작은 `SendInput` — 좌표는 가상 화면 전체를 0–65535 로 편 절대 좌표
  (`MOUSEEVENTF_VIRTUALDESK`), 휠은 40픽셀을 한 칸(120)으로 친다. 관리자 권한 창에는 UIPI 때문에 안 들어간다.

## 실측 (2026-10-29, 맥북 내장 화면 3024×1964)

| 크기·품질 | 한 장 (release) | JPEG |
|---|---|---|
| 1280 · q50 | 38ms | 110KB 안팎 |
| 1600 · q60 | 39ms | 140–180KB |
| 2560 · q70 | 64ms | 330–420KB |

`CGDisplayCreateImage` 만 15ms, 나머지는 줄여 그리기와 JPEG. 측정은
`cargo test --release -p kasaterm --bin kasaterm screen_capture::measure -- --ignored --nocapture`(화면 기록 권한이 있는 터미널에서).

## 검증

- `cargo test -p kasa-mcp --lib screen_mirror` — 가짜 화면으로 hello·창 2장·옛/미래 ack·같은 화면·tune·거절·없는 기기.
- `cargo test -p kasaterm --bin kasaterm screen_capture` — 줄이기·지문·두 바이트 순서의 JPEG 색.
- `flutter test test/screen_session_test.dart` — 프레임 머리 읽기, 푼 뒤 ack, 화면 바꾸기, 거절, 조작 메시지·권한 안내.
- `flutter test test/screen_mirror_control_test.dart` — 보기에서는 아무것도 안 가고, 조작에서 탭이 그 자리 비율의 클릭·끌기가 휠.
- `cargo test -p kasa-mcp --lib screen_mirror` 의 조작 셋 — 차례·직전 단추·버리는 메시지, 권한 안내 한 번·창 한 번, 끊길 때 단추 떼기.
- `cargo test -p kasaterm --bin kasaterm screen_input` — 키 이름 표·누름 수. 실제 HID 길은
  `screen_input::tests::pointer_lands -- --ignored`(커서를 3포인트 옮겼다 되돌린다. 커서가 있는 화면 기준이어야 한다 — 주 화면
  기준으로 풀면 옆 모니터의 커서가 주 화면 끝으로 끌려온다).
- Windows 쪽은 이 맥에서 `ring` 의 C 빌드 때문에 앱 통째 교차 검사가 안 된다. `screen_input.rs` 만 같은 `windows` 판으로 떼어
  `cargo check --target x86_64-pc-windows-msvc` 했다.
- 격리 리그 둘(A 원본, B 가 `KASATERM_MACHINES` 로 A 를 가리킴): B 의 `/m/rigA/screen/ws` 로 실제 화면이 오고 ack 를 안 하면 2장에서
  멈춘다. B 웹 칸에 `/m/rigA/screen` 을 열어 웹뷰 스냅샷(`web.drive shot`)으로 화면이 그려진 것을 봤다.

## 남은 것

- 조작 중 끌어 고르기(왼쪽 단추 끌기)는 폰에 없다 — 한 손가락 끌기가 휠이다. 웹은 된다.
- 붙여넣기(보는 쪽 클립보드 → 그 기기)는 없다. 글 넣기로 대신한다.
- 많이 움직이는 화면은 JPEG 장마다 보내 무겁다. 더 필요하면 ScreenCaptureKit(바뀐 영역·GPU 축소)이나 하드웨어 H.264 로 간다.
