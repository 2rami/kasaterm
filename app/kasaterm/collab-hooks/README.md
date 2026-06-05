# kasaterm 협업 hook (정본)

pane 간 협업(누가 뭘 하는지·충돌차단·분담·메시지)을 굴리는 Claude Code hook들의 **정본 소스**. Claude Code 는 `~/.claude/hooks/<name>` 에서 실행하므로, `install-hooks.sh` 로 그 경로에 배포한다(소유권=레포, 실행=Claude Code 경로).

데몬 쪽 협업 인프라(`PaneActivity`, `collab_board/notify`, inbox 파일IPC, transcript→intent)는 이미 레포 안 Rust(`app/kasaterm/src/daemon.rs`, `crates/kasa-socket`)에 있다. 이 디렉터리는 그 위에 얹는 **Claude 통합 레이어**다.

## 배포
```sh
./install-hooks.sh            # 복사 (친구 배포·재현)
./install-hooks.sh --symlink  # 심볼릭 (거노 개발 머신 — 레포 수정 즉시 반영)
```
`settings.json` 은 건드리지 않는다(이미 `~/.claude/hooks/<name>` 경로 등록).

## hook ↔ settings.json 이벤트 매핑
| 파일 | 이벤트 | 역할 |
|---|---|---|
| `kasaterm-bind-transcript.sh` | SessionStart, UserPromptSubmit | claude transcript 경로를 데몬에 등록(`kasaterm-cli bind-transcript`) |
| `kasaterm-collab-hint.sh` | SessionStart | 협업 체계 안내문 주입 |
| `kasaterm-board-context.py` | UserPromptSubmit | 매 턴 board+분담+메시지+알림을 프롬프트에 주입(pull) |
| `kasaterm-collab-notify.sh` | UserPromptSubmit(start), Stop(stop) | 턴 경계를 형제 pane에 방송(`kasaterm-cli notify`) |
| `kasaterm-conflict-guard.py` | PreToolUse(Edit/Write/MultiEdit) | 같은 파일 동시편집 차단. **transcript 직접판정(빌드무관)** |
| `kasacollab.py` | (CLI, hook 아님) | task 분담·msg·inbox·lead. `python3 ~/.claude/hooks/kasacollab.py` |
| `kasaterm-lead-watch.sh` | (Monitor 도구용) | 팀장이 idle+권한대기 pane을 peek로 확증·대리응답 |

## 설계 불변식
- **`conflict-guard` 는 데몬을 안 거친다** — transcript(`~/.claude/projects/<cwd>/*.jsonl`)를 직접 읽어 PreToolUse 동기 판정. 데몬이 죽어도/리빌드 중에도 충돌차단이 살아있게 하는 의도적 선택. 데몬으로 흡수하지 말 것.
- **board 는 pull** — `board-context.py` 가 매 턴 `kasaterm-cli board` 로 당겨 주입. background watcher push 방식은 폐기됨(턴을 못 넘겨 죽고-다시걸기 루프 + board intent 오염).
