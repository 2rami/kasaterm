# 카사텀 앱 재시작 — 등록된 어느 기기에서든, 한 대씩

대상은 **카사텀 앱 하나**다. OS 재부팅·나쵸 봇·request-journal·펫 재시작이 아니다. 대상 기기는 명부의
안정 `machine_id` 로만 고른다(호스트·명령을 받지 않는다). 핵심 코드는 `crates/kasa-socket/src/app_restart.rs`,
앱이 사실을 재는 곳은 `app/kasaterm/src/app_restart.rs`.

## 지금 되는 것과 안 되는 것

| 단계 | 상태 |
|---|---|
| 계획·거부 사유 보기(`plan`) | 된다 — 새 판이 깔린 앱끼리 |
| 작업 상태 보기(`status`) | 된다 |
| 실행(`run`, `POST /app/restart/jobs`) | **꺼져 있다** — 사람 승인 흐름이 없어 `approval_flow_unavailable` 로 거부(종료 코드 3 / HTTP 403) |
| 앱 안 실행 창구(검사 통과 → 도우미 → `exiting()`) | 아직 없다(승인 흐름과 함께 넣는다) |
| 윈도우 기기 | 미지원 OS 로 거부 |

**첫 배포는 사람이 한다.** 이 창구는 새 판이 깔린 앱에만 있다 — 두 기기에 굽고 설치한 뒤 처음 한 번은 사람이
직접 앱을 껐다 켜야 한다(옛 판 앱은 `capability_missing` 이나 닿지 못함으로 거부된다).

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

## 실행 절차(승인 흐름이 생기면 도는 모양)

1. 계획(`plan`) — 대상마다 정체 해시(`machine_id`·앱 경로·pid·실행 바이너리 inode/mtime·빌드·자기설치 예정·창구 판).
2. 사람 승인 — 계획 해시·기기 목록·만료(10분)·한 번. 승인한 계획과 지금 계획이 다르면 거부.
3. 원격 기기부터 한 대씩, **조종 기기는 마지막**(조종 앱이 꺼지면 원격 터널도 내려간다).
4. 대상마다: 사실을 다시 재고 → 해시가 달라졌으면 멈춤 → 작업 id(계획 해시+기기 id, 같은 요청은 같은 id) 로 작업을 건다.
5. 대상 앱이 도우미를 띄우고 스스로 `exiting()` 으로 끈다(세션 저장·창 크기·자기설치 판정·터널 정리가 그대로 돈다).
6. 도우미: 옛 pid 가 사라질 때까지 기다림(상한, **강제 종료 없음**) → 설치본을 도는 다른 앱이 있으면 멈춤 →
   `/usr/bin/open -a <설치본>` → 새 pid 기록. 새 앱이 부팅 때 「도착」을 적는다.
7. 조종 쪽이 다시 닿아 새 pid·같은 바이너리를 확인하면 `verified`. 못 닿거나 바이너리가 다르면(업그레이드가 됨) 실패.
8. 앞 기기가 실패하면 뒤 기기(조종 기기 포함)는 `skipped`.

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

거노가 계획 하나(해시·기기 목록)를 승인하면 그 범위 안에서는 기기마다 다시 묻지 않고 한 대씩 진행한다.
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

## 확인 방법(나쵸)

```sh
kasaterm-cli app-restart plan --machine <미니 id>,<맥북 id> --json | jq '.targets[] | {machine_id, controller, refusals}'
curl -s 127.0.0.1:8765/app/restart/facts | jq '{machine_id, pid, busy, install_pending, restorable_sessions}'
kasaterm-cli app-restart status <job_id> --machine <id>     # 실행이 켜진 뒤
```

검사: `cargo test -p kasa-socket --lib app_restart`(계획·거부·해시·승인·작업 기록·가짜 앱으로 도우미의 대기·재기동·
강제 종료 없음·둘째 인스턴스 거부·부모가 죽어도 사는 도우미·env 허용목록·순서·실패 중단·해시 변경·오프라인).
