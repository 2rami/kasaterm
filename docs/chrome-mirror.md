# 크롬 거울 — 폰에서 학생 크롬 탭 보기·손대기

학생(에이전트 pane)이 카사크롬으로 모는 크롬 탭을 폰에서 실시간으로 보고, 필요할 때 대신 눌러 준다.
터미널 거울은 PTY 바이트를 흘리면 끝이지만 크롬은 픽셀이라, CDP 스크린캐스트(JPEG)를 흘린다.

## 왜

- 폰은 지금 `browser_show_human` 으로 **주소만** 받는다. 폰이 그 주소를 새로 열기 때문에 데스크톱 크롬의 로그인·스크롤
  위치·쓰다 만 폼·학생이 만지는 화면은 오지 않는다.
- `ask_human`(로그인·2단계 인증·캡차)은 데스크톱 크롬 창을 앞으로 올린다. 사람이 폰을 보고 있으면 아무도 못 보고,
  학생은 멈춘 채 기다린다.

## 지금 있는 것 (2026-10-13 조사)

- 카사크롬은 확장 ↔ 다리(`127.0.0.1:8777`) ↔ MCP 셋이다. 다리는 JSON 텍스트로 요청·응답만 나른다. 인증·이벤트 흐름·
  바이너리가 없고, 부르기마다 시한(기본 30초)이 걸린다.
- 확장 `cdp.js` 는 `chrome.debugger` 로 붙고 15초 놀면 뗀다. `pin` 하면 안 뗀다. CDP 이벤트는 확장 안 링버퍼에만 쌓인다.
- 폰은 카사넷 직통으로도 8777 에 못 간다(`kasanet.rs` — 폰 연결은 앱 HTTP 입구로만). 폰 → 데스크톱은 카사텀 HTTP 하나다.
- 관문은 사용자 접두사 아래 웹소켓을 경로 제한 없이 나른다. 다만 흐름마다 큐가 256 프레임이고, 차면 그 흐름을 끊는다
  (`gateway.rs` `overflow`). 웹소켓 프레임은 쪼개지 않아서 큰 프레임 하나가 그 기기의 업링크 소켓을 붙든다.
- 폰에는 `FillViewer`(핀치 확대·`toScene`)가 남아 있다. 지운 그림 모드가 쓰던 자리다.

## 흐름

```
폰  BrowserMirrorScreen
 ⇅  /browser/ws  — 직통이면 카사넷, 아니면 관문 (기존 입구 규칙 그대로)
카사텀  crates/kasa-mcp  browser_mirror.rs  — 주인 폰만, 크레딧 창 2
 ⇅  ws://127.0.0.1:8777  — 다리 손님으로 붙는다
카사크롬 다리  bridge/server.mjs  — mirror.* 길잡이
 ⇅
카사크롬 확장  extension/mirror.js  — Page.startScreencast · 입력 주입
 ⇅  chrome.debugger
크롬 탭
```

거울 대상은 **폰이 고른 기기의 크롬**이다. 각 기기의 카사텀은 제 기기 다리에만 붙는다. 다른 기기 크롬을 모는 학생
(`kasachrome_machine`)의 탭은 그 크롬이 도는 기기 아래에 뜬다.

## 계약

### 확장 (`extension/mirror.js` 새로, `cdp.js` onEvent 에 한 갈래)

- 시작: `cdp.attach` → `cdp.pin(tab, 'mirror')` → `Page.startScreencast {format: 'jpeg', quality, maxWidth, maxHeight}`.
- 프레임: `Page.screencastFrame` 을 받으면 `{type: 'mirror.frame', mid, seq, data, meta}` 를 다리로 보낸다.
  **`Page.screencastFrameAck` 은 다리에서 `mirror.ack` 이 왔을 때만 한다.** 크롬은 ack 이 밀려 있으면 다음 프레임을
  만들지 않으므로, 이것이 탭에서 폰까지 이어지는 역압이 된다. 중간 어디에도 프레임이 쌓이지 않는다.
- meta 는 `deviceWidth`·`deviceHeight`(뷰포트 DIP), `offsetTop`, `pageScaleFactor`, `scrollOffsetX/Y`, `timestamp`.
  붙자마자 디버깅 띠가 내려오며 뷰포트 높이가 120ms 동안 줄어든다(632→583, `tools.js measureRoom`). 그래서 좌표는
  매 프레임의 meta 로 매긴다. 붙기 전 크기를 믿지 않는다.
- 안 바뀌는 화면은 프레임이 안 온다. 뒤 탭·가려진 창도 합성기가 안 그려 안 올 것이다(M0 에서 실측). 규칙은 상태가 아니라
  시간으로 건다: 프레임이 2초 없으면 1초마다 `Page.captureScreenshot` 하고, 직전과 같으면 안 보낸다. 이때 meta 에
  `slow: true`.
- 입력 `mirror.input` 은 `Input.dispatchMouseEvent`(누르고 떼기·`mouseWheel`), `Input.insertText`, `Input.dispatchKeyEvent`
  로 넣는다. **`markBusy`·활동 기록을 타지 않는다** — 손가락 한 번마다 덮개를 다시 그리고 기록을 쌓으면 학생 활동
  기록이 사람 손으로 덮인다. 대신 덮개 칩을 5초 동안 「사람 조작 중」으로 바꾼다(`data-mode="human"`).
- 끝: `chrome.debugger.onDetach`(개발자 도구 열기는 `canceled_by_user`, 탭 닫힘), 서비스 워커 재시작(디버거 세션이 같이
  사라진다), 다리 끊김. 어느 쪽이든 `mirror.ended {mid, reason}` 을 보내고 pin 을 푼다.
- 프레임 길은 `sessionChain`(도구 부르기 줄 세우기) 밖에서 돈다. 그 뒤에 서면 학생 도구 하나가 화면을 몇 초 멈춘다.
- 덮개(테두리·칩·학생 커서)는 프레임에 그대로 담는다. 누가 무엇을 누르는지가 거울을 보는 이유다.
- 디버거 세션이 탭마다 하나라 스크린캐스트도 탭마다 하나다. 같은 탭을 또 열면 확장은 흐름을 새로 만들지 않고 같은
  `mid` 를 돌려준다. 구독자를 여럿으로 나누는 일은 카사텀이 한다.

### 다리 (`bridge/server.mjs`)

- 손님 → 다리: `mirror.open {tabId, maxWidth, maxHeight, quality}`, `mirror.ack {mid, seq}`, `mirror.input {mid, ev}`,
  `mirror.tune {mid, maxWidth, maxHeight, quality}`, `mirror.close {mid}`.
- 확장 → 다리: `mirror.opened {mid, tab, viewport}`, `mirror.frame`, `mirror.tab {mid, title, url}`, `mirror.ended`.
  `mid` 로 연 손님에게만 보낸다. `pending`·시한 기계는 안 쓴다 — 30초 시한이 긴 흐름을 끊는다.
- 손님 소켓이 닫히면 그 손님이 연 거울을 전부 닫는다.
- 탭 목록은 기존 `call` 로 새 도구 `mirror_tabs` 를 부른다 →
  `[{tabId, windowId, title, url, active, owner: {name, color, pane} | null}]`. owner 는 `sessions.js` `tabOwner` 에서.
- 인증은 지금과 같다(루프백만). 다리는 이미 `eval_js` 를 받는 자리라 거울이 새 구멍을 내지 않는다. 밖으로 여는 문은
  카사텀 하나다.

### 카사텀 (`crates/kasa-mcp/src/browser_mirror.rs` 새로)

- `GET /browser/tabs` → `{chrome: bool, profiles: [{id, label, tabs: [...]}]}`. 다리가 없거나 확장이 안 붙었으면
  `chrome: false`.
- `GET /browser/ws?profile=&tab=` 웹소켓. `netfwd.rs` `tcp_ws_handler` 틀을 그대로 따른다: `Request` 를 받아
  `guest_denied` → `ws_origin_ok` → 업그레이드. **주인 폰만 연다** — 그 크롬 프로필에 주인의 로그인이 다 들어 있다.
  `/term/ws` 는 손님 검사가 없는데, 거울은 그걸 따라가면 안 된다.
- 붙으면 다리 손님으로 `hello {role: 'client', identity: {name: 'phone-mirror'}}` → `select` → `mirror.open`.
  다리에 다시 붙으면 `select` 가 풀리므로 다시 고른다.
- 서버 → 폰
  - 바이너리 = `[u32 seq BE][u16 meta 길이][meta JSON][JPEG]`. meta 를 프레임마다 싣는다. 따로 보내면 뷰포트가
    줄어드는 순간 좌표가 한 프레임 어긋난다.
  - 텍스트 `{"t":"tab",title,url}`, `{"t":"slow","on":bool}`, `{"t":"ended","reason":...}`.
- 폰 → 서버 텍스트: `{"t":"ack",seq}`, `{"t":"tap",x,y}`, `{"t":"scroll",x,y,dy}`, `{"t":"text",s}`, `{"t":"key",key}`,
  `{"t":"nav","to":"back|forward|reload"}`, `{"t":"tune",maxW,maxH,q}`. x·y 는 **뷰포트 DIP** 다(폰이 meta 로 바꿔 보낸다).
- **크레딧 창 2**: 구독자마다 ack 안 받은 프레임을 둘까지만 보낸다. 다리에 올리는 `mirror.ack` 은 구독자 하나라도 자리가
  있을 때만 보낸다 — 전부 막혔으면 크롬이 멈춘다. 자리 없는 구독자는 「마지막 프레임 한 칸」만 들고 있다가 자리가 나면
  그것을 보낸다. 느린 폰 하나가 빠른 폰을 붙들지 않고, 관문 큐 256 은 어떤 경우에도 안 찬다.
- 마지막 구독자가 떠나도 5초는 스크린캐스트를 유지한다(앱 전환·길 바뀜 재접속). 그 뒤 `mirror.close` → 확장이 pin 을
  풀고, 15초 뒤 자동으로 떼면서 디버깅 띠가 사라진다.
- 프레임은 어디에도 남기지 않는다(로그·파일·캐시).

### 폰 (`mobile/lib/screens/browser_mirror.dart`, `mobile/lib/browser_session.dart` 새로)

- 입구 셋
  1. 허브: 기기 구역마다 「브라우저」 묶음. 학생 탭(얼굴·학생 색 줄·제목)을 보이고, 주인이 연 탭은 「다른 탭 n개」로 접는다.
  2. 터미널 화면: 그 pane 이 쥔 탭이 있으면 앱 바 아이콘 하나.
  3. 알림·쪽지: `kind: browser` 를 누르면 손대기 모드로 바로 연다.
- 화면: 앱 바 56(제목·호스트·학생 색 줄 2px), 본문 `FillViewer` + `RawImage`(디코드가 끝난 뒤 ack), 상태 칩
  (`직통 · 18ms · 9fps`, 느린 모드면 `뒤 탭 · 1초마다`). 치수는 `docs/design.md` 「카사모바일 폰」 값을 쓴다.
- **보기 / 손대기 두 모드.** 보기가 기본이고, 핀치·끌기는 폰 안에서만 확대·이동한다. 손대기(앱 바 아이콘)에서는
  탭 = 클릭, 끌기 = 휠 스크롤이고, 아래 바에 입력칸(44)·Enter·Tab·뒤로·새로고침이 붙는다. 손대기 동안 본문 테두리를
  2px `accent` 로 두른다. 둘로 나누는 까닭: 핀치 확대와 페이지 스크롤이 같은 손짓이고, 보다가 실수로 학생 탭을 누르면
  학생 일이 꼬인다.
- 좌표: `toScene` → 이미지 px → `x = ix / imgW * deviceWidth`, `y = iy / imgH * deviceHeight - offsetTop`.
  순수 함수로 빼서 시험한다.
- 수명은 터미널과 같다. 앱이 뒤로 가면 소켓을 닫고 앞으로 오면 다시 연다. `routeChanges`(직통 ↔ 관문)면 다시 붙는다.
  `ended` 는 사유별 한 줄 + 다음 단추(「크롬에서 개발자 도구가 열려 중계가 끊겼어요」 · 「다시 연결」).
- 크기: 첫 `tune` 은 폰 화면 물리 px(상한 1280)·품질 60. 관문 길이면 폭 상한 900·품질 45. 길이 바뀌면 다시 보낸다.

### 「사람 손 필요」를 폰으로

- `ask_human` 은 지금 데스크톱 창을 앞으로 올린다. MCP 가 `human.onPhone` 이면 창을 올리지 않고 카사텀에 쪽지
  `{kind: 'browser', pane, asked: reason, browser: {machine, profile, tab}}` 를 남긴다 → 푸시 「OO · 브라우저에서 기다림」
  → 누르면 그 탭 거울이 손대기 모드로 열린다.
- 손볼 곳: `push.rs` 머리말 표, `notes_sheet.dart noteKindStyle`, `app_link.dart`(browser 필드), `push.dart _tap`,
  `main.dart _openLink`.

## 단계

| 단계 | 내용 | 끝 조건 |
|---|---|---|
| M0 | 확장·다리 `mirror.*` + `scripts/` 측정기(프레임을 파일로, fps·크기·첫 프레임까지 시간) | 앞 탭·뒤 탭·가려진 창·멈춘 화면의 fps와 프레임 크기, 붙인 뒤 뷰포트 변화, 개발자 도구를 열었을 때 끝 사유를 이 문서에 기록 |
| M1 | `browser_mirror.rs`: `/browser/tabs`·`/browser/ws`(보기만), 크레딧 창, 구독자 나누기 | Rust 시험 — 가짜 다리로 프레임 왕복, ack 안 하는 폰에 둘 넘게 안 보냄, 손님 폰·남의 Origin 403, 마지막 구독자가 떠나고 5초 뒤 close |
| M2 | 폰 보기: 허브·터미널 입구, `BrowserMirrorScreen`, 수명 | 격리 위젯 스크린샷, 격리 리그(`docs/verify-app.md`) + 시뮬레이터에서 실제 크롬 탭이 보임 — 직통·관문 둘 다 |
| M3 | 손대기: 탭·스크롤·글·키·뒤로, 덮개 「사람 조작 중」 | 좌표 함수 시험(띠가 내려온 뒤, 기기 흉내를 켠 탭 포함), 시뮬레이터에서 폼을 채워 제출 |
| M4 | `ask_human` → 폰 쪽지·푸시 → 손대기로 열기 | 격리 리그에서 학생이 `ask_human` → 폰 알림 → 눌러서 로그인을 마치고 학생이 이어 감 |

## 정해 둔 것

- **디버깅 띠는 받아들인다.** 거울을 여는 동안 크롬 위에 「디버깅 중」 띠가 뜨고 페이지가 49px쯤 줄어든다. 띠 없는 길
  (`captureVisibleTab`)은 앞 탭만·초당 2장이 한계라 기본으로 못 쓴다. 띠는 데스크톱 앞 사람에게 「지금 폰이 보는 중」
  표시도 된다.
- **주인이 연 탭도 보이게 하되 접어 둔다.** 주인 폰만 여는 문이라 막을 까닭이 없다.
- **손대기는 꺼진 채 시작한다.** 알림으로 들어올 때만 켜고 연다.
