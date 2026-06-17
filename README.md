<div align="center">

<img src="assets/AppIcon.png" width="120" alt="kasaterm" />

# kasaterm

**AI 에이전트와 함께 일하는, 터미널을 코어로 둔 작업 OS**

기성 터미널은 *터미널*에서 멈추고, IDE는 *편집기*에서 멈춘다.<br/>
kasaterm은 그 위로 한 층 더 올라가 — **여러 Claude를 거느리고, 작업이 굴러가는 걸 눈으로 보는** 자리에 있다.

[데모](#데모) · [이게 뭐야](#이게-뭐야) · [설치](#설치--실행) · [단축키](#단축키) · [로드맵](#로드맵)

![License](https://img.shields.io/badge/license-MIT-blue)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20%7C%20Linux-lightgrey)
![Built with Rust](https://img.shields.io/badge/built%20with-Rust-orange?logo=rust)
[![GitHub stars](https://img.shields.io/github/stars/2rami/kasaterm?style=social)](https://github.com/2rami/kasaterm/stargazers)
[![GitHub Sponsors](https://img.shields.io/github/sponsors/2rami?label=Sponsor&logo=githubsponsors&color=ff69b4)](https://github.com/sponsors/2rami)

</div>

---

## 데모

<!-- README의 심장. pane에서 굴러가는 작업이 BA GUI로 실시간으로 보이는 장면을 GIF로.
     BA GUI 완성도가 더 올라간 뒤 kasaterm 자체 캡처로 녹화해서 이 자리에 교체한다. -->

> 데모 영상 준비 중 — *pane에서 굴러가는 작업이 BA GUI로 한눈에 보이고, pane끼리 연동되는* 장면.

<div align="center">
  <img src="schale-light.png" width="720" alt="kasaterm — SCHALE OS / 아로나 모드" />
</div>

---

## 이게 뭐야

자체 제작 GUI 터미널 + Claude Code 런처. tmux를 prefix 키 대신 **GUI 버튼·드래그·자연어**로 다루는 네이티브 Rust 앱이다. 기성 라이브러리에 기대지 않고 렌더러·한글 IME·PTY까지 전부 직접 만들었다.

다른 터미널과 다른 점은 두 가지다:

- **작업이 굴러가는 걸 본다** — pane에서 Claude가 하는 일이 BA GUI(아로나 모드)로 실시간으로 보인다. 로그를 읽는 게 아니라 작업을 *지켜본다*.
- **pane끼리 연동된다** — pane이 서로 격리된 창이 아니라, 에이전트가 pane을 넘나들며 협업하는 하나의 작업 공간이다.

한 모노레포에 세 층이 쌓여 있고, 아래층이 위층을 떠받친다:

| 층 | 코드네임 | 역할 | 상태 |
|---|---|---|---|
| ① 엔진 | **kasaterm** | 터미널 — wgpu 셀 렌더 · PTY · 한글 IME · multipane | 거의 안정 |
| ② 작업환경 | **kasaspace** | 파일트리 · git 관리 · pane 간 에이전트 연결 | 진행 중 |
| ③ 오케스트레이션 | **blueclaudearchive** | 여러 Claude를 학생처럼 거느리는 하네스 GUI (아로나 모드) | 무게중심 |

①(터미널)은 거의 안정기, 무게중심은 ②③로 올라가는 중이다.

### 기술 스택

- **렌더** — `winit` + `wgpu` 위에 자체 cell-renderer (swash atlas + GPU instancing)
- **백엔드** — `portable-pty` + `alacritty_terminal` (macOS/Linux BSD PTY, Windows ConPTY 동일 코드 경로)
- **입력** — 자체 두벌식 한글 IME (OS IME 비의존, 복합 종성 지원)
- **색재현** — shader sRGB→DisplayP3 변환 + root CAMetalLayer (sugarloaf/ghostty 동급)

---

## 설치 & 실행

```bash
# 개발 빌드
cargo run -p kasaterm

# 체감(스크롤·입력 지연) 테스트는 반드시 release — 디버그는 원래 버벅임
cargo run --release -p kasaterm
```

macOS `.app` 번들은 별도 스크립트로 빌드한다(.icns/codesign/설치 포함). 앱을 실행하면 pane 제어 CLI(`kasaterm-cli`)와 MCP 서버가 함께 뜨고, MCP는 Claude Code/Antigravity 설정에 자동 등록된다.

### Claude Code 플러그인 (kasapane 스킬)

멀티 pane 제어·협업·긴 잡 사이클·UI 자체검증 워크플로우를 Claude Code 스킬로 묶었다:

```bash
claude plugin marketplace add 2rami/kasaterm
claude plugin install kasapane@kasaterm
```

설치 후 `/kasapane`으로 호출한다. 스킬이 쓰는 `kasaterm-cli`·MCP는 앱 빌드에 내장돼 있다.

---

## 단축키

macOS는 `Cmd`, Windows/Linux는 `Ctrl+Shift`를 "호스트 modifier"로 쓴다 (Ctrl+letter는 셸로 흘려보내기 위함). 폰트 zoom만 Windows/Linux에서 `Ctrl` 단독.

<details>
<summary><b>pane 조작 · 크기/폰트 · 윈도우 단축키 (펼치기)</b></summary>

### pane 조작

| 동작 | macOS | Windows / Linux |
|---|---|---|
| 가로 분할 (위아래로 쌓기) | `Cmd + D` | `Ctrl + Shift + D` |
| 세로 분할 (좌우로 나누기) | `Cmd + Shift + D` 또는 `Cmd + E` | `Ctrl + Shift + E` |
| 포커스된 pane 닫기 | `Cmd + W` | `Ctrl + Shift + W` |
| pane 포커스 순환 | `Cmd + [` / `Cmd + ]` | `Ctrl + Shift + [` / `]` |
| 방향 쪽 pane으로 포커스 이동 | `Cmd + Option + 방향키` | `Ctrl + Shift + Alt + 방향키` |
| 두 pane 위치 맞바꾸기(swap) | `Cmd + Option + Shift + 방향키` | (동일 패턴) |

### 크기 / 폰트

| 동작 | macOS | Windows / Linux |
|---|---|---|
| 전체 UI 확대 / 축소 / 리셋 | `Cmd + =` / `Cmd + -` / `Cmd + 0` | `Ctrl + =` / `Ctrl + -` / `Ctrl + 0` |
| 포커스된 pane만 폰트 확대 / 축소 / 리셋 | `Cmd + Shift + =` / `Cmd + Shift + -` / `Cmd + Shift + 0` | `Ctrl + Alt + =` / `Ctrl + Alt + -` / `Ctrl + Alt + 0` |

pane 사이 비율 조절은 **경계선(divider) 마우스 드래그**, pane을 끌어 합치거나 나누는 건 **drag → merge/split 존**으로 한다 (키보드 단축키 없음).

### 윈도우 / 셸 입력 보조

| 동작 | 키 |
|---|---|
| 새 윈도우 (PTY 백엔드) | `Cmd + T` |
| 윈도우 1~9 전환 | `Cmd + 1` ~ `Cmd + 9` |
| 자동완성 suggestion 수락 | `→` / `End` / `Ctrl + E` |
| suggestion 단어 단위 수락 | `Alt(Option) + F` |
| 단어 단위 삭제 | `Alt(Option) + Backspace` |

</details>

---

## 구조

<details>
<summary><b>워크스페이스 멤버 · 렌더러 env · MCP 도구 (펼치기)</b></summary>

### 워크스페이스 멤버 (`Cargo.toml`)

| 경로 | 역할 |
|---|---|
| `app/kasaterm` | 메인 바이너리. winit+wgpu 윈도우, chrome UI(탭·사이드바·이미지 pane 등), 입력·단축키 라우팅 |
| `crates/kasa-cells` | **기본 렌더러**. retained-mode GPU 셀 렌더러 (swash atlas + wgpu instance). P3 색재현 통합 |
| `crates/kasa-pty` | **기본 백엔드**. portable-pty + alacritty_terminal. 크로스플랫폼(ConPTY 포함) |
| `crates/kasa-ime` | 자체 두벌식 한글 입력 오토마타. OS IME 비의존, 복합 종성 지원 |
| `crates/kasa-socket` | cmux 호환 Unix-socket JSON-RPC 서버. Claude Code teammateMode 연동용. `kasaterm-cli` 바이너리 포함 |
| `crates/kasa-mcp` | kasaterm pane 제어를 모델 도구로 노출하는 streamable-HTTP MCP 서버 |
| `crates/kasa-shim` | kasaterm이 띄운 셸의 `tmux` 호출을 가로채 trace 후 진짜 tmux로 위임 |
| `crates/kasa-bridge` | tmux `-C`(control mode) 브리지. 레거시 백엔드 (현재 비기본) |
| `spikes/*` | iced/egui/gpui/warpui 등 GUI 프레임워크 PoC. 채택 안 된 실험 |

### 렌더러 / 환경 변수

기본 렌더러는 cell-renderer(`gpu.rs`) + P3. 주요 env 토글:

| 변수 | 효과 |
|---|---|
| `KASATERM_P3_ROOT=0` | P3 root-layer 경로 끄고 옛 sRGB sublayer 폴백 |
| `KASATERM_RENDERER=sugarloaf` | 참조용 sugarloaf 경로 (현재는 제거됨 — historical) |
| `KASATERM_TEXT_GAMMA` / `_CONTRAST` / `_COLOR_SAT` | 텍스트 감마·대비·채도 노브 |
| `KASATERM_AUTOCAPTURE_MS` / `_PATH` | N초 후 자동 스크린샷 (자체 테스트용) |
| `KASATERM_AUTOSEND` / `_MS` | N초 후 키 자동 전송 (자체 테스트용) |

### MCP 서버 도구

`crates/kasa-mcp`가 띄우는 streamable-HTTP MCP 서버로, Claude가 pane을 직접 제어한다. 앱이 부팅하면 자동으로 켜지고 Claude Code/Antigravity 설정에 자동 등록된다(별도 빌드·설치 불필요).

`kasaspace_list` · `kasaspace_split` · `kasaspace_close` · `kasaspace_focus` · `kasaspace_swap` · `kasaspace_rename` · `kasaspace_set_color` · `kasaspace_send` · `kasaspace_send_key` · `kasaspace_run_job` · `kasaspace_switch_window` · `kasaspace_workspace_list` · `kasaspace_workspace_current`

</details>

---

## 로드맵

세 층이 같이 진화 중이다. 아래층이 안정될수록 위층을 더 단단히 떠받친다.

| 층 | 항목 | 상태 |
|---|---|---|
| ① 엔진 | wgpu 셀 렌더 · P3 색재현 | 안정 |
| ① 엔진 | 두벌식 한글 IME (OS 비의존) | 안정 |
| ① 엔진 | 크로스플랫폼 PTY (macOS · Windows · Linux) | 안정 |
| ① 엔진 | `claude --resume` 세션 복원 | 예정 |
| ② 작업환경 | 파일트리 · git 패널 | 진행 중 |
| ② 작업환경 | pane 간 에이전트 연결 | 진행 중 |
| ② 작업환경 | 마크다운 기본 앱 연결 | 예정 |
| ③ 오케스트레이션 | BA GUI — 작업 실시간 시각화 | 진행 중 |
| ③ 오케스트레이션 | 여러 Claude 협업 (아로나 모드) | 진행 중 |

---

## 왜 만들었나

디자이너로 일하다 개발에 입문했다. tmux는 강력하지만 prefix 키 조합을 외우는 일이 늘 벽처럼 느껴졌다 — 터미널 멀티플렉싱을 GUI 버튼과 드래그로, 그리고 Claude Code를 한 번에 띄우는 런처로 다룰 수 있으면 좋겠다는 생각에서 시작했다.

기성 라이브러리에 기대지 않고 직접 만들고 싶었다. GPU 셀 렌더러(P3 색재현), 두벌식 한글 IME(OS IME 비의존), 크로스플랫폼 PTY까지 전부 자체 구현했다. 결과물보다 만들면서 배운 게 더 컸다.

무료로 공개한다. 누군가에게 쓸모가 되거나, 같은 길을 걷는 사람에게 참고가 되면 충분하다.

---

## 후원

혼자 만드는 프로젝트입니다. 쓸모가 있었다면 후원으로 응원해주세요.

[![GitHub Sponsors](https://img.shields.io/github/sponsors/2rami?label=Sponsor&logo=githubsponsors&color=ff69b4)](https://github.com/sponsors/2rami)

## 라이선스

[MIT](LICENSE)
