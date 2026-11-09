# ACP 클라이언트 — 화면 대신 이벤트로 학생 부리기

코드는 `crates/kasa-acp`(라이브러리와 runner 실행 파일 `kasa-acp`). 디스패처(`crates/kasa-students/src/dispatch.rs`)가
설정으로 켜면 쓰고, 맥 앱 묶음은 `kasa-acp` 를 본판 실행 파일 옆에 둔다.

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

## 디스패처에서 쓰기

디스패처가 새로 부르는 학생은 기본으로 runner 로 뜬다. `~/.config/kasaterm/dispatch.json` 에 `"runtime": "tui"` 를
두면 선택된 하네스의 TUI로 띄운다. 이미 떠 있는 학생의 하네스는 바꾸지 않는다.

| 키 | 뜻 |
|---|---|
| `runtime` | `"acp"`(기본) = runner, `"tui"` = 선택된 하네스의 TUI |
| `acp_harness` | 비움·`auto`(기본) = 현재 Claude·Codex 로그인 사용량 비교, `claude`·`codex` = 명시적 선택 |
| `acp_mode` | 세션 모드. 비우면 TUI 학생과 같게 둔다 — claude 는 그 기기 사용자 설정, codex 는 `agent-full-access`(칸 shim 이 TUI codex 의 샌드박스를 끄는 것과 같다). codex 자동 검토(`agent`)는 샌드박스라 학생의 `kasaterm-cli` 가 앱 소켓에 붙는 것부터 한 번 막는다 |
| `heavy_model`·`light_model` | runner 의 `--set model=…` 로 간다. `*_launcher`(게이트웨이 래퍼)는 runner 에 쓰이지 않는다 |

흐름:

모델·런처·ACP 모드를 명시하면 하네스를 자동으로 바꾸지 않는다. 사용량 자동 선택은 최신 조회에 성공한
현재 로그인끼리 비교하며, 미확인 수치를 0%로 간주하지 않는다. 다른 계정 슬롯으로의 전환은 기존 계정
전환 기능이 맡는다. 이 선택은 새 학생에게만 적용한다.

1. 칸을 하나 만들고 브리프를 `acp/<열쇠>.inbox` 에 먼저 놓은 뒤, `<앱>/kasa-acp run … --status acp/<열쇠>.status.json
   --inbox acp/<열쇠>.inbox --permission ask` 를 `surface.launch` 로 그 칸 셸(POSIX·PowerShell·cmd) 문법에 맞춰 친다.
   브리프는 셸 줄에 싣지 않는다(cmd·PowerShell 칸은 줄바꿈·큰따옴표를 못 넘긴다). 이 작업의 `acp_turn` 은 1이다.
   그래도 오는 권한 요청은 TUI 학생처럼 칸에서 사람이 고른다.
2. 매 tick 상태 파일을 읽어 board 와 같은 꼴의 줄을 지어 넣는다 — 파일 충돌(`files`)·컨텍스트 상한·캐릭터 중복은
   TUI 학생과 같은 길로 판정한다.
3. 상태의 `turn` 이 작업의 `acp_turn` 에 닿고 `idle` 이면 그 tick 에 `answer` 를 거둔다. 부팅 유예·idle 연속을
   기다리지 않는다. 상태 파일이 아직 없으면(runner 부팅 중·못 뜸) TUI 와 같은 유예·재시도로 간다.
4. 쉬는 runner 학생에게 다음 일은 inbox 로 주고 `acp_turn` 을 지금 턴 + 1 로 적는다.
5. runner 가 끝나면(`exited`) 그 턴을 마쳤으면 거두고, 아니면 까닭을 실어 실패로 접는다. 맡은 일 없이 끝난
   runner 칸은 일이 없을 때도 치운다.

`cargo run -p kasa-students --example dispatch_acp_e2e -- codex|claude` 가 이 사슬을 진짜 어댑터로 끝까지 돌린다
(먼저 `cargo build -p kasa-acp --bin kasa-acp`). 학생은 칸 환경에서 `kasaterm-cli done` 으로도 보고하지만,
디스패처는 그것을 기다리지 않는다.

## 제약 — Claude 구독 로그인

Agent SDK 문서는 사전 승인 없이 서드파티 제품이 claude.ai 로그인이나 구독 한도를 제공하는 것을 허용하지 않는다
(Agent SDK 로 만든 에이전트 포함). claude 어댑터 길을 공개 배포판의 기본 기능으로 켜기 전에 이 조항을 확인한다.
codex 어댑터는 ChatGPT 로그인을 지원한다고 명시한다.

## Windows

runner 는 어댑터를 새 콘솔 프로세스 그룹으로 띄워(`CREATE_NEW_PROCESS_GROUP`) 칸의 Ctrl-C 가 어댑터를 죽이지 않게 하고,
자기는 콘솔 제어 처리기로 Ctrl-C 를 받아 턴만 멈춘다. 끌 때는 `taskkill /T` 로 npx.cmd → node → 하네스 트리를 함께
끈다. MSI 는 `kasa-acp.exe` 를 `kasaterm.exe` 옆에 둔다(`app/kasaterm/wix/main.wxs`, `scripts/windows/package.ps1`).

## 칸 안의 runner — `kasa-acp run`

무인 학생도 칸에 보이게 둔다. 칸에서 `claude '<브리프>'` 대신 runner 를 띄우면, 화면에는 말·도구 호출·출력
몇 줄이 흐르고, 턴이 시작하고 끝날 때마다 상태 파일이 덮어써진다.

```sh
kasa-acp run --harness claude|codex [--cwd DIR] [--mode ID] [--set ID=VALUE]... \
             [--status FILE] [--inbox FILE] [--resume SESSION] \
             [--permission ask|allow|reject] [--once] [-- 첫 지시]
```

- 첫 지시는 실행 인자로 준다. 다음 지시는 칸 입력(`> `)이나 `--inbox` 파일 중 먼저 온 것을 받는다.
- 기계가 보내는 지시는 칸 입력이 아니라 inbox 파일로 보낸다. 터미널 정규 모드는 한 줄을 MAX_CANON(macOS
  1024바이트)까지만 받아 긴 한글 브리프가 잘리고, 여러 줄은 줄마다 다른 지시가 된다. 보내는 쪽은 임시 파일에
  쓰고 옮긴다. runner 는 쉬는 동안 그 파일을 읽고 지운다.
- Ctrl-C 는 도는 턴을 멈추고(`cancelled`), 멈추는 중에 한 번 더 누르거나 쉬는 동안 누르면 끝낸다. Ctrl-D 는
  칸 입력을 닫는다 — inbox 가 없으면 끝나고, 있으면 inbox 지시를 계속 받는다.
- 권한 요청은 `ask` 면 칸에서 번호로 고른다. stdin 이 터미널이 아니면 `ask` 는 `reject` 로 바뀐다 — 사람이 없는데
  묻고 기다리면 학생이 영영 멈춘다.
- codex 는 도구 셸의 env 를 `shell_environment_policy` 로 새로 지어, 사용자 설정이 `inherit = "core"` 면 `KASATERM_*` 가
  빠진다(학생의 `kasaterm-cli` 가 앱 소켓을 못 찾는다). runner 는 칸의 `KASA*`·`CODEX_HOME` 을 `CODEX_CONFIG` 의 점 경로 키
  (`shell_environment_policy.set.<이름>`)로 못박는다 — 칸 shim 이 `-c` 로 하는 것과 같다. claude 의 셸은 칸 env 를 그대로 받는다.
- claude 는 PATH 의 `claude` 를 쓰되 칸 shim(`kasaterm-shim-N`) 폴더는 건너뛴다. shim 은 대화형 실행에 훅·페르소나·
  session-id 플래그를 얹는데, 어댑터의 SDK 모드 호출에 섞이면 안 된다. 그래서 runner 학생에는 아직 협업 훅이 없다.

상태 파일(`--status`)은 한 줄 JSON 이고 임시 파일에 쓴 뒤 옮긴다.

| 키 | 뜻 |
|---|---|
| `state` | `working`(턴 도는 중)·`idle`(다음 지시 대기)·`exited`(runner 끝) |
| `turn` | 이 runner 의 몇 번째 턴인가. `working` 이면 도는 턴, 아니면 마지막으로 끝난 턴 |
| `stop` | 마지막으로 끝난 턴의 `end_turn`·`cancelled`·`max_tokens`·`refusal`…, 연결이 죽으면 `error` 와 `error` 키 |
| `answer` | 마지막 도구 호출 뒤의 말. 그게 비면 그 턴의 말 전부 |
| `files` | 이 세션이 편집 도구(edit·delete·move)로 고친 파일. 셸 명령으로 쓴 파일은 안 잡힌다 |
| `session_id` | 하네스 세션 id — `--resume` 이나 TUI resume 으로 이어받는다 |
| `context_used`·`context_size`·`cost_usd` | 마지막 사용량. claude 만 비용을 준다 |

## 써 보기

```sh
cargo run -p kasa-acp --example acp_turn -- codex /절대/작업/폴더 "할 일"
CLAUDE_BIN=$(command -v claude) cargo run -p kasa-acp --example acp_turn -- claude /절대/작업/폴더 "할 일" --allow
```

이벤트를 한 줄씩 찍고 끝에 `PromptOutcome` 을 낸다. 권한 요청은 `--allow` 면 한 번 허락, 아니면 거절한다.
