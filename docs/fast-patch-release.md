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
2. **나쵸 릴리스 승인 창구** — 나쵸 HTTP 승인은 지금 `kasaterm_restart` 만 열려 있다. `kasaterm_release` 는 아리스와 계약을
   맞췄고(아래) 나쵸 쪽 구현·켜기(주인 확인 카드)가 남았다. 그 전엔 `capabilities.approvals.http_actions` 에 없으므로
   카사텀이 `no_approval` 로 거절한다.
3. **정할 것** — 주인이 단추를 누른 뒤 `run --live` 를 나쵸 도구가 직접 칠지, 사람·학생이 칠지.

그 전에도 되는 것: `plan`·`dry-run`·`status`(읽기), `run`(검사·굽기를 격리 워크트리에서 실제로 하고 게시는 명령만).

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
python3 -m tools.release.fastpatch status <plan_id>
```

상태 폴더는 `~/.config/kasaterm/releases/`(`KASATERM_RELEASE_DIR`·`--state-dir`).

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
- 재개: 다시 소비하지 않는다. 나쵸 기록이 `consumed_by == 이 기기`·같은 해시이고, 카사텀 작업 기록의 승인 id·계획
  해시와도 맞을 때만 잇는다. 나쵸는 승인 기록을 7일 뒤 지우므로 그보다 오래된 재개는 새 승인이다.
- **로컬 `approval.json` 은 승인이 아니다** — 그런 파일이 있어도 도구는 보지 않는다.

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

원격 설치 창구는 아직 없어서, 대상 기기마다 「받는 길」(그 기기의 판 번호 줄·업데이터, 또는 릴리스 페이지)을 함께 싣는다.

2026-09-27 실제 `plan`(원격 main 5e4d1386 기준): 미니 `0.2.0 · 8933a0ca+` → update, 맥북 `0.2.0 · 84190e33` → **hold**,
windesktop `0.2.0 · 5e69f61f+` → update. 다음 판 v0.2.1, 커밋 843개, mac 서명 관문으로 live 막힘.

### 맥북 커밋 — 권위 main 에 먼저 합친다

맥북 판(84190e33)의 커밋은 origin 에 없다. 그 기기를 릴리스 판으로 올리면 그 커밋이 빠지므로 도구가 보류한다.
통합은 릴리스와 따로 한다: 맥북에서 그 커밋을 브랜치로 push → main 에 합침 → 새 계획. 공유 워킹트리에서
자동 rebase·stash 는 하지 않는다. (2026-09-25 확인: 맥북 로컬 커밋은 원격 8933a0ca 와 같은 패치였다 — 같은
패치면 합칠 때 빈 커밋이 되고, 그 뒤 계획은 update 로 바뀐다.)

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
- 원격 설치 창구(나쵸가 기기에 받기·설치 예약을 거는 것)는 없다. 붙이려면 기기 앱에 새 창구가 들어가야 하고, 그 판이
  한 번 사람 손으로 설치돼야 한다 — 설치·재시작은 앱 재시작과 같은 사실 검사(바쁜 학생·미저장 편집기)를 따른다.

## 검사

```sh
python3 -m unittest tools.release.tests.test_fastpatch      # 31건 — 아래
cargo test -p kasaterm --release version::tests             # 판 번호 줄 입구 선택
cargo test -p kasaterm --release self_install               # 자기설치 백업·되돌리기(실제 sh)
```

fastpatch 검사는 임시 저장소(원격은 bare 저장소)·임시 피드·가짜 기기(`/version`)·가짜 나쵸(승인 계약 그대로)로 돈다.
`git` 과 `openssl` 은 진짜로 돌려 push 명령 구성·원격 대조와 Ed25519 서명 확인을 실제 도구로 보고, `gh`·`codesign`·
`hdiutil`·`cargo` 만 가짜다. 다루는 것: 다음 버전·원격 태그·피드 바닥, 중복 태그·다운그레이드·더러운 트리·미push, 채널,
기기 버전+SHA 비교, 자체 서명 CI 의 live 차단·설치본 신원 미확인, dry-run 무기록, 로컬 실행(검사·굽기·ready 판 재검증,
로컬 태그 안 만듦), 검사 실패·시간 초과, 나쵸 승인 거절 전부(대기·만료·범위 변경·다른 동작·창구 닫힘·로컬 파일),
계획 뒤 피드 변경, 한 번 소비와 재개(다른 승인·7일), push 시간 초과·거절·main 이동·남의 태그·잃은 상태 파일, CI 대기·
실패·산출물 모자람·해시·dmg 신원, 피드 EdDSA·더 새 판.

Windows 쪽 러스트(`win_sparkle::available`)는 이 맥에 윈도 타깃이 없어 컴파일해 보지 않았다.
