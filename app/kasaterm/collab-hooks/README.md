# kasaterm 협업 hook (정본)

pane 간 협업(누가 뭘 하는지·충돌차단·분담·메시지)을 굴리는 Claude Code hook들의 **정본 소스**. 배포·등록이 따로 없다 — kasaterm 이 pane 을 스폰할 때 PATH 맨 앞에 `claude` 래퍼를 얹고(`install_claude_hook_shim`, main.rs), 래퍼가 `--settings <shim>/claude-hooks-settings.json` 으로 이 디렉터리의 hook 들을 **세션 스코프**로 주입한다. `~/.claude/settings.json`·`~/.claude/hooks` 는 건드리지 않는다(munder --settings 패턴). pane 밖 claude 는 무영향.

hook 경로 해석(`locate_collab_hooks_dir`): env `KASATERM_COLLAB_HOOKS_DIR` → `.app` 번들 `Contents/Resources/collab-hooks`(build-app.sh 가 복사) → 레포 소스(cargo run). 레포에서 고치면 dev 는 즉시, .app 은 재빌드 시 반영.

협업 인프라는 `app/kasaterm/src/socket.rs`, `crates/kasa-socket`, `crates/kasa-mcp`에 있다. 로컬 자료 수집과 기기별 관측을 공통 snapshot·changes·inspect 계약으로 제공하며, CLI·HTTP·네이티브 화면이 같은 계약을 사용한다.

## 주입되는 hook (claude-hooks-settings.json — main.rs hookSettings 가 생성)

| 파일 | 이벤트 | 역할 |
|---|---|---|
| `kasaterm-bind-transcript.sh` | SessionStart/PreToolUse | claude transcript 경로 등록(**board 데이터 소스**) + agent-roster 영속 기록(재시작 후 `claude --resume` 복원용) |
| `kasaterm-conflict-guard.py` | PreToolUse(Edit/Write/MultiEdit) | 같은 파일 동시편집 차단 + 최신 board 주소로 담당 조율 안내 |
| `kasaterm-stop-drain.sh` | Stop | 미읽 협업 메시지 있으면 멈춤 차단(inbox drain) + 작업 완료 알림 |
| `kasaterm-notify-attention.sh` | Notification | 권한/입력 대기 alert |
| `auto-imgopen.sh` | PostToolUse(SendUserFile) | 보낸 이미지를 image pane 으로 자동 표시 |

## 협업 흐름

사용자가 정한 범위에서 [협업 지침](collab-protocol.md)을 따른다. 지침은 학생 정체성(말투) 뒤에 붙어 실리고, 설정의 「말투」를 꺼도 정체성만 빠지고 지침은 그대로 실린다(`character::protocol_only`). `board --all`로 최신 주소를 확인하고, 필요한 대상만 `activity --address`로 읽고, `tell 이름@기계 "본문"`(또는 `tell --address`)으로 전달한다 — 다른 기기의 학생도 같은 한 줄이다. 변경 추적은 `board-watch --all --json`, 전송 확인은 `tell-status`, 본인 완료 보고는 `done`을 사용한다.

세부 형식과 지원 범위는 [조회 계약](../../../docs/board-collaboration.md)과 [전달 계약](../../../docs/tell-protocol.md)에 있다. 나쵸네코가 띄운 학생의 완료·막힘 보고(`nacho-report`)는 [나쵸 오케스트레이터 계약](../../../docs/nacho-orchestrator.md)이다. 접수·입력 전달은 모델이 읽었다는 보장이 아니며, 새 API 미지원 시 구형 전송으로 우회하지 않는다.

- 재시작 복구(후속 미구현): bind-transcript 가 `~/.config/kasaterm/agent-roster/<slug>.json` 에 pane↔session 을 영속 기록 → `claude --resume <uuid>` 로 세션 복원에 쓸 예정.

## 보관 hook (미주입 — 필요 시 hookSettings 에 추가)

| 파일 | 용도 |
|---|---|
| `kasaterm-collab-hint.sh` | 수동 등록 환경을 위한 SessionStart 안내. 기본 주입은 `collab-protocol.md` 사용 |

## 설계 불변식

- **관측과 작업 수명 구분** — 기기 연결 끊김·오래된 관측을 작업 종료로 읽지 않는다. 상세 활동은 지정한 대상에서 제한된 분량만 읽는다.
- **추적과 전달 구분** — 변경 기록은 제한된 메타데이터만 보관한다. 메시지는 최신 수신자 신원과 입력 상태를 검증하며, 같은 ID의 재시도는 기존 접수 기록을 반환한다.
- **개인 설정 무오염** — hook 은 `--settings` 세션 스코프로만 주입. `~/.claude` 에 아무것도 설치하지 않는다.
- **`conflict-guard` 는 transcript 직접판정** — `~/.claude/projects/<cwd>/*.jsonl` 직접 읽어 PreToolUse 동기 차단. 백엔드 무관하게 살아있는 안전망.
