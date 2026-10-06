# 카사텀 연결 mod — claude 안에서 앱으로 알리는 계약

claude 칸의 상태·상태줄·백그라운드·도구 활동·tell·대화·권한 요청을 **바깥에서 읽어 내던 것**(화면 격자
판독·기록 jsonl 꼬리·settings 훅 스크립트)을 claude 안에 실린 mod 가 엔진 이벤트로 직접 알린다.
mod 는 `app/kasaterm/collab-hooks/claude-mods/kasaterm-bridge/`, 앱 쪽 입구는 `kasa_mcp::claude_mod`.

- mod 가 없는 claude(`claude attach`·옛 판·mod 가 못 뜬 칸)와 codex·agy 는 옛 길 그대로다. 앱은 칸마다
  「이 칸은 mod 가 말한다」(hello 를 받았고 그 claude 가 살아 있다)를 따로 쥐고, 그 칸에서만 새 길을 정본으로 쓴다.
- 길은 이 기계의 loopback HTTP 하나: `http://127.0.0.1:$KASASPACE_MCP_PORT`. 아래 경로는 **loopback 에서 온 것만**
  받는다(프록시 헤더가 붙었거나 원격 peer 면 403). 다른 기기·폰은 이 경로에 직접 닿지 않는다.
- 모든 본문은 JSON, 칸은 `surface`(`$KASATERM_PANE_ID`, `%N`), 대화는 `session`(claude 세션 id).

## 이벤트 — `POST /claude-mod/event`

```json
{ "v": 1, "surface": "%10", "session": "<sid>", "events": [ { "kind": "turn", "at": 1790924345319, ... } ] }
```

응답 `{ "ok": true }`. 한 번에 여러 개를 묶어 보낸다(차례 그대로 적용). 모르는 `kind` 는 버린다.

| kind | 필드 | 엔진 자리 | 앱이 하는 일 |
|---|---|---|---|
| `hello` | `mod`(판), `claude`(엔진 판), `pid`(claude 프로세스), `cwd` | `session.start`, `/clear`·`/resume` 뒤 | 이 칸을 mod 칸으로 표시. 세션·pid 가 바뀌면 그 칸의 이전 mod 재료를 지운다 |
| `turn` | `phase`: `start`·`end`, `turn`, `text`?(시작의 프롬프트), `answer`?(끝의 답), `reason`?(`answer`·`aborted`·`refusal`·`error`) | Enter 순간의 `prompt.submit`(쉬던 칸, 훅이 버리면 `end`·`dropped`), `turn.start`, `turn.complete`(본 고리만) | 일하는 중·쉼, 활동의 prompt·say |
| `compact` | `phase`: `start`·`end`, `trigger` | `session.compact` 앞뒤(본 고리만) | 압축 중 |
| `permission` | `phase`: `ask`·`resolved`, `id`, `tool`, `outcome`?(`ran`·`denied`) | `classic.PermissionRequest`·그 도구의 `tool.call` 끝 | 승인 대기·풀림 |
| `question` | `phase`: `start`·`end`, `id` | `AskUserQuestion` 의 `tool.call` | 답 기다림 |
| `usage` | `context`{`tokens`,`window`,`percent`}, `limits`[{`kind`,`percent`,`resets_at`}], `cost_usd` | `session.measure` | 상태줄·보드 |
| `background` | `tasks`[{`id`,`type`(`subagent`·`shell`·`monitor`·`workflow`),`status`,`label`}] 전부 | `tool.call`·`$.agent.list()`·`classic.Stop` | 상태줄 수·보드 백그라운드 |
| `tool` | `phase`: `start`·`end`, `id`, `tool`, `label`(시작), `error`·`text`(끝, 결과 600자), `agent`? | `tool.call` 앞뒤 | 보드 활동(`collab.activity`) |
| `row` | `uuid`, `door` | `session.append`(본 고리만) | 대화 보기를 깨운다(내용은 기록 파일이 정본) |
| `bye` | `reason` | `session.end` | mod 칸 표시를 거둔다 |

`at` 은 mod 가 이벤트를 본 벽시계(ms). 앱은 도착 순서로 적용하고 `at` 은 표시에만 쓴다.

- **정본 조건**: 앱은 그 칸에 지금 도는 claude 의 pid 가 hello 의 `pid` 와 같을 때만 mod 사실을 쓴다(`claude_mod::live`).
  mod 없이 다시 뜬 claude 의 칸에 지난 mod 의 사실이 남아 판정을 쥐지 않게 한다.
- **옛 훅 쉬기**: mod 는 뜨면서 `KASATERM_MOD_BRIDGE=1` 을 claude 프로세스 env 에 건다. 그 claude 가 낳는 settings 훅이
  물려받아, 같은 일을 하던 `kasaterm-agent-status.sh`(도구 호출마다 돌던 백그라운드 추적)가 쉰다.

- **상태**: mod 칸의 판정은 `agent_state::resolve_module` 이 한다 — 승인 > 질문 > 압축 > 턴 > 오류 > 쉼. 엔진은 사람이 승인
  창에 답한 순간을 알리지 않으므로, 요청이 열린 뒤 그 칸에 사람이 무언가를 누르면(휠·호버 제외) 답한 것으로 본다.
- **상태줄**: `usage`·`background` 를 받으면 앱이 `$TMPDIR/kasaterm-statusline/<N>-mod.json`(세션·한도·비용·백그라운드 수)을
  쓰고, `kasaterm-cli statusline` 이 같은 세션 것만 읽어 한도(50% 넘은 것)·비용·`bg N` 을 덧붙인다.
- **대화 보기**: 내용은 기록 파일이 정본이다(재개한 세션도 전체가 보여야 한다). `GET /transcript-raw` 에 `wait_ms` 를 주면
  새 줄이 없을 때 그 칸의 다음 `row` 까지 쥐었다가 다시 읽는다 — 폰은 1.5초 바퀴 대신 행이 쌓이는 즉시 받는다.

## 권한 요청 — 원격 승인 계약

엔진의 승인 창은 `classic.PermissionRequest` 훅과 **동시에** 뜬다(2.1.287 실측: 훅 시작 5ms 뒤 창이 그려지고,
훅이 10초 뒤 `allow` 를 돌려주자 도구가 돌았다). 그래서 원격 결정은 엔진 창을 막지 않는다 — 자리의 사람은 늘 그
창으로 답할 수 있고, 먼저 온 답이 이긴다. 사람이 먼저 답하면 훅의 늦은 답은 엔진이 버린다.

### 1. mod → 앱: 요청 열기 (긴 폴링)

`POST /claude-mod/permission`

```json
{ "v": 1, "surface": "%10", "session": "<sid>",
  "request": { "id": "toolu_014U6…", "tool": "Bash", "input": { "command": "rm -rf build" },
               "preview": "rm -rf build", "cwd": "/Users/…/repo", "agent": null,
               "suggestions": [ … ], "created_at_ms": 1790924345314 } }
```

- `id` 는 그 도구 호출의 `tool_use_id`(`tool.check` 가 준다). 못 찾으면 mod 가 만든 `perm-<난수>`. 같은 id 로
  다시 열면 같은 요청이다(긴 폴링 재연결).
- `input` 은 도구 입력 **원문**(JSON, 64 KiB 까지 — 넘으면 `input_truncated: true` 와 앞부분만). `preview` 는 한 줄 요약
  (Bash 명령·파일 경로·URL). 원격 화면은 `preview` 만으로 결정하게 하지 말고 원문을 펼쳐 보여 준다.
- `suggestions` 는 엔진이 내놓은 「다시 묻지 않기」 규칙 후보다 — **원격 결정은 이것을 쓰지 않는다**(규칙을 영구히
  바꾸는 일은 자리에서만).

응답(앱이 최대 25초 쥔다):

```json
{ "decision": "allow" | "deny" | "none", "message": "…", "by": "<결정한 곳>", "pending": true }
```

- `allow`·`deny` — mod 가 `{ decision: { behavior } }` 로 엔진에 돌려준다(`deny` 면 `message` 가 모델이 읽는 이유).
- `none` + `pending: true` — 아직 결정 없음. mod 는 같은 `id` 로 다시 연다.
- `none` + `pending: false` — 요청이 끝났다(자리에서 답함·만료·세션 끝). mod 는 훅을 엔진 몫으로 돌려준다.

### 2. mod → 앱: 끝남 알림

자리에서 답했거나 도구가 끝나면 mod 가 `permission`·`resolved` 이벤트(`outcome`: `ran`·`denied`)를 보낸다.
앱은 그 요청을 닫고 쥐고 있던 긴 폴링에 `pending: false` 로 답한다. 엔진은 사람이 창에 답한 순간을 훅에 알리지
않으므로(도구가 끝나야 위 알림이 온다), 앱은 요청이 열린 뒤 그 칸에 사람 키가 들어오면 그 순간 요청을 닫는다 —
긴 도구가 도는 동안 원격 화면에 이미 답한 요청이 남지 않게. 이 알림 없이도 요청은 `expires_at_ms`
(열린 뒤 10분)에 닫힌다 — 그 뒤에는 원격 결정을 받지 않고 엔진 창만 남는다.

### 3. 결정하는 쪽 → 앱

`GET /claude-mod/permissions` — 지금 열린 요청 목록(loopback).

```json
{ "requests": [ { "id": "…", "surface": "%10", "session": "<sid>", "tool": "Bash", "input": { … },
                  "preview": "…", "cwd": "…", "created_at_ms": 0, "expires_at_ms": 0 } ] }
```

`POST /claude-mod/permission/decide` — `{ "id": "…", "surface": "%10", "session": "<sid>",
"decision": "allow" | "deny", "message"?: "…", "by": "<기기·사람>" }` → `{ "ok": true }` 또는
`{ "ok": false, "error": "unknown" | "expired" | "decided" | "mismatch" }`.
`surface`·`session` 이 요청과 다르면 `mismatch` — 다른 칸의 같은 id 로 결정이 새지 않게 한다.

- 이 두 경로는 loopback 전용이다. 폰·다른 기기의 원격 승인은 **그 기기를 계정 주인 기기로 확인하는 쪽**(관문·릴레이)이
  인증한 뒤 앱 안에서 `kasa_mcp::claude_mod::permissions()`·`decide()` 를 부른다. 이 계약은 인증을 하지 않는다.
- 요청마다 한 번 결정한다. 규칙 추가·모드 전환·「항상 허락」은 원격에 없다.
- 결정마다 감사 기록 한 줄(`<collab root>/claude-mod/permission-audit.jsonl`: 시각·칸·세션·id·도구·입력 지문·결정·by).
- 새 요청이 열리면 `kasa_mcp::claude_mod::subscribe()` 가 알린다(푸시 알림을 붙이는 자리).

## tell 받기

mod 칸에는 붙여넣기 대신 mod 의 `$.prompt.submit` 으로 넣는다 — 입력칸의 초안·한글 조합을 건드리지 않고, 정식
턴으로 들어간다. 받는 길은 긴 폴링 `GET /claude-mod/inbox?surface=%N&session=<sid>` →
`{ "messages": [ { "id": "kt1.…", "body": "…" } ] }`, mod 가 넣은 뒤 `POST /claude-mod/inbox/ack`
`{ "surface", "session", "id", "state": "submitted" | "failed", "reason"? }`.

- mod 는 **쉬는 순간에만** 받는다(턴이 없고, 승인 창·질문이 없을 때). 그래서 앱은 그 칸이 쉬는 동안만 내준다.
- `submitted` 는 `$.prompt.submit` 이 그 턴을 시작시켰다는 뜻이다(붙여넣기 길의 「Enter 를 썼다」보다 강하다).
  모델이 읽었다는 뜻은 아니다. ack 가 안 오면 영수증은 `uncertain` 이다 — 같은 id 를 다시 내주지 않는다.
- 옛 붙여넣기 길(`tell_delivery.rs`)은 codex·mod 없는 claude 몫으로 남는다.
- 길은 받는 칸만 가른다(그 칸 mod 가 살아 있나). 이름(`tell 모모이@기계`)은 CLI 가 보드 주소로 바꿔 `--address` 로 보내고
  `%N` 은 받는 앱이 같은 주소로 푼다 — 셋 다 같은 영수증 주소다. 같은 칸에 연달아 보낸 쪽지는 앞 것이 들어갈 때까지 줄을 서고,
  앞 것이 미뤄진 까닭(`waiting:busy` 등)을 함께 싣는다.
