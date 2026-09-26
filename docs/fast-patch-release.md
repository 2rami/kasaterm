# 빠른 패치 릴리스 — 여러 기기에 작은 수정을 올리는 길

카사텀을 쓰는 기기가 늘면서 작은 수정도 「버전 올리고 → 태그 → CI → 각 기기 반영」을 매번 손으로 했다.
이 문서는 그 흐름을 **계획 하나 + 승인 한 번**으로 묶는 도구(`tools/release/fastpatch.py`)와, 기기에서 새 판을
받는 두 입구를 적는다. 정본 파이프라인은 그대로다 — 새 배포 시스템을 따로 만들지 않는다.

## 한 장 요약

```
plan ──► (사람이 범위 1회 확인 → 나쵸가 approval.json) ──► run: verify → build → tag → release → feed → devices
                                                                 └ 끝난 단계는 건너뜀(재실행 안전) ┘
status: 기기마다 지금 판(버전 · SHA) → 목표 · 마지막 확인 · 오프라인 · 보류 까닭
```

| 입구 | 누가 | 어떻게 | 이미 있나 |
|---|---|---|---|
| (A) 나쵸가 올린다 | 나쵸 | `plan` → 사람 확인 1회 → `run` → `status` 로 기기 추적 | **이번에 새로** — 단, 게시 단계는 아직 막혀 있다(아래) |
| (B) 기기에서 받는다 | 그 기기의 사람 | 계정 메뉴 바닥 **판 번호 줄을 누른다** / 맥 앱 메뉴 「업데이트 확인…」 / 윈도 시작 뒤 토스트 [설치] | 맥 메뉴·윈도 토스트는 **원래 있음**, 판 번호 줄은 **이번에 새로** |

두 입구가 같은 것을 먹는다 — CI 가 서명해 올린 릴리스 파일(dmg·msi)과 그것을 가리키는 appcast
(`docs/appcast.xml`·`docs/appcast-win.xml`, EdDSA 서명). 기기에 소스 클론·빌드 환경이 없어도 된다.

## (A) 나쵸 입구 — 쓰는 명령

```sh
# 1) 계획: 커밋·다음 패치 버전·포함 변경·플랫폼/채널·기기 범위를 한 파일에 고정. 아무것도 안 바꾼다.
python3 -m tools.release.fastpatch plan
#    --devices devices.json   [{label, base}] — 없으면 이 기기와 명부(~/.config/kasaterm/machines.json)
#    --version 0.2.5          직접 고를 때(원격 태그·피드보다 높아야 한다)
#    --channel stable         업데이터에 있는 채널만(지금은 stable 하나)

# 2) 미리보기: 단계별로 무엇을 할지와 막힘. 아무것도 안 바꾼다.
python3 -m tools.release.fastpatch dry-run <plan_id>

# 3) 승인 틀: 나쵸가 사람 확인을 받은 뒤 채워 <상태폴더>/<plan_id>/approval.json 으로 둔다.
python3 -m tools.release.fastpatch approval-template <plan_id>

# 4) 실행: 승인에 묶인 단계를 끝까지. 이 판에서는 --mock 으로만 움직인다.
python3 -m tools.release.fastpatch run <plan_id> [--mock mock.json]

# 5) 추적: 기기별 지금 판·목표·마지막 확인·오프라인. 언제든 다시 불러도 된다(승인 불필요, 읽기만).
python3 -m tools.release.fastpatch status <plan_id>
```

상태 폴더는 `~/.config/kasaterm/releases/`(`KASATERM_RELEASE_DIR`·`--state-dir` 로 바꾼다).

### 계획이 고정하는 것

- **정확한 커밋** — 워킹트리가 깨끗하고 `origin/main` 에 들어간 커밋만. 계획 뒤 main 이 움직이면 태그 단계가
  멈추고 새 계획을 요구한다(계획에 없던 변경이 섞이지 않게).
- **다음 버전** — `max(원격 태그, 피드 버전, Cargo.toml) + 패치 1`. 로컬 태그는 믿지 않는다(미니에 v0.2.0 태그가
  없던 적이 있다 — `git ls-remote --tags` 로 본다). 이미 있는 태그·낮은 버전은 계획 단계에서 막힌다.
- **포함 변경** — 지난 태그 뒤 커밋과 바뀐 파일을 가른다: 네이티브(앱 업데이트 필요) · 모바일(TestFlight 판 필요)
  · 문서·인프라(기기에 안 감). iOS 가 끼면 빌드 번호(yymmddHHMM, 단조)를 함께 고정한다.
- **플랫폼 능력** — 코드에서 읽는다: 맥 Sparkle(채널, CI 서명, 공증 여부) · 윈도 WinSparkle · iOS TestFlight.
- **기기 범위** — `/version` 이 알려 준 버전 **과 SHA** 로 견준다(아래). 보류·더 새 판 기기는 범위에서 빠진다.
- **기존 빌드** — `dist/kasaterm.build.ready-*.json` 중 같은 커밋·깨끗한 것이 있으면 적는다(격리 ready 판).
- **계획 id** — 위 범위의 해시. 승인은 이 id·태그·커밋·단계·기기 목록에 묶인다.

### 승인 — 한 번, 이 계획에만

- 사람이 계획(버전·커밋·기기 범위)을 **한 번** 보고 나쵸가 `approval.json` 을 둔다. 도구는 스스로 승인하지 않는다.
- 단계마다 다시 묻지 않는다. 대신 승인은 **그 계획 그대로**에만 먹는다 — id·태그·커밋이 다르거나, 기기를 넓혔거나,
  만료됐거나(`expires_at_ms`), 승인한 사람이 비었으면 아무 단계도 안 움직인다. 앞으로의 모든 배포를 미리 허락하는
  승인은 없다.
- 계획에 막힘이 있으면 승인이 있어도 안 움직인다.

### 단계와 재실행

| 단계 | 하는 일 | 다시 돌리면 |
|---|---|---|
| verify | cargo 검사 3종(+모바일이면 flutter) | 끝났으면 건너뜀 |
| build | 태그 전 굽기 확인 — 격리 워크트리, 자동설치 `dist/kasaterm.app` 안 덮음. 같은 커밋 ready 판이 있으면 갈음 | 〃 |
| tag | `scripts/tag-release.sh` 와 같은 일(버전 커밋 + 태그 push) — main 이 계획 커밋 그대로일 때만 | 원격에 태그가 있으면 **다시 세우지 않는다**(계획 커밋 위의 버전 커밋이 아니면 손대지 않고 멈춤) |
| release | CI(`release.yml`)가 dmg·msi 를 올렸는지와 sha256 | 산출물이 모자라면 실패 — **appcast 단계로 안 넘어간다** |
| feed | appcast 가 목표 판을 가리키는지 | 이미 목표 판이면 그대로, 더 새 판이면 **되돌리지 않고 멈춤** |
| devices | 기기별 `/version` 추적 | 매번 다시 잰다 |

**이번 판에서 실제 백엔드는 모든 단계를 거절한다.** 태그 push·릴리스·appcast 게시·공증·설치·재시작은 하지 않고,
승인 연결과 mock 검사(임시 저장소 + bare 원격 · 로컬 피드 · 가짜 기기)까지만 있다. 실제 단계를 붙일 때는
`RealBackend` 의 단계를 하나씩 채우고, 같은 mock 검사를 실제 모양에 맞춰 늘린다.

### 기기 비교 — 「0.2.0」만으로는 못 가른다

지금 모든 기기가 0.2.0 이라고 말한다. 로컬 굽기는 판 번호를 안 올리기 때문이다. 그래서 `/version` 의 `build`
(git SHA, 끝에 `+` 면 미커밋 포함)까지 본다.

| 상태 | 뜻 |
|---|---|
| current | 목표 버전이고 SHA 가 계획 커밋이나 그 태그의 버전 커밋 |
| update | 목표보다 낮거나, 같은 번호의 다른 판 — 이 기기의 SHA 가 계획 커밋의 조상이다 |
| hold | 기기 판에 권위 main 에 없는 커밋이 있다 — 올리면 그 커밋이 빠진다. **범위에서 뺀다** |
| newer | 기기가 더 새 판 — 다운그레이드 안 함 |
| offline | 안 닿는다 — 마지막으로 본 판과 그 시각을 남긴다 |

2026-09-27 실제 `plan` 결과: 미니 `0.2.0 · 8933a0ca+` → update(미커밋 포함 판), 맥북 `0.2.0 · 84190e33` → **hold**,
windesktop → offline(닿은 기록 없음).

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

## 서명·공증 — 한 번 실기 확인이 필요한 곳

- 로컬 굽기 판은 Developer ID(L366799VND), CI 릴리스는 **kasaterm-ci 자체 서명**이고 **공증하지 않는다**.
- Sparkle 문서(「Rotating signing keys」)는 **Developer ID 로 서명하고 EdDSA 공개키를 넣은 앱**이면, EdDSA 가 맞는 한
  업데이트가 Apple 코드 서명 인증서를 바꿔도 받는다고 한다(EdDSA 키와 인증서를 한 번에 둘 다 바꾸지는 못한다).
  다만 kasaterm-ci 는 Apple 인증서가 아니라 자체 서명이다 — 이 경우도 받아 주는지, 받은 판이 공증 없이
  Gatekeeper 를 넘는지는 **실기 1회 확인 전까지 모른다.** 도구는 계획의 「주의」에 이 줄을 늘 싣는다.
- 새 키 발급·로그인·권한 추가는 이 흐름에 없다. appcast 서명 키는 CI 비밀(`SPARKLE_ED_PRIVATE_KEY`) 그대로다.

## 처음 한 번만 필요한 것

- 판 번호 줄 입구·자기설치 백업은 새 판이 한 번 설치돼야 생긴다(그 전 판에는 없는 코드다).
- 윈도는 MSI 로 한 번 설치돼 있어야 업데이터가 있다(windesktop 은 2026-09-27 오프라인).
- 서명이 갈리는 첫 Sparkle 업데이트(Developer ID → kasaterm-ci)는 사람이 한 기기에서 한 번 해 보고 결과를 적는다.

## 검사

```sh
python3 -m unittest tools.release.tests.test_fastpatch      # mock 저장소·피드·가짜 기기 21건
cargo test -p kasaterm --release version::tests             # 판 번호 줄 입구 선택
cargo test -p kasaterm --release self_install               # 자기설치 백업·되돌리기(실제 sh)
```
