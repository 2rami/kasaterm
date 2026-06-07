# kasaterm 협업 hook (정본)

pane 간 협업(누가 뭘 하는지·충돌차단·분담·메시지)을 굴리는 Claude Code hook들의 **정본 소스**. Claude Code 는 `~/.claude/hooks/<name>` 에서 실행하므로, `install-hooks.sh` 로 그 경로에 배포한다(소유권=레포, 실행=Claude Code 경로).

협업 인프라(`PaneActivity`, `collab_board`, transcript→활동 추출)는 레포 Rust(`app/kasaterm/src/socket.rs`, `crates/kasa-socket`)에 있다 — **로컬 PTY 모드, 데몬 없음**. board 는 호출 시점 pull(`collab_board` 가 각 pane transcript tail 을 그 자리서 읽어 생성), 상시 watcher 스레드 없음.

## 배포
```sh
./install-hooks.sh            # 복사 (친구 배포·재현)
./install-hooks.sh --symlink  # 심볼릭 (거노 개발 머신 — 레포 수정 즉시 반영)
```
`settings.json` 은 건드리지 않는다(이미 `~/.claude/hooks/<name>` 경로 등록).

## hook ↔ 이벤트 매핑
| 파일 | 이벤트 | 역할 |
|---|---|---|
| `kasaterm-bind-transcript.sh` | SessionStart, UserPromptSubmit | claude transcript 경로 등록(`kasaterm-cli bind-transcript`) — **board 데이터 소스, 필수** |
| `kasaterm-collab-hint.sh` | SessionStart | 협업 체계 안내문 주입 |
| `kasaterm-conflict-guard.py` | PreToolUse(Edit/Write/MultiEdit) | 같은 파일 동시편집 차단. **transcript 직접판정(백엔드무관)** |
| `kasacollab.py` | (CLI, hook 아님) | task 분담·msg. `python3 ~/.claude/hooks/kasacollab.py` |
| `kasaterm-lead-watch.sh` | (Monitor 도구용) | 팀장이 idle+권한대기 pane 을 peek 로 확증·대리응답 |

## board 모니터링 = Claude Code Monitor
board 를 "항상 보는" 건 감시할 pane 이 Monitor 도구로 board 워처를 거는 방식. 용도에 따라 둘 중 하나:

**1. board-watch (범용)** — "누가 뭐 하나" 상태변화 스트림:
```
Monitor(command="kasaterm-cli board-watch 3", persistent=true)
```
상태 바뀐 pane 만 한 줄씩(working↔idle↔building↔waiting, 합류=새 id, 종료=closed).

**2. lead-watch (팀장/오케스트레이터)** — 멈춘 워커를 잡아 대신 답한다:
```
kasacollab lead set     # 이 pane = 팀장 (한 방에 한 명, lead off 로 해제)
Monitor(command="bash ~/.claude/hooks/kasaterm-lead-watch.sh", persistent=true)
```
워커가 idle+사람입력대기(AskUserQuestion·`❯`)로 멈추면 화면째 팀장에게 넘기고, 팀장이 `kasaterm-cli send --surface %N "번호"` 로 대신 답한다. 팀장은 `peek`/`transcript`/`tell`/`split`/`focus`/`close` 로 **모든 pane 을 제어**한다(socket=PtyBackend). 팀장은 사람(거노)이 명시적으로 띄워 지정한다.

매 턴 프롬프트 주입(옛 `board-context.py`)·능동 푸시(옛 `collab-notify.sh`/mute)는 **폐기** — 모니터링은 Monitor 단일.

## 설계 불변식
- **`conflict-guard` 는 transcript 직접판정** — `~/.claude/projects/<cwd>/*.jsonl` 을 직접 읽어 PreToolUse 동기 차단. 백엔드가 죽어도/리빌드 중에도 충돌차단이 살아있게 하는 의도적 선택.
- **board 는 pull** — `collab_board` 가 호출 시점에 각 pane transcript tail(64KB)을 읽어 생성(`ai-title`·`last-prompt`·최근 답변·tool_use). 상시 watcher 스레드 없음(폐기).
- **notify(능동 푸시)·mute 는 폐기** — 턴 경계 방송 대신 Monitor pull 감시로 단일화.
