# 나쵸 읽기 중계 — 키 없는 기기의 작업 모드·권한 표

나쵸 앱 키(`nacho-app.key`)는 나쵸가 도는 기기 한 곳에만 있다. 키가 없는 기기의 데스크톱도 작업 탭에서
지금 모드와 권한 표를 **읽을** 수 있게, 키가 있는 기기의 카사텀이 고정된 읽기 창구를 연다.
쓰는 길은 없다 — 키 없는 기기에서는 모드를 바꾸지 못한다(나쵸가 도는 기기나 폰에서 바꾼다).

나쵸 쪽 계약(읽기 범위 `X-Kasa-Read: 1`, 응답 모양, 거절어)은 나쵸 레포
`docs/development/api/desk-api.md` 「작업 모드·기능 안내」의 「읽기 범위」와 고정 자료
`fixtures/work_mode.implemented.json` 의 `get_read` 가 정본이다. 여기는 카사텀 쪽 몫만 적는다.

## 창구 (`crates/kasa-mcp/src/nacho_relay.rs` `read_relay`)

- `GET /nacho/read/work-mode`, `GET /nacho/read/capabilities` 두 개뿐. 그 밖의 이름은 404, GET 밖은 405.
- 로컬 호출만 받는다. 기기 사이 SSH 터널(`-L <port>:127.0.0.1:8765`)은 로컬로 들어오니 명부 기기가 부를 수 있다.
  `x-forwarded-for`·`cf-connecting-ip` 가 붙은 요청(공용 터널·`/m/<기계>` 대리를 거친 원격)은 403.
- 폰 주소(`/u/<slug>/…`)로는 열지 않는다(403). 폰은 주인 중계(`/nacho/app/*`)를 그대로 쓴다.
- 호출자가 보낸 헤더·쿼리·몸통은 하나도 옮기지 않는다. 나쵸에 가는 헤더는 여기서 새로 짓는다:
  `X-Nacho-Token`(이 기기 키 파일) · `X-Kasa-Read: 1` · `X-Kasa-Owner: 0` · `X-Kasa-User: relay-read` · `X-Kasa-Machine`.
  주인이라고 말하지 않는다.
- 모드 응답의 `changed_by`(사람 이름)는 나쵸가 빼고, 중계도 한 번 더 뺀다.
- 키가 없으면 503(`nacho_key_missing`), 나쵸에 못 닿으면 502(`nacho_unreachable`).

진짜 경계는 이 허용목록과 헤더 새로 짓기다. 나쵸의 `read_only` 거절은 그 뒤의 이중 방어다.

## 읽어 올 기기 (`app/kasaterm/src/work_mode.rs`)

- 설정 `nacho_read_via` — 작업 탭에서 사람이 한 번 누른 명부 기기 이름. 요청마다 고르지 않는다.
- 찾는 곳은 **사람이 적은 명부**(`machines::listed_machines`)뿐이다. 알려 온 손님 기기·폴링으로 배운 id 는 대상이 못 된다.
  명부에 없는 이름이면 아무 데도 묻지 않고 그렇다고 적는다.
- 키가 있는 기기는 이 길을 안 탄다(자기 키로 바로 묻는다).

## 받은 값의 쓰임

- 작업 탭에 「○○ 경유로 읽은 값」으로 **표시만** 한다. 마지막 확인값(`work_mode_cache`)은 갱신한다.
- 모드 단추는 계속 꺼진다. 앱 재시작의 나쵸 권위(`NachoAuthority`)·승인·권한 판단은 이 길을 모른다 —
  `work_mode::tests::relayed_reads_never_reach_the_restart_authority` 가 지킨다.
- 작업 장부(`tasks`)·대화·이벤트는 읽지 않는다. 키 없는 기기의 조율 장부는 따로 정한다.

## 확인 방법

- `cargo test -p kasa-mcp nacho_relay` — 허용목록·405·403(원격·폰)·503, 헤더를 새로 짓는지, `changed_by` 빼기.
- `cargo test -p kasaterm work_mode native_board::side` — 명부 기기만 대상, 경유 값도 쓰기 꺼짐, 권위와 안 섞임.
- 나쵸 고정 자료 대조: `NACHO_DESK_FIXTURES=<나쵸>/docs/development/api/fixtures cargo test -p kasaterm nacho_read_scope_fixture_matches -- --ignored`
- 실제 나쵸에 읽기만: `cargo test -p kasa-mcp live_read_relay -- --ignored` (키가 있는 기기에서, GET 둘뿐).
