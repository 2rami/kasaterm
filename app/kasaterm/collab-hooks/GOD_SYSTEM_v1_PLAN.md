# kasaterm "god 매니저 체제 v1" — 구현 계획서

> 멀티 pane 협업의 총괄자(god)를 선출제로 도입한다. god이 ① 팀 실시간 모니터링
> ② 커밋 단독 ③ 작업내역 집계 ④ 부하 시 워커 자동 스폰을 맡아, 변경점 추적 불확실
> + 커밋 반복 질문 + 총괄 부재 문제를 푼다. 경쟁앱 munder-difflin의 god 패턴을
> kasaterm 네이티브로, 더 가볍게(선출제·텔레메트리 레이어 없이) 구현.

## 확정 설계 (변경 금지)

- **선출제 god (하이브리드)**: pane 2개+ 되면 god 없을 때 먼저 감지한 pane이
  `lead` 파일을 원자적(`O_CREAT|O_EXCL`)으로 선점해 god 자임. 사용자 수동 이전도 가능.
  god pane이 죽으면 남은 pane이 재선출. 항상 정확히 1 god.
- **god 시각 표시**: god = `color #FFD400`(노랑) + `rename "● god"`. 워커는 pane id
  해시 → 팔레트에서 안 겹치게 자동 색(노랑 제외).
- **선출 = 모니터링 강제 기동**: god-elect가 `god-loop.sh`를 백그라운드(nohup)로
  강제 기동. claude 협조 불필요(claude가 못 끔). `pkill`로 항상 1개만. board-context
  hook이 매 턴 자가치유로 이중 안전.
- **커밋 god 단독 (엄격)**: 워커는 git을 아예 안 만짐. 작업 끝 →
  `kasacollab msg <god> "done: <요약> | files: ..."`. god이 모아서 단독 커밋·푸시.
  single-committer로 index race 원천 차단.
- **board 실시간 종합 (god이 알 것)**: ① 어떤 pane 있나 ② claude 몇 개 ③ 작업 내용
  ④ 무슨 툴 ⑤ **뭐가 변경됐나** = Edit/Write tool_use 파일 ∪ `git status --short`
  교차검증. god-loop가 종합해 god pane에 push.
- **자동 부하분산**: god이 부하 판단 시 `split` + `send "claude --model <tier>"`로
  워커 스폰 + `msg` 위임. 모델 티어 = heavy→`claude-sonnet-4-6`, triage/포맷→`claude-haiku-4-5`.
- **tmux 위장 제거**: claude teammate(tmux 백엔드) 경로 삭제. god이 split으로 pane
  만드니 불필요.

## 재활용 부품 (이미 구현, 손대지 말 것)

- `kasacollab.py`: `task/msg/inbox/lead`. base=`/tmp/kasaterm-collab/<cwd-slug>/`.
- `kasaterm-cli`: `list surfaces`/`color`/`rename`/`split`/`send`/`board`/`board-watch`/`tell`/`peek`/`transcript`/`bind-transcript`.
- 소켓 위임: `socket.rs` → `UserEvent::Socket*` → `handler.rs`. **색/이름은 이 경로 그대로 — Rust 변경 0.**
- `transcript.rs`(235줄): claude jsonl 역순 tail. 현재 usage 버림.
- `kasaterm-bind-transcript.sh`(SessionStart/PreToolUse), `kasaterm-board-context.py`(UserPromptSubmit).
- board GUI = wry webview(`chrome.rs:945 open_board_panel`, `BOARD_PANEL_HTML` + MCP `/board` 폴링).

## 절대 건드리지 말 것

- `state.rs:241~244`의 `KASATERM_PANE_ID`/`KASATERM_SOCKET_PATH` (kasacollab 협업의 핵심).
- `install_tmux_shim` 안의 imgopen/mdopen preview shim (P4에서 분리 보존).
- 기존 `kasacollab lead set/off/who` (P0는 `lead claim` 순수 추가만).

---

## 단계별 구현 (P0 → P5)

### P0 — 선출 + 색표시 + 모니터링 강제기동 (Rust 빌드 0)
**파일**: `god-elect.sh`(신규), `god-loop.sh`(신규 골격), `kasacollab.py`(`lead claim` 추가), `install-hooks.sh`(`god-*.sh` 배포 glob 추가), settings.json hooks(SessionStart+UserPromptSubmit에 god-elect 연결)
**할 일**:
1. `kasacollab.py`에 `lead claim` 추가 — `os.open(O_CREAT|O_EXCL|O_WRONLY)` 성공=god 획득, `FileExistsError`=양보. 기존 `lead set/off/who` 불변.
2. `god-elect.sh`: `list surfaces`로 pane<2면 no-op. lead 살아있으면 no-op. 없/죽었으면 `lead claim` → 성공 시 `color #FFD400`+`rename "● god"`+god-loop 강제기동, 실패 시 워커 색(pane id 해시→팔레트, 노랑 제외).
3. god-loop 강제기동: `pkill -f god-loop.sh` → `nohup god-loop.sh %self &` → `tell %self "[god] 너가 god. 통솔 시작."`
4. 재선출: lead pane이 `list surfaces`에 없으면 stale → `lead off` 후 재claim(god-elect 자가치유).
**검증**: 두 pane claude 띄움 → 하나 노랑"● god"+나머지 워커색 스크린샷(`KASATERM_AUTOCAPTURE_MS`). god pane 닫고 다른 pane 턴 → 노랑 이동(재선출) 확인. `ps`로 god-loop 정확히 1개.
**완료 기준**: god 선출/표시/재선출/모니터링 자동기동이 사람 개입 없이 동작.

### P1 — 커밋 god 단독 위임 (Rust 빌드 0)
**파일**: `god-loop.sh`(inbox done 감지), `kasaterm-board-context.py`(워커에 "커밋 금지, done 보고" + "god=%N" 주입), `collab-hooks/README.md`
**할 일**:
1. 워커 규약 주입: 코드 완료 시 커밋 말고 `kasacollab msg <god> "done: <요약> | files: a,b"`. god pane id는 board-context가 lead 파일 읽어 주입.
2. god-loop: inbox `done:` 감지 → god claude에 "워커 %N 완료, 검토 후 커밋?" emit → god이 `git add <files> && commit && push` 단독.
**검증**: 워커서 `msg <god> "done: test"` → god pane이 받아 커밋 반응(peek/transcript). `git log`에 god만.
**완료 기준**: 워커 0 커밋, god 단독 커밋 동작.

### P2 — 작업내역 집계 + 변경점 종합 (Rust 빌드 0)
**파일**: `god-loop.sh`(집계 통합)
**할 일**:
1. god-loop: `board-watch` 구독 + 주기적 `git status --short` 교차검증 + `list surfaces` 집계 → "워커N god1 | %4 Edit auth.rs(미커밋) | %5 idle" 형태로 god에 push.
2. god이 주기/마무리 시 `.memory/MEMORY.md` 핸드오프 블록을 직접 Edit 갱신.
**검증**: god-loop가 pane 변화+git 변경+pane 수를 god에 흘리는지. god이 핸드오프 갱신하는지.
**완료 기준**: god이 "누가 뭘 바꿨고 미커밋인지" 실시간 파악.

### P3 — transcript.rs 확장: 토큰·툴·변경파일 (worktree 빌드)
**파일**: `crates/kasa-socket/src/backend.rs`(PaneActivity add-only 필드), `app/kasaterm/src/transcript.rs`
**할 일**:
1. `PaneActivity`에 `#[serde(default)]` 필드: `tokens_in/tokens_out/cache_read/cache_creation:u64`, `cost_usd:f64`, `tool_counts:Vec<(String,u32)>`, `changed_files:Vec<String>`.
2. `transcript.rs`: 루프 분리(채움=조기종료 / usage·tool·changed=tail 전체 누적). `message.usage` 합산, `message.model`로 모델 식별. Edit/Write tool_use → changed_files.
3. 비용: 모델별 단가 const(sonnet/haiku) × 토큰.
4. 단위테스트: usage 합산·비용·changed_files.
**검증**: `cargo test -p kasaterm transcript`. 빌드 후 `kasaterm-cli board` JSON에 토큰/툴/변경 필드. **git worktree 격리 빌드.**
**완료 기준**: board가 pane별 토큰·툴·변경파일·비용 노출.

### P4 — tmux 위장 제거 (worktree 빌드, P3와 병렬)
**파일**: `state.rs:185~234`, `main.rs:3304,3344~`, `crates/kasa-shim/`, `Cargo.toml:11`
**제거 순서(컴파일 안 깨지게)**:
1. `state.rs`: `KASATERM_TMUX_SHIM_DIR` PATH/ZDOTDIR(191~205)·가짜 `$TMUX`(224~226)·`CLAUDE_CODE_TEAMMATE_MODE`(227)·`TMUX_PANE`(234) 삭제. **241~244 보존.**
2. `main.rs`: `install_tmux_shim()` 호출(3304) 삭제. **`install_preview_shims`(imgopen/mdopen)를 독립 함수로 분리해 보존** — 이게 최대 함정.
3. 죽은 자유함수 삭제(`install_tmux_shim`/`locate_shim_binary`/`stage_shim`/`real_tmux_candidates`). `locate_cmux_compat_binary`(kasaterm-cli 스테이징)는 필요성 확인 후 preview 로직에 흡수.
4. `crates/kasa-shim/` 디렉토리 + `Cargo.toml` workspace member 삭제(마지막).
**검증**: `cargo build` 통과 + claude pane서 real tmux PATH 미노출 + **imgopen/mdopen 동작** 확인.
**완료 기준**: tmux 경로 제거, preview·협업 무손상, 빌드 통과.

### P5 — graph 시각화 패널 (webview)
**파일**: `BOARD_PANEL_HTML`(chrome.rs), MCP `/board`(`crates/kasa-mcp`), `theme.rs` 토큰 재활용
**할 일**: pane 카드 그리드(색=정체, 상태, intent, **토큰/툴/변경파일/미커밋 뱃지**, god 왕관 SVG) + god↔워커 위임 화살표 SVG. **이모지 금지, SVG 아이콘.** `/board` JSON에 P3 필드 + god 여부 포함.
**검증**: board 패널 열어 카드/색/왕관/변경뱃지 스크린샷.
**완료 기준**: god이 한눈에 팀 전체 상태 시각 확인.

---

## 진행 규칙

- **순서**: P0 → P1 → P2 → (P3 ‖ P4) → P5. P0~P2는 Rust 빌드 0, hook/Python만 — "빌드→스폰→스크린샷" 자동 검증.
- **격리**: P3/P4 Rust 빌드는 git worktree 격리(살아있는 멀티 pane과 충돌 방지).
- **add-only**: collab-hooks·board-context·PaneActivity 수정은 순수 추가로(살아있는 협업 무손상).
- **각 단계 완료 기준 통과 후 다음.** 검증은 사람 개입 없이 스크린샷/테스트/로그로.
- 코드 규칙: 이모지 금지(SVG), 주석 WHY만, 죽은 코드 즉시 삭제, 편집>재작성.

## 미결(진행 중 결정)

- god-elect 트리거: SessionStart + UserPromptSubmit 둘 다(제안, lead 1회 stat이라 가벼움).
- 모델 티어 자동 임계값: P3 토큰 데이터 나온 뒤 결정.
- 수동 god 이전(`lead set`)도 `claim` 경유로 통일할지.
