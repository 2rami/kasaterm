# 북마크·탭 동기화 (kasaterm 쪽)

카사브라우저의 북마크를 같은 계정의 기기끼리 합치고, 열린 탭은 기기마다 보낸다. 설계 전체와 브라우저 쪽 창구는
kasabrowser `docs/browser-sync.md`, 금고 열쇠와 관문 규칙은 [웹 로그인 동기화](login-vault-sync.md)다.
코드는 `kasa-device` `browser_sync`, 관문 `gateway_login_vault.rs` 의 `store_routes`, 설정 「계정」의 「북마크·탭 동기화」.

## 열쇠

- 금고 열쇠 `K` 를 그대로 쓰지 않고 `VaultKey::subkey("browser-sync")`(HKDF 정보 `subkey:browser-sync`)로 가른 열쇠를 쓴다.
  표식(`key_id`)·쓰기 서명·봉인·항목 이름이 모두 금고와 달라, 한쪽 덩어리나 서명을 다른 쪽에 끼워 넣지 못한다.
- 따로 등록하지 않는다. 금고 열쇠가 있고 사람이 지문을 확인한(`unconfirmed-<계정>` 이 없는) 기기만 일한다.
  `K` 원본은 밖으로 내보내지 않고 갈래만 쓴다.

## 켜기

- 「북마크 맞추기」·「탭 보내기·받기」를 따로 켠다. 둘 다 기본은 꺼짐이고 계정마다 `~/.config/kasaterm/browser-sync/settings-<계정>.json` 에 둔다.
- 북마크를 다시 켜면 관문을 처음부터 다시 받는다(꺼 둔 사이의 변경을 놓치지 않게). 탭을 끄면 내 탭 항목에 묘비를 달고
  받은 다른 기기 탭도 브라우저에서 지운다. 둘 다 끄고 뒷정리가 끝나면 관문에 묻지 않는다.

## 한 바퀴 (15초, 켜기·끄기는 앞당김)

1. 관문(`/relay/browser-sync`)에서 새 항목을 받는다. 기록이 없으면 갈래의 표식·서명 공개키로 만든다(처음 온 기기).
   관문 시계·차례 되돌림·안 풀리는 덩어리 규칙은 금고와 같다.
2. 브라우저 창구(`sync.read`)로 모음과 탭을 읽는다. 브라우저가 꺼져 있으면 그 바퀴는 상태를 건드리지 않고 넘어간다.
3. 모음마다 로컬 변경(내용·자리·삭제)을 찾고 관문 판과 묶음별로 합친 뒤, 계산한 트리가 되도록 `sync.apply`(읽은 rev 위)로 고친다.
   그사이 사람이 고쳤으면(`SYNC_STALE`) 그 모음은 아무것도 안 바꾸고 다음 바퀴로 미루며 관문 차례도 앞당기지 않는다.
4. 바뀐 기록을 200개·768KB 씩 서명해 올린다. 부딪히면 다음 바퀴가 새 판을 받아 다시 합친다.
5. 탭은 30초 묶어 내 항목 하나로 올리고, 다른 기기 탭이 바뀌었으면 `sync.tabs.put` 으로 넘긴다.
6. 180일 지난 묘비를 걷는다. 로그는 `[browser-sync] 올림 N · 브라우저 고침 N · …` 개수만.

## 브라우저 창구

- kb 와 같은 길이다. 맥은 데이터 폴더(`~/Library/Application Support/Kasa Browser OSR`)의 `agent.sock`(남이 쓸 수 있는 폴더면
  붙지 않음), Windows 는 `%APPDATA%\Kasa Browser OSR\agent.endpoint` 의 루프백 주소에 토큰을 보내지 않고 HMAC 으로 서로
  증명한다(`kasa-browser-bridge-v1`). 시험·격리 실행만 `KASATERM_BROWSER_DATA_DIR` 로 폴더를 바꾼다.

## 관문

- 금고의 받기·만들기·쓰기 창구를 `store_routes(경로, 보관소)` 로 나눠 같은 꼴로 연다. 보관소는 `browser-sync/<계정>.json`,
  한도 `RELAY_LIMITS`(항목 2만·덩어리 96KB·파일 64MB). 등록 창구는 없다. 금고 보관소의 한도·동작은 그대로다.

## 시험

- `cargo test -p kasa-device --lib browser_sync` — 가짜 브라우저(kasabrowser `bookmarks.Tree.Apply` 규칙)와 시험 관문으로 두 기기
  수렴, 이름 바꿈+옮김, 삭제 대 나중 고침, 지운 폴더에 더한 것, 서로 안으로 옮긴 폴더, 같은 가져오기, 큰 삭제 보류, STALE,
  탭 켜고 끄기, 브라우저 꺼짐, 하나 옮기면 하나만 올림, 다른 열쇠 거절, 450개 나눠 올리기, 자리 글자열 길이. 모음·주소는
  `kasatest-`·`kasa-test.example` 만.
- 진짜 Go 창구: kasabrowser `bridge.Listen` 으로 가짜 sync 서버를 띄우고
  `KASATERM_BROWSER_DATA_DIR=<폴더> cargo test -p kasa-device --lib browser_sync::bridge -- --ignored`.
- 관문: `cargo test -p kasa-gateway login_vault`(갈래 보관소가 금고와 따로이고 금고 서명으로는 못 쓰는지).
