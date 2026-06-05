tmuxify (자체 tmux GUI 터미널 + Claude 런처). 사용자: 거노 (디자이너→개발 입문). 패키지/바이너리명 = **`kasaterm`** (옛 `tmuxify` 아님).

[!] 작업 시작 전 반드시 [.memory/MEMORY.md](file://./.memory/MEMORY.md) 를 먼저 읽어라 — 거노님 개발 성향·todos·피드백, 그리고 **맨 위 핸드오프 블록**(직전 세션이 어디서 멈췄는지)부터 파악. 렌더러·색·아키텍처 배경, 렌더버그 카탈로그, 코드 수정 주의점은 전부 거기 토픽 파일에 있다.

## 자율 테스트 우선

사용자에게 "테스트 해보세요"라고 떠넘기지 말고 **너가 직접** 실행·확인·수정 사이클을 돌려라.

반드시 먼저 정리: `pkill -f "target/debug/kasaterm"; pkill -f "target/release/kasaterm"; pkill -f "tmux -C"; sleep 1; rm -rf /tmp/tmux-501`

1. **빌드/실행** — `cargo run -p kasaterm > /tmp/kasaterm-run.log 2>&1 &` (백그라운드)
2. **스크린샷** — `KASATERM_AUTOCAPTURE_MS=8000` 로 N초 후 자동 캡처. 기본 경로 `$TMPDIR/tmuxify.png` (`KASATERM_AUTOCAPTURE_PATH` 로 변경). Read tool 로 즉시 보기. macOS `screencapture` 는 권한 막혀 안 됨 — 무조건 자체 캡처.
3. **자동 입력** — `KASATERM_AUTOSEND="claude" KASATERM_AUTOSEND_MS=6000`. send_bytes 직접 주입이라 **IME 조합 경로는 재현 못 함** — 한글 조합 버그는 사용자가 직접 타이핑해야 함 (`KASATERM_IME_DEBUG=1` 로 키 코드포인트 로깅).
4. **체감(스크롤·입력 지연)은 반드시 release** — `cargo run --release -p kasaterm`. 디버그 빌드는 원래 버벅임(debug=느림, release/.app=빠름). 디버그로 "느리다" 판단 금지.
5. **시각 확인** — 스크린샷 본 후 어색한 부분 직접 짚어내고 수정. "어때보여요?" 묻지 말고 너의 판단으로 다음 액션.

## 함정·배경·수정 주의점은 메모리에

렌더러/색 파이프라인·아키텍처 배경, 렌더버그 디버깅 카탈로그, 코드 수정 주의점(PTY reader **`try_send` 필수** 등), 성능 히스토리는 CLAUDE.md 에 중복해 두지 않는다 — `.memory/MEMORY.md` 토픽 파일에 있다. 1순위 = [[feedback_tmuxify_rendering_pipeline]] (렌더버그 카탈로그 + try_send 트랩). 코드 만지기 전 관련 토픽을 먼저 recall 할 것.

## daemon-authoritative 불변식 (구조변경 GUI 액션 必)

데몬(`daemon.rs`)이 layout/pty/세션/docked **단일 권위**. GUI(`main.rs`)는 `DaemonState` broadcast 받을 때마다 `self.pty_layout`/`windows`/`docked`를 데몬 권위로 **통째 덮어쓴다**. → GUI 액션이 로컬(`self.pty_layout`/`ws.panes`/`next_pane_id`)만 바꾸면 다음 broadcast가 즉시 되돌려 **no-op/먹통/부활/증식**. drag 먹통·닫은 pane 부활·증식이 **전부 이 근원**이고 반복 재발했다.

새 구조변경 GUI 액션(split/close/move/dock/undock/swap/window·session 변경) 추가 시:
1. `self.pty_layout`/`ws.panes` **직접 수정 금지**. 함수 맨 앞에 `if let Some(client)=self.daemon_client.clone(){ client.<rpc>(...); return; }` 데몬 위임부터, 그 뒤에만 비데몬(로컬 PTY) fallback. 모범 = `split_active_pane`(focus→split_dir) / `move_pane`(surface.move) / `close`(close/dock).
2. `publish_pty_layout`은 cmux 미러(`ws.layout`)만 갱신, **데몬 미전파** — "publish 했으니 동기화" 착각 금지. 데몬 동기화는 RPC뿐.
3. 해당 RPC가 데몬에 없으면(예: swap) 데몬 모드 **early-return 차단** 후 `daemon.rs`/`methods.rs`/`stream.rs`/`backend.rs` 4곳에 `move_surface` 패턴 복제해 신설.
4. 성능: `DaemonState` 핸들러의 `resize_backend`/`chrome_dirty`/layout 덮어쓰기는 **`structural_unchanged` 게이트 안에서만**. cwd 1s 폴링이 leaf당 `client.resize` RPC를 쏘면 O(N) 낭비 → idle 안 가벼움.
5. 로컬 허용 예외: 인페인 보조탭(`spawn_new_tab` — 데몬은 primary pid만 소유). 디바이더 ratio는 **드래그 중에만** 로컬 ephemeral — release 시 `surface.resize_divider` RPC(ratio 직접 전송, 데몬 헤드리스라 pos 무의미)로 데몬 commit→`broadcast_state`→persist→재시작 복원. 윈도우 크기는 GUI 고유(데몬=헤드리스, 창 없음)라 데몬 아닌 `~/.config/kasaterm/window.json`(`exiting()` 저장 / `resumed()` 복원, logical/DPI 독립).

검증: RPC 실제 도달은 `daemon.rs` eprintln이 `/tmp/kasaterm-daemon.log`에 찍히는지로. 안 찍히면 로컬변형 버그. **미해결(후속):** 멀티탭 cross-pane drag(보조탭 한 탭 lift — 데몬이 pid 모름) GUI-local desync, `surface.swap` RPC 미구현(현재 데몬 모드 차단). 상세 [[project_kasaterm_session_lifecycle]].
