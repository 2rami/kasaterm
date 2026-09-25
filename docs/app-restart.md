# 카사텀 앱 재시작 — 등록된 어느 기기에서든, 한 대씩

대상은 **카사텀 앱 하나**다. OS 재부팅·나쵸 봇·request-journal·펫 재시작이 아니다. 대상 기기는 명부의
안정 `machine_id` 로만 고른다(호스트·명령을 받지 않는다). 핵심 코드는 `crates/kasa-socket/src/app_restart.rs`,
앱이 사실을 재는 곳은 `app/kasaterm/src/app_restart.rs`.

## 지금 되는 것과 안 되는 것

| 단계 | 상태 |
|---|---|
| 계획·거부 사유(`plan`) · 작업 상태(`status`) | 된다 — 새 판이 깔린 앱끼리 |
| 실행(`run --approval ap_…`) | 카사텀 쪽은 된다. **나쵸 승인 창구가 켜져야** 실제로 돈다 — 지금 나쵸는 `approvals_disabled`(아래 「켜려면」) |
| 앱 안 실행(수락 판정 → 도우미 → `exiting()`) | 된다 — 나쵸에서 읽은 승인과 자기 사실이 맞을 때만 |
| 윈도우 기기 | 미지원 OS 로 거부 |

**첫 배포는 사람이 한다.** 이 창구는 새 판이 깔린 앱에만 있다 — 두 기기에 굽고 설치한 뒤 처음 한 번은 사람이
직접 앱을 껐다 켜야 한다(옛 판 앱은 `capability_missing` 이나 닿지 못함으로 거부된다).

### 켜려면(권한 갈림길)

1. 나쵸(마시로): `approvals.ACTIONS` 에 `kasaterm_restart`(risk low), `GET /api/app/approvals/{id}`·
   `POST …/{id}/consume {scope, consumer_machine_id}`, 주인 확인 단추로만 decide, `NACHO_APPROVALS=on` env 게이트(기본 off).
2. 주인: 그 게이트를 켜는 결정. 실제 켜기(env·재시작)는 나쵸 selfcare.
3. 나쵸 키가 없는 기기(맥북): 그 기기의 명부 **파일**(`~/.config/kasaterm/machines.json`)에서 나쵸가 도는 기기 항목에
   `"machine_id": "<그 기기 id>"` 와 `"restart_approvals": true` 를 사람이 적는다. 이 표시가 없으면 그 기기는 어떤 승인도
   믿지 않는다(아래 「믿는 것과 안 믿는 것」).
4. 두 기기에 이 판 설치 + 한 번 손으로 재시작.

## 명령

```sh
kasaterm-cli app-restart plan                          # 이 기기만
kasaterm-cli app-restart plan --machine <id>,<id>      # 여러 기기 — 조종 기기는 알아서 맨 뒤
kasaterm-cli app-restart plan --machine <id> --json    # 나쵸·스크립트용 전체 계획
kasaterm-cli app-restart status <job_id> [--machine <id>]
kasaterm-cli app-restart run                           # 지금은 계획을 보여 주고 거부(종료 코드 3)
```

- `machine_id` 찾기: `kasaterm-cli board --all` 의 `sources[].machine_id`, 또는 `curl -s 127.0.0.1:8765/machines`
  의 `route`(`~<machine_id>`).
- HTTP: `GET /app/restart/facts`(이 기기 사실) · `GET /app/restart/jobs/<id>` · `POST /app/restart/jobs`(403).
  다른 기기는 명부 경유 `/m/~<machine_id>/app/restart/facts` — 명부에 없는 기기는 `not registered` 로 거부된다.
- 조종 기기(명령을 친 기기)의 id 는 그 앱이 스스로 댄 값이다.

## 계획이 거부하는 것

실행 직전에도 같은 검사를 다시 한다. 하나라도 걸리면 그 기기는 손대지 않는다.

| code | 뜻 |
|---|---|
| `unreachable` | 명부에 없거나 닿지 않는다(이유를 함께 싣는다) |
| `identity_mismatch` | 물은 기기가 아닌 기기가 답했다 |
| `unsupported_os` | macOS 가 아니다 |
| `capability_missing` | 그 앱엔 재시작 창구가 없다(옛 판) |
| `not_installed_app` | `~/Applications`·`/Applications` 의 설치본이 아니다(개발 실행) |
| `busy_students` | 일하는 중·압축 중·승인/질문을 기다리는 학생, 또는 학생이 아닌 창에서 명령이 도는 중(등록 서버 창은 뺀다) |
| `unsaved_editors` | 저장 안 한 편집기 |
| `self_install_pending` | 끄면 자기설치가 새 빌드를 깐다 — 재시작이 업그레이드가 되므로 거부 |
| `bake_in_progress` | `build-app.sh` 가 돈다 |
| `job_in_flight` | 이미 진행 중인 재시작 작업이 있다 |
| `stale_facts` | 사실이 30초보다 오래됐다 |

## 실행 절차

1. 주인이 나쵸 대화에서 재시작을 부탁한다 → 나쵸 도구가 `kasaterm-cli app-restart plan --machine … --json` 의 `.scope` 를
   **그대로** 승인 요청으로 만들고(모델이 JSON 을 짓지 않는다), 대상·도는 학생 수·「멈추고 --resume 로 돌아온다」를
   주인 확인 단추로 보인다. 누르면 나쵸가 decide(10분 만료, 한 번). `approval_id` 가 도구 결과로 나온다.
2. `kasaterm-cli app-restart run --machine <id>,… --approval ap_…` (나쵸 도구가 부른다). 조종 기기 앱이 계획을 다시 재고
   나쵸 `consume(approval_id, 지금 scope, 조종 기기 id)` — 서버가 해시를 다시 계산해 **한 번만** 성공. 대상이 바뀌었으면
   `scope_changed`, 이미 썼으면 `already_used`, 다른 기기가 쓰려 하면 `wrong_consumer` → 아무 기기도 안 건드린다.
3. 원격 기기부터 한 대씩, **조종 기기는 마지막**(조종 앱이 꺼지면 원격 터널도 내려간다).
4. 대상 앱은 요청이 뭐라 하든 **나쵸에서 읽은 승인**(요청한 그 id·승인됨·조종 기기가 소비함·만료 전·자기 machine_id 와
   지금 해시가 scope 에 있음)과 **지금 자기 사실**(거부 사유 없음)로만 판정한다. 나쵸 키가 없는 기기는 명부 파일에 위임한
   기기 하나의 카사텀이 읽기만 중계한다(`GET /app/restart/approvals/{id}` → `{machine_id, approval}`, 받는 쪽이 machine_id 를
   대조) — 키는 그 기기 밖으로 안 나간다.
5. 통과하면 작업을 적고(`create_new`, 같은 작업은 한 번) 도우미를 띄운 뒤 스스로 `exiting()` 으로 끈다(세션 저장·창 크기·
   터널 정리). 끄기 직전 자기설치가 끼었으면 끄지 않고 실패로 적는다.
6. 도우미: 옛 pid 가 사라질 때까지 기다림(60초 상한, **강제 종료 없음**) → 설치본을 도는 다른 앱이 있으면 멈춤 →
   `/usr/bin/open -a <설치본>` → 새 pid 기록. 새 앱이 부팅 때 「도착」을 적는다.
7. 조종 쪽이 다시 닿아 새 pid·같은 바이너리를 확인하면 `verified`. 못 닿거나(90초) 바이너리가 다르면 실패.
8. 앞 기기가 실패하면 뒤 기기(조종 기기 포함)는 `skipped`. 조종 기기는 넘긴 뒤(`handed_off`) 재기동 후 `status` 로 본다.

작업 기록은 대상 기기에 남는다: `~/.config/kasaterm/app-restart/<job_id>.json`(작업, `create_new` 로 한 번만) ·
`<job_id>.events`(상태 줄, 앞으로만) · `<job_id>.helper.log`. 상태: `accepted → helper_started → exited → relaunching
→ launched → booted → verified`, 끝은 `failed`·`cancelled`(취소는 도우미가 뜨기 전까지만).

도우미 env 는 허용목록뿐이다(`PATH` 고정·`HOME`·`USER`·`LOGNAME`·`TMPDIR`·`LANG`·`LC_*`) — claude 마커·토큰·카사텀
소켓 경로가 새 앱으로 새지 않는다. 새 앱은 LaunchServices 가 띄우므로 도우미의 부모 관계도 안 물려받는다.

## 다시 켜면 이어지는 것 / 아닌 것

- 이어진다: 방·창 배치, 창마다 cwd, 세션 id 가 있는 claude·codex 대화(`--resume`), 등록 서버(`kasaterm-cli server`).
- 이어지지 않는다: 일반 셸에서 돌던 명령·프로세스(새 셸로 돌아온다), 세션 id 없는 창, 화면 밖 상태.
- 그래서 계획은 「이어지는 학생 대화」「새 셸로만 오는 창」「등록 서버」 수를 따로 보여 준다.

## 승인 경계 — 한 번 승인한 뒤

주인이 계획 하나(해시·기기 목록)를 승인하면 그 범위 안에서는 기기마다 다시 묻지 않고 한 대씩 진행한다.
다음 중 하나라도 생기면 **그 자리에서 멈추고 다시 묻는다**:

- 계획 뒤 대상이 바뀜(다른 pid·다른 바이너리·자기설치 예정이 새로 생김) — 해시 불일치
- 자기설치가 끼어 재시작이 업그레이드가 되는 판(`self_install_pending`, 다시 뜬 바이너리가 다름)
- 바쁜 학생·미저장 편집기·굽기가 새로 생김
- 앞 기기 실패·재기동 뒤 다시 닿지 못함·제한 시간 초과
- 권한·설치·launchd·관문 변경이 필요해 보이는 모든 경우(이 절차는 그런 것을 하지 않는다)

## 이 절차가 하지 않는 것

강제 종료(`kill -9`·`pkill`·`killall`), 빌드·pull·push·설치, launchd 등록, SSH 관문(`nacho-tunnel-guard`·
`kasaterm-remote`) 변경, 명부 밖 호스트, 임의 명령. claude pane 안에서 앱을 띄우는 일(`open`·`relaunch.sh`)도
하지 않는다 — 재기동은 앱 밖 도우미가 LaunchServices 로 한다.

## 믿는 것과 안 믿는 것

- 믿는다: 나쵸가 적은 승인 상태(서버 원자적 1회 소비), 대상 앱이 지금 잰 자기 사실, 그리고 키 없는 기기에서는 사람이
  명부 파일에 `restart_approvals: true` 로 위임한 기기 **하나**의 중계.
- 안 믿는다: 요청 본문의 어떤 주장(`approved:true` 등 — 필드 자체를 안 읽는다), 요청이 말하는 대상 정체, 오래된 계획,
  **요청이 대는 승인 기기**(`authority` — 고르는 데 쓰지 않고, 위임한 기기와 다르면 거부), 명부에 등록됐다는 사실만
  (스스로 알려 와서 생기는 손님 항목·폴링으로 배운 id 는 위임이 아니다), `find_route` 의 주소(끊긴 파일 항목을 같은 id 를
  댄 손님 경로로 갈아 끼운다 — 위임 기기 주소는 파일 항목에서만 읽는다).
- 나쵸 쪽(마시로 확인, 2026-09-26): 소비는 consumer 가 나쵸가 도는 기기이면서 저장된 `scope.controller` 일 때만 한 번
  (그래서 **조종 기기는 나쵸가 도는 기기**여야 한다). GET 도 앱 키 필수, `consumed_by`·`consumed_at_ms` 는 저장값만, HTTP 로는
  `kasaterm_restart` 승인만 읽고 쓰며 결정 창구는 주인 확인 단추뿐이다.
- 한 승인은 기기마다 작업 하나만 만든다 — 작업 id 가 (계획 해시, 기기)이고 `create_new` 로 적는다. 재기동 뒤에는 pid 가
  바뀌어 같은 승인이 대상에 맞지 않는다.
- 남는 위험: 위임한 기기 자체나, 끊긴 터널 포트를 가로챈 같은 사용자 프로세스는 위조 응답을 낼 수 있다. 닫는 길은 나쵸가
  승인 view 에 서명하고(Ed25519, 대상 `{id, action, scope_hash, state, expires_at_ms, consumed_at_ms, consumed_by}` 정렬
  JSON) 각 기기가 공개키를 고정하는 것 — 새 키 발급이라 주인 결정, 3단계 후보.
- 같은 사용자의 로컬 프로세스(소켓 접근자)는 승인된 **그 동작**을 먼저 쓸 수는 있어도 새 권한을 만들 수 없다 —
  승인 생성·결정은 나쵸와 주인 확인 단추에만 있다.

## 확인 방법(나쵸)

```sh
kasaterm-cli app-restart plan --machine <미니 id>,<맥북 id> --json | jq '.targets[] | {machine_id, controller, refusals}'
curl -s 127.0.0.1:8765/app/restart/facts | jq '{machine_id, pid, busy, install_pending, restorable_sessions}'
kasaterm-cli app-restart status <job_id> --machine <id>     # 실행이 켜진 뒤
```

검사: `cargo test -p kasa-socket --lib app_restart` — 계획·거부·해시, 나쵸 승인(한 번·만료·범위·소비 기기·자기주장 거부),
신뢰원(등록만으로는 위임 아님·요청이 고른 기기엔 연결조차 안 함·다른 기기가 답한 중계·다른 소비 기기·다른 승인 id 거부),
대상 판정, 작업 기록, 가짜 앱으로 도우미(대기·재기동·강제 종료 없음·둘째 인스턴스 거부·부모가 죽어도 생존·env 허용목록),
순차 실행(순서·실패 중단·해시 변경·오프라인), **격리 E2E 두 벌**(원격 먼저→조종 기기 마지막·같은 승인 재전송 무효 /
원격 실패 시 조종 기기 무손상).

나쵸와의 상호운용: `scripts/nacho-restart-interop.sh [나쵸 레포]` — 나쵸 레포의 실제 승인 서버를 임시 폴더·가짜 앱 키로 띄우고
①고정 자료(`approval.kasaterm_restart.implemented.json`)의 대상 해시·plan 해시·scope·view 를 카사텀 러스트 함수로 대조
②러스트가 세운 scope 로 이 앱의 실제 나쵸 클라이언트가 소비 → 나쵸에서 읽은 승인으로 대상 판정(위임 중계 포함), 재소비·
다른 소비 기기·바뀐 scope·대기 중 승인은 막히고 막힌 시도는 한 번을 쓰지 않음 ③스위치 off 면 승인된 요청도 503, 틀린 키 403.
운영 키·운영 승인·앱은 건드리지 않는다. 두 레포 중 한쪽의 해시·모양이 바뀌면 여기서 먼저 깨진다.

앱 수준의 실제 종료→재기동은 격리 리그에서 돌리지 않는다: 재기동은 LaunchServices 가 해서 새 앱이 격리 env 를 잃고 사용자
세션을 복원하려 든다(2026-08-15 사고와 같은 모양). 앱 수준은 가짜 나쵸로 소비·중계·자기주장 거부·거부 계획의 무소비까지만 본다.
