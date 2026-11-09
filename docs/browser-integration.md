# 브라우저 앱과 카사텀의 연결 경계

카사브라우저는 별도 Go 저장소 `kasabrowser-go`의 앱이다. 데스크톱은 MyGo UI와 CEF OSR,
폰은 그 저장소의 `mobile/`에 있는 Flutter·OS 웹뷰를 쓴다. 카사텀의 `kasa-browser` 크레이트는
기기 간 URL 전달과 폰 명령 중계를 맡는다. 브라우저 엔진·탭·프로필은 이 크레이트에 두지 않는다.

## 코드 소속

| 맡는 일 | 소속 |
|---|---|
| 데스크톱 탭·주소창·기록·북마크·다운로드·페이지 입력 | `kasabrowser-go`: `internal/browser`, `internal/shell`, `internal/osrengine`, `internal/osr` |
| 브라우저 명령과 로컬 연결 | `kasabrowser-go`: `cmd/kb`, `internal/control`, `internal/bridge` |
| 폰 브라우저 화면·탭·웹뷰·명령 실행 | `kasabrowser-go/mobile`: `lib/browser`, `lib/engine`, `lib/agent` |
| 선택한 기기로 URL 열기, 원본 기기의 localhost 연결 | `crates/kasa-browser/src/target.rs`, `route.rs` |
| 폰 브라우저 명령 대기열·결과 전달 | `crates/kasa-browser/src/phone.rs` |
| 폰 명령 HTTP 입구·소유자 검사 | `crates/kasa-http/src/http/phone_browser.rs` |
| 명령 RPC·CLI 계약 | `crates/kasa-socket`: `Backend::phone_browser`, RPC `phone.browser`, CLI `phone browser` |
| 계정·기기 명부·푸시·직통 연결 | `kasa-identity`, `kasa-device`, `kasa-net` |
| 계정 관문·기기별 라우팅 | `kasa-gateway` |
| 기존 Google Chrome 프로필 조작 | `kasachrome/`: MV3 확장·로컬 브리지·`kc`·`kj` |
| 터미널 안 문서·웹 미리보기와 URL 열기 연결 | `app/kasaterm/src/webpane.rs`, `pane_shims.rs` |

`kasa-browser`는 호스트 순위 9이며 `kasa-remote`(8)의 일반 포트 전달·연결 인증과
`kasa-device`(6)의 기기 명부·푸시를 사용한다. `kasa-http`(10)와 앱(12)이 이를 받는다.
GUI·CEF를 의존하지 않고, 낮은 층에서 브라우저 크레이트로 되돌아가는 의존도 없다.

## 사용 기준

브라우저 조작과 인증은 카사브라우저의 `kb`를 사용한다. 연결이 없거나 지원하지 않는 동작이면
카사브라우저의 실행·설치·지원 여부를 확인하며 Google Chrome·`kc`·`kj`로 자동 전환하지 않는다.
기존 Chrome 프로필·로그인·북마크는 가져오기 원본으로 보존한다.

| 하려는 일 | 도구 |
|---|---|
| 카사브라우저 데스크톱 탭 읽기·조작 | `kb tabs`, `kb inspect`, `kb command`; `kb --help`와 `kb commands`에서 실제 계약 확인 |
| 로그인 항목 선택·안전한 채우기 | `kb command`의 `login.list`·`login.fill`; 비밀 값을 출력하거나 자동 제출하지 않는다 |
| 카사브라우저 폰 앱에 일 맡기기 | `kb command`의 `phone.*`, 또는 `kasaterm-cli phone browser` |
| 사람이 보는 기기에 링크 열기 | `kasaterm-cli open <url>`; 카사텀이 도착 기기를 정하고 OS 브라우저로 연다 |

OS 기본 브라우저 설정과 에이전트가 조작할 브라우저 선택은 별개다.
`kasachrome_machine`은 URL 도착 기기에도 쓰이는 기존 설정 키이므로 이 정리에서 바꾸지 않았다.

구현에는 기존 `kasachrome/` 도구, Chrome 전용 기기 연결, 격리 재인증의 Chrome 실행 경로가 남아 있다.
이들은 현재 사용 기준과 구분하며, Chrome 지원 폐기 때 함께 이행할 대상이다.
브라우저 엔진인 CEF/Chromium과 Chrome 데이터 가져오기는 Google Chrome 앱 조작과 구분한다.

## 폰 연결 계약

```text
kb phone.* → kasaterm-cli phone browser → RPC phone.browser
                                         ↓
                              kasa-browser::phone 대기열
                                         ↕
                 kasa-http /phone/browser/next · /phone/browser/result
                                         ↕ 계정 관문 또는 직통
                       카사브라우저 mobile/lib/agent → 웹뷰
```

실제 `kb` 호출은 `kb command '{"name":"phone.text"}'`처럼 이름 있는 명령을 사용한다.
폰이 `GET /phone/browser/next?wait=25`로 명령을 받아 실행한 뒤
`POST /phone/browser/result?id=...`에 JSON 또는 PNG를 올린다. 경로·RPC 이름·CLI 인자와
기존 푸시 분류 `phone-web`은 유지한다. 옛 `/phone/web/*`와 `phone web`은 복구하지 않는다.
주인이 아닌 폰은 거절한다. 브라우저가 열려 있지 않을 때 알림은 현재 카사텀 폰 앱으로 간다.

카사텀 폰 앱의 링크 보기·설치·계정 화면은 계속 카사텀 소속이다. 별도 브라우저 앱의 탭 상태나
프로필을 그 화면에 복제하지 않는다. 브라우저 자체 푸시와 데스크톱 탭의 폰 실시간 조작은 별도 구현 과제다.
[예전 Chrome 거울 설계](chrome-mirror.md)는 미구현 제안이며 현재 기능의 계약이 아니다.

## 변경 검증

- `cargo test -p kasa-browser -p kasa-device -p kasa-remote -p kasa-http`
- `cargo test -p kasa-socket`, `cargo test -p kasaterm --bin kasaterm`
- `cargo clippy --all-targets`, `python3 scripts/check-workspace.py`
- 브라우저 저장소의 연결 변경: `go test ./internal/kasaterm ./internal/browser ./internal/control ./cmd/kb`

카사텀 연결 변경은 카사텀 preview 절차로 낸다. 카사브라우저 엔진·UI 변경은 그 저장소의
격리 빌드·검증·설치 절차를 따른다. 사용자 브라우저·로그인 프로필을 재시작하거나 교체해서 검증하지 않는다.
