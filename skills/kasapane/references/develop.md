# 카사텀 개발

카사텀 자체를 수정·검증·배포할 때만 읽는다. 아래 경로는 카사텀 저장소 기준이다. 문서의 목차에서 현재 영역을 찾아 필요한 절만 읽고, 관련 없는 문서는 열지 않는다.

## 구현

- Rust 수정은 `docs/code-map.md`의 해당 모듈을 먼저 찾는다. `main.rs`에는 타입·자유 함수·진입점·시험을 두고 App 메서드는 기능별 모듈로 둔다. 의존성 변경은 `docs/workspace.md`의 경계를 따른다. 코드 규칙은 `docs/rust-style.md`다.
- 화면 수정은 `docs/design.md`의 공통 배치 계약과 해당 화면 절을 읽는다. 공통 토큰·부품을 사용하고 그리기·레이아웃·클릭 영역을 함께 맞춘다. 새 기준은 문서와 코드에 함께 반영한다. 전면 개편 요청은 기존 값을 재검토할 수 있다.
- 시안도 PC는 Rust/wgpu, 폰은 Flutter 실제 화면으로 만든다. HTML 대체 시안은 만들지 않는다. 레이아웃·가독성은 px로 확인한다.
- 남의 미완성 변경을 포함하지 않는다. 공유 트리의 브랜치 전환·강제 푸시·이력 재작성은 임의로 하지 않고, 필요하면 격리 worktree를 쓴다.
- 격리 빌드의 target 은 `~/Library/Caches/kasaterm-builds/<일>/` 이나 `/private/tmp/kt-*` 에 둔다. `scripts/prune-build-outputs.py` 가 이 자리와 레포 `target` 의 합이 15GiB(`KASATERM_BUILD_CAP_GIB`)를 넘을 때, 30분 넘게 안 바뀌고 어느 프로세스도 그 target·작업 공간을 쓰지 않는(실제 경로로 본 작업 폴더·실행 파일·열린 파일·`CARGO_TARGET_DIR`) 것부터 지운다. 다른 target 의 심볼릭 링크가 가리키는 것은 남긴다. 지금은 `KASATERM_PRUNE_BUILDS=1` 일 때만 `scripts/build-app.sh` 끝에서 돈다. 손으로는 `--dry-run` 으로 먼저 본다.

## 검증

- 검증 앱을 띄우기 전에 `docs/verify-app.md`의 격리 규칙과 해당 리그 절을 읽고 그 환경을 적용한다. 앱과 CLI 모두 같은 검증 소켓을 쓴다. 사용자 세션·설정·창 크기·기기 명부·신원·캐시를 빌려 쓰지 않는다.
- 직접 띄운 PID만 종료한다. `pkill`·`killall`, 본판 `session.json` 삭제·교체, 사용자 앱 재시작으로 검증을 대신하지 않는다.
- 실제 창 캡처를 직접 확인한다. 먼저 축소한 뒤 필요한 장면만 읽는다. 성능 체감은 release 빌드로 측정한다. 합성 키 입력을 실제 OS 한글 IME 검증으로 보고하지 않는다.
- 변경에 맞는 시험을 실행하고 결과와 미확인을 구분한다. 앱 시험은 `cargo test -p kasaterm --bin kasaterm`, 의존 경계는 `python3 scripts/check-workspace.py`, Rust 규칙은 `cargo clippy --all-targets`. UI 검증을 사용자에게 떠넘기지 않는다.

## 커밋·배포

- 커밋에 실제 작성 모델 공동저자를 유지하고 `Co-Authored-By: NachoNekoBot <322779791+NachoNekoBot@users.noreply.github.com>`를 추가한다. 공개 주석·문서·커밋에 개인 호칭·페르소나를 넣지 않는다.
- 앱 동작 변경은 검사 → 정확한 커밋의 main 푸시 → `python3 -m tools.release.auto enqueue <40자리 SHA>` → 게시 확인까지다. 실행 전 `docs/automatic-preview-updates.md`의 발행 절을 읽는다. 등록·로컬 빌드·피드 게시·기기 설치를 구분한다. stable 공개는 별도 승인이다.
- `mobile/` 변경은 main 푸시 뒤 깨끗한 worktree에서 `mobile/tool/adhoc.sh`로 올린다. `docs/ios-adhoc-install.md`의 배포 절을 읽고 판 번호·설치 링크를 보고한다. TestFlight는 쓰지 않는다.
- 문서만 바뀌면 앱 배포를 등록하지 않는다. 빌드·검사·공증·CI·피드가 막히면 정확한 단계와 원인을 알린다. 사용자 앱을 임의로 종료·재시작하지 않는다.
