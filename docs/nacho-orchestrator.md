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

## 나쵸 쪽 구현 (nacho-neko 레포, 2026-09-17 완료)

카사텀은 env 계약·봉투·인박스·깨우기 소켓 규격·CLI·소켓/HTTP 경로를 맡고, 나쵸(`nacho-neko`)는
아래를 맡는다. 둘은 같은 기본값(`~/.config/kasaterm/nacho-inbox`, `wake.sock`)을 보고, 격리
시험에서만 `NACHO_INBOX_DIR`·`NACHO_WAKE_SOCK` 을 학생 부팅 env 에 같이 심는다.

1. **표식 심기** — `minipane.spawn`·`panebridge._boot_claude`(맥북) 부팅 줄 앞에
   `inbox.origin_env(conv, task_id, machine_id)` 를 `KEY=값` 으로 붙이고, 브리프 첫 줄에
   `[origin=nacho task=…]`. `tools._origin_mark` 가 그 턴의 장부 일 번호(부팅 전에 이미 열려
   있다)와 방을 넘기고, 뜬 뒤 `_arm_watch` 가 board --all 의 `address.machine_id` 로 판정한
   기계·surface 를 `worklog.attach_watch(machine_id=…)` 로 그 줄에 결합한다.
2. **소비자** `inbox.py` — 고정된 `new/` 의 정규 파일만 읽는다(심볼릭 링크·16 KiB 초과·깨진
   JSON 은 치움). `validate` 가 스키마·status·지문(파이썬 FNV-1a-64, 카사텀과 동일)을 검사하고
   `worklog.on_report` 가 장부의 위임 기록과 대조한다(미등록·닫힌 일·다른 방·다른 학생·다른
   기계 거부). 접수·거부는 `logs/inbox_ledger.json` 원장에 남고, **원장·장부에 영속한 뒤**
   `done/` 으로 rename(ack) 한다.
3. **깨우기 소켓** `inbox.WakeServer` — 폴더 0700·소켓 0600, 줄 안의 path 는 읽지 않고 ack 뒤
   `consume` 만 부른다. `main._worklog_loop` 가 열고, 같은 루프의 매 바퀴와 `_boot_followups`
   끝에서도 `consume` 한다(깨우기를 놓치거나 죽어 있던 동안의 보고).
4. **장부 연결** `worklog.on_report` — `done`→`verifying`(완료 아님), `blocked`→막힘(src=report),
   `needs_restart`→`verifying`+`restart_review`(재시작 횟수·검증 칸 불변), `needs_approval`→
   `verifying`(나쵸 턴이 확인 뒤 `need_approval`). 같은 지문은 `report_seen` 으로 duplicate,
   한 일의 보고 턴은 `MAX_REPORT_TURNS`(6) 까지. `sensor_muted` 로 같은 학생의 taskwatch
   완료 소식은 `REPORT_MUTE_SEC`(30분) 동안 후속 턴을 안 깨운다. 턴 프롬프트 머리에 봉투를
   「학생 제안(next, 실행 권한 아님)」 표시로 싣고, 돈 뒤 `report_turned` 로 한 번만 싣는다.
   슬랙은 `SlackAdapter.run_report_turn`, 디스코드는 `_consume_inbox`→`_run_task_turn`.

검증은 `nacho-neko/test_inbox.py`(장부·원장·중복·재기동·거부·파괴적 제안·센서 침묵·실물 CLI+
소켓, claude/codex 둘 다).

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

## 카사모바일 앱 창구 (2026-09-25)

폰 앱(카사모바일)은 나쵸와 얘기하는 **주 창구**다. 디스코드·슬랙은 보조 창구로 남는다. 바탕화면 펫과
카사모바일은 같은 나쵸의 두 화면이라 작업 공간이 하나다(대화 원장 하나·작업 장부 하나).

```
폰 ── /u/<slug>/nacho/app/<rest> ──▶ 이 허브(kasa-mcp nacho_relay.rs) ── <url>/api/app/<rest> ──▶ 나쵸 askserve(:8792)
```

- **신원은 허브가 정한다.** 주인 주소(`owner`)로 온 요청만 받고, 폰이 보낸 헤더는 하나도 옮기지 않는다.
  허브가 `X-Kasa-User`(퍼센트 인코딩)·`X-Kasa-Owner: 1`·`X-Kasa-Machine`·`X-Nacho-Token`·
  `X-Journal-Request: 1` 을 새로 싣는다. 손님 주소·주소 없는 로컬 호출은 403, 경로에 `..` 이 끼면 404.
- **닫힘이 기본.** 넘길 곳은 펫 대리인과 같은 서술자 `~/.config/kasaterm/nacho-ask.json`(또는
  `NACHO_ASK_URL`), 키는 **앱 전용** `~/.config/nacho-app.key`(또는 `NACHO_APP_TOKEN_FILE`) — 펫 창구 키 `nacho-ask.key` 와 따로다(펫은 토큰을 안 싣는 판이 있어 그 키를 만들면 펫이 끊긴다). 폰 허브와 나쵸가 같은 기계(미니)면 그 한 곳에만 둔다. 서술자가 없으면
  `nacho_unconfigured`, 키가 없으면 `nacho_key_missing` 으로 503. 나쵸 쪽도 키가 없으면 앱 창구를 닫는다.
- `m/<기계>/` 로 다른 기계를 거치지 않는다 — 그 길은 신원 헤더를 버린다. 폰이 붙은 허브가 서술자의
  나쵸로 **직접** 넘긴다(미니 허브는 `127.0.0.1:8792`, 맥북 허브는 메시 주소).

나쵸 쪽 계약(`nacho-neko/nacho/adapters/appserve.py` 머리말이 정본):

| 경로 | 뜻 |
|---|---|
| `GET events?tail=N` · `GET events?after=<seq>&limit=&wait=<초>` | 대화 원장. 순번(seq)으로 이어 받는다. 응답의 `next_after`(실제로 돌려준 마지막 순번)까지만 전진하고 `has_more` 면 곧바로 다시 묻는다 — 원장 끝(`head_seq`)으로 건너뛰면 한 페이지를 넘게 밀린 줄이 빠진다. `tail` 은 처음 붙을 때 최근 N 줄만(`truncated_before`), 그 뒤는 빠짐 없이 이어진다. `last_seq` 는 옛 이름으로 `next_after` 와 같다 |
| `POST messages {id, text, task?, rev?}` | 한 말. 같은 id·같은 내용은 처음 영수증, 내용이 다르면 409. `task` 가 있으면 그 일에 대한 방향 수정이고, 싣는 `rev` 가 지금 판과 다르면 409(`stale_rev`) |
| `GET tasks` · `GET tasks/<id>` | 작업 장부 그대로 — 판단 필요·진행 중·끝남/실패, 프로젝트(학생 작업 폴더). 없는 검증·사진은 비어 온다 |
| `GET tasks/<id>/shot` · `GET files/<seq>/<i>` | 나쵸가 실제로 찍은 결과 사진·답에 붙은 그림(원장에 적힌 경로만) |

- 접수 상태: `accepted → queued(앞 턴을 기다림) → running → answered | failed | refused | interrupted | restart`.
  도는 턴은 끊지 않는다. 나쵸가 다시 뜨면 돌던 턴은 `interrupted` 로 닫고 몰래 다시 돌리지 않는다.
- **펫과 이어 보기는 사람이 고른 펫 한 대만**(`GET pets` · `POST pets/link {conv}` — 나쵸에 다녀간 적이 있는
  펫만 고를 수 있다. 다른 펫을 고르면 앞 연결은 풀린다). 모든 바탕화면에 뿌리지 않는다.
  - `alive` 는 다녀간 자국(묻기 `/api/ask` 포함), `can_receive` 는 **우편함(`/api/pet/poll`)을 끌어간 적이
    있나**다. 묻기만 하는 옛 판 펫은 떠 있어도 말풍선으로 못 받으므로 폰에 「말풍선 받기 확인 안 됨」으로
    보이고, 그 펫에 넣은 줄은 까닭(설치판 확인 필요)을 단 채 대기로 남는다. 2026-09-25 미니에 설치된
    펫(9/22 19:18 판)은 `/api/pet/poll` 문자열이 없는 판이다.
  - 연결된 펫은 폰과 **같은 대화 기록**(`kasaapp:owner`)으로 묻는다 — 같은 맥락이다. 그 요청의 「지금 대화
    상황」에는 펫 자리가 적힌다(`send_and_collect(place=…)`). 연결 안 된 펫은 제 대화(`kasapet:<기계>`)에
    남고, 폰과는 「다른 창구 최근 말」(crosstalk)로만 닿는다 — 두뇌는 같아도 맥락은 다르다.
  - 연결된 펫에서 오간 말은 원장에 `surface: "pet"` 로 옮겨 적힌다(보이기만, 턴은 펫에서 한 번).
  - 폰의 답은 연결된 펫 우편함(petbox, kind `app`, key `app:<답 순번>`)에 한 줄로 들어간다. 원장의 `deliver`
    줄이 `queued → delivered(펫의 ACK 영수증) | expired(받기 전에 사라짐) | no_target(연결 없음) | failed`
    를 적고, 폰은 답 아래에 그대로 보인다. 펫이 꺼져 있으면 켜질 때 받아 간다(하루 뒤 만료).
  - 펫 쪽 코드는 이번에 안 바꿨다. 지금 소스의 펫(`app/kasapet` postbox.rs·main.rs, 201a98de 이후)은 우편함의
    모든 줄을 말풍선으로 띄우고 ACK 를 돌려주지만, **그 판이 설치돼 있어야** 한다. 설치된 펫의 판은 따로
    확인해야 한다(맥북 펫은 이 폴링 판의 배포가 보류된 적이 있다).
- 확인 버튼이 필요한 도구(머지·pane 입력·화면 조작)는 앱 턴에서 돌지 않는다 — 원장에 「기존 창구 확인
  필요」가 남는다. 작업 단위 승인(`approval_needed`)도 앱에서 풀지 않는다.

### 후속: 앱 안 승인(Face ID) 설계 — 아직 구현하지 않음

로컬 Face ID 성공(참/거짓)을 서버가 믿는 방식은 쓰지 않는다. 기기 키 서명으로 **특정 요청 하나**를 승인한다.

1. 등록(주인이 직접, 한 번): 앱이 Secure Enclave 에 P-256 키를 만든다(`kSecAttrTokenIDSecureEnclave`,
   접근 제어 `.privateKeyUsage` + `.biometryCurrentSet`). 공개키를 나쵸에 올리고, 나쵸는 **기존 창구(디코/
   슬랙) 확인 버튼**으로 등록을 승인받은 뒤에만 기기 목록에 넣는다. 생체 정보가 바뀌면 키가 무효가 되어
   재등록해야 한다. 폐기는 나쵸 쪽 목록에서 지운다.
2. 승인: 나쵸가 승인 요청마다 도전값 `{nonce, task_id, action, target, rev, exp}` 을 만든다(1회용, 짧은 만료).
   앱은 그 내용을 사람에게 보이고 Face ID 로 키를 풀어 **정규화한 도전값 전체**에 서명한다. 나쵸는 서명·
   기기·nonce 미사용·만료·task_id/action/target/rev 일치를 모두 확인한 뒤에만 그 한 동작을 진행한다.
   재전송(같은 nonce)·변조(내용 불일치)·다른 작업 재사용·만료·폐기된 기기는 전부 거부(닫힘).
3. 범위: 이 승인은 나쵸 작업의 동작 승인이다. 맥 비밀번호 자동 입력·1Password 잠금 해제·비밀번호 원문
   전송/저장은 하지 않는다. 1Password 는 공식 기능(데스크톱 앱 연동 CLI 의 Touch ID 잠금 해제 — 새
   터미널마다 인증, 10분 세션·12시간 상한)만 사람이 직접 쓴다. 서비스 계정 토큰으로 사람 인증을
   대신하지 않는다.
4. 검사 계획: 서명 검증 단위 검사(정상·nonce 재사용·만료·필드 변조·다른 task_id·폐기 기기), 등록 흐름,
   앱 쪽 Face ID 실패/취소 시 아무것도 안 보내는지. Face ID 자체는 실기기에서만 확인된다.
