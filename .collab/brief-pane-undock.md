# 브리프: pane 별도창(undock) 기능 — 담당 아루(%10)

오케스트레이터: 프라나(%6). 모든 보고·질문·완료 통지는 `kasaterm-cli tell %6 "..."` 로. (SendMessage 금지 — 우리는 다른 방이라 안 닿는다. tell이 정식 경로다.)

## 임무
kasaterm의 일반 pane을 **별도 OS 창으로 분리(undock)** 하는 기능. 데몬 제거 때 빠진 미구현 항목(dock/undock)의 부활이다. 거노 원문: "그냥 pane도 별도창 기능해줘".

## 작업 디렉토리
레포 = `/Users/kasa/Desktop/momewomo/tmuxify` — 네 세션 cwd가 sionic이어도 무관하게 **절대경로로** Read/Edit 해라.

## 먼저 읽을 것 (read-only 탐색 단계)
1. `/Users/kasa/Desktop/momewomo/tmuxify/CLAUDE.md` — 코드맵(App 8모듈 분할)·병렬 작업 충돌 회피 규칙
2. `.memory/MEMORY.md` 의 관련 토픽 — 특히 설정화면이 오버레이→**별도 aux wgpu 윈도우**로 이전된 전례(reference_kasaterm_settings_screen)
3. 별도 창 전례 코드: `app/kasaterm/src/auxwin.rs`(설정 aux 윈도우), `main.rs`의 `preview_windows`/`aux_windows` 필드, `OpenMarkdownWindow`(md 풀뷰어 새 워크스페이스), `mcp__kasaspace__kasaspace_panel`(git/session 패널 별도창)
4. `session.rs`/`layout.rs` — pane·PtySession 소유 구조(App.pty가 PTY 직접 소유, 로컬 모드)

## 산출 순서 (중요 — 충돌 회피)
1. **1단계(지금): 탐색 + 설계안** — 어떤 접근이 맞는지(기존 aux 윈도우 패턴 재사용 vs 새 윈도우+PTY 핸들 이전 vs 별도 프로세스), 터치할 파일 목록, 리스크. **코드 수정 금지.** 설계안을 tell %6 으로 보고.
2. **2단계(내 "구현 시작" 신호 후): 구현** — 나는 지금 main.rs(shim)·render.rs를 커밋 직전이다. 네가 main.rs/handler.rs를 먼저 만지면 충돌난다. 신호 오면 시작해라.
3. **커밋 금지** — 커밋은 오케스트레이터(나) 단독.

## 검증 기준
- pane 하나를 별도 창으로 분리해도 그 pane의 셸/claude 세션이 끊기지 않을 것
- 분리 창을 닫으면 원래 윈도우로 복귀(dock)하거나 최소한 세션이 안 죽을 것
- `cargo build -p kasaterm` 통과 + 기존 테스트 통과
