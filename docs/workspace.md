# 워크스페이스 — 크레이트 층과 의존 방향

카사텀 Rust 워크스페이스의 크레이트가 어느 층에 있고, 누가 누구에게 기댈 수 있는지를 정한다.
검사는 `python3 scripts/check-workspace.py` 다. 크레이트를 더하거나 의존을 바꾸면 이 문서와
검사 표(`LAYERS`)를 함께 고친다. 엔진을 떼는 큰 그림은 [`terminal-engine.md`](terminal-engine.md),
본판 앱 안 모듈 지도(부팅 `bootstrap`·칸 shim `pane_shims`·`session/` 자식 포함)는 [`code-map.md`](code-map.md)다.
큰 크레이트의 안쪽 지도는 그 진입 파일 머리말이 정본이다 — `kasa-pty` 는 `crates/kasa-pty/src/state.rs`(`state/`),
`kasa-mcp` 의 HTTP 서버는 `crates/kasa-mcp/src/http.rs`(`http/`).

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
| 협업 | 3 | `kasa-agents` | 하네스 지식: Claude Code·Codex 대화 기록 읽기 | 금지 | feature 뒤만 |
| 협업 | 4 | `kasa-collab` | 보드 수집기·tell 장부·칸 열쇠·기계 id. 다른 기계와의 HTTP 는 feature `net` | 금지 | feature 뒤만 |
| 호스트 | 5 | `kasa-mcp` | 이 기기 HTTP 호스트(보드·웹 칸·원격 방·모바일)·관문 `kasa-relay`·`kasa-device`·`kasa-serve-web`. 이름은 MCP 서버였던 시절의 것 | 금지 | 허용 |
| 앱 | 6 | `kasa-pet-config` | 본판과 펫이 함께 읽는 펫 설정. 앱만 받는다 | 금지 | 허용 |
| 앱 | 7 | `kasaterm`·`kasapet` | 본판 GUI·바탕화면 펫 실행 파일. 산출물이라 받는 쪽이 없다 | 허용 | 허용 |
| 실험 | — | `spikes/*` | 렌더러·IME 시험판. 제품이 아니다 | — | — |

지금 내부 의존(일반·빌드):

```text
kasaterm ─┬─ kasa-mcp ─┬─ kasa-collab ─┬─ kasa-socket
          │            │               └─ kasa-pty ── kasa-screen
          │            ├─ kasa-net
          │            ├─ kasa-socket
          │            ├─ kasa-pty
          │            └─ kasa-screen
          ├─ kasa-agents ── kasa-socket
          ├─ kasa-gridview ─┬─ kasa-cells
          │                 └─ kasa-screen
          ├─ kasa-bridge ── kasa-screen
          ├─ kasa-pty · kasa-socket · kasa-ime · kasa-cells · kasa-pet-config
kasapet ── kasa-pet-config
kasa-net-ffi ── kasa-net
spikes/* ── kasa-bridge
```

### 층을 이렇게 나눈 이유

- **핵심은 GUI·협업·호스트를 모른다.** 본판·카사라이트·`kasa tui` 가 같은 엔진을 git rev 로 받는다.
  `kasa-pty` 가 학생·보드를 알게 되면 TUI 와 라이트가 그 무게를 함께 진다.
- **프로토콜은 핵심 위, 협업 아래다.** `kasa-socket` 은 줄 단위 계약과 전송이라 GUI 없는 호스트
  (`kasa tui`·관문)도 싣는다. 협업이 그 계약(`Backend`·`board`·`tell`)을 쓰지, 반대로 가지 않는다.
- **전송은 협업·호스트 아래다.** `kasa-net` 은 길만 낸다. 무엇을 실어 나를지는 호스트가 정한다.
- **호스트는 협업·전송·프로토콜·핵심을 엮는다.** `kasa-mcp` 가 GUI 없이 도는 것은 관문(리눅스)에서
  `kasa-relay` 를 굽기 때문이다. 창·GPU 의존이 들어가면 관문 굽기가 깨진다.
- **`kasa-bridge` 는 줄여 가는 크레이트다.** 화면 낱말은 `kasa-screen` 으로 옮겼고(재수출로 옛 경로 유지),
  `kasa-mcp` 도 이제 `kasa-screen` 을 바로 받는다. 남은 소비자는 본판(옛 tmux 백엔드와 옛 경로
  `kasa_bridge::screen` 을 쓰는 곳)과 실험뿐이다.

## 2. 검사 규칙

`scripts/check-workspace.py` 가 `cargo metadata --no-deps` 와 각 `Cargo.toml` 을 읽어 본다.
내려받기·빌드는 하지 않아 몇 초면 끝난다.

| 규칙 | 막는 것 |
|---|---|
| 경계 역전 | 순위가 같거나 높은 크레이트에 기대기(일반·빌드·개발 의존 모두) |
| 층 미정 | `LAYERS` 에 없는 제품 크레이트, 워크스페이스에 없는 표 항목 |
| 산출물 | `kasaterm`·`kasapet`·`kasa-net-ffi` 를 받는 크레이트 |
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
  - `kasa-socket`·`kasa-pty` 는 루트에서 끈다. 앱·호스트(`kasaterm`·`kasa-mcp`)만
    `{ workspace = true, default-features = true }` 로 받는다. `kasa-agents`·`kasa-collab` 은 그냥
    `workspace = true` 로 받아 ring·arboard 가 새지 않는다.
  - `reqwest`·`image` 는 루트에서 끄고 쓰는 기능만 켠다(rustls·형식 다섯).
- **features 는 더하기만 된다.** 루트 항목의 features 에 받는 쪽 것이 더해진다. 그래서 루트에는 모두가
  함께 쓰는 것만 둔다(`serde` 의 derive, `uuid` 의 v4). `tokio`·`serde_json`·`objc2-*`·`windows-sys`
  는 루트에 판만 두고 기능은 크레이트마다 적는다.
- **실험은 따르지 않아도 된다.** `spikes/*` 는 내부 크레이트만 루트에서 받고, 외부 판은 시험 당시
  그대로 둔다(`iced-term` 의 `tokio = "1"` 처럼 제품과 판 요구가 다른 것이 있다).
- **판을 올리지 않는다.** 이 표는 같은 판 요구를 한 곳에 모은 것이다. 판 올리기는 따로 한다.

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
- **호스트가 PTY 기본 기능을 싣는다.** `kasa-mcp` 가 `kasa-pty` 를 기본 기능(`os-clipboard` → arboard)째
  받아 관문 `kasa-relay` 굽기에도 arboard 가 들어간다. `kasa-serve-web` 칸의 OSC 52 가 이 기능에 기대므로
  그 동작을 정하기 전에는 끄지 않는다.
- **무거운 실험은 굽힘을 장담하지 않는다.** `iced-term`·`egui-term`·`winit-ime-test`·`p3-poc` 는
  `default-members` 밖이라 평소 검사에 안 든다. 다시 쓰려면 `cargo check -p <이름>` 부터 본다.
