tmuxify (자체 tmux GUI 터미널 + Claude 런처). 사용자: 거노 (디자이너→개발 입문).

## 이 프로젝트에서 작업할 때

### 자율 테스트 우선
사용자에게 "테스트 해보세요"라고 떠넘기지 말고 **너가 직접** 실행·확인·수정 사이클을 돌려라:

1. **빌드/실행** — `cargo run -p tmuxify > /tmp/tmuxify-run.log 2>&1 &` 백그라운드로 띄우기 (반드시 `pkill -f "target/debug/tmuxify"; pkill -f "tmux -C"; sleep 1; rm -rf /tmp/tmux-501` 먼저)
2. **스크린샷** — `TMUXIFY_AUTOCAPTURE_MS=8000`로 N초 후 자동 캡처, `/tmp/tmuxify-screenshot.png` 생성. Read tool로 즉시 보기. `screencapture`는 Mac 권한 막혀서 안 됨, 무조건 tmuxify 자체 캡처 사용
3. **자동 입력** — `TMUXIFY_AUTOSEND="claude" TMUXIFY_AUTOSEND_MS=6000`로 특정 시간 후 키 자동 전송. 사용자 손 거치지 않고 시나리오 재현 가능
4. **시각 확인** — 스크린샷 본 후 어색한 부분 직접 짚어내고 수정. "어때보여요?" 묻지 말고 너의 판단으로 다음 액션

### 알려진 함정 1순위 ([[tmuxify-rendering-pipeline]] 메모리 참조)
claude UI 깨질 때 의심 순서: wgpu adapter.limits → UTF-8 line reader → %output lossy → vt100 parser size → 폰트 매칭. 다섯 개 다 한 세션에 잡은 적 있음.

### 박스 문자
`─│┌┐└┘╭╮╰╯` 등은 `block_rects()`에서 wgpu quad로 직접 그림 ([[tmuxify-box-drawing-quads]]). 폰트 글리프가 변종마다 chevron으로 매핑돼 있어서 quad가 안전. 새 박스 문자 깨지면 거기 추가.

### Retina 2x 더블 스케일
UI 상수는 LOGICAL 값 × 2 = PHYSICAL pixel로 저장. FONT_SIZE=32, TITLE_HEIGHT=52, SIDEBAR_W=320 등. `main.rs::TITLE_BAR`와 `render.rs::TITLE_HEIGHT`는 반드시 같이 움직여야 hit_test 깨지지 않음.

### tmux 셸 함수 (`claude` 등)
`crates/tmux-bridge/src/session.rs`에서 new-session 시 `exec $SHELL -il` 명시함. 안 그러면 `.zshrc` 로드 안 돼서 사용자 정의 함수 사용 불가.

### 새 pane 스폰
런처에서 spawn은 **`split-window -h`** 사용. `new-window`는 새 탭이 매번 쌓여서 사용자가 짜증. send-keys는 같은 tmux_cmd 호출에서 바로 연쇄.

### 성능 핫스팟
`build_body_cells`가 셀마다 `cosmic_text::Buffer` 새로 생성 — 164×63 = 10000+ buffers/frame. 다음 큰 작업 후보. `_build_body_segments_unused`처럼 연속 같은 색 셀 묶기 OR (char, color) LRU 캐시.
