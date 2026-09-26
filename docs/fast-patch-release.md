# 빠른 패치 릴리스 — 여러 기기에 작은 수정을 올리는 길

카사텀을 쓰는 기기가 늘면서 작은 수정도 「버전 올리고 → 태그 → CI → 각 기기 반영」을 매번 손으로 했다.
이 문서는 그 흐름을 **계획 하나 + 승인 한 번**으로 묶는 도구(`tools/release/fastpatch.py`)와, 기기에서 새 판을
받는 두 입구를 적는다. 정본 파이프라인은 그대로다 — 새 배포 시스템을 따로 만들지 않는다.

## 한 장 요약

```
plan ─► 나쵸가 approval_scope 로 승인 요청 ─► 주인 확인 단추(10분·한 번) ─► run --live --approval ap_…
         verify → build(실제, 격리) ─► [나쵸 consume 1회] ─► tag → release → feed → devices
                                                             └ 끝난 단계는 건너뜀 · 원격을 다시 읽어 맞춤 ┘
status: 원격 태그·CI·피드 + 기기마다 지금 판(버전 · SHA) → 목표 · 마지막 확인 · 오프라인 · 보류 까닭
```

| 입구 | 누가 | 어떻게 | 이미 있나 |
|---|---|---|---|
| (A) 나쵸가 올린다 | 나쵸 | `plan` → 주인 확인 1회 → `run --live` → `status` | 카사텀 쪽 코드는 **이번에 새로**. 실제 게시는 아래 「지금 막힌 것」 둘이 풀려야 돈다 |
| (B) 기기에서 받는다 | 그 기기의 사람 | 계정 메뉴 바닥 **판 번호 줄** / 맥 앱 메뉴 「업데이트 확인…」 / 윈도 시작 뒤 토스트 [설치] | 맥 메뉴·윈도 토스트는 **원래 있음**, 판 번호 줄은 **이번에 새로** |

두 입구가 같은 것을 먹는다 — CI 가 올린 릴리스 파일(dmg·msi)과 그것을 가리키는 appcast
(`docs/appcast.xml`·`docs/appcast-win.xml`, EdDSA 서명). 기기에 소스 클론·빌드 환경이 없어도 된다.

## 지금 막힌 것 (2026-09-27 실제 `plan` 읽기)

1. **mac 서명 관문** — CI mac 판은 `kasaterm-ci` 자체 서명·미공증, 설치본은 Developer ID(L366799VND)다. 태그를 올리면
   CI 가 mac appcast 까지 게시하므로 **태그 단계부터** 막는다(`live_blocks`). 풀려면 CI 가 설치본과 같은 Developer ID 로
   서명·공증해야 한다 — 새 키·신뢰 권한이라 사람 결정이고 이 도구 범위 밖이다. 보안 설정을 끄는 우회는 없다.
2. **나쵸 릴리스 승인 창구** — 나쵸 쪽 구현은 끝났다(나쵸 94a3033, 대조 자료
   `docs/development/api/fixtures/approval.kasaterm_release.implemented.json`). 창구는 `NACHO_APPROVAL_HTTP_ACTIONS` 에
   `kasaterm_release` 가 있어야 열리고 기본은 닫혀 있다 — 여는 것은 주인 확인 카드로 따로 한다.
3. **원격 설치 창구** — 기기 앱 창구는 붙었다([app-update.md](app-update.md)): 공식 릴리스만 받아 sha256·EdDSA·팀·
   공증을 보고 곁에 둔 뒤, 바쁜 학생이 없을 때 스스로 꺼서 갈아 끼우고 새 판이 안 뜨면 되돌린다. 다만 설치 실행은 기기마다
   기본 꺼짐이고(`KASATERM_APP_UPDATE`), 나쵸 `kasaterm_update` 승인 창구도 기본 닫혀 있다. 키 없는 맥북은 명부 파일의
   `update_approvals` 위임이 있어야 승인을 읽는다(지금 없음). 창구가 든 판을 기기마다 한 번은 사람이 설치해야 한다.

실행 주체는 나쵸 도구다(주인 결정). 학생·에이전트 창에서 live 를 치면 도구가 거절한다(아래 「누가 치나」).

그 전에도 되는 것: `plan`·`dry-run`·`status`(읽기), `run`(검사·굽기를 격리 워크트리에서 실제로 하고 게시는 명령만),
`device-plan`(읽기 — 기기마다 보낼 업데이트 작업과 승인 범위를 지어 보인다).

## (A) 나쵸 입구 — 쓰는 명령

```sh
# 1) 계획: 커밋·다음 패치 버전·포함 변경·플랫폼·기기 범위·기준 피드 해시·서명 관문을 고정. 아무것도 안 바꾼다.
python3 -m tools.release.fastpatch plan [--json]
#    --devices devices.json   [{label, base}] — 없으면 이 기기와 명부(~/.config/kasaterm/machines.json)
#    --version 0.2.5          직접 고를 때(원격 태그·피드보다 높아야 한다)
#    --json                   나쵸 도구가 approval_scope 를 그대로 승인 요청으로 만든다(모델이 JSON 을 짓지 않는다)

# 2) 미리보기: 모든 단계의 명령과 원격 사실(태그·main·CI·피드). 읽기만, 상태 파일도 안 쓴다.
python3 -m tools.release.fastpatch dry-run <plan_id>

# 3) 로컬 실행: 검사·굽기를 격리 워크트리에서 실제로(자동설치 dist 안 덮음), 게시 단계는 명령만.
python3 -m tools.release.fastpatch run <plan_id>

# 4) 게시: 주인이 승인한 나쵸 승인 id 로. 게시 첫 단계 직전에 한 번 소비한다.
python3 -m tools.release.fastpatch run <plan_id> --live --approval ap_…

# 5) 추적: 원격 사실 + 기기별 지금 판·목표·마지막 확인. 언제든(읽기만, 승인 불필요).
python3 -m tools.release.fastpatch status <plan_id> [--json]

# 6) 기기 업데이트 계획(읽기): 앱 재시작 사실로 기기마다 막힘·여섯 단계, --json 은 보낼 작업·나쵸 승인 범위까지.
python3 -m tools.release.fastpatch device-plan <plan_id> [--json]
# 7) 기기 업데이트 러너(나쵸 도구만): rollout 하나를 한 번 승인으로 차례로 — 조종 기기 마지막, 앞 기기가 새 판이어야 다음.
python3 -m tools.release.fastpatch device-apply <plan_id> --rollout <rollout_id> --approval ap_… [--live]
```

상태 폴더는 `~/.config/kasaterm/releases/`(`KASATERM_RELEASE_DIR`·`--state-dir`).

### 도구는 PATH 에 맡기지 않는다

`tools/release/deps.py` 가 도구를 재서 고르고 계획의 `tools` 에 **절대경로**로 못 박는다. 나쵸 기본 셸은 `/usr/bin` 이
PATH 앞이라, 사람 셸에서 멀쩡하던 같은 명령이 다른 도구를 잡았다(2026-09-27 나쵸 직접 검증: 검사 31건 중 27건 오류).

| 도구 | 고르는 법 | 없으면 |
|---|---|---|
| openssl | `KASATERM_OPENSSL` 이 있으면 **그것만**(몰래 바꾸지 않는다), 없으면 PATH → openssl@3 알려진 자리. RFC 8032 시험 벡터로 맞는 서명 통과·틀린 서명 거부를 둘 다 확인한 것만. `/usr/bin/openssl`(LibreSSL 3.3.6)은 Ed25519 원문 검증이 안 돼 빠진다 | live 막힘 · 피드 단계 거절 |
| git-lfs | PATH → `~/.local/bin` · homebrew. git 을 부를 때 그 폴더를 PATH 앞에 붙인다(git 이 LFS 필터를 이름으로 부른다 — 없으면 격리 워크트리가 「git-lfs: command not found」로 깨졌다) | LFS 저장소면 계획 오류 |
| gh | PATH → `~/.local/bin` · homebrew, `gh auth status` 까지 | live 막힘(로그인은 사람이) |
| cargo | PATH → `~/.cargo/bin` · homebrew. 검사·굽기 env 에 cargo·gh·git-lfs 폴더를 앞에 붙인다(build-app.sh 가 이름으로 부른다) | 검사·굽기 단계가 까닭을 말하고 멈춤 |
| codesign · spctl · hdiutil | `/usr/bin`·`/usr/sbin` 고정 경로 | live 막힘 |

게시 도중에도 피드 단계는 고정한 openssl 을 시험 벡터로 다시 잰다 — 그 사이 바뀌었으면 서명을 확인하지 않고 멈춘다.
자동 설치는 하지 않는다.

명령마다 종류가 있다(`tools/release/proc.py`): **read**(원격·서명 읽기, 늘 돈다) · **local**(격리 워크트리·검사·굽기·
다운로드, `dry-run` 에선 적기만) · **publish**(태그 push, `--live` 에서만). 모든 명령은 시간 제한이 있고, 돈 것과 건너뛴
것이 다 기록된다.

### 계획이 고정하는 것

- **정확한 커밋** — 워킹트리가 깨끗하고 `origin/main` 에 들어간 커밋만. 계획 뒤 main 이 움직이면 태그 단계가
  멈추고 새 계획을 요구한다.
- **다음 버전** — `max(원격 태그, 두 피드 버전, Cargo.toml) + 패치 1`. 로컬 태그는 믿지 않는다(`git ls-remote --tags`).
- **기준 피드 해시(feed_base)** — 계획 때의 두 appcast 해시. 승인 뒤 소비 직전에 다시 재서 다르면(다른 게시가 끼었다)
  새 계획을 요구한다.
- **포함 변경** — 네이티브(앱 업데이트 필요) · 모바일(TestFlight 판, 이 단계들 밖) · 문서·인프라(기기에 안 감).
- **서명 관문** — CI 가 쓸 mac 서명 신원(release.yml 에서 예상)과 이 기기 설치본의 실제 신원(`codesign -dvv`·`spctl`).
  같은 팀의 Developer ID·공증이 아니면 live 게시를 막는다.
- **기기 범위** — `/version` 의 버전 **과 SHA**, machine_id 를 댄 기기만. 보류·더 새 판·id 없는 기기는 빠진다.
- **계획 id** — 위 범위의 해시. 승인 scope 는 이 id 와 커밋·버전·태그·채널·피드 해시·플랫폼·기기·조종 기기를 싣는다.

### 승인 — 나쵸 장부에서만, 이 계획에 한 번

앱 재시작 승인([app-restart.md](app-restart.md))과 같은 틀이고, 나쵸 쪽 계약은 아리스와 맞췄다(2026-09-27):

- scope 는 키 10개 그대로: `action`(kasaterm_release) · `plan` · `controller` · `tag`(=`v`+version) · `commit`(소문자 40자)
  · `version` · `channel`(stable) · `feed_base` · `platforms`(정렬·중복 없음·비지 않음) · `devices`(정렬, 0~16, 명부 기기).
  모르는 칸은 나쵸가 거절한다. 카사텀도 같은 검사를 먼저 한다(`nacho.scope_problem`).
- 결정은 주인 확인 단추로만(ALWAYS_HIGH), 10분 만료, 한 번. 단추 글: 태그·커밋 앞 12자·채널·플랫폼·기기 수·feed_base
  앞 12자·「태그·피드 게시는 되돌리기 어렵다」. 같은 범위가 살아 있으면 다시 묻지 않는다.
- 소비: `run --live` 가 게시 첫 단계 직전에 `GET /api/app/approvals/{id}` 로 읽고(동작·승인됨·안 씀·안 지남·해시 같음)
  `POST …/consume {scope, consumer_machine_id}` — 서버가 해시를 다시 재고, 소비 기기가 scope 의 조종 기기이면서 나쵸
  기기일 때만 한 번 성공한다. HTTP 창구는 동작마다 켠다(`NACHO_APPROVAL_HTTP_ACTIONS`, 기본 kasaterm_restart 만).
- 재개: 다시 소비하지 않는다. 작업 기록의 승인 id·계획·해시·7일을 먼저 보고, `POST …/{id}/resume {scope,
  consumer_machine_id, stage, remote:{main, tag_parent}}` 로 나쵸 기록과 대조한다 — 소비 안 된 승인(409 not_consumed)·
  안 쓴 채 지난 승인(410 expired, 절대 안 살아남)·소비 7일 초과(410 resume_expired)·다른 기기·다른 해시·태그 부모가
  계획 커밋이 아님(409 remote_mismatch)이면 멈춘다. 재개마다 나쵸가 기록을 남긴다.
- **승인 10분이 굽기에 먹히지 않게**: live 는 verify·build 가 done 이 아니면 「먼저 run」 으로 거절한다. 나쵸 도구는
  `status --json` 의 `live_ready` 가 참일 때만 단추를 띄우고, 누르면 `run --live` 가 끝난 단계를 건너뛰어 몇 초 안에 소비한다.
- 계획 파일을 손대면(core 해시가 plan_id 와 다르면) live 를 거절한다.
- **로컬 `approval.json` 은 승인이 아니다** — 그런 파일이 있어도 도구는 보지 않는다.

### 누가 치나

`run --live` 는 나쵸 도구만 친다. 창 표식(`KASATERM_PANE_ID`·`CLAUDECODE`·`CLAUDE_CODE_SESSION_ID`·`KASATERM_ORIGIN`·
`CODEX_SANDBOX`)이 있으면 거절하고, `KASATERM_RELEASE_INVOKER=nacho-tool` 이 없어도 거절한다. 나쵸 도구는 창 표식을 걷은
env 로 띄운다. **사고 방지 표식이지 보안 경계가 아니다** — 경계는 주인 승인(정확한 범위·10분·한 번)이다.

### 나쵸와의 계약 (고정 칸)

- core 키: `schema`(kasa-release-plan/2) · `commit` · `branch` · `remote` · `version` · `tag` · `channel` · `platforms` ·
  `ios_build` · `devices` · `device_ids` · `controller` · `feed_base` · `stages`. `plan_id = sha256(canonical(core))[:16]`
  (canonical = 키 정렬·`ensure_ascii=False`·구분자 `,` `:`). 나쵸가 다시 잰다 — 키를 바꾸면 SCHEMA 를 올린다.
- 고정 칸: `base{tag, commit}` · `changes.commits[{sha, subject}]` · `changes.files{native, mobile, feed, docs, infra}` ·
  `errors[]` · `live_blocks[]` · `approval_scope` · `approval_scope_hash`. `tools` 는 core 밖이다.
- `status --json`: `plan_id` · `plan_hash_ok` · `stages.<단계>.status` · `approval{id, consumed_at_ms}|null` · `remote{tag, main,
  ci, feed_macos, feed_windows}` · `live_ready` · `live_blocks` (+ `tag`·`commit`·`errors`·`devices`).
- 검사 `NachoFixtureTests` 가 나쵸 대조 자료에 이 코드를 그대로 대고(나쵸 저장소가 없으면 건너뜀),
  `scripts/nacho-release-interop.sh` 는 나쵸 레포의 실제 승인 서버를 임시 폴더·가짜 키로 띄워 이 클라이언트로 왕복한다 —
  창구 on(소비 1회·재소비 거절·resume·remote_mismatch·bad_remote·bad_stage·not_consumed·not_approved·scope_changed·
  wrong_consumer·bad_token)과 창구 off(capabilities 에 없음·no_approval). 운영 키·저장소는 안 건드린다.

### 단계와 재실행

| 단계 | 하는 일 | 다시 돌리면·실패하면 |
|---|---|---|
| verify | 계획 커밋의 격리 워크트리에서 cargo 검사 3종(격리 `CARGO_TARGET_DIR`) | 끝났으면 건너뜀. 실패·시간 초과는 그 명령과 끝 줄을 남기고 멈춤 |
| build | 같은 커밋 ready 판이 있으면 바이너리 해시·서명을 다시 재 갈음, 없으면 격리 워크트리에서 `build-app.sh` | 〃 |
| tag | 격리 워크트리에서 tag-release.sh 와 같은 버전 치환·같은 커밋 메시지 → `git push --atomic origin HEAD:main HEAD:refs/tags/vX`. 로컬 태그는 안 만든다 | 원격에 태그가 있으면 계획 커밋 위의 버전 커밋인지 보고 **다시 세우지 않는다**(아니면 손대지 않고 멈춤). push 가 실패·시간 초과여도 원격을 다시 읽어 반영됐으면 완료, 그대로면 재시도 가능, 반쪽이면 멈춤 |
| release | `gh run list` 로 그 태그의 release.yml 실행 확인 → `gh release view`·`download` 로 dmg·msi 크기·해시 → dmg 를 읽기 전용으로 열어 서명 신원·공증 | CI 가 안 떴거나 도는 중이면 **기다림**(실패 아님). CI 실패·산출물 모자람·해시 불일치·신원 불일치면 멈추고 피드 확인으로 안 넘어감 |
| feed | 두 appcast 가 목표 판·산출물 이름·크기를 가리키는지, EdDSA 서명이 받은 파일과 맞는지(저장소의 Sparkle 공개키로 `openssl` 확인) | 아직 옛 판이면 기다림, 더 새 판이면 되돌리지 않고 멈춤, 확인한 피드가 그 뒤 바뀌면 멈춤 |
| devices | 기기별 `/version` 추적 — 설치·재시작은 하지 않는다 | 매번 다시 잰다(승인 불필요) |

`tag-release.sh` 를 그대로 부르지 않는 까닭: 그 스크립트는 로컬 태그만 보고, 지금 체크아웃의 `main` 가지를 push 한다.
공유 워킹트리에서는 그 `main` 이 계획 커밋이라는 보장이 없다.

### 기기 비교 — 「0.2.0」만으로는 못 가른다

로컬 굽기는 판 번호를 안 올리므로 모든 기기가 0.2.0 이라고 말한다. 그래서 `/version` 의 `build`(git SHA, 끝에 `+` 면
미커밋 포함)까지 본다.

| 상태 | 뜻 |
|---|---|
| current | 목표 버전이고 SHA 가 계획 커밋이나 그 태그의 버전 커밋 |
| update | 목표보다 낮거나 같은 번호의 다른 판 — 기기 SHA 가 계획 커밋의 조상 |
| hold | 기기 판에 권위 main 에 없는 커밋이 있다 — 올리면 그 커밋이 빠진다. **범위에서 뺀다** |
| newer | 기기가 더 새 판 — 다운그레이드 안 함 |
| unscoped | machine_id 를 안 알려 승인 범위에 못 넣는다 |
| offline | 안 닿는다 — 마지막으로 본 판과 그 시각을 남긴다 |

원격 설치는 기기 창구가 켜지고 승인 동작이 생긴 뒤의 일이라, 대상 기기마다 「받는 길」(그 기기의 판 번호 줄·업데이터,
또는 릴리스 페이지)을 함께 싣는다.

2026-09-27 실제 `plan`(원격 main 5e4d1386 기준): 미니 `0.2.0 · 8933a0ca+` → update, 맥북 `0.2.0 · 84190e33` → **hold**,
windesktop `0.2.0 · 5e69f61f+` → update. 다음 판 v0.2.1, 커밋 843개, mac 서명 관문으로 live 막힘.

### 맥북 커밋 — 권위 main 에 먼저 합친다

맥북 판(84190e33)의 커밋은 origin 에 없다. 그 기기를 릴리스 판으로 올리면 그 커밋이 빠지므로 도구가 보류한다.
통합은 릴리스와 따로 한다: 맥북에서 그 커밋을 브랜치로 push → main 에 합침 → 새 계획. 공유 워킹트리에서
자동 rebase·stash 는 하지 않는다. (2026-09-25 확인: 맥북 로컬 커밋은 원격 8933a0ca 와 같은 패치였다 — 같은
패치면 합칠 때 빈 커밋이 되고, 그 뒤 계획은 update 로 바뀐다.)

## 기기 업데이트 — 작업 짓기 (창구는 [app-update.md](app-update.md))

`device-plan` 은 앱 재시작 계획(`kasaterm-cli app-restart plan --json`, [app-restart.md](app-restart.md))에서 기기마다의
사실(OS·설치본·업데이트 창구 판·설치 스위치·바쁜 학생·미저장 편집기·자기설치 대기·굽는 중·진행 중 작업)을 읽어, 기기가 할
여섯 단계와 지금 막힌 까닭을 보인다.

| 단계 | mac(기기 창구) | 윈도 |
|---|---|---|
| 1 받기 | 공식 피드·공식 릴리스의 그 태그 dmg 하나만(https·크기 상한) | msi → `%LOCALAPPDATA%\kasaterm\updates\<tag>\` |
| 2 확인 | 크기·sha256(릴리스 단계 값) · EdDSA · dmg 읽기 전용 · `codesign --verify --deep --strict` · 설치본과 같은 팀 · 공증 · 판 번호 | 크기·sha256 · EdDSA |
| 3 준비 | 확인한 번들을 `.kasaterm.app.next` 에(설치본 안 건드림) | 확인한 msi 를 둔다 |
| 4 적용 | 바쁜 학생·미저장 편집기가 없을 때 앱이 스스로 끄고 도우미가 갈아 끼움(이전 판 `.kasaterm.app.previous`) | WinSparkle 토스트 [설치] |
| 5 재기동 | 도우미가 다시 띄움 — 새 판이 부팅 표식 없이 꺼지면 이전 판 되돌림(강제 종료 없음) | MSI 가 닫고 연다(사람이 고름) |
| 6 검증 | 부팅 표식의 빌드가 태그 커밋 — 아니면 실패 | `/version` |

상태: `ready`(창구 켜짐·막힘 없음) · `deferred`(받기·준비는 되고 적용만 기다림 — 바쁜 학생·미저장 편집기 등) · `blocked`.
막힘 사유: `update_endpoint_missing`(창구가 든 판이 아직 안 깔림) · `update_disabled`(그 기기 설치 스위치 꺼짐) · `signing` ·
`artifacts_unverified` · 재시작 쪽 사유. `--json` 은 기기마다 `job`(기기의 `check_job` 이 받는 모양)·`job_problem`·
`scope`(나쵸 `kasaterm_update` 승인 범위)를 싣는다 — 작업은 릴리스 단계의 mac 산출물과 피드 단계가 확인한 EdDSA 서명이
있어야 지어진다. 보내는 길(`kasaterm-cli app-update start`)은 있지만 승인 동작이 나쵸에 없어 운영에서는 통과하지 못한다.

2026-09-27 실제 결과(나쵸 기본 PATH, 창구 전 판): 이 기기 → 창구 없음·서명 관문·바쁜 학생·산출물 미확인으로 blocked,
windesktop → 앱 재시작 사실이 안 닿아 blocked. 기기마다 「지금 받는 길」(판 번호 줄·릴리스 파일 주소)을 함께 싣는다.

## (B) 기기 입구 — 이미 있는 것과 새로 붙인 것

| 플랫폼 | 원래 있던 것 | 이번에 붙인 것 |
|---|---|---|
| 맥 (.app) | 앱 메뉴 「업데이트 확인…」 → Sparkle 표준 창(확인 → 받기 → EdDSA 확인 → 「설치 후 재실행」). 하루 한 번 자동 확인. 계정 메뉴 바닥에 판 번호와 「v… 나왔음 / 최신 / 새 판 준비됨 · 껐다 켜기」 | 판 번호 줄을 누르면 같은 Sparkle 창 |
| 윈도 (MSI) | 시작 6초 뒤 피드 확인 → 토스트 [설치] → WinSparkle(받기 → EdDSA 확인 → MSI) | 판 번호 줄 → 같은 WinSparkle 설치 흐름. **손으로 확인하는 길이 처음 생김** |
| 업데이터 없는 판 (개발 실행, MSI 아닌 윈도) | 없음 | 판 번호 줄 → 까닭을 토스트로 알리고 릴리스 페이지(서명된 파일)를 연다 |
| 폰 (iOS) | TestFlight 앱에서 사람이 업데이트 | 없음 — 아래 |

- 확인·받기와 설치·재실행은 갈려 있다. 설치·재실행은 업데이터 창에서 **사람이 고른다.** 카사텀이 스스로 끄거나
  다시 띄우지 않는다 — 도는 학생·미저장 입력이 있는 창을 강제로 닫는 길은 없다.
- 로컬 굽기(자기설치)는 따로다: 종료할 때 `dist/kasaterm.app` 이 더 새것이면 설치본을 바꾼다. 이번에 **이전 판을
  `~/Applications/.kasaterm.app.previous` 로 남기고, 복사가 실패하면 되돌리게** 했다(전에는 지우고 복사라 실패하면
  앱이 사라졌다). 이 보호는 새 판이 한 번 설치된 뒤부터 먹는다 — 끌 때 도는 판의 스크립트가 쓰이기 때문이다.

## 채널 — 지어내지 않는다

- 맥 Sparkle: 앱이 `SPUStandardUpdaterController` 를 대리자 없이 만든다 → `allowedChannels` 가 없어 **stable 하나**.
  preview 채널이 필요하면 대리자(`allowedChannelsForUpdater:`)와 appcast 의 `<sparkle:channel>` 을 함께 넣어야 하고,
  그 전까지 `plan --channel preview` 는 막힌다.
- 윈도 WinSparkle: 채널 개념이 없다(피드 하나).
- iOS: TestFlight 내부 · TestFlight 외부(베타 심사) · App Store(심사). 앱 바이너리를 심사 없이 바꾸는 길은 없고
  만들지 않는다. 폰에 즉시 반영되는 것은 서버·허브 쪽 데이터와 설정뿐이다.

## 네이티브 변경과 동적 반영

| 바뀐 것 | 기기에 닿는 길 |
|---|---|
| Rust 앱·크레이트(`app/`·`crates/`) | 새 판 설치 필요(Sparkle·WinSparkle·자기설치) |
| 폰 앱(`mobile/`) | TestFlight 새 빌드 |
| 나쵸 쪽 모드·권한 문구, 장부 내용 | 앱 업데이트 없이 다음 읽기에 반영(앱은 창구에서 읽기만) |
| appcast·문서·스크립트 | 기기에 안 감 |

학생 지침(`collab-hooks`)·에셋은 번들 안에 있어 네이티브와 같이 새 판이 필요하다.

## 서명·공증

- 로컬 굽기 판은 Developer ID(L366799VND), CI 릴리스는 **kasaterm-ci 자체 서명**이고 **공증하지 않는다**.
- Sparkle 문서(「Rotating signing keys」)는 Developer ID 로 서명하고 EdDSA 공개키를 넣은 앱이면, EdDSA 가 맞는 한 업데이트가
  Apple 코드 서명 인증서를 바꿔도 받는다고 한다. 하지만 kasaterm-ci 는 Apple 인증서가 아니라 자체 서명이고 공증도 없다.
  그래서 받아 주는지와 관계없이 **자동 배포하지 않는다** — 도구가 태그 단계 전에 막는다(위 「지금 막힌 것」 1).
- 새 키 발급·로그인·권한 추가는 이 흐름에 없다. appcast 서명 키는 CI 비밀(`SPARKLE_ED_PRIVATE_KEY`) 그대로다.

## 처음 한 번만 필요한 것

- 판 번호 줄 입구·자기설치 백업은 새 판이 한 번 설치돼야 생긴다(그 전 판에는 없는 코드다).
- 윈도는 MSI 로 한 번 설치돼 있어야 업데이터가 있다(windesktop 은 2026-09-27 에 `5e69f61f+` 로 닿았다 — 로컬 굽기 판이면
  WinSparkle 이 없을 수 있다).
- 원격 설치 창구([app-update.md](app-update.md))는 창구가 든 판이 기기마다 한 번 사람 손으로 설치돼야 생긴다. 그 뒤에도
  설치 실행은 기기마다 `KASATERM_APP_UPDATE=on` 으로 켜야 하고, 나쵸 `kasaterm_update` 승인 동작이 있어야 작업이 통과한다.

## 검사

```sh
python3 -m unittest tools.release.tests.test_fastpatch      # 55건 — 아래
cargo test -p kasa-socket app_update                         # 기기 업데이트 창구·러너 32건(app-update.md 「검사」)
bash scripts/nacho-update-interop.sh                         # 실제 나쵸 update 승인 서버(격리)와 러너·기기 왕복
bash scripts/nacho-release-interop.sh                        # 실제 나쵸 승인 서버(격리)와 왕복 17건
cargo test -p kasaterm --release version::tests             # 판 번호 줄 입구 선택
cargo test -p kasaterm --release self_install               # 자기설치 백업·되돌리기(실제 sh)
```

fastpatch 검사는 임시 저장소(원격은 bare 저장소)·임시 피드·가짜 기기(`/version`)·가짜 나쵸(승인 계약 그대로)로 돈다.
`git` 과 `openssl` 은 진짜로 돌려 push 명령 구성·원격 대조와 Ed25519 서명 확인을 실제 도구로 보고, `gh`·`codesign`·
`hdiutil`·`cargo` 만 가짜다. 다루는 것: 다음 버전·원격 태그·피드 바닥, 중복 태그·다운그레이드·더러운 트리·미push, 채널,
기기 버전+SHA 비교, 자체 서명 CI 의 live 차단·설치본 신원 미확인, dry-run 무기록, 로컬 실행(검사·굽기·ready 판 재검증,
로컬 태그 안 만듦), 검사 실패·시간 초과, 나쵸 승인 거절 전부(대기·만료·범위 변경·다른 동작·창구 닫힘·로컬 파일),
계획 뒤 피드 변경, 한 번 소비와 재개(다른 승인·7일·원격 불일치·미소비), push 시간 초과·거절·main 이동·남의 태그·잃은
상태 파일, CI 대기·실패·산출물 모자람·해시·dmg 신원, 피드 EdDSA·더 새 판, 도구 고르기(LibreSSL·거짓 통과·지정 경로 고수·
고정 openssl 변질·git-lfs 없음·PATH 폴더), 계약 키·위조 계획·live 준비 조건·창 안 실행 거부, 기기 업데이트 계획·작업 모양·작업 id 대조, 나쵸 대조 자료.
나쵸 기본 PATH(`/usr/bin` 먼저)로도 돌려 확인한다. `KASATERM_OPENSSL=/usr/bin/openssl` 처럼 쓸 수 없는 openssl 을 가리키면
서명이 필요한 검사는 까닭을 밝히고 건너뛴다(오류가 아니다).

Windows 쪽 러스트(`win_sparkle::available`)는 이 맥에 윈도 타깃이 없어 컴파일해 보지 않았다.
