# 워크스페이스 — 크레이트 층과 의존 방향

카사텀 Rust 워크스페이스의 크레이트가 어느 층에 있고, 누가 누구에게 기댈 수 있는지를 정한다.
검사는 `python3 scripts/check-workspace.py` 다. 크레이트를 더하거나 의존을 바꾸면 이 문서와
검사 표(`LAYERS`)를 함께 고친다. 엔진을 떼는 큰 그림은 [`terminal-engine.md`](terminal-engine.md),
본판 앱 안 모듈 지도(부팅 `bootstrap`·칸 shim `pane_shims`·`session/` 자식 포함)는 [`code-map.md`](code-map.md)다.
큰 크레이트의 안쪽 지도는 그 진입 파일 머리말이 정본이다 — `kasa-pty` 는 `crates/kasa-pty/src/state.rs`(`state/`),
`kasa-http` 의 HTTP 서버는 `crates/kasa-http/src/http.rs`(`http/`).

## 1. 층

의존은 **순위가 더 작은 크레이트로만** 흐른다. 같은 순위끼리도 기대지 않는다.

| 층 | 순위 | 크레이트 | 맡는 것 | 창·GPU 의존 | 네트워크 스택 |
|---|---|---|---|---|---|
| 핵심 | 0 | `kasa-screen` | 셀·색·행·`ScreenUpdate`·ANSI 직렬화·리플로우·칸 배치 낱말 | 금지 | 금지 |
| 핵심 | 0 | `kasa-ime` | 두벌식 한글 조합기. 의존 0 | 금지 | 금지 |
| 핵심 | 0 | `kasa-cells` | 글리프 아틀라스·셰이퍼·wgpu 셀 파이프라인. crates.io 공개용 | 허용 | 금지 |
| 핵심 | 1 | `kasa-pty` | PTY·VT(alacritty)·스크롤백·OSC·kitty 그림·fd 인계·칸 등록부·`PtyLayout` | 금지 | 금지 |
| 핵심 | 1 | `kasa-bridge` | 옛 tmux 제어 모드 백엔드(`KASATERM_BACKEND=tmux`). 화면 낱말은 `kasa-screen` 재수출 | 금지 | 금지 |
| 핵심 | 1 | `kasa-gridview` | 칸 하나 그리기: 격자·커서·선택·조합 글자·인라인 그림·P3 레이어 | 허용 | 금지 |
| 프로토콜 | 2 | `kasa-socket` | 줄 단위 JSON-RPC·서버·전송(유닉스 소켓·named pipe)·`kasaterm-cli`·앱 업데이트 계약 | 금지 | 금지 |
| 전송 | 2 | `kasa-net` | 카사넷 — 기기끼리 iroh QUIC 직통 길 | 금지 | 허용 |
| 전송 | 3 | `kasa-net-ffi` | 카사넷 C ABI(폰 앱 정적 라이브러리). 산출물이라 받는 쪽이 없다 | 금지 | 허용 |
| 협업 | 3 | `kasa-agents` | 하네스 지식: Claude Code·Codex 대화 기록, CLI 위치·공용 명부 캐시 | 금지 | feature 뒤만 |
| 협업 | 3 | `kasa-acp` | ACP 클라이언트와 칸 runner `kasa-acp` — Claude Code·Codex 어댑터를 stdio JSON-RPC 로 부린다(`docs/acp.md`). 디스패처(`kasa-students`)가 받는다 | 금지 | 금지 |
| 협업 | 4 | `kasa-collab` | 보드 수집기·tell 장부·칸 열쇠·기계 id. 다른 기계와의 HTTP 는 feature `net` | 금지 | feature 뒤만 |
| 호스트 | 5 | `kasa-identity` | 관문 계정·OAuth·권한 연결·봉인 자격의 공통 구현 | 금지 | 허용 |
| 호스트 | 5 | `kasa-characters` | 캐릭터 자산·페르소나·세션 표시 규칙 | 금지 | 허용 |
| 호스트 | 5 | `kasa-wire` | 터미널·화면 전송 형식과 레이아웃·변경 피드 | 금지 | 허용 |
| 호스트 | 5 | `kasa-host` | 창 없는 카사텀 — 다른 프로그램(나쵸)의 셸 세션을 `kasa-pty` 로 붙들고 유닉스 소켓으로 내준다. 산출물이라 받는 쪽이 없다 | 금지 | 금지 |
| 호스트 | 6 | `kasa-device` | 이 기기 계정·기기 명부·폰 입구·업링크·계정 동기화. kasa-device 실행 파일 | 금지 | 허용 |
| 호스트 | 7 | `kasa-backend` | 창 없는 Backend 구현과 거울 시험용 호스트 | 금지 | 허용 |
| 호스트 | 7 | `kasa-students` | 학생 디스패치·팀·명령 중계·예약·작업 HTTP 표면 | 금지 | 허용 |
| 호스트 | 8 | `kasa-remote` | 원격 칸·포트 전달·기기 화면·레포 전송·대화 보관 전송 | 금지 | 허용 |
| 호스트 | 9 | `kasa-browser` | 브라우저 도착 기기·localhost 연결·카사브라우저 폰 명령 중계. 엔진·UI는 별도 Go 앱 | 금지 | 허용 |
| 호스트 | 9 | `kasa-git` | Git 작업·상태 판·파일 감시·셸 경로 판독 | 금지 | 허용 |
| 호스트 | 9 | `kasa-gateway` | 폰 관문 서버·계정 라우팅·설치판 배포 창구. kasa-relay 실행 파일 | 금지 | 허용 |
| 호스트 | 10 | `kasa-http` | 이 기기 HTTP 라우터·칸·화면 API와 서버 수명 | 금지 | 허용 |
| 호스트 | 11 | `kasa-serve` | 독립 HTTP 서버 kasa-serve-web 실행 파일 | 금지 | 허용 |
| 앱 | 12 | `kasaterm` | 본판 GUI 실행 파일. 산출물이라 받는 쪽이 없다(바탕화면 펫 `kasapet` 은 2026-10-19 레포 밖으로 뗐다) | 허용 | 허용 |
| 실험 | — | `spikes/*` | 렌더러·IME 시험판. 제품이 아니다 | — | — |

주요 내부 의존(공통 하위 의존은 마지막 줄에 묶었다):

```text
kasaterm (12) ─┬─ kasa-http (10) ─┬─ kasa-git (9) ── kasa-remote (8)
               │                  ├─ kasa-students (7) ── kasa-device (6)
               │                  ├─ kasa-remote (8) ── kasa-device (6)
               │                  ├─ kasa-browser (9) ── kasa-remote (8), kasa-device (6)
               │                  └─ kasa-wire · kasa-characters (5)
               ├─ kasa-device (6) ── kasa-identity · kasa-characters (5)
               └─ kasa-agents (3), kasa-gridview, kasa-pty, kasa-socket
kasa-gateway (9) ── kasa-remote (8), kasa-device (6), kasa-identity (5)
kasa-serve (11) ── kasa-http (10), kasa-backend (7) ── kasa-device (6)
kasa-http ── kasa-backend (7) [시험]
호스트 공통 ── kasa-collab (4) · kasa-agents (3) · kasa-net (2) · kasa-socket (2)
kasa-collab ── kasa-socket · kasa-pty ── kasa-screen
kasa-agents ── kasa-socket
kasa-gridview ── kasa-cells · kasa-screen
kasa-net-ffi ── kasa-net
kasa-acp (3, 내부 의존 없음) ← kasa-students (7)
kasa-host ── kasa-pty · kasa-screen
spikes/* ── kasa-bridge ── kasa-screen
```

### 층을 이렇게 나눈 이유

- **핵심은 GUI·협업·호스트를 모른다.** 본판·카사라이트·`kasa tui` 가 같은 엔진을 git rev 로 받는다.
  `kasa-pty` 가 학생·보드를 알게 되면 TUI 와 라이트가 그 무게를 함께 진다.
- **프로토콜은 핵심 위, 협업 아래다.** `kasa-socket` 은 줄 단위 계약과 전송이라 GUI 없는 호스트
  (`kasa tui`·관문)도 싣는다. 협업이 그 계약(`Backend`·`board`·`tell`)을 쓰지, 반대로 가지 않는다.
- **전송은 협업·호스트 아래다.** `kasa-net` 은 길만 낸다. 무엇을 실어 나를지는 호스트가 정한다.
- **호스트는 협업·전송·프로토콜·핵심을 엮는다.** 호스트 안에서도 의존 순서대로 5~11을 나눈다.
  `kasa-gateway`가 리눅스 관문을 만들므로 이 층 전체는 창·GPU 없이 돈다.
- **계정과 업링크의 상태 수명은 한 곳에 둔다.** 로그인·계정 동기화·기기 발견·업링크는 서로 상태를 갱신하므로
  `kasa-device`에 함께 둔다. 억지로 쪼개 콜백이나 이중 상태를 만들지 않는다. HTTP 조립·관문 서버·원격 클라이언트는
  이 상태를 아래 방향으로만 읽는다.
- **클라이언트와 서버를 함께 띄우는 시험은 위에 둔다.** 원격 왕복 시험은 `kasa-http`에 있고 순수 클라이언트 시험은
  `kasa-remote`에 남는다. 개발 의존으로 층을 거슬러 순환을 만들지 않는다.
  공용 목업은 `test-support`, 기존 시험의 계정·네트워크 격리는 `test-isolation`으로 개발 의존에서만 잇는다.
  일반 바이너리 빌드는 이 기능들을 켜지 않는다.
- **브라우저 앱과 연결 호스트는 따로 둔다.** `kasa-browser`는 URL 도착 기기·폰 명령 중계만 맡고,
  별도 `kasabrowser-go`의 엔진·화면·프로필을 싣지 않는다. 코드 소속과 `kb`·`kc`의 구분은
  [browser-integration.md](browser-integration.md)가 정한다.
- **바이너리·HTTP·설정 계약은 유지한다.** 패키지는 갈렸지만 `kasa-relay`, `kasa-device`, `kasa-serve-web` 이름과
  경로·설정 키·파일 위치는 유지한다. 공유 동기화 기능은 제거했다. 기존 사용자 폴더와 바탕화면 링크를 지우는 마이그레이션은 없다.
- **`kasa-bridge` 는 줄여 가는 크레이트다.** 화면 낱말은 `kasa-screen` 으로 옮겼고(재수출로 옛 경로 유지),
  `kasa-http` 도 이제 `kasa-screen` 을 바로 받는다. 남은 소비자는 본판(옛 tmux 백엔드와 옛 경로
  `kasa_bridge::screen` 을 쓰는 곳)과 실험뿐이다.

## 2. 검사 규칙

`scripts/check-workspace.py` 가 `cargo metadata --no-deps` 와 각 `Cargo.toml` 을 읽어 본다.
내려받기·빌드는 하지 않아 몇 초면 끝난다.

| 규칙 | 막는 것 |
|---|---|
| 경계 역전 | 순위가 같거나 높은 크레이트에 기대기(일반·빌드·개발 의존 모두) |
| 층 미정 | `LAYERS` 에 없는 제품 크레이트, 워크스페이스에 없는 표 항목 |
| 산출물 | `kasaterm`·`kasa-net-ffi`·`kasa-host`·`kasa-gateway`·`kasa-serve`를 받는 크레이트 |
| 실험 | 실험 크레이트에 기대기, 실험이 `default-members` 에 들기, 제품이 `default-members` 에서 빠지기 |
| 옛 tmux 백엔드 | 본판 밖 제품 크레이트가 `kasa-bridge` 에 새로 기대기 |
| 다른 이름 | 내부 크레이트를 `package =` 로 다른 이름에 받기(경로가 감춰진다) |
| 창·GPU 의존 | 「창·GPU 의존」이 금지인 크레이트에 winit·wgpu·wry·muda·objc2-app-kit·raw-window-handle 등 |
| 네트워크 스택 | 핵심·프로토콜 층의 tokio·reqwest·axum·hyper·tower·iroh, 협업 층의 optional 아닌 것 |
| 가벼운 소비자 | 호스트보다 아래 층이 `kasa-socket`·`kasa-pty` 를 기본 기능(또는 `app-update`·`os-clipboard`)째 받기 |
| 기본 기능 끔 | reqwest·image·zip·similar·pulldown-cmark·objc2-intents 를 기본 기능째 받기 |
| 공개 크레이트 | `kasa-cells` 에 경로 의존이 끼기, description·license·repository 빠지기 |
| 판 모으기 | 제품 크레이트 둘 이상이 같은 외부 크레이트 판을 따로 적기, `[workspace.dependencies]` 에 있는 것을 따로 적기, 내부 크레이트를 `path =` 로 받기 |
| 안 쓰는 항목 | 아무도 받지 않는 `[workspace.dependencies]` 항목 |
| 버전 줄 | 루트 `Cargo.toml` 의 첫 `version = ` 줄이 `[workspace.package]` 버전이 아님 |

`--self-test` 는 지금 트리에 위반을 하나씩 심어 각 규칙이 잡는지 확인한다.

## 3. `[workspace.dependencies]`

- **제품 크레이트 둘 이상이 쓰는 외부 크레이트와 모든 내부 크레이트**를 루트 한 곳에 둔다.
  크레이트는 `x.workspace = true` 로 받고 자기 몫 `features`·`optional` 만 더한다. 한 곳에서만
  쓰는 외부 크레이트는 그 크레이트 `Cargo.toml` 에 남긴다(왜 쓰는지 주석이 그 옆에 있어야 읽힌다).
- **default-features 는 루트가 정한다.** 루트에서 끈 것은 받는 쪽이 `default-features = true` 로만
  다시 켤 수 있고, 루트에서 켠 것은 받는 쪽이 `default-features = false` 를 적어도 꺼지지 않는다
  (cargo 가 경고만 내고 무시한다). 그래서:
  - `kasa-socket`·`kasa-pty` 는 루트에서 끈다. 앱·호스트(`kasaterm`·`kasa-http`)만
    `{ workspace = true, default-features = true }` 로 받는다. `kasa-agents`·`kasa-collab` 은 그냥
    `workspace = true` 로 받아 ring·arboard 가 새지 않는다.
  - `reqwest`·`image` 는 루트에서 끄고 쓰는 기능만 켠다(rustls·형식 다섯).
- **features 는 더하기만 된다.** 루트 항목의 features 에 받는 쪽 것이 더해진다. 그래서 루트에는 모두가
  함께 쓰는 것만 둔다(`serde` 의 derive, `uuid` 의 v4). `tokio`·`serde_json`·`objc2-*`·`windows-sys`
  는 루트에 판만 두고 기능은 크레이트마다 적는다.
- **실험은 따르지 않아도 된다.** `spikes/*` 는 내부 크레이트만 루트에서 받고, 외부 판은 시험 당시
  그대로 둔다(`iced-term` 의 `tokio = "1"` 처럼 제품과 판 요구가 다른 것이 있다).
- **판을 올리지 않는다.** 이 표는 같은 판 요구를 한 곳에 모은 것이다. 판 올리기는 따로 한다.

`miniz_oxide`는 `kasa-pty`의 Kitty 그림 해제와 `kasa-wire`의 PC 터미널 미러 압축이 함께 쓰므로
기존 0.8 판 요구를 루트에 둔다. 미러는 64KiB 조각을 무손실 압축·제한 해제하며 네이티브 라이브러리나 GUI 의존을 더하지 않는다.

## 4. default-members 와 실험

- 루트 `default-members` 는 제품 크레이트 전부(`app/*`·`crates/*`)다. `-p` 없이 루트에서 부르는
  `cargo build`·`cargo test`·`cargo check` 는 실험을 굽지 않는다.
- 실험까지 보려면 `--workspace` 를 붙이거나 `-p iced-term` 처럼 고른다.
- 굽기·릴리스 스크립트는 처음부터 `-p` 로 크레이트를 고르므로 이 변경의 영향이 없다.
  resolver 2 는 함께 고른 패키지끼리만 기능을 합치므로, `-p kasaterm` 결과물도 그대로다.

## 5. 매니페스트만 바꿀 때 증명하는 법

의존 의미(판 요구·target·features·default-features·optional·이름 바꾸기·경로)가 그대로인지는
`--dump` 를 전후로 떠서 비교한다.

```bash
python3 scripts/check-workspace.py --dump > /tmp/ws-before.json
# … Cargo.toml 수정 …
python3 scripts/check-workspace.py --dump > /tmp/ws-after.json
diff /tmp/ws-before.json /tmp/ws-after.json      # 의도한 의존만 달라야 한다
cargo tree --offline --target all -p <크레이트> -e normal,build -f '{p} [{f}]' --prefix none | sort -u
```

`cargo tree` 의 기능 목록까지 같으면 실제로 굽는 것이 같다. `Cargo.lock` 은 git 에 없으니 전후
사본을 따로 떠서 비교한다.

## 6. 자동 검사

`.github/workflows/workspace.yml` 이 `Cargo.toml`(루트·크레이트 전부)·`scripts/check-workspace.py`·이 문서·
그 워크플로 자신이 바뀐 push·PR 에서 검사와 `--self-test` 를 돌린다. `cargo metadata --no-deps` 만
부르므로 빈 `CARGO_HOME`·`Cargo.lock` 없는 체크아웃에서도 아무것도 받지 않고 끝난다.

로컬에서는 매니페스트를 고친 뒤 같은 두 줄을 돌린다.

```bash
python3 scripts/check-workspace.py && python3 scripts/check-workspace.py --self-test
```

## 7. 알려진 빚

검사가 막지 않지만 경계를 흐리는 것들이다. 고치면 여기서 지운다.

- **본판이 화면 낱말을 옛 경로로 받는다.** `app/kasaterm` 이 `kasa_bridge::screen` 을 100곳 넘게 쓴다.
  `kasa_screen` 으로 옮기면 본판의 `kasa-bridge` 의존은 옛 tmux 백엔드(`TmuxSession`)만 남고,
  `LEGACY_DEPENDENTS` 를 더 좁힐 수 있다.
- **호스트가 PTY 기본 기능을 싣는다.** `kasa-backend`·`kasa-remote`·`kasa-http`는 `kasa-pty` 기본 기능(`os-clipboard` → arboard)을 유지한다.
  관문도 원격 클라이언트를 통해 이 기능을 받는다. `kasa-serve-web` 칸의 OSC 52 가 이 기능에 기대므로
  그 동작을 정하기 전에는 끄지 않는다.
- **무거운 실험은 굽힘을 장담하지 않는다.** `iced-term`·`egui-term`·`winit-ime-test`·`p3-poc` 는
  `default-members` 밖이라 평소 검사에 안 든다. 다시 쓰려면 `cargo check -p <이름>` 부터 본다.
