# 기기 화면 거울 — 폰·다른 PC 에서 기기 화면 보기

각 기기의 카사텀이 자기 화면을 JPEG 로 떠서 흘리고, 폰과 다른 기기의 카사텀이 그것을 본다. 보기만 한다(누르기·키 입력 없음).

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
  페이지)으로 뜬다. 화면공유(「화면 보기」)·「KVM 보기」는 그대로 둔다 — 조작이 필요하면 그쪽이다.
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

## OS 별

- **macOS** — `CGPreflightScreenCaptureAccess` 가 거짓이면 `permission`. 권한 없이도 한 장이 뜨이지만 남의 창이 빠진
  배경화면이라 거짓 화면이 된다. 처음 거절될 때 연결마다 한 번 `CGRequestScreenCaptureAccess` 로 그 기기 앞에 권한 창을 띄운다.
  **권한을 켠 뒤 그 기기의 카사텀을 다시 열어야** 풀린다(macOS 가 프로세스마다 판정을 붙든다). `CGDisplayCreateImage` 는 SDK
  헤더에서 빠져 `dlsym` 으로 찾는다. 화면 제 색 공간 그대로 비트맵에 그린다 — sRGB 로 바꾸면 한 장에 16ms 가 더 든다.
- **Windows** — 모니터 번호는 `EnumDisplayMonitors` 차례. `StretchBlt(HALFTONE, CAPTUREBLT)` 로 줄여 받는다. 잠금 화면·UAC 같은
  보안 데스크톱에서는 실패해 그 동안 지난 장이 남는다.

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
- `flutter test test/screen_session_test.dart` — 프레임 머리 읽기, 푼 뒤 ack, 화면 바꾸기, 거절.
- 격리 리그 둘(A 원본, B 가 `KASATERM_MACHINES` 로 A 를 가리킴): B 의 `/m/rigA/screen/ws` 로 실제 화면이 오고 ack 를 안 하면 2장에서
  멈춘다. B 웹 칸에 `/m/rigA/screen` 을 열어 웹뷰 스냅샷(`web.drive shot`)으로 화면이 그려진 것을 봤다.

## 남은 것

- 손대기(누르기·스크롤·글자). 크롬 거울(`docs/chrome-mirror.md`)의 보기/손대기 두 모드와 좌표 규칙을 따른다.
- 많이 움직이는 화면은 JPEG 장마다 보내 무겁다. 더 필요하면 ScreenCaptureKit(바뀐 영역·GPU 축소)이나 하드웨어 H.264 로 간다.
