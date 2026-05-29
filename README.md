# kasaterm

자체 제작 GUI 터미널 + Claude Code 런처. tmux를 GUI 버튼·드래그·자연어로 다루는 걸 목표로 한 네이티브 Rust 앱.

- 렌더: `winit` + `wgpu` 위에 자체 cell-renderer (swash atlas + GPU instancing)
- 백엔드: `portable-pty` + `alacritty_terminal` (macOS/Linux의 BSD PTY, Windows의 ConPTY 동일 코드 경로)
- 입력: 자체 두벌식 한글 IME (OS IME 비의존)
- 색재현: shader sRGB→DisplayP3 변환 + root CAMetalLayer install (sugarloaf/ghostty 동급)

> 패키지/바이너리 이름은 `kasaterm`. (옛 이름 `tmuxify`)

## 빌드 & 실행

```bash
# 개발 빌드로 실행
cargo run -p kasaterm

# 체감(스크롤·입력 지연) 테스트는 반드시 release — 디버그 빌드는 원래 버벅임
cargo run --release -p kasaterm
```

macOS `.app` 번들 빌드는 별도 스크립트 사용(.icns/codesign/설치 포함).

## 단축키

macOS는 `Cmd`를, Windows/Linux는 `Ctrl+Shift`를 "호스트 modifier"로 쓴다 (Ctrl+letter를 셸로 흘려보내기 위함). 폰트 zoom만 Windows/Linux에서 `Ctrl` 단독.

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

## 구조

워크스페이스 멤버 (`Cargo.toml`):

| 경로 | 역할 |
|---|---|
| `app/kasaterm` | 메인 바이너리. winit+wgpu 윈도우, chrome UI(탭·사이드바·이미지 pane 등), 입력·단축키 라우팅 |
| `crates/cell-renderer` | **기본 렌더러**. retained-mode GPU 셀 렌더러 (swash atlas + wgpu instance). P3 색재현 통합 |
| `crates/pty-backend` | **기본 백엔드**. portable-pty + alacritty_terminal. 크로스플랫폼(ConPTY 포함) |
| `crates/hangul-ime` | 자체 두벌식 한글 입력 오토마타. OS IME 비의존, 복합 종성 지원 |
| `crates/agent-socket` | cmux 호환 Unix-socket JSON-RPC 서버. Claude Code teammateMode 연동용 |
| `crates/kasaspace-mcp` | kasaterm pane 제어를 모델 도구로 노출하는 streamable-HTTP MCP 서버 |
| `crates/tmux-shim` | kasaterm이 띄운 셸의 `tmux` 호출을 가로채 trace 후 진짜 tmux로 위임 |
| `crates/tmux-bridge` | tmux `-C`(control mode) 브리지. 레거시 백엔드 (현재 비기본) |
| `spikes/*` | iced/egui/gpui/warpui 등 GUI 프레임워크 PoC. 채택 안 된 실험 |

## 렌더러 / 환경 변수

기본 렌더러는 cell-renderer(`gpu.rs`) + P3. 주요 env 토글:

| 변수 | 효과 |
|---|---|
| `KASATERM_P3_ROOT=0` | P3 root-layer 경로 끄고 옛 sRGB sublayer 폴백 |
| `KASATERM_RENDERER=sugarloaf` | 참조용 sugarloaf 경로 (현재는 제거됨 — historical) |
| `KASATERM_TEXT_GAMMA` / `_CONTRAST` / `_COLOR_SAT` | 텍스트 감마·대비·채도 노브 |
| `KASATERM_AUTOCAPTURE_MS` / `_PATH` | N초 후 자동 스크린샷 (자체 테스트용) |
| `KASATERM_AUTOSEND` / `_MS` | N초 후 키 자동 전송 (자체 테스트용) |

## kasaspace MCP

`crates/kasaspace-mcp`가 띄우는 MCP 서버로, Claude가 pane을 직접 제어할 수 있다. 도구 목록:

`kasaspace_list` · `kasaspace_split` · `kasaspace_close` · `kasaspace_focus` · `kasaspace_swap` · `kasaspace_rename` · `kasaspace_set_color` · `kasaspace_send` · `kasaspace_send_key` · `kasaspace_run_job` · `kasaspace_switch_window` · `kasaspace_workspace_list` · `kasaspace_workspace_current`

## 라이선스

MIT
