tmuxify (자체 tmux GUI 터미널 + Claude 런처). 사용자: 거노 (디자이너→개발 입문).

[!] 중요 규칙: 작업을 시작하기 전에 반드시 프로젝트 폴더 내의 가상 바로가기인 [.memory/MEMORY.md](file://./.memory/MEMORY.md) 문서를 먼저 읽고 거노님의 개발 성향, 인생의 목표(todos), 피드백 등을 완벽히 숙지하고 작업에 임해야 합니다.

## 이 프로젝트에서 작업할 때


### 자율 테스트 우선
사용자에게 "테스트 해보세요"라고 떠넘기지 말고 **너가 직접** 실행·확인·수정 사이클을 돌려라:

패키지/바이너리명은 **`kasaterm`** (옛 `tmuxify` 아님).

1. **빌드/실행** — `cargo run -p kasaterm > /tmp/kasaterm-run.log 2>&1 &` 백그라운드로 띄우기 (반드시 `pkill -f "target/debug/kasaterm"; pkill -f "target/release/kasaterm"; pkill -f "tmux -C"; sleep 1; rm -rf /tmp/tmux-501` 먼저)
2. **스크린샷** — `KASATERM_AUTOCAPTURE_MS=8000`로 N초 후 자동 캡처, 기본 경로 `$TMPDIR/tmuxify.png` (`KASATERM_AUTOCAPTURE_PATH`로 변경). Read tool로 즉시 보기. `screencapture`는 Mac 권한 막혀서 안 됨, 무조건 자체 캡처 사용
3. **자동 입력** — `KASATERM_AUTOSEND="claude" KASATERM_AUTOSEND_MS=6000`로 특정 시간 후 키 자동 전송. 단 send_bytes 직접 주입이라 **IME 조합 경로는 재현 못 함** — 한글 조합 버그는 사용자가 직접 타이핑해야 함 (`KASATERM_IME_DEBUG=1`로 키 코드포인트 로깅)
4. **체감(스크롤·입력 지연) 테스트는 반드시 release** — `cargo run --release -p kasaterm`. 디버그 빌드는 원래 버벅임(A/B 확인: debug=느림, release/.app=빠름). 디버그로 "느리다" 판단 금지
5. **시각 확인** — 스크린샷 본 후 어색한 부분 직접 짚어내고 수정. "어때보여요?" 묻지 말고 너의 판단으로 다음 액션

### 알려진 함정 1순위 ([[tmuxify-rendering-pipeline]] 메모리 참조)
현재 기본 백엔드 = pty-backend(portable-pty + alacritty_terminal) + cell-renderer(`gpu.rs`). 깨질 때 의심 순서:
- **입력/커서가 0.5초 늦음** → `main.rs::about_to_wait`가 `WaitUntil(blink)`로 파킹해 펜딩 redraw를 미룸. `chrome_dirty || pane.dirty`면 즉시 깨워야 함. 렌더 2경로(sugarloaf/gpu) 둘 다 점검.
- **한글이 깨져 보임** → 입력 Composer 말고 **렌더/damage 경로부터** 의심. macOS 입력 경로(set_ime_allowed(false) + hangul-ime)는 정상이고 셸에선 멀쩡함. preedit는 chrome 오버레이라 변경 시 chrome_dirty 필요.
- **동기화/화면 멈춤** → DECSET 2026은 alacritty vte `Processor`가 내장 처리 ([[reference_kasaterm_decset_2026]]). 수동 파싱 금지.
- 옛 tmux-bridge/wgpu-raw 시절 5대 함정(wgpu limits·UTF-8 reader·%output lossy·vt100 size·폰트매칭)은 [[tmuxify-rendering-pipeline]] 메모리에.

### 박스 문자
`─│┌┐└┘╭╮╰╯` 등은 `block_rects()`에서 wgpu quad로 직접 그림 ([[tmuxify-box-drawing-quads]]). 폰트 글리프가 변종마다 chevron으로 매핑돼 있어서 quad가 안전. 새 박스 문자 깨지면 거기 추가.

### Retina 2x 더블 스케일
UI 상수는 LOGICAL 값 × 2 = PHYSICAL pixel로 저장. FONT_SIZE=32, TITLE_HEIGHT=52, SIDEBAR_W=320 등. `main.rs::TITLE_BAR`와 `render.rs::TITLE_HEIGHT`는 반드시 같이 움직여야 hit_test 깨지지 않음.

### 셸 함수 (`claude` 등) 로딩
셸을 `$SHELL -il`(login+interactive)로 띄워야 `.zshrc` 로드돼서 사용자 정의 함수 사용 가능. 기본 백엔드는 `crates/pty-backend/src/state.rs`(CommandBuilder `-il`), 레거시는 `crates/tmux-bridge/src/session.rs`(new-session `exec $SHELL -il`).

### 새 pane 스폰
런처에서 spawn은 **`split-window -h`** 사용. `new-window`는 새 탭이 매번 쌓여서 사용자가 짜증. send-keys는 같은 tmux_cmd 호출에서 바로 연쇄.

### 성능 (해결됨, 참고)
옛 sugarloaf 경로의 `build_body_cells`가 셀마다 `cosmic_text::Buffer` 생성하던 30-50ms/frame 병목은 cell-renderer(swash atlas + wgpu instance, 1542셀 ~80μs)로 교체돼 해결됨(커밋 `1033b56`). 기본 렌더러 = `gpu.rs`. sugarloaf는 `KASATERM_RENDERER=sugarloaf`로 A/B용만 남음.
