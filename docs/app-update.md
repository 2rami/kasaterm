# 앱 업데이트 창구 — 기기가 공식 릴리스를 스스로 받아 갈아 끼운다

조종 쪽(나쵸 도구·`tools/release`)이 작업 하나를 보내면, 기기 앱이 공식 릴리스 파일을 받아 확인하고 설치본 곁에 둔 뒤,
바쁜 학생·미저장 편집기가 없을 때 스스로 꺼서 도우미가 갈아 끼우게 한다. 새 판이 제대로 안 뜨면 이전 판으로 되돌린다.

틀은 [앱 재시작](app-restart.md) 그대로다 — 기기 정체(`target_hash`)·나쵸 승인(조종 기기가 한 번 소비한 것만)·원자적 작업
기록·앞으로만 가는 상태·강제 종료 없는 도우미·부팅 표식. 코드: `crates/kasa-socket/src/app_update.rs`(계약·검증·도우미),
`app/kasaterm/src/app_update.rs`(수락·받기 스레드·부팅 표식), 조종 쪽 작업 짓기 `tools/release/devices.py`.

## 지금 상태 — 설치 실행은 꺼져 있다

- 창구는 이 판부터 앱에 있다. 사실(`app-restart plan --json` 의 `facts`)에 `update_capability: 1` 이 실린다.
- 설치 실행은 **기기마다 기본 꺼짐**이다(`update_enabled: false`). 앱을 `KASATERM_APP_UPDATE=on` 으로 띄운 기기만 받는다 —
  꺼져 있으면 받기조차 하지 않고 `update_disabled` 로 거절한다.
- 작업은 나쵸 승인 `kasaterm_update` 를 조종 기기가 한 번 소비해야 받아들여진다. 나쵸 쪽 동작은 구현됐지만(나쵸 a2a3bd5)
  HTTP 창구는 기본 닫혀 있다(`NACHO_APPROVAL_HTTP_ACTIONS` 에 없으면 `no_approval`) — 그래서 운영에서는 지금 어떤 작업도
  통과하지 못한다.
- 앱의 「업데이트 확인」·판 번호 줄은 이 창구와 따로다 — 사람이 누르는 그 길은 전처럼 Sparkle(같은 서명 배포본)이다.

## 요청이 싣는 것 — 주소·경로·명령은 못 싣는다

`POST /app/update/jobs`(16 KiB 상한) · 소켓 `app.update_start` · `kasaterm-cli app-update start --machine ID --request FILE|-`.

```json
{"job": {"schema": "kasaterm-update/1", "job_id": "up…", "plan_hash": "…", "machine_id": "…", "target_hash": "…",
         "tag": "v0.2.1", "version": "0.2.1", "commit": "<40 hex>", "build": "<태그 커밋>",
         "asset": {"name": "kasaterm-v0.2.1.dmg", "url": "https://github.com/2rami/kasaterm/releases/download/v0.2.1/kasaterm-v0.2.1.dmg",
                   "size": 29032789, "sha256": "sha256:…", "ed_signature": "<64바이트 base64>"},
         "team": "L366799VND", "require_notarized": true, "old_pid": 4242, "created_at_ms": 0},
 "approval_id": "ap_…", "authority": "<조종 기기 machine_id>"}
```

- `asset.name` 은 그 태그의 dmg 하나(`kasaterm-<tag>.dmg`), `asset.url` 은 그 이름으로 지은 공식 주소와 **글자까지 같아야**
  한다(`check_job`). 다른 호스트·다른 파일·`..` 는 모양에서 걸린다. 크기 상한 512 MiB.
- `job_id` = `up` + fnv(계획·기기·sha256) — 같은 계획·기기·파일이면 늘 같은 id 라 다시 보내도 새 작업이 안 생긴다.
  조종 쪽 `devices.update_job_id` 와 양쪽 검사에 같은 값이 박혀 있다.
- 받을 곳은 요청이 아니라 기기가 정한다: 피드는 `https://2rami.github.io/kasaterm/appcast.xml` 하나, 공개키는 설치본에 박힌
  Sparkle `SUPublicEDKey` 와 같은 값(검사가 `scripts/build-app.sh` 와 대조한다).

## 기기가 판정하는 것 (`authorize`)

| 순서 | 거절 | 까닭 |
|---|---|---|
| 1 | `update_disabled` | 이 기기 설치 스위치가 꺼져 있다 |
| 2 | `unsupported_os` | mac 만. 윈도는 WinSparkle(사람이 [설치]) |
| 3 | 모양 | 위의 `check_job` — 공식 주소가 아니면 여기서 걸린다 |
| 4 | 재시작 쪽 거부 | 다른 기기의 답·설치본 아닌 실행·자기설치 예정·다른 작업 진행 중 |
| 5 | 정체 | `machine_id`·`target_hash`·`old_pid` 가 지금 이 기기와 다름 |
| 6 | `downgrade` | 지금 판보다 새 판이 아님 |
| 7 | 승인 | 나쵸에서 읽은 승인이 `kasaterm_update`·approved(거둔 `revoked` 아님)·조종 기기가 소비·만료 전이고, 범위(rollout·태그·판·커밋·파일 이름·sha256·크기·대상 기기+정체)가 작업과 같음 |

바쁜 학생·미저장 편집기·굽는 중은 **받기·준비를 막지 않는다** — 설치본을 안 건드리니까. 갈아 끼우기 직전에 다시 잰다.
판정은 받을 때마다 한다 — 다시 보내기도 처음처럼 나쵸에서 승인을 다시 읽는다.

승인을 읽는 곳: 이 기기에 나쵸 앱 키가 있으면 나쵸에 직접. 없으면 명부 파일에 사람이 **`update_approvals: true`** 로 위임한
기기의 `/app/update/approvals/{id}` 중계 — 재시작 위임(`restart_approvals`)으로는 update 승인을 못 읽는다. 중계 창구는 동작마다
갈려서 `/app/restart/approvals` 는 `kasaterm_restart` 만, `/app/update/approvals` 는 `kasaterm_update` 만 답한다(다른 동작은
404). 나쵸는 그 기기 자신의 읽기와 중계를 가를 수 없어서(같은 키) 여기서 거른다. 지금 명부 파일에 `update_approvals` 위임은
없다 — 키 없는 맥북은 update 승인을 못 읽는다(아래 「새로 정해야 할 것」).

## 받기·확인·준비 (`prepare`)

1. 공식 피드를 읽어 작업과 **같은 파일**(판·주소·크기·EdDSA 서명)을 말하는지 본다.
2. `curl --proto =https --proto-redir =https --max-filesize` 로 `.part` 에 받는다 → 크기 → sha256 → 이름을 바꾼다.
3. 받은 원문으로 EdDSA(ring Ed25519)를 검증한다.
4. 설치본 자신의 서명 팀을 잰다 — 요구 팀은 이것이다(요청의 `team` 은 같아야 할 뿐, 다르면 거절). 요청이 느슨한 값을 대서
   검사를 풀지 못하게, 공증도 늘 요구한다(`require_notarized: false` 는 모양에서 거절).
5. dmg 를 읽기 전용으로 열어 안 번들의 `codesign --verify --deep --strict`·서명 팀(=설치본 팀)·공증(`spctl`)·
   `CFBundleShortVersionString` 을 본다.
6. `ditto` 로 설치본 곁 `.kasaterm.app.next` 에 두고 dmg 를 닫은 뒤, 둔 번들의 서명을 다시 본다.

- **끊김은 끝이 아니다** — 피드가 안 닿거나 받기가 도중에 끊기면 반쯤 받은 것을 걷고 `retry:` 까닭만 적는다. 상태는
  그대로라 같은 작업을 다시 보내면 이어 한다.
- **내용이 다르면 끝이다** — 피드가 다른 파일을 말함·상한 초과·크기·해시·서명·팀·공증·판이 다르면 걷고 `failed`.
- 받은 파일은 내용(sha256)으로 가른 자리(`~/Library/Caches/kasaterm/updates/<sha 앞 16자>/`)에 남는다. 쓰기 전에 늘 다시
  재므로 바꿔치기된 옛 파일은 다시 받는다.

## 갈아 끼우기 (`drive` → 도우미)

`drive` 는 준비 뒤 **지금** 사실을 GUI 스레드에서 다시 받아 잰다:

- 바쁜 학생·미저장 편집기·굽는 중 → `waiting:` 까닭을 적고 준비한 채 기다린다. 같은 작업을 다시 보내면 다시 잰다.
- 승인 뒤 정체(pid·바이너리·자기설치 예정)가 바뀜, 작업이 30분 넘게 기다림 → `failed`(새 계획·승인으로 다시).
- 막힘이 없으면 도우미를 띄우고(`armed`) 앱이 정상 종료한다(`UpdateExit`). 그 순간 자기설치가 끼면 끄지 않고 `failed`.

도우미(`swap_script`, 실제 `/bin/sh`)는 재시작 도우미처럼 강제 종료가 없고 기다림에 상한이 있다:

| 상황 | 도우미가 하는 일 |
|---|---|
| 앱이 60초 안에 안 꺼짐 | 아무것도 안 바꾸고 `failed`(끄지 않는다) |
| 같은 앱이 이미 또 떠 있음·준비한 번들이 없음 | 설치본을 안 건드리고 `failed` |
| 정상 | 설치본 → `.kasaterm.app.previous`, 새 번들 → 설치본(`swapped`), `open -a` 로 다시 띄움 |
| 다시 띄우기 명령이 실패 | 이전 판을 제자리로(`rolled_back`) |
| 새 앱이 부팅 표식 없이 꺼짐(90초 안) | 새 판을 `.kasaterm.app.failed` 로 치우고 이전 판을 되돌려 다시 띄움(`rolled_back`) |
| 새 앱이 살아 있는데 표식이 없음 | 끄지 않는다 — `failed`, 이전 판은 `.previous` 에 그대로 |

새 앱은 부팅 때 `mark_update_booted` 로 빌드 표식을 대조한다: 태그 커밋과 같으면 `booted → done`, 다르면 `booted → failed`.

상태: `accepted → fetched → checked → staged → armed → helper_started → exited → swapped → launched → booted → done`,
끝은 `done`·`failed`·`rolled_back`·`cancelled`. 기록은 `~/.config/kasaterm/app-update/<id>.json`·`.events`(격리 리그는 collab
루트 아래). 업데이트가 도는 동안은 이 기기의 재시작도 막히고(`active_job`), 반대도 같다.

## 조종 쪽 — 한 번 승인, 차례로

1. **rollout 짓기** — `python3 -m tools.release.fastpatch device-plan <plan_id> --json` 이 ready·deferred 인 mac 을 계획 순서로,
   조종 기기는 맨 뒤에 묶어 `rollout` 한 칸을 낸다: `{id, release_plan, created_at_ms, approval_scope, approval_scope_hash, jobs,
   excluded}`. 파일은 `<상태 폴더>/<plan_id>/rollouts/<id>.json`. `id` 는 fnv(릴리스 plan_id·sha256·대상 `machine=hash`·만든 시각)
   — 같은 계획을 다시 굴려도 기기 작업 id 가 옛 실패 기록과 안 겹친다. `approval_scope` 는 키 8개(action·plan=rollout id·
   controller·tag·version·commit·asset{name,sha256,size}·targets[{order,machine_id,hash}]), 해시는 나쵸와 같은 정규화 sha256.
   작업은 릴리스 단계의 mac 산출물과 피드 단계가 확인한 EdDSA 서명이 있어야 지어진다.
2. **나쵸 확인** — 나쵸 도구가 rollout 을 다시 대조하고(`update_rollout_problem`) 주인에게 한 번 묻는다.
3. **러너** — `python3 -m tools.release.fastpatch device-apply <plan_id> --rollout <id> --approval ap_… --live`(나쵸 도구만:
   창 표식이 있거나 `KASATERM_RELEASE_INVOKER=nacho-tool` 이 없으면 거절, `--live` 없으면 미리보기). 계획 파일·게시(release·feed
   done)·rollout 을 맞춰 본 뒤 `kasaterm-cli app-update run --approval … --rollout FILE --record <id>.grant.json` 에 넘긴다.
4. `app-update run`(`kasa_socket::app_update::run`)은 조종 기기에서만 돈다:
   - 대상마다 지금 사실을 읽어 창구·스위치·거부 사유·`target_hash` 를 본다. 하나라도 안 맞으면 **소비하지 않고** 멈춘다
     (바쁜 학생은 막지 않는다 — 적용만 기다린다).
   - 범위를 작업들로 다시 지어 파일과 대조한 뒤 나쵸에서 **한 번** 소비하고, 소비 기록(`Grant`)을 원자적으로 남긴다. 기록이 있으면
     다시 굴릴 때 소비하지 않고 읽기만 하며(같은 승인·해시·조종 기기가 소비·만료 전·거두지 않음), 이미 새 판인 기기는 건너뛴다.
   - 대상 k 가 `done`(부팅 표식의 빌드 = 태그 커밋, 다시 읽은 사실의 pid 가 새것)을 보여야 k+1 로 간다. 조종 기기는 도우미에 넘기면
     `handed_off`.
   - 기기가 `waiting:`·`retry:` 를 적는 동안 20초마다 같은 작업을 다시 보낸다 — 기기는 그때마다 사실·승인을 다시 잰다. 기기가
     다시 보내기를 거절하면(거둔 승인·만료) 거기서 멈춘다.
   - 한 대라도 `updated`·`handed_off` 가 아니면 뒤 기기는 건드리지 않는다.

## 유효 시간 — 나쵸 창과 기기 한도

| 누가 | 한도 |
|---|---|
| 나쵸 | 결정·소비 창 10분(안 쓴 승인은 어떤 길로도 안 살아남음). 소비한 update 만 `expires_at = 소비 시각 + 대상 수 × 35분` |
| 기기 | 받은 뒤(**자기 시계**) 30분 안에 적용 못 하면 `failed`. 도우미에 넘기기 직전에 승인을 한 번 더 읽어 거둠·만료면 `failed` |
| 러너 | 대상마다 적용 전 30분 + 넘긴 뒤 종료 60초·부팅 90초·여유 60초 = 최대 33.5분 < 35분. 재기동 중 못 닿는 것은 연달아 60번까지 |

주인이 도중에 멈추려면 나쵸 `update_stop`(소비된 update 를 `revoked` 로). 다음 대상·다시 보내기·적용 직전 확인이 거절한다.
이미 도우미에 넘긴 기기는 멈출 수 없다.

## 새로 정해야 할 것

1. 나쵸 HTTP 창구에 `kasaterm_update` 를 열지 — 운영 게이트다(주인 결정).
2. 키 없는 기기(맥북)의 중계 위임 — 명부 파일에 `update_approvals: true` 를 어느 기기에 둘지. 없으면 맥북은 update 승인을 못 읽어
   「맥북 먼저」가 거기서 막힌다. 신뢰 설정이라 사람이 적는다.
3. 기기 설치 스위치를 켤 기기와 시점 — 켜는 것은 그 기기 앱의 실행 환경(`KASATERM_APP_UPDATE=on`)이다.
4. CI mac 판 서명 — 지금 CI 판은 자체 서명·미공증이라 이 창구가 **팀·공증 검사에서 거절한다**(의도한 것). Developer ID
   서명·공증을 CI 에 넣을지는 [fast-patch-release.md](fast-patch-release.md) 「서명·공증」.

## 검사

```sh
cargo test -p kasa-socket app_update      # 32건 — 기기(가짜 받기·가짜 dmg·실제 sh 도우미·가짜 앱)·러너(가짜 기기·가짜 시계)
cargo test -p kasa-mcp update_http_e2e    # 4건 — 기기 서버 둘을 실제 HTTP 로 띄워 러너가 두드린다·중계 창구 거름
python3 -m unittest tools.release.tests.test_fastpatch   # rollout·device-apply·작업 모양·id·해시 대조 포함
bash scripts/nacho-update-interop.sh      # 실제 나쵸 승인 서버(격리)와 왕복 — 고정 자료·소비·창·거둠·창구 닫힘·틀린 키
```

도우미 검사는 실제 `/bin/sh` 로 돌고, 앱은 `exec -a <설치본 실행 파일> /bin/sleep` 으로 흉내 낸다(명령줄이 설치본 경로로
시작하는 프로세스). 다루는 것: 공식 주소 외 거절, 피드 불일치, 켜짐·OS·재시작 거부·정체·다운그레이드·승인(미소비·만료·
다른 동작·범위), 한 작업 한 번·앞으로만, 둘째 작업 거절과 다시 보내기, 받기 끊김 뒤 이어 하기, 중단된 받기 재사용과 바꿔치기
파일, 확인 실패 여덟 가지와 EdDSA 불일치(흔적 없음·dmg 닫힘·설치본 그대로), 바쁜 학생 기다림 → 이어서 갈아 끼우기 → 부팅
표식으로 끝, 앱이 안 꺼짐(강제 안 함), 준비 번들 없음·둘째 인스턴스, 다시 띄우기 실패 되돌림, 부팅 전 죽은 새 판 되돌림, 틀린
빌드 표식, 받은 지 30분 넘은 작업(기기 시계), 기다리는 사이 거둔 승인. 러너: 한 번 소비·차례·조종 기기 마지막, 모두 준비돼야
소비(창구·스위치·정체), 바쁜 기기 20초 박자로 다시 보내기, 실패·거절·30분·틀린 빌드면 뒤 기기 안 건드림, 재기동 중 끊김 허용과
한도, 기록으로 다시 굴리기(두 번 소비 안 함·끝난 기기 건너뜀·다른 승인 거절·거둔 승인), 다음 기기 전 만료. 러스트·파이썬·나쵸가
같은 값을 짓는지(작업 id·target_hash·범위 해시)는 양쪽 검사에 같은 상수로 박혀 있다.

격리 리그(실제 앱 바이너리, `docs/verify-app.md` 격리 env, 2026-09-27): 스위치 꺼짐 → `409 update_disabled`, 켜짐인데 개발
실행 → `409 설치본으로 도는 앱이 아니다`(나쵸에 묻기 전에 멈춤), 다른 주소 → `409 공식 릴리스 주소가 아니다`, 16 KiB 넘는
본문 → `413`, 없는 작업 → `404`, 소켓 `kasaterm-cli app-update status` 동작, 거절 때 작업 기록 안 생김. 사실에
`update_capability: 1`·`update_enabled` 가 실린다.

Windows 는 이 창구를 쓰지 않는다(`#[cfg(unix)]`) — 윈도 타깃 컴파일은 이 맥에서 해 보지 않았다.
