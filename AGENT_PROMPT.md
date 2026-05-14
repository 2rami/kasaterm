# tmuxify · iced 트랙

너는 거노(디자이너→개발입문)의 **tmuxify** 프로젝트 iced 트랙 담당자야. 옆 워크트리들과 6 트랙 비교 실험 중이고 너는 그 중 iced 라이브러리 트랙.

**프로젝트 한 줄**: tmux GUI for Claude Code sessions. Warp/Ghostty 톤의 빠른 자체 터미널 + 사이드바·탭바·온보딩 chrome.

**현재 워크트리**: `/Users/kasa/Desktop/momewomo/tmuxifyworktree/iced-fix` (브랜치 `iced-fix`, iced 0.14 + iced_term 0.8 코드 이미 들어있음, ~500줄 main.rs + chrome 완성)

## 너의 임무 — 미해결 3건 해결

1. **한글 IME 안 됨**: 한글 치면 `ㅌㅣㅁㅁㅗㄷㅡ`처럼 자모 분리. iced_term이 `Shell::request_input_method` 안 호출해서 winit `set_ime_allowed(true)` 안 켜짐. main.rs에 `Event::InputMethod(Commit(s))` 핸들러는 추가됐는데 IME 자체가 활성화 안 됨. 옵션: iced_term fork / dummy text_input으로 IME 활성 유도 / `crates/hangul-ime` crate 활용 (이 crate가 dubeolsik 매핑 가짐, 옛 native-rust 브랜치에서 직접 호출하는 패턴 참고).

2. **Cmd+D 분할 안 됨**: 활성 세션에 tmux prefix(C-b, 0x02) + `%` 보내려는 코드 있는데 iced_term이 keyboard event 먼저 swallow함. iced_term::Command::AddBindings로 커스텀 binding 추가하거나, modifier 매칭 디버그.

3. **tmux 자동 시작 안 됨**: 현재 BackendSettings.program = $SHELL. 폴더 열면 그냥 zsh. → `program = "tmux"`, `args = ["new-session", "-A", "-s", <cwd-based-name>]`로 변경. claude team mode가 tmux pane 기반이라 필수.

## 활용 자산

- `archive/wgpu-raw-poc` 브랜치 (`git show archive/wgpu-raw-poc:app/tmuxify/src/main.rs`) — 옛 winit+wgpu raw 구현. hangul-ime 직접 호출 패턴 참고.
- `crates/hangul-ime` — Hangul Composer (dubeolsik 매핑).
- `crates/tmux-bridge` — 안 쓸 거 (iced_term이 pty 직접 잡음).
- `iced` 브랜치 (`/Users/kasa/Desktop/momewomo/tmuxify`) — 현재 iced 작업의 메인 워크트리. 동일 코드.

## 검증 절차 (필수)

```bash
cd /Users/kasa/Desktop/momewomo/tmuxifyworktree/iced-fix
pkill -f "target/debug/tmuxify"; pkill -f "tmux -C"; sleep 1; rm -rf /tmp/tmux-501
cargo build -p tmuxify
TMUXIFY_AUTOOPEN=$HOME cargo run -p tmuxify > /tmp/iced-fix.log 2>&1 &
sleep 5
screencapture -x -o -t png /tmp/iced-fix.png
# Read /tmp/iced-fix.png 로 확인
```

검증 포인트:
- 한글: 키보드 한영 전환 후 `안녕` 입력. 화면에 `안녕` 그대로 보이는지 (자모 분리 X).
- tmux 자동: 셸 프롬프트 옆에 `[tmux]` 같은 표시 또는 `echo $TMUX` 결과 비어있지 않으면 OK.
- Cmd+D: Cmd+D 누르면 pane 분할되는지.

## 보고

작업 마치면 짧게 (300자 이내):
- 3개 fix 여부 (한글 / tmux 자동 / Cmd+D)
- 변경 파일 + 핵심 변경 줄
- 다음 polish 거리

진행해. 막히면 `git log --oneline` 같은 거 자유롭게 사용. 다른 워크트리 코드도 자유롭게 읽어.
