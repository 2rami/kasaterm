# kasaterm 협업 hook (정본)

pane 간 협업(누가 뭘 하는지·충돌차단·분담·메시지)을 굴리는 Claude Code hook들의 **정본 소스**. 배포·등록이 따로 없다 — kasaterm 이 pane 을 스폰할 때 PATH 맨 앞에 `claude` 래퍼를 얹고(`install_claude_hook_shim`, main.rs), 래퍼가 `--settings <shim>/claude-hooks-settings.json` 으로 이 디렉터리의 hook 들을 **세션 스코프**로 주입한다. `~/.claude/settings.json`·`~/.claude/hooks` 는 건드리지 않는다(munder --settings 패턴). pane 밖 claude 는 무영향.

hook 경로 해석(`locate_collab_hooks_dir`): env `KASATERM_COLLAB_HOOKS_DIR` → `.app` 번들 `Contents/Resources/collab-hooks`(build-app.sh 가 복사) → 레포 소스(cargo run). 레포에서 고치면 dev 는 즉시, .app 은 재빌드 시 반영.

협업 인프라(`PaneActivity`, `collab_board`, transcript→활동 추출)는 레포 Rust(`app/kasaterm/src/socket.rs`, `crates/kasa-socket`)에 있다 — **로컬 PTY 모드, 데몬 없음**. board 는 호출 시점 pull(`collab_board` 가 각 pane transcript tail 을 그 자리서 읽어 생성), 상시 watcher 스레드 없음.

## 주입되는 hook (claude-hooks-settings.json — main.rs hookSettings 가 생성)
| 파일 | 이벤트 | 역할 |
|---|---|---|
| `kasaterm-bind-transcript.sh` | UserPromptSubmit | claude transcript 경로 등록(**board 데이터 소스**) + agent-roster 영속 기록(재시작 후 god 복구용) |
| `kasaterm-board-context.py` | UserPromptSubmit | 매 턴 board(제목·상태·시킨일)+god 규약+복구 후보 주입, god-elect 트리거 |
| `kasaterm-stop-drain.sh` | Stop | 미읽 협업 메시지 있으면 멈춤 차단(inbox drain) + 작업 완료 알림 |
| `kasaterm-notify-attention.sh` | Notification | 권한/입력 대기 alert |
| `auto-imgopen.sh` | PostToolUse(SendUserFile) | 보낸 이미지를 image pane 으로 자동 표시 |

## board 모니터링 = 매 턴 board-context 주입 (기본)
모든 pane 이 자기 턴 시작 시 board(다른 pane 활동)를 프롬프트로 **자동으로 받는다** — `board-context.py`(UserPromptSubmit hook). 따로 Monitor 를 걸 필요도, 팀장 지정도 없이 모든 pane 이 서로를 인지한다(pull). 혼자면 조용.

## god 체제
- `god-elect.sh` — board-context 가 매 턴 fire-and-forget 호출. pane 2개+ 면 O_EXCL 원자 claim 으로 god 선출 → pane 헤더+세션 라벨 "● god"(window.rename) + `god-loop.sh` 기동.
- `god-loop.sh` — 외부 워처(정확히 1개). 워커가 승인/입력 대기로 막히면 god 에게 1회 msg 알림.
- `kasacollab.py` — (CLI) task 분담·msg(보내기 전 살아있는 pane 검증)·lead·drain-stop.
- 재시작 복구: bind-transcript 가 `~/.config/kasaterm/agent-roster/<slug>.json` 에 pane↔session 을 영속 기록 → 재시작 후 board-context 가 "[복구 가능 에이전트]" 를 주입(god 선출 전 단독 pane 에도) → god 이 `split`(기본 no-focus)+`tell 'claude --resume <uuid>'` 로 워커 부활.

## 보관 hook (미주입 — 필요 시 hookSettings 에 추가)
| 파일 | 용도 |
|---|---|
| `kasaterm-collab-hint.sh` | SessionStart 협업 안내문 주입 |
| `kasaterm-conflict-guard.py` | PreToolUse 같은 파일 동시편집 차단(transcript 직접판정) |
| `kasaterm-lead-watch.sh` | (Monitor 도구용) 팀장이 멈춘 워커 peek 확증·대리응답 |

## 설계 불변식
- **board 는 pull** — `collab_board` 가 호출 시점에 각 pane transcript tail(64KB)을 읽어 생성(`ai-title`·`last-prompt`·최근 답변·tool_use). 상시 watcher 스레드 없음(폐기).
- **모니터링 = 매 턴 주입(board-context)이 기본** — 상태 히스토리 누적(fleet.log)은 폐기, 이벤트는 msg push(done·막힘·stop-drain).
- **개인 설정 무오염** — hook 은 `--settings` 세션 스코프로만 주입. `~/.claude` 에 아무것도 설치하지 않는다.
- **`conflict-guard` 는 transcript 직접판정**(보관) — `~/.claude/projects/<cwd>/*.jsonl` 직접 읽어 PreToolUse 동기 차단. 백엔드 무관하게 살아있는 안전망.
