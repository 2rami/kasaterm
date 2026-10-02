# 폰·아이패드 설치 링크(Ad Hoc)

TestFlight 를 거치지 않고 KASATERM 폰 앱을 링크 하나로 넣는 길. 개인 팀의 TestFlight 설치가 애플 서버에서
막혔을 때(2026-10-01) 만들었다. 애플 규칙상 등록된 기기에만 깔리고, 등록은 링크를 연 기기가 스스로 한다.

## 사람이 보는 흐름

1. 링크(`https://kasaterm.debimarlene.com/relay/install/<token>/`)를 폰 Safari 에서 연다.
2. 처음인 기기: 「기기 등록」 → 「허용」 → 설정 앱 위쪽 「프로파일이 다운로드됨」 → 「설치」 한 번.
   폰이 고유번호를 보내면 관문이 개발자 계정에 등록하고 앱을 그 기기까지 넣어 다시 서명한다(보통 1분 안).
3. Safari 가 같은 링크의 진행 화면으로 돌아오고, 끝나면 「설치」.

이미 등록된 기기는 1 없이 바로 「설치」. 앱은 관리자 계정으로 로그인돼 있으면 새 판이 올라왔을 때
알림 줄과 설정의 「앱 판」 칸으로 알린다. 허브가 앞에 있는 동안 `/relay/install/latest?have=<가진 빌드>&wait=40` 에
늘 매달려 있고, 관문은 다른 빌드가 올라오는 순간(1초 안) 답한다 — 쓰는 중에도 곧바로 뜬다. `waits` 가 없는
옛 관문이면 30초마다 묻는다.

## 판 올리기

```sh
mobile/tool/adhoc.sh          # 아카이브 → Ad Hoc ipa → 미니에서 재서명 → 링크 공개, KASA-share 에 링크
mobile/tool/adhoc.sh --show   # 링크를 사람이 보는 기기로도 띄운다
```

관문 기계(서명 준비가 있는 기계)에서 돌리면 ssh 없이 그 자리에서 올리고, 개발 인증서 없이 서명 없는 아카이브를 그 기계의 배포 인증서로 한 번에 서명한다(권한은 프로파일에서 고르되 `keychain-access-groups` 는 뺀다 — 넣으면 폰에 저장된 로그인을 못 읽는다). 판마다 token 이 새로 나오고 지금 판과 바로 앞 판만 남는다. 기기 손 등록은 `mobile/tool/asc.py device <UDID> <이름>`.

## 어디서 무엇이 도나

| 자리 | 하는 일 |
|---|---|
| `mobile/tool/adhoc.sh` (이 맥) | 아카이브·내보내기, ipa·`meta.json` 을 미니 `~/.config/kasaterm/relay-install/.up-<token>/` 로, 서명기 파일도 함께 부친다 |
| `mobile/tool/adhoc_sign.py` (미니) | ASC 기기 등록, 기기 전부를 든 Ad Hoc 프로파일 재발급, ipa 재서명, 기기 수 `devices.json`, 등록 프로파일 서명 |
| `crates/kasa-mcp/src/gateway_install.rs` (관문) | 설치 페이지·manifest·ipa·`/relay/install/latest`, 관리 화면 칸. 2026-10-02 부터 관문은 서울, 설치 창구는 미니의 설치 전용 관문(서울이 역터널로 넘김) — `latest`·관리 화면만 서울이라 `adhoc.sh` 가 판을 서울에도 놓는다(`docs/seoul-gateway.md`) |
| `crates/kasa-mcp/src/gateway_install/enroll.rs` (관문) | UDID 받는 Profile Service, 일회용 challenge, 작업 실행·기록 |

서명 키는 미니의 전용 키체인 `~/Library/Keychains/ios-adhoc.keychain-db` 에만 있다(암호 파일
`~/.config/kasaterm/asc/ios-adhoc.pw`). 관문은 서명기에 판 경로·UDID·이름만 넘긴다.

## 관문 설정

launchd plist `EnvironmentVariables`:

- `KASA_INSTALL_SIGNER` = `/Users/<사용자>/.local/share/kasa-relay/adhoc/adhoc_sign.py` — 없으면 자동 등록 단계를 감춘다.
- `KASA_INSTALL_DAILY_CAP` — 하루 새 기기 등록 상한(기본 10). 애플 한도는 기종마다 멤버십 한 해 100대이고
  해지한 기기도 갱신 전까지 센다. 바닥나면 한 해를 못 쓰므로 낮게 둔다.

env 를 바꾸면 `kickstart` 로는 안 먹는다 — `launchctl bootout` 뒤 `bootstrap`.

미니 준비(한 번): `~/.local/share/kasa-relay/adhoc/adhoc_sign.py setup` — Apple Distribution 인증서를 API 로
받아 전용 키체인에 넣고 사용자 검색 목록에 올린다. codesign 은 `--keychain` 을 줘도 검색 목록에 없는 키체인에서는
신원을 못 찾는다(10-01 실측).

## 남용 막기와 기록

- 링크 token(판마다 32자)이 있어야 등록 프로파일이 나오고, challenge 는 그 token 에 묶인 30분 일회용이다.
- IP 마다 10분에 10번, 새 기기 등록은 하루 상한까지.
- 폰이 보낸 CMS 서명은 검증하지 않는다 — 위 제한이 막고, 값은 꼴 검사 뒤 서명기 인자로만 간다.
- 모든 시도는 `relay-install/registrations.jsonl`(때·결과·기종·이름·UDID·IP)에 남고, 관리 화면
  (`/relay/admin`)이 기종별 기기 수·올해 남은 자리·최근 등록 20건을 보인다(UDID 는 끝 6자만).
