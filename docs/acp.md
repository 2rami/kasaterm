# ACP 클라이언트 — 화면 대신 이벤트로 학생 부리기

코드는 `crates/kasa-acp`. 아직 앱·호스트에 연결하지 않은 라이브러리다.

## 왜

카사텀은 학생의 상태(끝났나·권한을 기다리나·컨텍스트가 얼마나 찼나)를 Claude Code·Codex 의 TUI 화면과 훅으로
짐작한다. 사람이 함께 보는 칸에는 그게 맞다. 사람 없이 도는 학생(디스패처가 부르는 학생)에게는 그 짐작이
문제였다 — 막 띄운 셸 칸을 죽은 학생으로 읽어 다시 부르는 증식, idle 디바운스와 수확 유예, 입력칸 주입의
오배송.

ACP(Agent Client Protocol)는 클라이언트와 코딩 에이전트가 stdio 위 줄 단위 JSON-RPC 로 주고받는 표준이다.
클라이언트가 되면 같은 사실을 에이전트가 보낸 이벤트로 받는다.

| 알고 싶은 것 | 화면 방식 | ACP |
|---|---|---|
| 턴이 끝났나 | idle 2틱·유예 | `session/prompt` 응답의 `stopReason` |
| 컨텍스트 비율 | 상태줄 파싱 | `usage_update` 의 `used / size` |
| 세션 id | 훅의 `transcript_path` | `session/new` 응답 |
| 권한 요청 | 화면의 선택지 창 | `session/request_permission` 요청 |
| 일 시키기 | 입력칸에 붙여넣고 CR | `session/prompt` |

## 어댑터

claude·codex 는 ACP 를 직접 말하지 않아 어댑터를 띄운다. 설치된 어댑터가 PATH 에 있으면 그것을, 없으면
`npx -y` 로 실측한 판을 받는다(`launch.rs`).

| 하네스 | 어댑터 | 안에서 |
|---|---|---|
| Claude Code | `@agentclientprotocol/claude-agent-acp@0.101.0` | Claude Agent SDK 로 `claude` 를 화면 없이 돌린다. `CLAUDE_CODE_EXECUTABLE` 로 사용자의 판을 지정한다 |
| Codex | `@agentclientprotocol/codex-acp@2.5.0` | Codex App Server 를 띄워 번역한다 |

`gemini --acp`·`opencode acp`·`cursor-agent acp` 처럼 직접 말하는 에이전트는 `LaunchSpec::new` 로 바로 띄운다.

## 실측 (2026-11)

- 첫 실행의 `initialize` 는 npx 내려받기로 21초 걸렸다. 그 뒤 `session/new` 1.3초, 파일 하나 만들고 명령 하나
  돌리는 턴 10~12초.
- codex 는 명령을 자기가 직접 실행한다. 파일·터미널 capability 를 켜 줘도 `terminal/create`·`fs/*` 요청이 오지
  않았다. 그래서 이 클라이언트는 두 capability 를 끄고 권한 요청만 받는다.
- 도구 출력 자리가 다르다. codex 는 `_meta.terminal_output_delta.data` 에 조각으로(`ToolOutput::Delta`), claude 는
  `rawOutput` 에 통째로(`ToolOutput::Full`) 보낸다.
- 세션은 하네스의 평소 대화 파일로 남는다 — codex `~/.codex/sessions/…/rollout-…-<id>.jsonl`, claude
  `~/.claude/projects/<cwd>/<id>.jsonl`. 사람이 TUI 의 resume 으로 이어받을 길이 있다(이어받기는 미실측).
- 기본 모드는 사용자 설정을 따른다(claude 는 그 기기 설정의 `bypassPermissions`, codex 는 자동 검토 `agent`).
  무인 학생은 `set_mode`·`set_config_option` 으로 모드를 명시해야 한다.
- 어댑터는 npx → node → 하네스로 손자를 낳는다. 프로세스 그룹으로 띄우고 그룹째 죽인다(시험
  `shutdown_kills_the_whole_process_group`).

## 제약 — Claude 구독 로그인

Agent SDK 문서는 사전 승인 없이 서드파티 제품이 claude.ai 로그인이나 구독 한도를 제공하는 것을 허용하지 않는다
(Agent SDK 로 만든 에이전트 포함). claude 어댑터 길을 공개 배포판의 기본 기능으로 켜기 전에 이 조항을 확인한다.
codex 어댑터는 ChatGPT 로그인을 지원한다고 명시한다.

## 써 보기

```sh
cargo run -p kasa-acp --example acp_turn -- codex /절대/작업/폴더 "할 일"
CLAUDE_BIN=$(command -v claude) cargo run -p kasa-acp --example acp_turn -- claude /절대/작업/폴더 "할 일" --allow
```

이벤트를 한 줄씩 찍고 끝에 `PromptOutcome` 을 낸다. 권한 요청은 `--allow` 면 한 번 허락, 아니면 거절한다.
