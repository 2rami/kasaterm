# 나쵸 읽기 중계 — 키 없는 기기의 권한 표

나쵸 앱 키(`nacho-app.key`)는 나쵸가 도는 기기 한 곳에만 있다. 키가 없는 기기도 권한 표를 **읽을** 수 있게,
키가 있는 기기의 카사텀이 고정된 읽기 창구를 연다. 쓰는 길은 없다.

나쵸 쪽 계약(읽기 범위 `X-Kasa-Read: 1`, 응답 모양, 거절어)은 나쵸 레포
`docs/development/api/desk-api.md` 의 「읽기 범위」가 정본이다. 여기는 카사텀 쪽 몫만 적는다.
작업 모드(`work-mode`)와 작업 장부(`tasks`)는 나쵸가 카사텀 학생을 다루던 길이라 2026-10-14 에 함께 걷었다.

## 창구 (`crates/kasa-mcp/src/nacho_relay.rs` `read_relay`)

- `GET /nacho/read/capabilities` 하나뿐. 그 밖의 이름은 404, GET 밖은 405.
- 로컬 호출만 받는다. 기기 사이 SSH 터널(`-L <port>:127.0.0.1:8765`)은 로컬로 들어오니 명부 기기가 부를 수 있다.
  `x-forwarded-for`·`cf-connecting-ip` 가 붙은 요청(공용 터널·`/m/<기계>` 대리를 거친 원격)은 403.
- 폰 주소(`/u/<slug>/…`)로는 열지 않는다(403). 폰은 주인 중계(`/nacho/app/*`)를 그대로 쓴다.
- 호출자가 보낸 헤더·쿼리·몸통은 하나도 옮기지 않는다. 나쵸에 가는 헤더는 여기서 새로 짓는다:
  `X-Nacho-Token`(이 기기 키 파일) · `X-Kasa-Read: 1` · `X-Kasa-Owner: 0` · `X-Kasa-User: relay-read` · `X-Kasa-Machine`.
  주인이라고 말하지 않는다.
- 키가 없으면 503(`nacho_key_missing`), 나쵸에 못 닿으면 502(`nacho_unreachable`).

진짜 경계는 이 허용목록과 헤더 새로 짓기다. 나쵸의 `read_only` 거절은 그 뒤의 이중 방어다.

## 받은 값의 쓰임

- 앱 재시작의 나쵸 권위(`NachoAuthority`)·승인·권한 판단은 이 길을 모른다 — 자기 키로 바로 묻는다.
- 데스크톱의 읽는 쪽(작업 탭·보드 판)은 걷었다(2026-10-01, 나쵸 대화·작업은 독립 앱). 창구는 관문·폰 쪽 길로 남는다.

## 확인 방법

- `cargo test -p kasa-mcp nacho_relay` — 허용목록·405·403(원격·폰)·503, 헤더를 새로 짓는지.
- 실제 나쵸에 읽기만: `cargo test -p kasa-mcp live_read_relay -- --ignored` (키가 있는 기기에서, GET 하나뿐).
