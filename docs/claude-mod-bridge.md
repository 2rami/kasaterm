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
| `status` | `line`(`kasaterm-cli statusline` 의 ANSI 출력 한 줄) | 모델·effort·문맥·경로·브랜치가 바뀐 순간(`statusline` mod) | 엔진이 자기 상태줄을 다시 그릴 때까지 그 행에 덧그린다 |
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
- **바로 그리는 상태줄**(`statusline` mod): 엔진은 상태줄 명령을 `refreshInterval` 1초 박자에 300ms 를 더 미뤄 다시 돌려,
  모델·effort·브랜치를 바꾼 뒤 실측 0.1~2.6초(브랜치는 늘 0.6~1.1초) 늦게 그렸다. 엔진엔 mod 가 상태줄 명령을 다시 돌리게 할
  길이 없고(칸 크기 흔들기로도 안 돈다, 2.1.291 실측), 상태줄은 `ui.render` 자리도 아니다. 그래서 mod 가 바뀐 순간을
  안다 — `classic.PostModelSwitch`·`command.run`(`/effort` 는 인자로)·`turn.step`(요청마다 실제 effort)·`session.measure`·
  `classic.CwdChanged`·Bash 끝·150ms 마다 `.git/HEAD` 와 cwd 다시 읽기. 알면 엔진이 마지막으로 넘긴 입력
  (`kasaterm-cli statusline` 이 매번 `$TMPDIR/kasaterm-statusline/<N>-engine.json` 에 남긴다) 위에 그보다 늦게 안 사실만
  얹어 같은 명령을 `KASATERM_STATUSLINE_DRAW_ONLY=1`(보고·스냅샷 안 함)로 돌려 줄을 짓고 `status` 로 보낸다. 모양은 Rust
  한 곳이다. 두 경우는 짓지 않는다 — ①모델을 막 바꿨는데 그 뒤의 effort 를 모를 때: 엔진은 모델을 바꾸면 effort 를 그 모델의
  기본으로 되돌려(Opus xhigh → Sonnet medium) 옛 effort 를 얹은 줄이 엔진 줄과 번갈아 1초 남짓 보였다. 엔진은 0.1~0.2초 안에
  다시 그린다. ②짓는 사이 엔진 입력이 새로 왔을 때: 옛 입력으로 지은 줄이 엔진의 새 줄을 다음 박자까지 덮으니 새 입력으로
  다시 짓는다(2026-10-07 실측). 앱은 그 줄을 학생 표식(없으면 모델 표식) 행에 덧그리고(`screenread::paint_status_line`), 엔진이 그 행을
  다시 그리거나 5초(`STATUS_HOLD`)가 지나면 손을 뗀다 — 엔진 줄이 늘 정본이라 mod 가 틀려도 1초 남짓 뒤 바로잡힌다.
  사람이 「상태줄 직접 설정」으로 다른 상태줄을 고른 칸은 건드리지 않는다. kasaterm 밖(칸 id 없음)에서는 상태줄 자리가
  명령 상태줄 전용이라, 같은 차례·색의 줄을 프롬프트 아래 모드 자리(`SessionMode`)에 직접 그린다.
- **학생 얼굴**(`student-face` mod): 답의 첫 블록 위에 그 칸 학생의 얼굴(4×2칸 그림)과 이름을 학생 색으로 그린다.
  `GET /claude-mod/face?name=<KASATERM_CHARACTER>` 가 `{name, color, file, generation}` 을 준다 — 얼굴은 화면이 쓰는 자르기
  그대로 `$TMPDIR/kasaterm-faces/<slug>.png` 에 두고 경로만 넘긴다(mod 의 HTTP 는 글자만 받는다). 「캐릭터 외형」이
  꺼져 있으면 `{}` 라 아무것도 안 그리고, 그림이 없으면 이름만, 학생이 없는 칸은 손대지 않는다. 그림은 터미널만 그린다.
- **대화 보기**: 내용은 기록 파일이 정본이다(재개한 세션도 전체가 보여야 한다). `GET /transcript-raw` 에 `wait_ms` 를 주면
  새 줄이 없을 때 그 칸의 다음 `row` 까지 쥐었다가 다시 읽는다 — 폰은 1.5초 바퀴 대신 행이 쌓이는 즉시 받는다.

- **깃 신호는 없다**: Git 열·배지는 누가 고쳤든 같은 길로 안다 — 작업 트리 파일 감시(`kasa_mcp::git_watch`, macOS FSEvents·
  Windows ReadDirectoryChangesW)와 git 지문. codex·셸 칸에도 같아야 해서 mod 신호(`git` 이벤트)는 걷었다(2026-10-07). 옛 mod 가
  보내는 `git` 이벤트는 앱이 버린다. 상세는 `docs/remote-git-panel.md`.
- **Info 카드**(오른쪽 Info 열 맨 위 「지금 보는 칸」, `app/kasaterm/src/info_focus.rs`): 새 이벤트 없이 `tool`·`turn`·
  `background`·`usage`·`permission` 으로 짓는다. 앱은 칸마다 도는 도구(시작 시각)·끝난 도구 최근 5개(걸린 시간·실패)·
  백그라운드를 처음 본 때를 쥐고 `claude_mod::focus_facts` 로 내준다(나이는 부른 순간 기준 ms — 거울로 건너가도 시계가
  안 갈린다). 이벤트 묶음·승인 결정마다 그 칸의 판 번호(`claude_mod::seq`)가 오르고 `set_focus_listener` 가 GUI 감시를
  깨운다 — 판정이 바뀔 때만 부르는 `set_listener` 와 달리 도구 하나가 시작·끝나도 부른다. 다른 기기 칸은 원본의
  `GET /term/pane-info?schema=kasa.pane-info.v1&machine_id&pane&surface_key&since&wait_ms`(최대 8초)가 판 번호로 풀리고,
  프로세스·포트가 바뀌면 1초 안에 답한다. 신원은 Git 열과 같이 기계 id·칸·surface_key 로 확인하고 거울 칸이면 거절한다.
  답 `{schema, ok, seq, card}` 의 `card` 는 칸 종류·셸·셸 위 프로그램·폴더·모델·추론 강도·위 mod 사실·그 칸 프로세스
  트리의 listen 포트다. 옛 원본(404)·옛 판(`update_needed`)은 카드에 까닭을 적고 드물게 다시 묻는다.

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

- 거울(다른 기기·폰)은 `/term/mod-live`·`/term/mod-decide` 로 같은 요청을 보고 답한다 — `/term/*` 처럼 서버 관문(원격이면
  토큰)을 탄다. 그 거울은 이미 `/send` 로 이 칸에 무엇이든 칠 수 있어 새 권한이 아니다. 감사 줄의 `by` 는 `mirror:<곳>`.
  규칙은 `docs/mirror-render.md` 「구현 자리」.
- 이 두 경로는 loopback 전용이다. 폰·다른 기기의 원격 승인은 **그 기기를 계정 주인 기기로 확인하는 쪽**(관문·릴레이)이
  인증한 뒤 앱 안에서 `kasa_mcp::claude_mod::permissions()`·`decide()` 를 부른다. 이 계약은 인증을 하지 않는다.
- 요청마다 한 번 결정한다. 규칙 추가·모드 전환·「항상 허락」은 원격에 없다.
- 결정마다 감사 기록 한 줄(`<collab root>/claude-mod/permission-audit.jsonl`: 시각·칸·세션·id·도구·입력 지문·결정·by).
- 새 요청이 열리면 `kasa_mcp::claude_mod::subscribe()` 가 알린다(푸시 알림을 붙이는 자리).

## tell·거울 대화 입력 받기 — mod 를 거치지 않는다

tell·done 과 거울(다른 기기·폰) 대화 보기의 입력(`POST /term/chat-send`)은 mod 칸에도 입력칸 붙여넣기+Enter 로 넣는다
(`tell_delivery.rs`, `docs/tell-protocol.md`). 쉬는 칸은 새 턴, 일하는 칸은 사람이 일하는 중에 친 말처럼 진행 중인 턴
안으로 들어간다. 승인·질문 창 대기, 초안·한글 조합 보존, 영수증·순서·같은 ID 멱등은 붙여넣기 길의 것 그대로다.

10-02~10-07 에는 mod 의 받은편지함(`/claude-mod/inbox`)과 `$.prompt.submit` 으로 넣었다. 엔진은 plugin 의 프롬프트를
쉰 뒤에만 돌려(`wait` 가 거짓) 일하는 칸에서는 턴이 끝날 때까지 `waiting:busy` 로 묶였고, 받는 쪽 대화에
「The kasaterm-bridge plugin sent a message」 머리가 붙었다. 그래서 받은편지함째 걷었다 — mod 는 이제 프롬프트를 스스로
넣지 않는다.

## 보낸 칸 알림 — 토스트

내가 보낸 쪽지가 받는 쪽에서 버려지면(영수증 `failed`) 앱이 보낸 칸의 mod 에게 한 줄을 맡기고, mod 가 `$.ui.toast` 로
20초 띄운다. 길은 긴 폴링 `GET /claude-mod/notices?surface=%N&wait_ms=` → `{ "notices": [ "쪽지 못 감 → …" ] }`.

- 토스트는 대화에도 모델에도 안 들어가 턴을 일으키지 않는다. 예전에는 대기(2분)·버려짐·확인 못 함을 보낸 칸에 tell 로
  넣었는데, 그것이 정식 프롬프트라 보낸 학생의 턴을 한 시간에 여러 번 깨워 일을 끊었다(2026-10-06). 대기와 확인 못 함은
  영수증(`tell --status`)과 받는 칸의 「쪽지 N 대기」에만 남는다.
- mod 는 턴과 상관없이 늘 이 폴링을 연다(받은편지함은 쉬는 동안만). 앱은 칸이 mod 칸일 때만 맡기고(`claude_mod::notice`),
  10분 안에 안 가져가면 버린다. mod 없는 칸에는 띄울 자리가 없다 — 영수증이 정본이다.
