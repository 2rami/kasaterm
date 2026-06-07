# kasaterm 협업 hook (정본)

pane 간 협업(누가 뭘 하는지·충돌차단·분담·메시지)을 굴리는 Claude Code hook들의 **정본 소스**. Claude Code 는 `~/.claude/hooks/<name>` 에서 실행하므로, `install-hooks.sh` 로 그 경로에 배포한다(소유권=레포, 실행=Claude Code 경로).

협업 인프라(`PaneActivity`, `collab_board`, transcript→활동 추출)는 레포 Rust(`app/kasaterm/src/socket.rs`, `crates/kasa-socket`)에 있다 — **로컬 PTY 모드, 데몬 없음**. board 는 호출 시점 pull(`collab_board` 가 각 pane transcript tail 을 그 자리서 읽어 생성), 상시 watcher 스레드 없음.

## 배포
```sh
./install-hooks.sh            # 복사 (친구 배포·재현)
./install-hooks.sh --symlink  # 심볼릭 (거노 개발 머신 — 레포 수정 즉시 반영)
```
hook 파일은 배포하지만 `settings.json` 등록은 별도다 — Claude Code 가 `settings.json` 의 `hooks` 에 명시된 것만 실행한다(파일이 `~/.claude/hooks/` 에 있어도 등록 안 하면 안 돈다).

## board 모니터링 = 매 턴 board-context 주입 (기본)
모든 pane 이 자기 턴 시작 시 board(다른 pane 활동)를 프롬프트로 **자동으로 받는다** — `board-context.py`(UserPromptSubmit hook). 따로 Monitor 를 걸 필요도, 팀장 지정도 없이 모든 pane 이 서로를 인지한다(pull). 혼자면 조용.

board 를 채우는 건 `bind-transcript.sh`(UserPromptSubmit) — 각 pane 의 claude transcript 경로를 등록해 `collab_board` 가 읽게 한다. **이 둘이 모니터링의 최소 한 쌍이고, 현재 `settings.json` 에 등록된 kasaterm hook 은 이 둘뿐이다.**

## 현재 등록 hook (settings.json)
| 파일 | 이벤트 | 역할 |
|---|---|---|
| `kasaterm-bind-transcript.sh` | UserPromptSubmit | claude transcript 경로 등록 — **board 데이터 소스** |
| `kasaterm-board-context.py` | UserPromptSubmit | 매 턴 모든 pane 에 board(제목·상태·시킨일) 주입 |

## 보관 hook (settings 미등록 — 필요 시 재등록)
| 파일 | 용도 |
|---|---|
| `kasaterm-collab-hint.sh` | SessionStart 협업 안내문 주입 |
| `kasaterm-conflict-guard.py` | PreToolUse 같은 파일 동시편집 차단(transcript 직접판정) |
| `kasacollab.py` | (CLI) task 분담·msg·lead |
| `kasaterm-lead-watch.sh` | (Monitor 도구용) 팀장이 멈춘 워커 peek 확증·대리응답 |

## (옵션) Monitor 기반 감시
매 턴 주입 외에, 감시 전담 pane 이 Claude Code Monitor 로 board 워처를 걸 수도 있다(기본 hook 으론 안 건다):
- `Monitor(command="kasaterm-cli board-watch 3", persistent=true)` — 상태 바뀐 pane 만 스트림(working↔idle↔building↔waiting, 합류=새 id, 종료=closed).
- `kasacollab lead set` → `Monitor(command="bash ~/.claude/hooks/kasaterm-lead-watch.sh", persistent=true)` — 팀장이 idle+사람입력대기로 멈춘 워커를 화면째 받아 `kasaterm-cli send --surface %N "번호"` 로 대신 답.

## 설계 불변식
- **board 는 pull** — `collab_board` 가 호출 시점에 각 pane transcript tail(64KB)을 읽어 생성(`ai-title`·`last-prompt`·최근 답변·tool_use). 상시 watcher 스레드 없음(폐기).
- **모니터링 = 매 턴 주입(board-context)이 기본** — 모든 pane 이 자기 턴에 board 를 본다. Monitor(board-watch/lead-watch)는 감시 전담용 옵션.
- **notify(능동 푸시)·mute 는 폐기** — 턴 경계 방송 대신 board pull 로 단일화.
- **`conflict-guard` 는 transcript 직접판정**(보관) — `~/.claude/projects/<cwd>/*.jsonl` 직접 읽어 PreToolUse 동기 차단. 백엔드 무관하게 살아있는 안전망.
