# 브라우징 대상 — 기기와 목적지 (2026-09-10)

「사람이 볼 페이지」가 어느 기기의 무엇으로 열리는지를 한 곳에서 정한다. 학생이
`kasaterm-cli open <url>` 이나 셸 `open https://…` 를 부르면 이 선택을 따라간다.
학생 자신의 확인용 브라우저(`kasaterm-cli web`, KasaChrome `browser_*`)는 목적지가
아니라 **기기 모양**(폰 크기·모바일 UA)만 빌린다 — 사람 눈앞에 띄우는 것과 학생이
들여다보는 것은 계속 다른 길이다.

## 설정 키 (`~/.config/kasaterm/settings.json`)

| 키 | 값 | 뜻 |
|---|---|---|
| `browse_device` | 없음 | 자동 — 거울로 보는 사람이 있으면 그 기계, 없으면 이 기계 |
| | `""` | 이 기기 |
| | `~<machine_id>` | 다른 기계(`machines.json` 의 `machine_id`) — KasaChrome 도 같은 기계를 잡는다 |
| | `phone:<이름>` | 폰(`mobile-users.json` 의 사용자 이름) |
| `browse_open` | `chrome`(기본) | 그 기기의 브라우저(맥은 기본 브라우저, 폰은 사파리) |
| | `web` | 내장 웹 — 맥은 요청한 pane 의 **탭**으로, 폰은 앱 안 웹 화면으로 |

기계를 고르면 `kasachrome_machine`/`kasachrome_bridge_urls` 도 함께 맞춘다(있던 키).
폰을 고르면 KasaChrome 은 이 기계 크롬에 남고, 새 탭에 그 폰의 화면 크기를 흉내 낸다.

## HTTP

- `GET /browse/devices` →
  `{"ok":true,"open":"chrome","selected":"phone:geono","auto":false,
    "devices":[{"id":"","label":"이 기기","kind":"desktop","online":true},
               {"id":"~1a2b","label":"MacBook","kind":"desktop","online":true},
               {"id":"phone:geono","label":"geono 폰","kind":"phone","online":true,
                "model":"iPhone","viewport":{"width":393,"height":852,"dpr":3}}]}`
  `online` — 폰은 제어 소켓이 붙어 있을 때, 기계는 명부 폴링이 살아 있을 때.
- `POST /settings/action` `{"action":"browse-device","id":"<id>"}` — 위 `id`. `"auto"` 를 주면 키를 지운다.
- `POST /settings/action` `{"action":"browse-open","id":"web"|"chrome"}`.
- `POST /mobile/device` (폰 인증) `{"width":393,"height":852,"dpr":3,"model":"iPhone","platform":"ios"}`
  → `{"ok":true,"id":"phone:<이름>"}`. 논리 픽셀(CSS px) 로 준다. `mobile-devices.json` 에 남는다.
- `GET /mobile/ws` (폰 인증, WebSocket) — 폰 제어 채널. 앱이 앞에 있는 동안 붙어 있는다.
  - 서버→폰 `{"t":"hello","name":"<이름>","id":"phone:<이름>"}` 접속 직후.
  - 서버→폰 `{"t":"open-url","url":"…","mode":"web"|"chrome","req":"<번호>"}`.
  - 폰→서버 `{"t":"opened","req":"<번호>","ok":true}` 또는 `{"t":"opened","req":"…","ok":false,"error":"…"}`.
  - 폰→서버 `{"t":"ping"}` 은 무시된다(살아 있음 확인용).
- `GET /open-url?url=&pane=&local=1[&web=1]` — 다른 기계가 되돌려 보낼 때. `web=1` 이면
  받는 쪽이 브라우저 대신 내장 웹 탭으로 연다.

## 검증

- 맥: `KASATERM_OPEN_URL_SINK=<파일>` 이면 실제 브라우저 대신 `<from>\t<url>` 한 줄.
- 폰: `opened` 응답이 오면 상태줄 토스트에 「폰에서 열렸어요」, 안 오면 3초 뒤 「폰이 응답하지 않아요」.
- 기계: 상대 `/open-url` 의 `{"ok":true}` 가 곧 그 기계의 `open_url_here` 가 돈 것이다.
