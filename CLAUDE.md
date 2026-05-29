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
