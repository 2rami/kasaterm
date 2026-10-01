# 개인 기기의 빠른 업데이트

일반 사용자는 기존 안정판을 받는다. 빠른 업데이트에 참여한 Mac만 별도 preview 피드를 확인하고, 새 판이 있으면 앱 오른쪽 위에 「새 판 있음」 알림을 띄운다. 사람이 [업데이트] 를 눌렀을 때만 받아서 설치하고 다시 켠다. 몰래 받아 두거나 종료할 때 설치하지 않는다.

## 작업 완료와 발행

1. 에이전트는 수정·검사·커밋을 끝내고 정확한 커밋을 `origin/main`에 올린다. 다른 작업자의 미완성 변경은 포함하지 않는다.
2. 같은 저장소에서 `python3 -m tools.release.auto enqueue <40자리 커밋 SHA>`를 실행한다. 이것은 `preview-ready/<SHA>` 원격 태그 하나만 멱등 등록한다. 임의 브랜치나 아직 main에 없는 커밋은 발행하지 않는다.
3. 전용 controller가 대기열을 직렬 처리한다. 깨끗한 별도 checkout에서 검사 → 패치 버전 증가 → Developer ID 서명 → 공증 → 태그·prerelease → CI 검증·EdDSA → preview 피드 순서다.
4. 완료는 명령 종료 코드가 아니라 `feed` 단계까지 검증된 상태다. CI 대기 중인 작업을 성공으로 보고하지 않는다. 실패·대기는 상태와 원인을 남긴다.

CI의 피드 push만으로는 GitHub Pages가 다시 빌드되지 않는다. appcast job에만 `pages: write`를 주고,
검증된 피드 커밋 뒤 기존 `main:/docs` 사이트에 빌드를 한 번 요청한다. 게시 커밋을 포함한 Pages 빌드 성공과
공개 피드의 버전·주소·서명·크기를 최대 60회(확인 사이 10초) 대조한다. 실패·시간 초과는 CI 실패이며,
이미 올라간 피드 커밋·태그·산출물을 되돌리거나 새 릴리스를 만들지 않는다. 사이트 설정도 자동 변경하지 않는다.
원인을 해결한 뒤 해당 appcast job만 명시적으로 재실행하면 같은 검증 피드를 확인하고 Pages 게시를 재시도한다.
근거: [Pages의 GITHUB_TOKEN 제한](https://docs.github.com/en/pages/getting-started-with-github-pages/configuring-a-publishing-source-for-your-github-pages-site#troubleshooting-publishing-from-a-branch),
[Pages 빌드 요청과 권한](https://docs.github.com/en/rest/pages/pages#request-a-github-pages-build).

일반적인 `build-app.sh` 실행이나 자유문구 `done`만으로 공개 발행을 승인하지 않는다. 새 커밋을 검증하고 위 대기열에 등록하는 것까지가 빠른 업데이트 작업의 완료 기준이다.

## 처음 한 번: controller

controller는 Mac 한 대다. 소스와 상태, LFS 캐시는 Desktop·Documents·Downloads 밖에 둔다. 상시 정책 파일은 저장소 밖의 소유자 전용 0600 파일이고 저장소·기계 id·preview 피드·Mac·현재 minor의 패치 증가·서명 팀·공증 프로필·LFS 경로에 결속된다.

LFS 경로는 미니 LFS 서버의 저장소(`~/.local/share/kasaterm-lfs`)를 그대로 쓴다([fast-patch-release.md](fast-patch-release.md) 「미니 LFS 서버」). 사람이 올린 그림이 곧 controller 의 로컬 캐시라 격리 워크트리가 네트워크 없이 선다. 2026-09-29 GitHub LFS 예산 초과로 controller 가 멈춘 뒤 합쳤다(정책 revision 4).

직렬 controller는 상태 폴더의 0700 `target/`에 컴파일 캐시를 재사용한다. 계획별 소스·검사 결과·서명 산출물은 계속 분리하며, 새 커밋의 검증을 캐시가 대신하지 않는다.

```sh
python3 -m tools.release.auto --state-dir /절대/상태폴더 enable \
  --controller 기계_ID --minor 0.2 \
  --lfs-storage ~/.local/share/kasaterm-lfs --unlock-signing
python3 -m tools.release.auto --state-dir /절대/상태폴더 install \
  --source /절대/controller소스 --python /절대/python3 --apply
```

`install`은 `--apply`가 없으면 LaunchAgent 내용을 보여줄 뿐이다. 서명 키·비밀번호·공증 자격 내용은 정책이나 로그에 넣지 않는다. 기존 키체인과 이름 있는 공증 프로필을 쓴다.

```sh
python3 -m tools.release.auto --state-dir /절대/상태폴더 status
python3 -m tools.release.auto --state-dir /절대/상태폴더 disable
python3 -m tools.release.auto --state-dir /절대/상태폴더 observe
```

`disable`은 미착수 controller 작업을 막는다. 이미 외부 태그·DMG 업로드가 시작된 트랜잭션은 CI가 검증 후 피드 게시를 마무리할 수 있다. 이미 시작한 외부 작업을 취소했다고 보고하지 않는다. 이 경계는 `publication.commit_point`와 `ci_may_finish_after_disable`에 기록한다.

정책을 껐다 다시 켠 뒤에는 같은 범위의 정확한 계획만 `reauthorize <plan_id>`로 재허가한다. 태그가 없는 계획만 `replan <plan_id>`로 새 검사·계획을 요청할 수 있다. 이미 태그가 있는 계획을 버리고 다른 릴리스를 만들어 우회하지 않는다.

## 처음 한 번: 받는 Mac

기기별 `settings.json`에서 다음 두 항목을 명시한다. 계정 공통 설정 동기화에는 포함하지 않는다.

```json
{"update_channel":"preview","automatic_update_on_quit":true}
```

`automatic_update_on_quit`는 이름이 옛 동작에서 왔지만 지금은 preview 참여 표식이다. 이미 켜 둔 기기를 그대로 잇기 위해 이름을 바꾸지 않는다.

이 동작을 지원하는 앱을 처음 한 번 전달해야 한다. 이후 정식 설치본은 켠 지 10초 뒤와 한 시간마다 `https://2rami.github.io/kasaterm/appcast-preview.xml`을 **확인만** 한다(Sparkle 확인 전용 `checkForUpdateInformation`, 받지 않는다).

- 새 판이 있으면 「새 판 vX.Y.Z 있어요」 알림에 [업데이트][닫기]가 선다. 같은 판은 한 번만 세운다. [닫기]나 알림 본문을 누르면 그 판을 기기 설정 `update_notice_dismissed`(계정 동기화 밖)에 적고, 다음 판이 나올 때까지 다시 띄우지 않는다. 계정 메뉴 판 번호 줄의 「업데이트 확인」은 닫은 판도 다시 보여 주고, 없으면 「최신 판이에요」라고 답한다.
- [업데이트]를 누르면 그때만 Sparkle 자동 받기를 이 프로세스에 켜고 받는다. 서명 검증이 끝나면 바로 설치하고 다시 켠다(`immediateInstallationBlock`). 종료는 winit `exiting`을 지나 세션이 저장된다.
- 다시 켜면 끊길 일이 있으면 한 번 더 묻는다 — 일하거나 사람을 기다리는 학생, 학생이 아닌 창에서 도는 명령, 저장 안 한 문서. 알림이 「지금 업데이트하면 끊겨요 · 일하는 창: …」로 바뀌고 [그래도 업데이트][닫기]가 선다. 기다렸다 저절로 다시 켜지 않는 것은, 사람이 모르는 때에 화면이 사라지기 때문이다. Sparkle의 종료는 ⌘Q의 확인을 지나지 않으므로 여기서 묻는다.
- 받기가 실패하거나 설치 없이 끝나면 「업데이트 실패 · 까닭」을 알리고 자동 받기를 다시 끈다. 켜 둔 채면 Sparkle의 예약 확인이 사람 없이 받아 종료 때 설치한다.
- Sparkle은 한 번 받은 판을 종료 때 반드시 설치한다. 그래서 누르기 전에는 받지 않는 것이 유일한 막음이다. [업데이트]를 누른 뒤 받는 중에 종료하면 그 종료에서 설치될 수 있다.

최초 전달은 `python3 -m tools.release.bootstrap stage --app <설치본> --source <검증한 새 번들> --expected-machine <기기 id> --expected-sha256 <실행파일 SHA-256> --expected-version <버전>`으로 준비한다. 이 단계는 실행 중인 설치본을 바꾸지 않는다. 반환된 id를 `arm --plan <id> --python <Desktop 밖의 절대 Python 경로>`에 주면 전용 LaunchAgent가 프로세스 종료를 기다린다.

기기 id·bundle id·실행파일명·버전·서명 팀·해시를 다시 확인하고 macOS 원자 교환으로 두 번들을 바꾼다. 이전 번들은 전용 backup으로 보존하며 앱을 강제 종료하거나 재실행하지 않는다. `status --plan <id>`로 설치 여부를 확인한다. 등록 응답이 불확실하면 새 helper를 중복 등록하지 않고, `rollback_required`는 원본과 신규 번들을 보존한 채 운영자가 확인한다. Kasaterm의 정상 종료 후에만 위 두 설정을 기존 설정에 병합한다. Viewer만 갱신할 때는 공통 설정을 바꾸지 않는다.

preview 업데이터를 시작한 프로세스에서는 Sparkle이 설치를 전담한다. 로컬 dist 자기설치와 기존 원격 app-update 도우미를 함께 실행하지 않는다. 개발 실행·격리 검증·Lite는 이 자동 설치를 켜지 않는다. 기본 stable 사용자는 기존 업데이트 선택을 유지한다.

## 안정판 보호

- `.github/release-channel.json`은 버전 커밋의 채널·태그·부모 SHA·플랫폼을 고정한다. CI는 그 태그의 파일을 검증하고 분기한다.
- preview는 Mac DMG 하나, GitHub prerelease, `latest=false`, `appcast-preview.xml`만 사용한다. 안정판 피드·Windows 피드·일반 latest는 바꾸지 않는다.
- 기존 나쵸 단발 승인 방식은 그대로다. 자동 publisher는 별도 정책 허가이며 두 허가를 같은 작업에서 섞지 않는다.
- 버전·태그·파일 해시·서명·공증 검사를 낮추지 않는다. main이 움직이면 재계획하거나 검증한 피드 커밋을 비교 후 다시 제출한다. 강제 push는 하지 않는다.
- 모바일 바이너리는 이 Mac 경로의 대상이 아니다. 모바일 업데이트는 TestFlight/App Store의 정상 배포 경로를 사용한다.
