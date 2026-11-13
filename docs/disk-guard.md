# 디스크 지키기

2026-11-13 18:08 맥북 데이터 볼륨의 남은 공간이 0 이 되자 카사텀이 stderr 한 줄을 못 써 panic 하고 잠금 오염 연쇄로
죽었다(그 panic 은 c339fe76 에서 따로 고쳤다). 채운 것은 학생 작업 폴더의 cargo target(`/tmp/kt-live-rt` 40GB,
`/tmp/kt-voice-*` 16GB, 레포 `target` 16GB), Xcode DerivedData 11GB, flutter build, `~/Library/Caches` 126GB 였다.
같은 날 맥미니도 98%(남은 10GB)였다 — 거기서는 크롬의 서명 복제본(`/var/folders/…/X/com.google.Chrome.code_sign_clone`)이
69GB, 릴리스 도구 상태(`~/.local/state/kasaterm-preview`)가 57GB 였다.

앱이 남은 공간을 지켜보다 문턱 아래면 사람을 부르고, 다시 만들 수 있는 빌드 산출물만 스스로 치운다. 나머지(캐시·다른
앱 데이터)는 크기만 재서 「큰 곳」으로 보여 준다. 코드는 `crates/kasa-socket/src/disk_guard.rs`(지키미·정리·기록,
`disk_guard/outputs.rs` 찾기·지우기, `disk_guard/in_use.rs` 쓰는 중 판정), 앱 쪽은 `app/kasaterm/src/disk_warning.rs`,
CLI 는 `kasaterm-cli disk`, 폰은 `/term/disk` 를 읽는다.

## 지켜보기

- 앱이 뒤 스레드(`disk-guard`) 하나로 1분마다 홈이 있는 볼륨의 남은 공간을 잰다. `statfs` 시스템 호출 하나라 자식
  프로세스가 없다. 값은 `df` 의 Avail 과 같다 — APFS 의 「지울 수 있는 공간」은 빼고 본다(쓰기가 실패하는 기준에 가깝다).
- GUI 스레드는 지키미가 낸 마지막 값을 이벤트로 받아 들고만 있다. 큰 곳 재기(`du`)·정리(`ps`·`lsof`·지우기)는 모두 그
  뒤 스레드에서 돈다 — 렌더 프레임에서 자식 프로세스를 띄워 창이 멎은 일이 세 번 있었다. `disk_warning.rs` 의 시험이
  그리기 경로에서 재기·정리를 부르는지 소스를 훑어 막는다.
- 수준은 셋이다. 남은 공간이 경고 문턱(설정, 기본 20GB) 아래면 「문턱 아래」, 위험 문턱(기본 5GB) 아래면 「위험」.
- 알림은 수준이 **새로 올라갈 때만** 한 번 — 「문턱 아래」로 내려갈 때, 「위험」으로 더 내려갈 때. 문턱 위로 2GB 넘게
  올라서야 다시 울릴 수 있다. 문턱 언저리를 오가며 같은 알림이 되풀이되지 않게 하려는 것이다. 앱을 다시 켰을 때 이미
  문턱 아래면 한 번 울린다.
- 알림은 「봐야 할 것」이라 [design.md 「에이전트 알림 판정」](design.md)의 학생 알림과 같은 길로 간다 — 맥 알림과
  폰 푸시가 같은 말을 한다. 폰은 자리에 없을 때 보는 것이라 제목에 기기 이름을 싣는다(「맥미니 디스크 9.8GB 남음」).
  본문은 위험이면 「곧 앱과 빌드가 멈출 수 있어요」, 방금 지운 빌드 산출물, 큰 곳 앞 셋. 폰 푸시 종류는 관문이 이미 아는
  `waiting:blocked`(대기 소리)이고 칸은 `disk` 다 — 관문은 빈 칸을 받지 않고, 폰은 없는 칸이면 눌러도 허브에 머문다.
- 문턱 아래로 내려가면 그 자리에서 정리부터 한 바퀴 돌고, 그 뒤에도 문턱 아래일 때 알린다. 스스로 치워 문턱 위로 올라섰으면
  사람을 부르지 않는다(기록에만 남는다).

## 자동으로 지우는 것 — 다시 만들 수 있는 빌드 산출물만

| 종류 | 무엇 | 표식 |
|---|---|---|
| cargo target | 뿌리에 `CACHEDIR.TAG` 가 있고 그 둘째 줄이 「created by cargo」 | 서명 줄은 캐시 폴더 표준이라 pytest 도 똑같이 쓴다 — 레포의 `.pytest_cache` 가 target 으로 잡혔다 |
| flutter 빌드 결과 | `pubspec.yaml` 옆의 `build/`, `.dart_tool/flutter_build/` | `.dart_tool` 의 나머지(패키지 설정)는 남긴다 |
| Xcode DerivedData | `~/Library/Developer/Xcode/DerivedData/` 바로 아래 항목 하나하나 | |

찾는 자리:

- `/private/tmp/kt-*`·`/private/tmp/kasaterm-*` 아래 두 겹(학생 격리 작업 폴더)
- `~/Library/Caches/kasaterm-builds/` 아래 두 겹
- `scripts/build-app.sh` 가 구운 레포(`disk prune --repo` 가 `~/.config/kasaterm/disk-guard-repos.json` 에 기억한다, 최근 20개)의
  한 겹 — 레포의 `target`·`mobile`
- DerivedData

심볼릭 링크는 따라가지 않는다.

### 언제·얼마나

- **정기** — 앱이 켜지고 5분 뒤, 그 뒤 한 시간마다. 산출물 합이 상한(설정, 기본 15GB) 밑으로 내려갈 때까지 마지막으로 쓴
  때가 오래된 것부터 지운다. 마지막으로 쓴 때는 그 폴더와 두 겹 아래의 가장 늦은 수정 시각이다(빌드가 돌면 debug·release·
  `Build/Intermediates.noindex` 가 바뀐다).
- **문턱 아래** — 10분마다. 상한은 보지 않고, 남은 공간이 경고 문턱 + 5GB 위로 올라설 때까지 같은 순서로 지운다. 하나 지울
  때마다 남은 공간을 다시 잰다. 다음 빌드 한 번에 다시 문턱 밑으로 내려가지 않게 여유를 둔다.
- **빌드 직후** — `scripts/build-app.sh` 가 굽고 나서 뒤에서 `kasaterm-cli disk prune --after-build --repo <레포>` 를 돈다.
  릴리스 도구처럼 `CARGO_TARGET_DIR` 로 격리해 구울 때는 건너뛴다. `KASATERM_PRUNE_BUILDS=0` 으로 끈다.
- 손으로는 `kasaterm-cli disk prune --dry-run` 으로 먼저 본다. `kasaterm-cli disk status --places` 는 남은 공간과 큰 곳.

### 남기는 것

- 최근 30분(`disk_idle_minutes`) 안에 바뀐 것 — 문턱 아래여도 같다. 막 만든 산출물을 지우면 바로 다시 굽는다.
- 쓰는 중인 것 — 이 사용자의 어느 프로세스든 작업 폴더·실행 파일·열린 파일(`lsof`)이 그 산출물이나 그 작업 공간(cargo 는
  target 의 부모, flutter 는 프로젝트, DerivedData 는 그 항목) 안에 있거나, `CARGO_TARGET_DIR`(`ps -E`, 상대 경로는 그
  프로세스의 `PWD` 기준)이 그 target 이면 남긴다. 경로는 모두 실제 경로로 맞춰 견준다(`/tmp` ↔ `/private/tmp`).
  레포 `target` 은 그 레포에 학생 칸이 하나라도 서 있으면 쓰는 중이라 사실상 안 지워진다 — 의도한 것이다.
- 남는 다른 target 안의 심볼릭 링크가 가리키는 것 — 가리키는 쪽도 같이 지울 때만 지운다.
- 프로세스 조회가 실패하면 아무것도 지우지 않는다. 지우기 직전에 다시 재서(계획 뒤에 시작한 빌드가 잡았을 수 있다) 바뀌었거나
  쓰는 중이면 건너뛴다.

### 지우기 가드

지우기 직전 `disk_guard/outputs.rs` 의 `remove` 가 다시 본다 — 링크가 아니고, 실제 경로가 찾는 자리 **안**(그 자리 자체는
아님)이고, 손대지 않는 자리(`~/.local/state/kasaterm-preview` — 릴리스 도구의 격리 굽기, 제 정리가 따로 있다)와 겹치지
않고, 표식이 그대로인 폴더만 지운다. 하나라도 어긋나면 까닭을 기록에 남기고 지우지 않는다. 앱과 CLI 는 잠금
(`~/.config/kasaterm/disk-guard.lock`)으로 한 번에 하나만 돈다.

격리 실행(검증 리그 — 설정·세션·창 파일을 옮겼거나 `KASATERM_AUTOQUIT_MS` 등)은 남은 공간만 재고 아무것도 지우지 않는다.
리그는 `KASATERM_DISK_ROOTS=<폴더>[:<폴더>]` 로 준 폴더 안에서만 정리한다.

## 지우지 않는 것 — 크기만 보여 준다

`~/Library/Caches`·`~/Library/Developer`·`~/Library/Application Support`·`~/.cache`·`~/.cargo`·`~/.rustup`·
`~/.local/state/kasaterm-preview`·`/tmp`·이 사용자의 `/var/folders` 임시 묶음을 문턱 아래일 때(한 시간에 한 번, 수준이
올라가면 바로) `du -skx` 로 재서 1GB 넘는 곳 앞 다섯을 「큰 곳」으로 보여 준다. 캐시와 다른 앱의 데이터는 그 앱이 아직
쓰는지 알 수 없고 지우면 로그인·설정이 날아가는 것도 섞여 있어 사람이 고른다. 다른 앱의 컨테이너·받은 파일·휴지통은
macOS 가 접근 허락 창을 띄우므로 재지 않는다.

## 화면

- 사이드바: 트레이(「기기 추가」) 바로 위 경고 카드 — [design.md 「디스크 경고」](design.md). 누르면 가장 큰 곳을 파인더에서
  연다(큰 곳을 아직 못 쟀으면 정리 기록).
- 폰 허브: 그 기기 머리 밑 경고 카드. 기기마다 `/term/disk` 를 읽는다(옛 판 기기는 404 라 조용하다).
- 설정 「연결 기기 → 디스크 지키기」: 남은 공간 경고(10·20·40GB), 빌드 산출물 자동 정리 켬·끔, 빌드 산출물 상한(10·15·30GB).

## 설정 값

| 키 | 기본 | 뜻 |
|---|---|---|
| `disk_warn_free_gib` | 20 | 경고 문턱(GB) |
| `disk_critical_free_gib` | 5 | 위험 문턱(GB, 경고 문턱을 넘지 않는다) |
| `disk_build_cap_gib` | 15 | 정기 정리가 지키는 빌드 산출물 합의 상한(GB) |
| `disk_idle_minutes` | 30 | 이보다 최근에 바뀐 산출물은 지우지 않는다 |
| `disk_auto_prune` | true | 끄면 알림·큰 곳만 하고 지우지 않는다(빌드 직후 정리도 건너뛴다) |

## 기록

`~/Library/Logs/kasaterm-disk-guard.jsonl`(리눅스는 `~/.local/state/kasaterm/disk-guard.jsonl`, 리그는 설정 파일 옆
`disk-guard.jsonl`). 정리 한 바퀴가 한 줄 — 까닭(`routine`·`pressure`·`build`·`manual`), 앞뒤 남은 공간, 산출물 수·합·상한,
지운 것(경로·종류·크기), 건너뛴 것(경로·종류·크기·까닭), 멈춘 까닭. 아무것도 안 한 정기 정리는 남기지 않는다. 알림도 한 줄
(`"event": "alert"`). 1MB 를 넘으면 `.jsonl.1` 로 돌린다. 디스크가 꽉 차 못 써도 조용히 넘어간다.

## 맥미니

같은 앱이라 맥미니에서도 그대로 돈다(맥미니의 카사텀이 새 판이어야 한다). 맥미니의 알림은 그 맥에 사람이 없으니 폰 푸시가
본 길이다. 맥미니를 채운 것 대부분(크롬 서명 복제본·릴리스 도구 상태)은 자동으로 지우는 것이 아니라 「큰 곳」으로 뜬다.

## Windows

범위 밖이다. 쓰는 중 판정의 두 기둥이 없다 — 다른 프로세스의 환경 변수(`CARGO_TARGET_DIR`·`PWD`)를 읽는 `ps -E`, 이
사용자의 열린 파일·작업 폴더를 보는 `lsof`. Windows 에서 이것을 하려면 프로세스마다 PEB 를 읽고 핸들을 훑어야 하고(관리자
권한·Restart Manager), 그 없이 지우면 도는 빌드의 target 을 지울 수 있다. 학생 격리 빌드가 쌓이는 곳도 맥이다. 그래서
Windows 판은 지키미를 세우지 않고(경고 카드·알림도 없다) `kasaterm-cli disk prune` 은 까닭을 말하고 끝난다.
