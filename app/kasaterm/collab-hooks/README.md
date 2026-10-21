# 카사텀 협업 훅

훅 구현·등록·상태 연동을 고칠 때 읽는다. 사용 절차는 `kasaterm-cli guide collab`로 필요한 때 조회한다.

PC 앱은 `pane_shims/claude.rs`에서 세션 전용 `--settings`를 만든다. 전역 Claude 설정은 변경하지 않는다.
훅 경로는 앱 번들의 Resources(Windows는 exe 옆) → 개발 실행의 `KASATERM_COLLAB_HOOKS_DIR` → 저장소 소스 순이다. 설치본은 함께 배포한 훅을 사용한다.
GUI 없는 호스트는 `crates/kasa-collab/src/hooks.rs`에서 별도로 구성하므로 적용 범위를 구분한다.

| 훅 | PC 등록 시점·역할 |
|---|---|
| `kasaterm-bind-transcript.sh` | SessionStart: 대화·학생 신원 연동 |
| `kasaterm-conflict-guard.py` | Edit/Write/MultiEdit 전: 동시 편집과 미커밋 파일 소유 보호 |
| `kasaterm-subagent-guard.py` | Agent/Task 전: 학생 칸 위임 안내, 읽기 전용 탐색·명시 요청 예외 |
| `kasaterm-build-expiry-guard.py` | Bash 전: 설치된 앱을 멈추게 하는 빌드 만료 차단 |
| `kasaterm-closed-pane-guard.py` | 도구 호출·프롬프트 제출 전: 닫힌 칸 작업 거부 |
| `kasaterm-agent-status.sh` | 도구 전후·Stop: 진행 상태 기록 |
| `kasaterm-turn.sh` | 프롬프트·Stop·PreCompact: 턴·압축 경계 기록 |
| `kasaterm-notify-attention.sh` | Notification: 승인·질문·입력 대기 알림 |
| `auto-imgopen.sh` | SendUserFile 뒤: 이미지 전송 이력만 기록 |
| `kasaterm-steer-hook.sh` | 도구 뒤: 남아 있는 steer 메시지 소비 |
| `kasaterm-peer-name.py` | ListAgents 뒤·프롬프트 제출: 학생 이름 대응 |
| `ultracode-mark.py` | 프롬프트 제출: 해당 턴의 표시 상태 |

시작 지침에는 선택한 말투와 짧은 주제 안내만 들어간다. 전체 협업 규약을 매번 붙이지 않는다. 새 작업 브리프는 시작 안내와 done 한 줄을 덧붙인다.
`kasaterm-collab-hint.sh`는 수동 등록된 구형 환경을 위해 짧은 안내만 유지하며 기본 등록에는 없다. `kasaterm-stop-drain.sh`도 구형 Stop 호환용이다.

- 관측과 작업 수명을 구분한다. 끊김·idle·잠정 Stop을 완료 증거로 쓰지 않는다.
- 대화·학생 신원은 해당 칸의 실제 하네스에 결속한다. 그 안에서 실행한 `claude -p`가 칸의 대화를 바꾸지 않게 한다.
- 파일 잡기는 커밋·되돌리기·세션 종료로 풀며, 넘길 때 가드의 `release`를 사용한다.
- 전송의 신원·입력 상태·영수증 규칙은 `docs/tell-protocol.md`, 조회는 `docs/board-collaboration.md`가 정본이다.
