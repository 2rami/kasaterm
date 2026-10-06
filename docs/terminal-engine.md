# 공통 터미널 엔진 — 카사텀·새 카사라이트·`kasa tui`

상태: **설계**(2026-10-06). 코드는 아직 바꾸지 않았다. 이 문서는 세 제품이 같은 코드를 쓰게 하려면
무엇을 어디로 떼는지, 새 카사라이트와 TUI 판을 어떤 순서로 세우는지를 정한다.

- **카사텀**: 지금 본판 GUI(`app/kasaterm`). 학생·보드·나쵸·계정·웹뷰까지 다 있는 판.
- **새 카사라이트**: 바닐라 GUI 터미널. 칸 나누기·탭·한글·kitty 그림만 있고, 120Hz로 가볍게 돈다.
- **`kasa tui`**: 앱을 깔지 않고 아무 터미널(Ghostty·iTerm2·Windows Terminal·SSH 너머 서버) 안에서
  tmux처럼 도는 다중 칸 터미널이다. 카사텀 협업(보드·tell·학생 상태·done·summon)이 함께 돈다.

## 0. 결정 (2026-10-06)

| 갈림길 | 결정 |
|---|---|
| 큰 방향 | 공통 엔진 하나 위에 새 카사라이트와 TUI 판을 함께 세운다 |
| 레포 | 새 카사라이트와 TUI는 **kasalite 레포**(`github.com/2rami/kasalite`)에 함께 둔다 |
| 엔진 원본 | **kasaterm `crates/`** 에 둔다. kasalite는 git 커밋을 고정해 가져간다 |
| TUI 이름 | 명령은 `kasa`이고 TUI는 하위 명령 **`kasa tui`** 로 연다. 패키지 이름은 **`kasa-tui`**(npm의 `kasa`는 남의 것) |
| 첫 단계 | **TUI 판 먼저**. 렌더러를 떼는 일은 그 뒤 라이트 단계에서 한다 |
| TUI 첫 판 범위 | 처음부터 **협업까지**(보드·tell·학생 상태·done·summon) |

엔진을 본판에 둔 이유: `kasa-pty` 는 본판에서 가장 자주 고치는 곳이다. 엔진을 kasalite로 옮기면 본판에서
PTY를 고칠 때마다 레포를 오가야 한다. kasalite는 엔진을 올릴 때만 바꿔 받는다. 그래서 「업데이트 안 하는
터미널」이라는 라이트의 성격도 그대로 남는다.

## 1. 지금 모습 (실측)

줄 수는 origin/main `d5b818fd` 기준이다. 렌더 함수의 구간 분류만 `8d21cbeb` 에서 쟀다(차이는 30줄 안쪽).

### 1.1 그리기 쪽 — 한 함수에 다 엉켜 있다

- `render.rs` 는 14,881줄이고, 그중 `render_frame_gpu` 하나가 **12,063줄**이다.
  - 앞머리에 지역 `let` 이 154개 있다.
  - 본문 약 10,140줄이 `if let Some(g) = self.gpu.as_mut()` 블록 하나 안에 있다.
  - `self` 멤버 221개를 792번 직접 읽는다.
  - 지역 `macro_rules!` 가 11개 있다. 상태줄 위젯 10개가 둘레의 지역 변수를 잡아 쓴다.
  - 지역 클로저가 19개이고, 그중 13개가 지역 변수를 잡는다.
- 구간을 나누면 터미널 그리기는 **≈2,040줄(17%)**, 부가 기능은 **≈10,050줄(83%)** 이다.

  | 부가 기능 | 줄 수 |
  |---|---|
  | 상태줄(창 아래 줄·칸 아래 줄·드롭다운) | ≈2,040 |
  | 사이드바 | ≈1,850 |
  | 계정 메뉴·판 알림 | ≈1,490 |
  | Git·Info·세션·MCP 열과 커밋 창 | ≈1,390 |
  | 파일 트리 | ≈930 |

  터미널 그리기 쪽은 셀 그리기·인라인 그림·커서/선택/조합 중 글자(`paint_gpu_overlays`)·창 탭 줄·
  칸 경계·칸 번호·도크·닫기 확인 창이다.
- `terminal_scene.rs` 의 `compose_terminal_pane`(1,462줄)
  - 터미널 핵심은 **65~100줄**뿐이다: 행을 폭에 맞추기, OSC 1337·kitty 그림 칸 만들기.
  - 나머지는 학생·claude 화면을 고쳐 쓰는 코드다.
  - 읽는 `self` 멤버 18개 가운데 엔진에 필요한 것은 `cell` 하나다.
- `gpu.rs`(7,323줄)
  - `GpuRenderer` 필드 54개 중 약 16개가 마크다운·편집기, 3개가 날씨다. 파일의 약 35%가 마크다운·편집기다.
  - `crate::theme` 을 95곳에서 부른다. `theme.rs` 안에는 학생 외형 판정도 섞여 있다.
- `struct App` 은 337필드이고, 그중 터미널 핵심은 약 100개다.
- **feature 플래그가 하나도 없다**(`[features]` 0, `cfg(feature)` 0).
  - 라이트 모드는 런타임 분기 약 66곳으로만 갈린다.
  - 그래서 라이트 바이너리도 wry·tree-sitter·kasa-mcp를 전부 링크한다.

### 1.2 엔진이 될 크레이트 — 생각보다 이미 떨어져 있다

| 크레이트 | 줄 수 | 지금 상태 |
|---|---|---|
| `kasa-pty` | 10,670 | PTY·VT·스크롤백·OSC 133/1337·kitty(`kitty.rs` 1,197)·인계·칸 나누기 모델(`PtyLayout`) |
| `kasa-bridge` | 2,434 | 셀·색·`ScreenUpdate` 공용 낱말에 옛 tmux 백엔드가 섞여 있다 |
| `kasa-cells` | 1,650 | 글리프 아틀라스·셰이퍼(swash)·wgpu 셀 파이프라인 |
| `kasa-ime` | 486 | 두벌식 조합기. 의존 0 |
| `kasa-socket` | 18,284 | 줄 단위 JSON 프로토콜·서버·전송·`kasaterm-cli`·협업 낱말·앱 업데이트 |
| `kasa-mcp` | 60,395 | HTTP 서버(axum·tokio·reqwest·iroh)·**보드 수집기·tell 장부**·원격 칸 |

- **`kasa-pty` 는 GUI 없이 돈다.**
  - winit·wgpu 의존이 없다. alacritty_terminal 0.26 타입을 밖에 내놓지 않는다(밖으로는 `kasa_bridge::screen` 만 나간다).
  - `kasa-serve-web` 이 이미 이것을 GUI 없이 쓴다.
  - 엉켜 있는 것:
    - 에이전트 감지 ≈900줄. 그중 60%가 에이전트 전용이다.
    - tell 입력 가드 ≈170줄.
    - `TERM_PROGRAM=kasaterm` 이 고정돼 있다.
    - OSC 52가 arboard로만 간다. SSH 너머에서는 틀린 동작이다.
    - 「Last login」 줄이 `~/.config/kasaterm` 에 쓴다.
- **`PtyLayout` 도 GUI와 무관하다.**
  - 칸 좌표만 다룬다. 본판 10개 파일이 87곳에서 쓰고, 직렬화는 serde다.
  - 칸 사이 경계 칸을 따로 남기지 않는다. TUI는 테두리 칸을 직접 떼야 한다.
- **협업은 무겁게 엉켜 있다.**
  - 보드 수집기(`board_service`)와 tell 장부(`tell_service`)는 kasa-mcp 안에 있다. 수집기는 `spawn_http_server` 안에서만 켜진다.
  - 그래서 옛 라이트는 「collaboration collector is not running」 을 낸다.
  - 앱에만 있는 것:
    - 보드 행(`collab_board_source`)
    - tell 전달 루프(`tell_delivery.rs` 942)
    - claude 훅 shim 설치(`main.rs` ≈575)
    - 입력창 판독(`screenread.rs` 8,627줄 중 순수 판정 부분)
    - 상태 정본(`agent_state.rs` 1,052)
    - 기록 읽기(`transcript.rs` 2,330)
  - 이 가운데 GUI에 의존하는 것은 없다. 자리만 앱에 있다.

### 1.3 무게와 체감

- **옛 카사라이트 v0.1.0**
  - 2026-09-22 본판 `64c7e0eb` 를 통째로 복사한 판이다. 그 뒤 본판에서 `app/`·`crates/` 를 건드린 커밋 167개를 따라오지 못했다(+59,215 / −7,016줄).
  - 바이너리 92MB의 내역:

    | 부분 | 크기 |
    |---|---|
    | 코드 | 30.9MB |
    | 상수 | 36.8MB |
    | 링크 정보 | 15.4MB |

    strip·LTO가 없다. 박힌 글꼴이 ≈21MB, 학생 스프라이트가 ≈11.5MB다. WebKit을 포함해 프레임워크 21개를 링크한다.
  - kitty가 옛 임시판이라, 그림을 본판 8765 포트로 보낼 수 있다.
- **본판 `.app`**: 161MB.
- **본판 체감**(2026-10-06 실측, 계측 코드는 미커밋)

  | 항목 | 값 |
  |---|---|
  | 그리기 | p50 10.5ms · p95 16ms |
  | 표시 | 84fps, 프레임 간격 p99 36ms |
  | 키 반향 | p50 0.8ms |
  | 키→프레임 | p50 10.9ms · p95 18.6ms |
  | 실행→첫 프레임 | 1.5초 |
  | 실물 메모리(칸 14개 남짓) | 757MB, 최고 1.0GB |

  **120Hz 한 프레임(8.33ms)보다 그리기가 더 오래 걸린다.**
- **프레임 조절**
  - `PresentMode::AutoNoVsync` 에 `desired_maximum_frame_latency: 1` 을 쓴다.
  - 애니메이션은 33ms/8ms `WaitUntil` 펌프로 돌린다.
  - 디스플레이 링크는 없다.

## 2. 엔진 경계

### 2.1 원칙

1. **엔진 크레이트는 학생·나쵸·계정·웹뷰를 모른다.** 의존은 엔진 → 호스트 한쪽으로만 흐른다.
2. **원본은 kasaterm `crates/` 다.** 옮길 때는 옛 경로를 재수출해, 본판 호출부를 0줄 바꾸는 것부터 한다.
3. **엔진 크레이트에는 LFS 파일과 큰 자산을 두지 않는다.**
   - cargo가 git 의존을 받을 때는 LFS를 풀지 않는다.
   - 시험 크레이트로 확인했다(§7): `kasa-cells` 의 박힌 글꼴 두 개가 LFS 포인터 그대로 들어온다.
   - 그래서 글꼴 바이트는 호스트가 넘기게 바꾼다.
4. **VT는 alacritty_terminal 0.26을 유지한다.**
   - libghostty-vt(Rust 바인딩 0.2.2)는 지금 쓰지 않는다. 이유는 셋이다.
     - 빌드에 Zig 0.16이 필요해서, brew·npm·winget 굽는 배관이 무거워진다.
     - API가 아직 1.0 전이다.
     - 타입이 `!Send` 다. 지금 kasa-pty는 `Term` 을 Mutex로 여러 스레드가 함께 쓴다.
   - kasa-pty가 alacritty 타입을 밖에 내놓지 않는 지금의 경계만 지키면, 나중에 갈아 끼울 수 있다.

### 2.2 크레이트

| 크레이트 | 맡는 것 | 출처 | 쓰는 쪽 |
|---|---|---|---|
| `kasa-screen` (새) | `Cell`·`Color`·`Row`·`ScreenUpdate`·`InlineImageView`·`CellClip`·ANSI 직렬화·reflow | kasa-bridge `screen.rs`·`reflow.rs`(≈1,350). kasa-bridge는 옛 tmux 백엔드만 남기고 재수출 | 셋 다 |
| `kasa-pty` (다이어트) | PTY·VT·스크롤백·OSC 133·OSC 1337·kitty·fd 인계·세션 등록부·`PtyLayout` | 그대로. 아래 다섯 가지를 뺀다 | 셋 다 |
| `kasa-keys` (새, 작음) | 키 → 바이트 인코더(커서 키 모드·kitty 키보드 플래그·브래킷 붙여넣기) | `input.rs` `forward_key`(1,039줄) 안의 순수 부분 | 셋 다 |
| `kasa-agents` (새) | 프로세스 표·에이전트 표(30종)·프롬프트 앵커·출력 박동·화면 신호(스피너·승인 위젯·입력창 판독)·`agent_state`·`transcript` | kasa-pty 감지부 ≈900, `screenread` 순수 판정부, `agent_state.rs`, `transcript.rs` | 본판·TUI |
| `kasa-collab` (새) | 보드 수집기·tell 장부·tell 전달 루프·claude 훅/칸 shim 설치·`Backend` 협업 메서드 기본 구현. 기기 사이 관측은 feature `net` | kasa-mcp `board_service`·`tell_service`, 앱 `tell_delivery.rs`·`install_claude_hook_shim`·`install_pane_shims`·`collab_board_source` | 본판·TUI(라이트는 선택) |
| `kasa-socket` (유지) | 프로토콜·서버·전송(유닉스 소켓·윈도우 named pipe)·`kasaterm-cli` | 앱 업데이트·재시작(3,918줄)은 feature로 빼서 TUI에 안 실리게 | 셋 다 |
| `kasa-ime` (유지) | 두벌식 조합 | 그대로 | 본판·라이트(GUI만) |
| `kasa-cells` (정리) | 아틀라스·셰이퍼·wgpu 셀 파이프라인 | 아래 두 가지를 정리한다 | 본판·라이트 |
| `kasa-gridview` (새) | 칸 하나 그리기: 셀 격자·커서·선택·조합 중 글자·인라인 그림(업로드·잘라 그리기·내쫓기)·글꼴 찾기·팔레트·macOS 레이어/P3·프레임 박자 | 아래 출처 목록 | 본판·라이트 |

`kasa-pty` 에서 뺄 다섯 가지:

- 에이전트 감지 → `kasa-agents`
- 「Last login」 → 호스트
- 환경 이름(`TERM_PROGRAM` 등) → `PtyOptions` 의 env 정책
- OSC 52 → `ClipboardSink` 훅. TUI는 바깥 터미널로 OSC 52를 다시 낸다.
- 셀 픽셀 크기 → 옵션

`kasa-cells` 에서 정리할 두 가지:

- 앱 전용 깃발 3개(`FLAG_WORKING_BAR`·`FLAG_PULSE_BAR`·`FLAG_COMPACT_BAR`) → 일반 「장식 띠」
- 박힌 글꼴 → 호스트가 바이트로 넘긴다

`kasa-gridview` 의 출처:

- `gpu.rs` 핵심 ≈4,700줄
- `render.rs` 의 `paint_gpu_overlays`·`sync_ime_cursor_area`
- `screenread.rs` 의 `paint_inline_images`
- `cells.rs` 의 팔레트
- `terminal_scene.rs` 의 행 폭·그림 칸

엔진에 넣지 않는 것:

- **칸 나누기 크롬**(탭 줄·경계선·칸 이름줄). 크롬은 제품마다 모양이 달라 각자 둔다. 레이아웃 계산(`PtyLayout`)만 함께 쓴다.
- **설정 파일 읽기**. 엔진은 `TermConfig`(글꼴·크기·팔레트·스크롤백·셸) 값만 받고, 파일은 호스트가 읽는다.

```text
                 kasa-screen
               ┌──────┴───────────────┐
           kasa-pty                kasa-cells
     ┌────────┼────────┐                │
 kasa-keys  kasa-agents │          kasa-gridview ── kasa-ime
              │         │                │
          kasa-collab ──┴─ kasa-socket   │
              │                          │
   ┌──────────┼──────────────┬───────────┘
 본판 GUI   kasa tui     새 카사라이트 GUI
(kasaterm) (kasalite)     (kasalite)
```

### 2.3 본판이 갈아타는 법

- `kasa-screen`·`kasa-keys`·`kasa-agents`·`kasa-collab` 은 옛 경로를 재수출하고 옮긴다. 호출부는 그대로 둔다.
- `render_frame_gpu` 는 손대지 않는다. 핵심 조각 네 개만 `kasa-gridview` 호출로 바꾼다.
  - 셀 그리기
  - 오버레이
  - 인라인 그림
  - 그림 업로드
- `compose_terminal_pane` 은 둘로 나눈다. 엔진이 행과 그림 칸을 만들고, 앱이 장식 패스로 그 위를 고쳐 쓴다.
- 12,063줄 함수를 쪼개는 일은 이 설계의 범위가 아니다. 라이트와 TUI는 그 함수를 쓰지 않는다. **본판의 120Hz는 이 설계로 풀리지 않는다.**

## 3. `kasa tui` (kasalite 레포, 첫 단계)

### 3.1 명령

| 명령 | 하는 일 |
|---|---|
| `kasa tui` | 서버에 붙는다. 서버가 없으면 띄운다 |
| `kasa attach [이름]` | 떨어진 세션에 다시 붙는다 |
| `kasa ls` | 세션 목록을 본다 |
| `kasa kill <이름>` | 세션을 끝낸다 |
| `kasa server` | 서버 본체(내부용) |
| `kasaterm-cli …` | 같은 바이너리를 이 이름으로 부르면 CLI로 돈다(멀티콜). 칸 shim 폴더에 링크를 둔다 |

나중에 `kasa` 만 치면 라이트 창이 뜨게 한다(§4).

### 3.2 구조

- **서버와 클라이언트를 한 바이너리에 둔다.**
  - 서버가 쥐는 것: PTY(`kasa-pty`), 레이아웃(`PtyLayout`), 방·탭, 협업 호스트(`kasa-collab`)
  - 클라이언트가 하는 일: 바깥 터미널을 날 모드로 잡고, 그리고, 입력만 넘긴다.
  - 창을 닫아도 칸과 학생은 산다.
- **레이아웃 권한은 서버 한 곳에만 둔다.**
  - 2026-06의 데몬이 죽은 원인이 레이아웃 권한 이중화였다.
  - 클라이언트는 「나눠 줘」「옮겨 줘」 같은 의도만 보낸다.
- **서버와 클라이언트 사이 통신**
  - 길: 유닉스 소켓 / 윈도우 named pipe, 길이 머리 + bincode
  - 서버 → 클라이언트: 칸별 `ScreenUpdate` 차분, 레이아웃, 보드 행
  - 클라이언트 → 서버: 키 바이트, 마우스, 크기, 의도
  - 그리기를 클라이언트가 맡는 이유: 바깥 터미널마다 능력(kitty 그림·색 깊이)이 달라서, 붙은 클라이언트마다 다르게 그려야 한다.
- **칸 격자 크기**는 마지막으로 만진 클라이언트를 따른다. 본판 `mirror_follow` 와 같은 규칙이고, tmux의 `window-size latest` 에 해당한다.
- **그리기**: ratatui 0.30 + crossterm 0.29.
  - 칸 위젯은 `kasa-screen::Cell` 을 ratatui `Buffer` 로 옮긴다. tui-term이 vt100에 하는 일과 같다.
  - 앞뒤 화면 차분은 ratatui가 한다.

### 3.3 입력·한글·마우스

- **키**
  - stdin을 해석해 접두키(기본 `Ctrl-b`, 설정 가능)와 마우스만 가로챈다.
  - 나머지는 칸의 모드(커서 키·kitty 키보드 플래그·브래킷 붙여넣기)에 맞춰 `kasa-keys` 로 다시 인코딩한다.
  - 바깥 터미널이 kitty 키보드 프로토콜을 알면 켠다. 그래야 Shift+Enter를 구분해 claude에 넘길 수 있다.
- **한글**
  - 조합은 바깥 터미널이 하고, TUI는 조합이 끝난 UTF-8을 받는다(`kasa-ime` 는 쓰지 않는다).
  - 클라이언트는 매 프레임 **진짜 커서를 초점 칸의 커서 자리에** 둔다. 안 그러면 조합 중 글자가 엉뚱한 곳에 뜬다.
- **글자 폭**
  - 한글 음절은 2칸이다. 리더가 이미 NFC로 정규화하므로 자모가 갈라지지 않는다.
  - 폭이 갈리는 글자(이모지 ZWJ·모호 폭)가 있으면 넓은 글자마다 커서를 절대 좌표로 옮겨, 줄이 밀리지 않게 막는다.
  - 폭 고문 파일로 Ghostty·iTerm2·Windows Terminal 스크린샷을 비교한다.
- **마우스**: SGR(1006)과 버튼 이벤트(1002)를 켠다.
  - 경계선 끌기·탭·칸 초점은 TUI가 처리한다.
  - 칸 안에서는 칸이 마우스 모드를 켰을 때 좌표를 옮겨 넘긴다.
  - 칸이 마우스 모드를 안 켰으면 TUI가 처리한다. 선택은 OSC 52로 바깥 클립보드에 넣고, 휠은 스크롤백으로 쓴다.
- **OSC 52**: 칸이 클립보드에 쓰면 바깥 터미널로 다시 낸다. SSH 너머에서도 맞게 하려는 것이다.
- **셀 픽셀 크기**: 바깥 터미널에 `CSI 16 t` 로 물어 PTY(`TIOCSWINSZ`)에 싣는다.
- **환경**
  - `TERM=xterm-256color` 로 둔다. `TERM_PROGRAM` 은 엔진 env 정책으로 정한다.
  - `TERM_PROGRAM=kasaterm` 은 TUI 능력 판정에서 막힌 적이 있다.

### 3.4 kitty 그림 넘기기

- **바깥 터미널이 kitty를 아는지**는 질의(`a=q`)와 그 뒤의 DA1 답으로 판정한다.
- **아는 경우**
  - 칸에서 온 그림을 한 번만 보낸다: 전송만(`a=t,q=2`) + 가상 놓기(`U=1`).
  - 칸 자리에는 `U+10EEEE` 자리표시 글자를 그린다.
  - 자리표시는 글자라서 칸 잘림과 스크롤을 그대로 따른다.
  - kasa-pty `kitty.rs` 가 이미 같은 자리표시 방식이다. 서버는 그림 바이트(임시 PNG)와 자리표시 칸만 넘기면 된다.
- **tmux가 2026-03에 같은 길을 가며 밟은 함정**(tmux PR #5274)
  - `a=T` 는 유령 놓기를 남긴다. 전송만 하는 `a=t` 를 쓴다.
  - 자리표시는 UTF-8 바이트로 직접 쓴다. 로캘 함수를 거치면 빈 칸이 된다.
  - 256색 그림 id 38·48은 SGR 확장색 머리와 부딪힌다. 24비트 트루컬러 id를 쓴다.
  - 그림 줄은 진짜 줄바꿈으로 끝낸다. 접힌 줄로 표시하면 리플로우 때 그림이 지워진다.
- **모르는 경우**: `[그림 640×480]` 글자 자리표시를 그리고, 누르면 OS 기본 보기로 연다.
- **바깥 터미널별 지원**

  | 바깥 터미널 | kitty | 처리 |
  |---|---|---|
  | Ghostty·kitty·WezTerm | 지원 | 자리표시로 그린다 |
  | Windows Terminal(1.24 기준) | 없음(sixel만) | 글자 자리표시 |
  | iTerm2 | 자리표시 지원 여부 미확인 | 실측 뒤 정한다 |

### 3.5 협업 (첫 판에 넣는다)

- 서버가 `kasa_socket::Server` 를 띄우고 칸마다 `KASATERM_SOCKET_PATH` 를 넣는다. 그러면 칸 안의 `kasaterm-cli board/tell/done/summon` 이 그대로 간다.
- **`Backend` 구현 `TuiBackend`**
  - 필수 7개: `list_workspaces`·`current_workspace`·`list_surfaces`·`focus_surface`·`split_surface`·`send_text`·`send_key`
  - 칸 조작: `new_tab`·`rename_surface`·`pane_done`
  - 협업: `collab_board_source`·`bind_transcript`·`turn`·`attention`·`notify`·`agent_status`·`collab_tell`·`collab_tell_status`·`collab_tell_identity`
- **summon 이 되려면 claude 훅이 돌아야 한다.**
  - `kasa-collab` 의 shim 설치를 그대로 쓴다. 훅 쪽 근거는 `bind_transcript`·`turn`·`attention`·`notify`·`agent_status` 다.
  - 칸 머리줄에 학생 이름과 상태 점(작업·대기·끝)을 그린다. 정본은 `agent_state` 이고, 화면은 둘째 눈이다.
- **한 기계에 본판 앱과 TUI 서버가 함께 있을 때**
  - 둘 다 보드 원천(source)이 된다. 원천 종류 `tui` 를 더한다.
  - 주소(`surface_key`)가 원천마다 달라 tell이 섞이지 않는다.
  - 본판 수집기는 이 기계의 TUI 서버 소켓이 있으면 그 행을 함께 싣는다. TUI 쪽도 마찬가지다.
- **기기 사이**(`board --all`, `tell 이름@기계`)는 `kasa-collab` feature `net` 이 맡는다. 지금 kasa-mcp의 원격 관측과 `/collab` 엔드포인트를 옮겨 온 것이다.
  - 첫 판에 넣되 관문(T4)을 따로 둔다. 가장 무거운 의존(tokio·HTTP·kasanet)이 여기서 들어온다.

### 3.6 배포

| 길 | 방법 | 필요한 비밀 |
|---|---|---|
| 굽기 | **dist**(옛 cargo-dist, 0.32 2026-05) 설정 하나로 shell·powershell·npm·homebrew·msi 설치기를 함께 굽는다. 대상: macOS arm64·x86_64, Linux x86_64·aarch64(musl), Windows x86_64 | — |
| Homebrew | 자체 탭 `2rami/homebrew-tap` → `brew install 2rami/tap/kasa-tui`. 홈브루 코어는 남이 구운 바이너리를 받지 않는다 | `HOMEBREW_TAP_TOKEN` |
| npm | 패키지 `kasa-tui`, 명령 `kasa` → `npm i -g kasa-tui` | `NPM_TOKEN` |
| winget | dist에는 없다. GitHub 릴리스를 게시한 뒤 winget-releaser(Komac) 액션이 `microsoft/winget-pkgs` 에 PR을 낸다. **첫 판은 `komac new` 로 손으로 한 번** 넣어야 액션이 이어 받는다. id 후보 `2rami.kasa-tui` | 클래식 PAT(`public_repo`) |
| 셸 | `curl -fsSL …/kasa-tui-installer.sh \| sh`, `irm …/kasa-tui-installer.ps1 \| iex` | — |

- **서명**
  - macOS: dist의 codesign(실험 기능)으로 Developer ID 서명을 한다. 공증은 dist에 없어서 `notarytool` 단계를 따로 더한다. 낱 바이너리는 staple이 안 되고 온라인 확인에 맡긴다.
  - 본판의 인증서·공증 비밀은 맥미니에만 있다. kasalite CI에 넣을지, 미니에서 서명할지는 따로 정한다(§6).
  - Windows: 첫 판은 서명 없이 낸다. winget 검증은 통과하지만 SmartScreen 경고가 날 수 있다. 이후 dist가 지원하는 SSL.com eSigner나 Azure Trusted Signing을 쓴다.
- **업데이트**는 brew·npm·winget이 맡는다. 앱 안 자동 업데이트는 넣지 않는다.
- **엔진을 kasaterm에서 git으로 받는 조건**: kasaterm 레포가 공개여야 하고(지금 공개다), 엔진 크레이트에 LFS 파일이 없어야 한다(§2.1-3).

## 4. 새 카사라이트 (GUI, 둘째 단계)

### 4.1 범위와 레포

- **들어가는 것**
  - 창·방(탭)·칸 나누기, 한글 IME, kitty·OSC 1337 그림
  - 선택·복사·붙여넣기, 스크롤백, 글자 크기, 팔레트
  - `kasaterm-cli` 최소판: 칸 나누기·보내기·이름·목록
- **빠지는 것**: 학생·보드·나쵸·계정·웹뷰·마크다운·Git
- **협업**: `kasa-collab` 을 feature로 켤 수 있게만 둔다. 기본은 끈다.
- **kasalite 레포 구조**
  - 옛 판(본판 통째 복사, v0.1.0)은 `legacy-v0.1` 가지·태그로 남긴다.
  - main은 새 구조로 바꾼다: `app/lite`(GUI), `app/kasa`(TUI·CLI 멀티콜), `assets/`(글꼴). Cargo 워크스페이스는 하나다.
- **이어짐**
  - 번들 id `com.kasa.kasaterm.lite` 와 설정 폴더 `~/.config/kasaterm-lite` 는 그대로 둔다. 옛 사용자 설정이 이어진다.
  - MSI UpgradeCode는 옛 라이트 것을 그대로 쓴다. **본판 UpgradeCode와 같으면 라이트 설치가 본판을 지운다.**

### 4.2 120Hz

- **예산**: 120Hz 한 프레임은 8.33ms다.
  - 그리기 p95 목표는 ≤ 4ms다(200×60 칸 전체를 다시 그리는 기준).
  - 손상된 줄만 다시 그리면 ≤ 1ms다.
  - 본판보다 빠를 수 있는 이유: 매 프레임 크롬 전체를 다시 짓지 않고, 줄 단위 손상과 인스턴스 버퍼 유지로 그린다.
- **macOS ProMotion**
  - **박자는 디스플레이 링크로 맞춘다.**
    - macOS 14+: `NSView.displayLink(target:selector:)`(CADisplayLink) + `preferredFrameRateRange`(최소 80, 최대·선호 120)
    - 13 이하: CVDisplayLink
    - 화면 최고 주사율은 `NSScreen.maximumFramesPerSecond` 로 읽는다. 외부 144Hz 모니터도 이걸로 맞는다.
  - **ProMotion은 그리기를 멈추면 주사율을 내린다**(Zed 2024 실측).
    - 입력·출력 뒤 1초 동안은 매 박자 present해 120Hz를 붙잡는다.
    - 그 뒤엔 링크를 멈추고 완전히 잔다(`ControlFlow::Wait`).
    - 같은 화면을 다시 present할 때는 오프스크린 결과를 표면에 복사만 한다(≈0.2ms).
  - **키 입력은 박자를 기다리지 않고 바로 그린다.** 애니메이션·스크롤만 박자를 탄다.
  - **present 방식**
    - 지금 쓰는 `AutoNoVsync` + 대기열 1에서 시작한다.
    - `Fifo`+링크와 `Immediate`+링크를 A/B로 재서 고른다. 기준은 키 지연과 찢김이다.
    - `presentsWithTransaction`+`waitUntilScheduled` 는 Zed에서 120Hz를 60에 묶었다. 그래서 창 크기를 바꾸는 동안만 켠다.
- **Windows**
  - DX12 flip 모델, `desired_maximum_frame_latency: 1`
  - 박자: winit `MonitorHandle::refresh_rate_millihertz` 로 잰 주사율에 맞춘 타이머
  - 입력은 바로 그린다.
- **크기 줄이기**
  - 박힌 글꼴 21MB를 빼고 시스템 글꼴을 쓴다.
    - macOS: SF Mono·Menlo + Apple SD Gothic Neo
    - Windows: Cascadia Mono·Consolas + 맑은 고딕
  - Nerd 기호(1.9MB)만 넣거나, 쓰는 글자만 서브셋한다.
  - wry·tree-sitter·rusqlite·kasa-mcp·스프라이트를 넣지 않는다.
  - release 프로필: `lto = "fat"`, `codegen-units = 1`, `strip = true`, `panic = "abort"`

### 4.3 목표 수치

| 항목 | 목표 | 재는 법 | 지금 본판 |
|---|---|---|---|
| 키→present(앱 안 계측) | p50 ≤ 3ms, p95 ≤ 6ms | 키 이벤트 → present 시각 | 키→프레임 p50 10.9 · p95 18.6ms |
| 키→화면(소프트웨어 측정) | 평균 ≤ 8ms | Typometer. 참고: Alacritty 6.9, xterm 5.3, kitty 23.8ms(2024 리눅스) | — |
| 출력 폭주·스크롤 중 표시 | 120Hz 화면에서 ≥ 115fps, 간격 p99 ≤ 12ms | Metal HUD·프레임 간격 계측 | 84fps, p99 36ms |
| 바이너리 | ≤ 15MB(arm64) | `ls -l` | 92MB(옛 라이트) |
| 번들 | `.app` ≤ 20MB, dmg ≤ 10MB | `du` | 90MB / dmg 44MB |
| 켜기 | 실행 → 첫 프레임 ≤ 150ms | 첫 프레임 시각 | 1.5초 |
| 메모리 | 창 1·칸 1일 때 실물 ≤ 90MB, 칸 하나(스크롤백 1만 줄)마다 ≤ +10MB | `footprint` | 757MB(칸 14개 남짓) |
| 유휴 | 커서 깜빡임이 꺼져 있으면 깨어남 0회/초 | 깨어남 횟수 | — |

## 5. 단계·검증·규모

| 단계 | 레포 | 한 일 | 검증 기준 | 규모(대략) |
|---|---|---|---|---|
| **T0 엔진 준비** | kasaterm | `kasa-screen` 떼기(재수출), kasa-pty env 정책·`ClipboardSink`·셀 픽셀 옵션, 「Last login」을 호스트로, 앱 업데이트를 feature로 | 본판 시험 통과. 격리 리그로 칸 열기·한글·`kitty icat` 전후 동일. kasalite에서 git 의존으로 kasa-pty 빌드 | 1~2일, 수백 줄 이동 |
| **T1 TUI 뼈대** | kasalite | `kasa server/tui/attach/ls/kill`, 칸 나누기·탭·접두키·마우스·한글 커서·스크롤백·선택·OSC 52, `kasa-keys` | Ghostty·iTerm2·Windows Terminal에서 vim·htop·claude를 칸 넷에 띄워 화면 깨짐 0. 닫았다 붙어도 칸이 산다. 폭 고문 파일 스크린샷 일치. 한글 조합은 사람이 직접 친다(자동 재현 불가) | 3~5천 줄 |
| **T2 kitty 넘기기** | kasalite | 감지·`a=t`+`U=1`·자리표시·글자 대체 | Ghostty·kitty에서 `kitty icat` 과 Claude Code 그림이 칸 안에 잘려 보이고 스크롤을 따른다. Windows Terminal은 글자 자리표시 | ≈800줄 |
| **T3 협업 떼기** | kasaterm | `kasa-agents`·`kasa-collab` 크레이트를 만들고 본판이 그것을 쓰게 | 본판 보드·tell·summon·done 회귀 없음(격리 리그 + 기존 시험). kasa-mcp는 재수출로 호출부 0줄 | **가장 큼**. 6~9천 줄 이동, 약 1주 |
| **T4 TUI 협업** | kasalite | `TuiBackend`·훅 shim·칸 머리 학생 상태·한 기계 두 원천 합치기·feature `net` | TUI 안에서 `kasaterm-cli summon` → 브리프 tell 접수 → done이 부른 칸으로 온다. `board --local` 에 본판·TUI 행이 함께 뜬다. 기기 사이 tell 왕복 | 2~3천 줄 |
| **T5 첫 공개** | kasalite | dist 설정, homebrew-tap 레포, npm, winget 첫 손 제출, 맥 서명·공증 | 깨끗한 맥·윈도·리눅스(도커)에서 brew·npm·winget·셸로 설치 → `kasa tui` 실행. 설치 시간·크기 기록 | 반나절~1일 + 비밀 승인 대기 |
| **L1 렌더 엔진 떼기** | kasaterm | `kasa-gridview`, `kasa-cells` 정리, 글꼴을 호스트가 넘기게 | 본판 화면 격리 리그 전후 px 차 0(또는 의도한 것만). 본판 그리기 시간 회귀 없음 | 4~5천 줄 이동, 3~5일 |
| **L2 새 라이트** | kasalite | 창·탭·칸·IME·그림·디스플레이 링크 박자 | §4.3 목표 수치 전부를 맥·윈도 둘 다에서 | 4~6천 줄 |
| **L3 라이트 릴리스** | kasalite | 기존 `release.yml` 을 새 구조로, dmg·msi | 옛 v0.1 설정이 이어진다. 본판과 함께 깔아도 서로 지우지 않는다 | 반나절 |

순서는 T0 → T1 → T2 → T3 → T4 → T5(첫 공개) → L1 → L2 → L3다. T3(kasaterm)은 T1·T2(kasalite)와 나란히 갈 수 있다. 레포가 달라 겹치지 않는다.

## 6. 남은 갈림길·위험

- **맥 서명 비밀**을 kasalite CI에 넣을지, 본판처럼 맥미니에서만 서명할지. 비밀을 다루는 일이라 따로 승인을 받는다.
- **winget id와 Windows 서명 수단**
- **접두키 기본값**: `Ctrl-b` 는 tmux와 같다. tmux 안에서 TUI를 돌리면 겹친다.
- **iTerm2의 kitty 자리표시 지원** 실측
- **한 기계 두 원천 합치기**의 세부: 원천 id, 같은 학생 이름이 둘일 때의 주소 고르기
- **알려진 함정**
  - `kasa-pty` 의 `screens` 채널(256)이 차면 부분 손상 프레임이 버려지고, 그 줄은 바뀔 때까지 다시 오지 않는다. TUI 서버는 바로 비우거나 `full_snapshot()` 으로 당겨야 한다.
  - `PtyLayout` 은 칸 사이 경계 칸을 남기지 않는다.
- **비교 대상**: herdr. Rust·ratatui·tui-term·vt100으로 만든 에이전트 칸 터미널이다. 화면을 읽어 에이전트 상태를 감지하고 소켓 API가 있다. 기기 사이 협업은 없다. 별 4만 개.

## 7. 실측·시험 기록

- **엔진을 git 의존으로 받기**
  - 시험 크레이트를 레포 밖에 만들었다. `kasa-pty`·`kasa-cells` 를 `git = …kasaterm, rev = d5b818fd` 로 받아 release로 빌드했다.
  - 결과: 빌드는 통과했다. 워크스페이스 상속(`kasa-bridge.workspace = true`)은 git 의존에서도 풀린다.
  - 그러나 `kasa_cells::CASCADIA_CODE_NF`·`SYMBOLS_NERD_FONT_MONO` 는 **둘 다 132바이트**로 들어왔다. LFS 포인터 파일이 그대로 박힌 것이고, 컴파일 오류 없이 실행 중에 글꼴이 깨진다.
  - 그래서 §2.1-3 규칙이 필요하다. `kasa-pty` 쪽은 LFS 파일이 없어 문제가 없다.
- 렌더·크레이트·라이트 실측은 이 문서 §1의 출처 그대로다. 줄 수는 `wc -l`, 함수 경계는 중괄호 스캔, 바이너리는 `size -m`·`otool -L`, 메모리는 `footprint` 로 쟀다.

## 참고

- Zed, 「Optimizing the Metal pipeline to maintain 120 FPS in GPUI」(2024): <https://zed.dev/blog/120fps>
- wgpu #8109(macOS vsync + present_with_transaction 흔들림): <https://github.com/gfx-rs/wgpu/issues/8109>
- kitty 그림 프로토콜, 유니코드 자리표시: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>
- tmux kitty 그림(2026-03)과 자리표시 PR: <https://github.com/tmux/tmux/issues/4902>, <https://github.com/tmux/tmux/pull/5274>
- Windows Terminal kitty 요청(미지원): <https://github.com/microsoft/terminal/issues/8389>
- libghostty-vt Rust 바인딩: <https://crates.io/crates/libghostty-vt>
- dist 설치기: <https://axodotdev.github.io/cargo-dist/book/installers/index.html>
- winget-releaser / Komac: <https://github.com/vedantmgoyal9/winget-releaser>, <https://github.com/russellbanks/komac>
- herdr: <https://github.com/herdrdev/herdr>
- 터미널 입력 지연 Typometer 측정(2024): <https://beuke.org/terminal-latency/>
