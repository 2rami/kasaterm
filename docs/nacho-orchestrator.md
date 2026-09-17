# Nacho orchestrator contract

나쵸네코(거노 개인비서, `nacho-neko` 상주 프로세스)가 카사텀 학생을 띄워 일을 맡기고,
학생이 끝나거나 막혔을 때 **구조화 보고 한 통**으로 나쵸를 깨우는 규약이다.
학생 쪽 규약 문장은 [collab-protocol.md](../app/kasaterm/collab-hooks/collab-protocol.md)의
「나쵸네코가 띄운 일」 절이 정본이고, 이 문서는 그 밑의 기계 계약이다.

## 왜 이 길 하나인가

- 나쵸는 pane 이 아니라 상주 프로세스다. `tell`·SendMessage 가 닿을 입력창이 없다.
- 보고는 나쵸가 **죽어 있어도** 남아야 한다. 그래서 디스크가 정본이고 깨우기는 덤이다.
- 나쵸 쪽에는 이미 센서가 셋 있다(taskwatch 의 pane 훑기·boardwatch·worklog 주기 reconcile).
  여기에 네 번째 「추정」을 얹지 않는다 — 학생이 **직접 말한 것**을 실어 나르고, 나쵸의
  `worklog` 한 장부가 그 보고를 소비한다. 같은 일에 소식이 여러 갈래로 오면 그때마다
  후속 턴이 다시 나갔던 것이 2026-09 의 실제 고장이라, 중복은 지문으로 입구에서 막는다.

## origin 표식 — 누가 띄웠나

나쵸가 학생을 띄울 때 **부팅 명령의 env** 로 심는다. 셰임(`claude`/`codex` 래퍼)이
그대로 물려주므로 학생·그 자식 프로세스·`kasaterm-cli` 가 전부 같은 값을 본다.

| env | 뜻 |
|---|---|
| `KASATERM_ORIGIN=nacho` | 이 학생은 나쵸가 띄웠다. 정확히 `nacho` 일 때만 성립 |
| `KASATERM_ORIGIN_CONV` | 나쵸의 대화 id(`discord:<채널>`·`slack:<채널>`) — 보고가 돌아갈 방 |
| `KASATERM_ORIGIN_TASK` | 나쵸 작업 장부(worklog)의 일 번호 |
| `KASATERM_ORIGIN_MACHINE` | 나쵸가 사는 기계의 `machine_id`(또는 명부 라벨). 비면 「이 기계」 |

브리프 첫 줄에도 같은 표식을 사람이 읽게 넣는다: `[origin=nacho task=<번호>]`.
거노가 손수 띄운 학생에는 이 env 가 없다. `kasaterm-cli nacho-report` 는 env 가 없으면
파라미터도 안 만들고 거부하고, 서버(`nacho.report`)도 `origin != "nacho"` 를 거부한다.
새 학생을 `split`/`tab` 으로 띄우면 env 는 **물려지지 않는다**(pane env 는 앱이 새로
만든다) — 나쵸 학생이 띄운 손자 학생은 나쵸 것이 아니다.

## 봉투 (`nacho-report/1`)

```json
{"schema":"nacho-report/1","report_id":"nr1.<unix-ms>.<hex>","at_ms":0,"fingerprint":"<fnv1a64>",
 "origin":"nacho","conv":"discord:123","task_id":"t-…","surface":"%7",
 "host":{"machine_id":"…","label":"nachoneko"},"cwd":"/Users/…/nacho-neko",
 "harness":"claude|codex|","character":"와카모",
 "status":"done|blocked|needs_restart|needs_approval",
 "summary":"한 일 또는 막힌 자리, 한두 줄","changed":["llm.py","tools.py"],
 "tests":"돌린 검사와 결과","next":"나쵸가 할 것 / 정해야 할 것"}
```

- `status` 는 넷뿐이다. `blocked`·`needs_restart` 는 `next` 가 비면 거부된다.
- `summary`·`changed`·`tests`·`next` 에 토큰·키·비밀번호처럼 보이는 것이 있으면 **접수 자체를
  거부**한다(`sk-`·`xox?-`·`ghp_`·`AKIA`·JWT·`Bearer …`·private key 블록·`token=…` 꼴).
  인박스는 나쵸의 대화 프롬프트로 그대로 올라가므로 여기서 새면 모델 컨텍스트로 샌다.
- 전체 16 KiB 상한. 제어문자(LF·TAB 제외) 거부. CRLF 는 LF 로.
- `fingerprint` 는 origin·conv·task_id·surface·status·summary·changed·tests·next 의
  FNV-1a-64. `report_id`·`at_ms` 는 안 들어간다 — 같은 말을 두 번 치면 같은 지문이다.
- `host` 는 학생이 도는 기계다(소켓·HTTP 로 오면 서버가 채운다). 나쵸가 사는 기계는
  `machine_id` 파라미터(=`KASATERM_ORIGIN_MACHINE`)로 따로 간다.

## 인박스 — 디스크가 정본

기본 자리 `~/.config/kasaterm/nacho-inbox/` (`NACHO_INBOX_DIR` 로 바꿈), 0700.

```
new/<fingerprint>.json   접수됨, 나쵸가 아직 안 집음
done/<fingerprint>.json  나쵸가 소비함(새 보고와의 중복 판정에만 쓰고 48시간 뒤 걷힘)
tmp/                     쓰는 중(fsync 뒤 new/ 로 rename — 반쪽 파일은 new/ 에 없다)
wake.sock                나쵸가 여는 깨우기 소켓(`NACHO_WAKE_SOCK` 으로 자리 바꿈)
```

- **원자적**: `tmp/` 에 쓰고 `sync_all` 한 뒤 `rename`. 읽는 쪽은 `new/*.json` 만 본다.
- **중복**: 같은 지문이 `new/` 나 `done/` 에 있으면 안 쓰고 기존 영수증을 `state:"duplicate"`
  로 돌려준다. 깨우기도 안 한다(`wake:"skipped"`).
- **깨우기**: `wake.sock` 에 `{"kind":"report","path":"…/new/<fp>.json"}\n` 한 줄을 보내고
  ack 한 줄(아무 내용)을 3초 기다린다. 성공이면 `wake:"socket"`, `nacho_alive:true`.
  소켓이 없거나 안 받으면 `wake:"queued"` — 파일은 그대로 있고 나쵸가 다음 부팅·폴링에서
  집는다. **시그널은 쓰지 않는다**(핸들러 없는 옛 나쵸에 SIGUSR1 을 보내면 죽는다).
  유닉스 소켓 경로는 104바이트 상한이라 깊은 경로에서는 `NACHO_WAKE_SOCK=/tmp/…` 로 뺀다.
- 나쵸가 죽어 있으면 **재기동은 이 규약의 일이 아니다** — 나쵸 앱(dockbot)·selfcare 가
  살리고, 뜬 나쵸가 `new/` 를 비운다.

## 전달 경로 (`nacho.report`)

| 학생 자리 | 길 |
|---|---|
| 나쵸와 같은 기계(`KASATERM_ORIGIN_MACHINE` 비었거나 이 기계) | `kasaterm-cli nacho-report` 가 **소켓을 안 거치고** 인박스 파일에 바로 놓는다. 앱이 꺼져 있어도, 옛 앱이라도 된다 |
| 다른 기계 | CLI → 이 기계 앱 소켓 `nacho.report {machine_id,…}` → 앱이 명부(`known_route` → machines.json base → 관문 우회)로 그 기계의 `POST /nacho/report` 에 넘긴다. 넘기기 전에 `/collab/board?scope=local` 로 그 기계의 `machine_id`·online 을 확인한다(tell 과 같은 신원 검사). 넘어간 본문은 `local_only:true` 라 받는 쪽은 다음 홉이 없다 |
| HTTP 전용 호스트 | `kasaterm-cli --api BASE nacho-report …` — 같은 메서드가 `/nacho/report` 로 간다 |

영수증(모든 길이 같은 모양):

```json
{"ok":true,"report_id":"nr1.…","fingerprint":"…","status":"done","state":"accepted|duplicate",
 "path":"…/new/<fp>.json","inbox":"…","wake":"socket|queued|skipped","wake_note":"…","nacho_alive":true,
 "via":"remote","machine":"미니"}
```

`accepted` 는 디스크에 남았다는 뜻이고 `wake:"socket"` 은 나쵸 프로세스가 받았다는 뜻이다.
어느 것도 나쵸 **모델**이 읽었다는 뜻은 아니다 — 그건 나쵸의 후속 tell·완료 보고로 안다.

CLI:

```sh
kasaterm-cli nacho-report --status <done|blocked|needs_restart|needs_approval> \
  --summary "…" [--changed "a.py,b.py"]… [--tests "…"] [--next "…"] \
  [--conv …] [--task …] [--machine …]   # env 를 덮는 용도, 보통 안 쓴다
  [--stdin]                              # JSON 객체로 필드를 주는 자동화용(origin·conv·task 는 env 가 이긴다)
  [--dry-run]                            # 봉투만 찍고 안 보낸다
```

## 나쵸가 받은 뒤 하는 일 (규약)

1. `new/<fp>.json` 을 읽고 `done/` 으로 옮긴다(옮긴 뒤에 처리 — 두 번 떠도 두 번 안 돈다).
2. `task_id` 로 작업 장부(worklog)의 그 일을 찾아 상태를 옮긴다.
   `done`→`verifying`, `blocked`→`working`+막힌 이유, `needs_restart`→`restart_pending` 후보,
   `needs_approval`→`approval_needed`.
3. `conv` 의 방에서 **장부 턴 하나**(`_run_task_turn`)를 돈다. 프롬프트 머리에 봉투를
   그대로 싣는다. 그 턴에서 나쵸가 보드·`git status`·검사를 직접 확인하고 다음을 정한다:
   학생에게 tell 로 보완 요청 / `after_restart` 등록 뒤 `selfcare.sh restart` / 승인 요청 /
   `task_ledger finish`.
4. 같은 surface 의 taskwatch `done_*` 추정 소식은 **보고가 먼저 왔으면 무시**한다 —
   센서와 보고가 같은 일을 두 번 깨우지 않게. 보고가 없는 학생(거노가 띄운 것)은
   종전대로 센서가 맡는다.

## 나쵸 쪽에 필요한 변경 (nacho-neko 레포 — 이 레포에서 손대지 않는다)

카사텀은 여기까지 만들었다: env 계약·봉투·인박스·깨우기 소켓 규격·CLI·소켓/HTTP 경로.
나쵸 쪽은 아래 넷이다. 파일은 `nacho-neko` 레포의 것이고 정확한 자리는 그쪽 세션이 정한다.

1. **띄울 때 표식 심기** — `minipane.spawn(task, work, character)` 의 부팅 줄과
   `panebridge` 의 맥북 스폰 줄:
   ```python
   origin = (f"KASATERM_ORIGIN=nacho KASATERM_ORIGIN_CONV={shlex.quote(conv)} "
             f"KASATERM_ORIGIN_TASK={shlex.quote(tid)} KASATERM_ORIGIN_MACHINE={shlex.quote(MY_MACHINE_ID)} ")
   boot = f"cd {work} && {origin}claude --dangerously-skip-permissions"
   ```
   `MY_MACHINE_ID` 는 나쵸 기계의 `~/.config/kasaterm/machine-id` 한 줄. 맥북 학생은 이
   값 덕에 미니의 인박스로 넘어온다. 브리프 첫 줄에 `[origin=nacho task=<tid>] 끝나거나
   막히면 kasaterm-cli nacho-report 로 보고` 를 붙인다. `conv`·`tid` 는 `tools.py` 의
   `kasaterm` 도구가 `worklog.current()` 와 대화 id 에서 넘긴다.
2. **인박스 소비자** `inbox.py`(신규): `NACHO_INBOX_DIR`(기본 `~/.config/kasaterm/nacho-inbox`)
   의 `new/*.json` 을 `at_ms` 순으로 읽어 `done/` 으로 rename 한 뒤 `worklog.on_report(env)`
   에 준다. 지문·report_id 를 `logs/inbox_seen.json` 에 24시간 기억해 두 번 안 돈다.
3. **깨우기 소켓** — `main.py` 부팅에서 `asyncio.start_unix_server` 로
   `NACHO_WAKE_SOCK`(기본 `<inbox>/wake.sock`, 104바이트 넘으면 `/tmp/nacho-wake.sock`)을
   연다. 연결이 오면 한 줄 읽고 `{"ok":true}\n` 을 답한 뒤 `_consume_inbox()` 를 **직렬로**
   예약한다(`_run_task_turn` 의 `_turn_lock` 안). 죽을 때 소켓 파일을 지운다.
   폴링 안전망: `_worklog_loop` 틱마다 `new/` 를 한 번 훑는다(깨우기를 놓쳐도 1분 안).
   부팅 시 `_boot_followups` 에서도 한 번 훑는다(죽어 있는 동안 쌓인 것).
4. **장부 연결** `worklog.on_report(env)`: `task_id` 로 일을 찾고(없으면 `conv`+`surface` 로
   `find_by_surface`), `env["status"]` 로 전이한다(위 「나쵸가 받은 뒤」 2번). 일에
   `report`(봉투)와 `reported_at` 을 적고 `next_at=now` 로 즉시 깨운다. `on_watch_event` 는
   같은 surface 의 `done_*` 소식을 `reported_at` 이 더 새면 버린다. `next_action` 의 머리에
   `report` 가 있으면 봉투(status·summary·changed·tests·next)를 그대로 싣는다.

검증(그쪽에서): `kasaterm-cli nacho-report --status done …` 을 나쵸 학생 창에서 치면 ①`new/`
에 파일 ②wake.log 대신 나쵸 로그에 「인박스 보고 …」 ③그 방에 장부 턴 한 번 ④같은 명령을
다시 쳐도 턴이 두 번 안 도는 것.

## 검증 자산

- `kasa-socket` 단위: `nacho_inbox::tests`(origin·status·비밀·원자성·중복·wake 소켓·지문)
  · `methods::tests::nacho_report_gates_origin_and_status`.
- `kasa-mcp` 단위: `nacho_service::tests`(신원 미확인이면 POST 0회 · 넘어간 본문의
  `local_only:true`·host 보존 · `local_only` 인데 다른 기계면 거부).
- CLI 단위: `nacho_report_requires_origin_env_and_carries_it`.
- E2E(2026-09-17, nachoneko 미니): 가짜 나쵸(`wake.sock` 리스너)를 `/tmp/nacho-e2e` 에 두고
  claude 학생(유우카 %10)·codex 학생(시로코 %12)을 `KASATERM_ORIGIN=nacho …` 부팅 env 로 띄워
  각자 같은 `nacho-report` 를 두 번 치게 했다 — 둘 다 `new/` 에 봉투 한 통(harness·character·
  surface·task_id·conv·host 정확), `wake.log` 에 학생당 한 줄(두 번째는 `duplicate`, 깨우기 없음).
  origin env 없는 창(이 문서를 쓴 사오리 창)에서는 거부.
  ⚠️실측 함정 둘: ①학생은 PATH 의 **앱 번들 CLI** 를 잡는다 — 새 `nacho-report` 는 앱을 다시
  굽고 설치해야 학생에게 닿는다(첫 시도는 둘 다 `unknown command`, 절대경로로 재시험).
  ②갓 뜬 codex 는 rollout 이 없어 `tell` 이 「conversation ambiguous」로 미룬다 — 첫 브리프는
  나쵸가 지금 하듯 `/send`(+Enter)로 넣는다. 그리고 codex 는 명령이 둘 다 실패했는데도
  `done succeeded` 를 쳤다 — 나쵸는 `done` 요약이 아니라 **인박스 봉투와 실제 변경**을 믿어야 한다.
