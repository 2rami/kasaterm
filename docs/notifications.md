# 알림 — 한 사건에 한 줄 (2026-09-10)

사람에게 닿는 알림 통로는 넷이다. 같은 사건이 여러 통로로 들어와도 **각 통로에서 한 번**만
보이게 하는 것이 규칙이고, 그 열쇠는 아래 표다.

| 통로 | 발사구 | 열쇠(중복 접기) | 창 |
|---|---|---|---|
| 데스크톱 배너 | `chrome.rs notify_desktop` | `done:<pane>` / `approval:<pane>` / `acct-exhausted:…` | 8초(`NOTIFY_DEDUP_WINDOW`) |
| 폰 푸시(APNs) | `push.rs send` — `push_loop`(상태 감시)·`note_arrived`(나쵸 쪽지)·`browse` | collapse `wait-<route>/<pane>` · `done-<route>/<pane>` · `note-…` + `recently_sent` 15초 | `MIN_GAP` |
| 슬랙 DM | 나쵸네코 `panewatch` | `host\x1fsurface` 상태 서명 | 90초 폴 |
| 디스코드 방 | 나쵸네코 `taskwatch`(감시 건) — `boardwatch` 는 taskwatch 가 안 보는 pane 만 | `host\x1fsurface` | 60/90초 폴 |

## 같은 사건이 두 길로 오는 곳과 막는 자리

- **턴 완료**: Stop 훅(`kasaterm-cli notify`)과 OSC 777 이 몇 초 차이로 둘 다
  `handle_notify` 에 온다 → 열쇠 `done:<pane>` 로 접는다. 거울 pane 의 OSC 777 은
  원본 기계가 이미 알렸으니 `session.rs` 에서 버린다.
- **승인 대기**: Notification 훅(`⚠ 권한 필요`, 종류를 안다)과 화면 감지(`⚠ 승인 필요`,
  종류를 모른다)가 같은 `approval:<pane>` 열쇠. 화면 감지는 훅이 적어 둔 attention
  표식을 **덮지 않는다** — 덮으면 종류가 번갈아 바뀌어 폰이 두 번 운다.
- **폰**: `push_loop` 가 상태 전이를 보고 쏘고, 나쵸가 같은 사건의 쪽지를 `/term/notes`
  로 넣으면 `note_arrived` 가 또 쏘던 것 → 같은 collapse 열쇠 + 15초 공유 문(`recently_sent`).
  `push_loop` 의 마지막 상태는 한 바퀴 안 보였다고 잊지 않는다(60초) — 하네스 감지가
  잠깐 빠지거나 원격 캐시가 늙으면 「새로 기다리기 시작」으로 또 울리던 원인.
- **나쵸네코**: 세 감시(`panewatch`·`taskwatch`·`boardwatch`)가 같은 board 를 각자
  읽는다. `boardwatch` 는 같은 방에서 `taskwatch` 가 이미 보는 pane 을 빼고, host 없는
  항목을 미니·맥북 양쪽에 등록하지 않는다(남의 pane 이 잡히던 원인).

## 종류 어휘

`permission` · `question` · `waiting`(답 기다림) · `idle`(오래 기다림) · `done_ok` · `done_fail` · `dead`.
훅이 준 종류가 정본이고, 화면 감지는 훅이 없을 때만 `permission` 을 적는다. 모르는
사유는 「막힘(사유 모름)」이지 「답 다 써놓고 기다림」이 아니다.
