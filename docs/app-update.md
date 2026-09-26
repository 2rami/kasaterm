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
- 작업은 나쵸 승인 `kasaterm_update` 를 조종 기기가 한 번 소비해야 받아들여진다. **이 동작은 나쵸에 아직 없다** — 그래서
  운영에서는 지금 어떤 작업도 통과하지 못한다. `device-plan --json` 이 기기마다 보낼 작업과 승인 범위를 지어 보이기만 한다.

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
| 7 | 승인 | 나쵸에서 읽은 승인이 `kasaterm_update`·approved·조종 기기가 소비·만료 전이고, 범위(계획·태그·판·커밋·파일 이름·sha256·크기·대상 기기+정체)가 작업과 같음 |

바쁜 학생·미저장 편집기·굽는 중은 **받기·준비를 막지 않는다** — 설치본을 안 건드리니까. 갈아 끼우기 직전에 다시 잰다.
승인은 재시작과 같은 신뢰원에서 읽는다(이 기기의 나쵸 앱 키, 없으면 사람이 명부 파일에 위임한 기기의 중계).

## 받기·확인·준비 (`prepare`)

1. 공식 피드를 읽어 작업과 **같은 파일**(판·주소·크기·EdDSA 서명)을 말하는지 본다.
2. `curl --proto =https --proto-redir =https --max-filesize` 로 `.part` 에 받는다 → 크기 → sha256 → 이름을 바꾼다.
3. 받은 원문으로 EdDSA(ring Ed25519)를 검증한다.
4. dmg 를 읽기 전용으로 열어 안 번들의 `codesign --verify --deep --strict`·서명 팀(설치본과 같아야)·공증(`spctl`)·
   `CFBundleShortVersionString` 을 본다.
5. `ditto` 로 설치본 곁 `.kasaterm.app.next` 에 두고 dmg 를 닫은 뒤, 둔 번들의 서명을 다시 본다.

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

## 조종 쪽

`python3 -m tools.release.fastpatch device-plan <plan_id> --json` 이 기기마다 `job`(위 모양)·`job_problem`·`scope`(나쵸에
만들 승인 범위)를 싣는다. 작업은 릴리스 단계가 잰 mac 산출물(크기·sha256)과 피드 단계가 확인한 EdDSA 서명이 있어야 지어진다.
상태는 `kasaterm-cli app-update status JOB --machine ID` 또는 `GET /app/update/jobs/{id}`.

## 새로 정해야 할 것

1. 나쵸 승인 동작 `kasaterm_update` — 위 범위 모양(action·plan·controller·tag·version·commit·asset{name,sha256,size}·
   targets[{order,machine_id,hash}])으로 받아들일지, 소비 1회·만료 시간. 정해지면 조종 쪽 소비·순서 실행을 붙인다.
2. 기기 설치 스위치를 켤 기기와 시점 — 켜는 것은 그 기기 앱의 실행 환경(`KASATERM_APP_UPDATE=on`)이다.
3. CI mac 판 서명 — 지금 CI 판은 자체 서명·미공증이라 이 창구가 **팀·공증 검사에서 거절한다**(의도한 것). Developer ID
   서명·공증을 CI 에 넣을지는 [fast-patch-release.md](fast-patch-release.md) 「서명·공증」.

## 검사

```sh
cargo test -p kasa-socket app_update      # 20건 — 가짜 받기·가짜 dmg·실제 sh 도우미·가짜 앱 프로세스
python3 -m unittest tools.release.tests.test_fastpatch   # 작업 모양·id 대조 포함
```

도우미 검사는 실제 `/bin/sh` 로 돌고, 앱은 `exec -a <설치본 실행 파일> /bin/sleep` 으로 흉내 낸다(명령줄이 설치본 경로로
시작하는 프로세스). 다루는 것: 공식 주소 외 거절, 피드 불일치, 켜짐·OS·재시작 거부·정체·다운그레이드·승인(미소비·만료·
다른 동작·범위), 한 작업 한 번·앞으로만, 둘째 작업 거절과 다시 보내기, 받기 끊김 뒤 이어 하기, 중단된 받기 재사용과 바꿔치기
파일, 확인 실패 여덟 가지와 EdDSA 불일치(흔적 없음·dmg 닫힘·설치본 그대로), 바쁜 학생 기다림 → 이어서 갈아 끼우기 → 부팅
표식으로 끝, 앱이 안 꺼짐(강제 안 함), 준비 번들 없음·둘째 인스턴스, 다시 띄우기 실패 되돌림, 부팅 전 죽은 새 판 되돌림, 틀린
빌드 표식, 30분 넘은 작업.

격리 리그(실제 앱 바이너리, `docs/verify-app.md` 격리 env, 2026-09-27): 스위치 꺼짐 → `409 update_disabled`, 켜짐인데 개발
실행 → `409 설치본으로 도는 앱이 아니다`(나쵸에 묻기 전에 멈춤), 다른 주소 → `409 공식 릴리스 주소가 아니다`, 16 KiB 넘는
본문 → `413`, 없는 작업 → `404`, 소켓 `kasaterm-cli app-update status` 동작, 거절 때 작업 기록 안 생김. 사실에
`update_capability: 1`·`update_enabled` 가 실린다.

Windows 는 이 창구를 쓰지 않는다(`#[cfg(unix)]`) — 윈도 타깃 컴파일은 이 맥에서 해 보지 않았다.
