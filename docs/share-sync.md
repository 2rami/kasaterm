# KASA-share — 결과물 폴더를 기기끼리 맞추기

학생이 만든 시안·스크린샷·문서를 어느 기기에서 만들었든 **모든 기기의 바탕화면과 폰**에서 보게 한다.
옮기는 것은 카사텀 자신이다(구글 드라이브·Syncthing 없음). 코드는 `crates/kasa-mcp/src/share/`.

## 자리

- 실폴더 `~/.config/kasaterm/share`, 바탕화면 `KASA-share` 는 거기를 가리키는 링크(맥 symlink, 윈도우 정션).
  바탕화면을 실폴더로 두지 않는 이유: iCloud 바탕화면 동기화와 겹치고, 「저장 공간 최적화」가 파일을 비우면
  지운 것으로 읽혀 삭제가 다른 기기로 번진다. 같은 이름이 이미 있으면 링크를 안 만든다.
- 학생은 `kasaterm-cli share new <주제>` 가 찍어 주는 `<날짜>-<주제>/` 에 넣는다(전역 지침·kasapane 스킬 `collab.md`).
- 색인·상태·휴지통은 `<share>/.kasaterm/` — `index.json`, `status.json`, `trash/<ms>/`(7일), `tmp/*.part`.

## 판정 (`version.rs`)

파일마다 기기별 번호표(version vector)와 sha256. mtime 은 「바뀌었나」의 힌트일 뿐 판정에 안 쓴다 —
기기 시계가 어긋나고 `cp -p`·압축 풀기가 옛 mtime 을 들고 온다. 번호는 `max(이전+1, 지금 ms)` 라 색인을 잃어도
뒤로 안 간다.

| 상대 판 | 하는 일 |
|---|---|
| 내 것 이하 | 없음 |
| 내 것보다 새것 | 받기 / 지운 판이면 휴지통 |
| 엇갈림, 내용 같음 | 번호표만 합침(처음 맞추는 두 기기도 여기) |
| 엇갈림, 지움 vs 고침 | 고친 쪽이 산다 |
| 엇갈림, 둘 다 고침 | 한쪽이 경로를 갖고 다른 내용은 `이름 (만든 기기).ext` 사본으로 — 어느 쪽도 안 잃는다 |

받은 판은 원래 번호표 그대로 다시 내주므로 맥북끼리 직접 길이 없어도 맥미니를 거쳐 퍼지고, 가끔 켜지는
윈도우도 켜질 때 따라잡는다. 지운 기록은 영구 보관(오래 꺼져 있던 기기가 되살리지 않게).

## 흐름 (`mod.rs`·`pull.rs`)

- 3초마다 훑기. 두 번 연속 크기·mtime 이 그대로여야 판을 매긴다(쓰는 중인 파일 제외). 링크·숨김·`*.part`·
  `Thumbs.db` 등은 무시, 1GB 넘는 파일은 만든 기기에만.
- 받은 파일은 rename 직후 색인에 적어 다시 판이 안 오른다(되돌려 보내기 없음). 받기 전에 이 기기 사본이
  그사이 바뀌었으면 손대지 않고 다음 판으로 미룬다.
- 한 번에 30% 넘게(10개 이상) 사라지면 삭제를 멈추고 `share status` 에 적는다 → `kasaterm-cli share accept-deletes`.
- 다른 기기는 `machines::snapshot()` 의 온라인 기기, 길은 보드와 같다(`known_route` → 직통 base → 관문 우회).
  목록은 `since`/`epoch` 로 바뀐 것만 받고, 파일은 4MB 조각으로 받아 sha 로 검증한다 — 관문은 큐가 넘치면
  본문을 자른 채 정상 종료로 보내므로 받은 데부터 잇는다.
- 앱 본체에서만 돈다(`http.rs` 의 `run_scheduler`). 격리 리그는 `KASATERM_SHARE_DIR` 을 줘야 돈다.

## 창구 (`serve.rs`)

`GET /term/share/manifest?since=&epoch=`, `GET /term/share/file?path=&sha=&offset=&len=`(기기 사이 조각 받기),
`GET /term/share/f/<경로>`(폰이 여는 주소 — html 안의 상대 자원이 같은 폴더로 풀린다), `GET /term/share/list`(폰).
읽기 전용·폴더 밖 불가(조상에 링크가 끼면 거부)·주인 아닌 폰 주소 403. 파일 응답에는
`Content-Security-Policy: sandbox` — 학생이 만든 html 이 관문 주소에서 스크립트를 돌려 `/send` 로 셸에 치지 못하게.

## 검증

- `cargo test -p kasa-mcp share::` — 판정·이름·훑기·휴지통 단위 테스트, 기기 넷(A↔M↔B, 늦게 켜지는 W)
  시뮬레이션, 두 엔진을 진짜 HTTP 로 붙인 통합 테스트(9MB 조각 받기·충돌·지움·보안).
- 격리 앱 둘: `docs/verify-app.md` 부팅 줄에 `KASATERM_SHARE_DIR`·`KASATERM_MACHINE_ID`·`KASATERM_SELF_LABEL`·
  `KASATERM_MACHINES_FILE`·`KASATERM_MOBILE_USERS`·`KASATERM_GATEWAY=off` 를 더해 띄우고, 로그에서 포트를 읽어
  서로의 base 를 명부 파일에 적는다.
