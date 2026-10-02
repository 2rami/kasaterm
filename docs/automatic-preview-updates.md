# 개인 기기의 빠른 업데이트

일반 사용자는 기존 안정판을 받는다. 빠른 업데이트에 참여한 Mac만 별도 preview 피드를 확인하고, 새 판이 있으면 Sparkle 표준 창(새 판 안내 → 업데이트 설치 → 진행 막대 → 다시 켜기)을 띄운다. 사람이 표준 창에서 설치를 눌렀을 때만 받는다. 누르기 전에 몰래 받아 두거나 종료할 때 설치하지 않는다.

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

### 무엇을 굽나 — 등록된 커밋 그대로

controller는 마지막 발행 뒤 main에 들어온 등록 커밋 가운데 가장 새 것을 **그 커밋 그대로** 굽는다. main 끝이 아니다.
1번의 「정확한 커밋」·「미완성 변경 제외」가 등록의 뜻이라, 등록 안 된 커밋은 판에 싣지 않는 것이 정책에 맞는다.

- main 끝이 등록 안 된 커밋(폰·문서·관문만 바꾼 것 등)이어도 기다리지 않는다. 그 아래 가장 새 등록 커밋이 나간다.
- 그 판이 품은 더 오래된 등록은 「이미 나간 판에 포함」으로 끝낸다. 마지막 발행을 품지 않은 곁가지 커밋은 내지 않는다(앞 판에 실린 변경이 빠진 판이 되므로).
- 검사가 실패해 아직 굽지 못한 계획은 더 새 등록이 오면 양보한다. 다 구운 계획은 더 새 등록이 와도 먼저 낸다.

### 굽는 사이 main 이 움직이면

굽기가 20분 남짓이고 그 사이 여러 학생이 main 에 push 한다. 그래서 태그 직전 판정은 「main 이 계획 커밋 그대로인가」가 아니라
다음 셋이다(`policy.untagged_stale`).

- origin/main 이 계획 커밋을 품는다(조상이거나 같다).
- 계획 판 이상의 `v*` 태그가 원격에 없다.
- preview 피드가 계획 때 그대로다.

셋 중 하나라도 어긋나면 그 계획은 태그 없이 버리고 다음 tick 이 새 계획을 잡는다. 통과하면 태그는 계획 커밋에 판 번호·채널 기록만
바꾼 버전 커밋에 선다. main 이 계획 커밋 그대로면 그 버전 커밋을 main 에 올리고, 앞서 갔으면 **main 끝 + 버전 커밋 합침 커밋**
(트리는 main 끝 그대로에 `Cargo.toml` 판 번호 줄과 `.github/release-channel.json` 만 바꾼 것)을 태그와 `--atomic` 으로 함께 올린다.
판에 실리는 것은 태그 커밋이고, 그 사이 들어온 남의 변경은 main 에만 남는다. push 하는 사이 main 이 또 움직이면 원격이 태그 없이
그대로이므로 새 끝에 다시 합쳐 5번까지 올린다. 판 번호는 계속 오르고, 나가는 커밋은 모두 main 에 있다.

이 완화는 preview 만이다. stable(나쵸 단발 승인)은 예전대로 main 이 계획 커밋일 때만 태그를 올린다.

### controller 소스 갱신

`tools/release` 를 고쳐 main 에 올렸으면 controller 가 도는 소스 checkout 을 그 커밋으로 옮긴다. LaunchAgent 의 `WorkingDirectory` 가
그 폴더다(미니는 `~/.local/share/kasaterm-preview-controller`).

```sh
export PATH="$HOME/.local/bin:$PATH" GIT_LFS_SKIP_SMUDGE=1   # git-lfs 위치(미니). 소스는 파이썬 도구만 쓰니 그림은 안 받는다
git -C /절대/controller소스 fetch -q origin main
git -C /절대/controller소스 merge-base --is-ancestor <40자리 SHA> origin/main   # main 에 있는 커밋만
git -C /절대/controller소스 checkout -q --detach <40자리 SHA>
python3 -m tools.release.auto --state-dir /절대/상태폴더 status                  # 새 코드로 정책·대기열을 읽는지
```

도는 tick 은 시작할 때 모듈을 다 읽어 둔 채 끝까지 옛 코드로 가고, 다음 tick(1분 간격)부터 새 코드다. 그래서 굽는 중에 옮겨도 된다.
LaunchAgent 는 다시 등록하지 않는다. 그 checkout 은 사람이 손대지 않는 깨끗한 detached 상태로 둔다.

ssh 로 들어간 셸은 PATH 에 git-lfs 가 없어 첫 줄을 빼면 checkout 이 LFS 필터에서 반쯤 멈춘다 — HEAD 는 옛 커밋인데 파일만
바뀐 채가 되고 다음 tick 이 그 섞인 파일로 돈다(2026-10-01). 직전이 깨끗했다면 그 변경은 실패한 checkout 산물뿐이니 같은 SHA 로
`checkout -q -f --detach` 해 마무리한다.

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

이 동작을 지원하는 앱을 처음 한 번 전달해야 한다. 이후 정식 설치본은 켠 지 10초 뒤와 한 시간마다 `https://2rami.github.io/kasaterm/appcast-preview.xml`을 확인한다(`checkForUpdatesInBackground`). 이 프로세스에서만 Sparkle 자동 확인·자동 받기를 꺼 두므로(`NSArgumentDomain`) 확인은 판을 찾기만 하고 받지 않는다.

- 새 판이 있으면 Sparkle 표준 「새 판 있음」 창을 띄운다. 띄우는 때는 앱이 고른다 — 이 앱이 앞에 있고 키 입력이 2초 멈춘 틈이다. Sparkle 기본값은 일반 앱이면 「앱이 다시 활성화될 때」까지 미뤄, 앱 안에 있는 사람에겐 끝내 안 뜬다. 타자 중에 띄우면 키 창을 빼앗아 Return 이 「업데이트 설치」를 누른다. 다른 앱에 있으면 돌아왔을 때 뜬다.
- 표준 창의 단추는 [이 버전 건너뛰기][업데이트 설치] 둘이다. Sparkle 은 자동 확인이 꺼진 앱에서 「나중에 알림」을 숨긴다 — 창을 닫으면 같은 뜻이고(`Dismiss`) 한 시간 뒤 확인에서 다시 뜬다. 「앞으로 자동으로 받기」 칸도 자동 확인을 따라 숨는다. 예전 판의 창에서 그 칸을 켰어도 이 프로세스에선 꺼진다.
- [업데이트 설치]를 누르면 Sparkle 이 받고(진행 막대) 풀고 「설치 후 다시 시작」을 묻는다. 계정 메뉴 판 번호 줄의 「업데이트 확인」은 표준 확인 창을 띄우고, 없으면 Sparkle 이 「최신 판」이라고 답한다.
- 「설치 후 다시 시작」을 누르면 다시 켜기 직전에 Sparkle 위임(`shouldPostponeRelaunchForUpdate`)으로 한 번 멈춘다. 일하거나 사람을 기다리는 학생, 학생이 아닌 창에서 도는 명령, 저장 안 한 문서가 있으면 OS 시트로 「지금 다시 켜면 끊겨요 · 일하는 창: …」을 묻는다. Return 은 [나중에], [그래도 다시 켜기]는 위험 표시다. 끊길 것이 없으면 묻지 않고 다시 켠다. 종료는 winit `exiting`을 지나 세션이 저장된다. Sparkle의 종료는 ⌘Q의 확인을 지나지 않으므로 여기서 묻는다.
- [나중에]는 표준 창을 걷고 「새 판은 다음에 끌 때 설치돼요」를 알린다. 받은 판은 Sparkle 설치기가 쥐고 있다가 다음 정상 종료 때 설치한다(다시 켜지는 않는다). 「업데이트 확인」을 누르면 다시 켤지를 다시 묻는다.
- 기다렸다 저절로 다시 켜지 않는 것은, 사람이 모르는 때에 화면이 사라지기 때문이다.
- 안정판도 같은 위임을 단다(표준 창을 띄우는 때, 다시 켜기 전 확인). 피드·확인 주기·자동 받기 선택은 Info.plist 와 사람이 고른 값 그대로다.

### 0.2.18~0.2.27 에서 넘어오기

이 판들은 자체 알림 「새 판 있어요 [업데이트]」를 띄웠다. [업데이트]는 이 프로세스에 자동 받기를 켜고 확인을 다시 돌렸지만, Sparkle 은 자동 받기 대신 표준 창 경로를 골랐다(2026-10-02 `scripts/update-rig.py` 리그 재현). 표준 창은 일반 앱이면 다시 활성화될 때까지 미뤄져, 앱 안에서 누른 사람에겐 진행 표시 없이 아무 일도 없었다.

넘어오는 길: [업데이트]를 누른 뒤 표준 「새 판 있음」 창이 안 보이면 다른 앱을 한 번 눌렀다 kasaterm 으로 돌아온다. 그 창에서 [업데이트 설치]를 누르면 받고 풀고 **묻지 않고 바로** 다시 켠다(옛 위임이 즉시 설치한다). 학생 일이 끝난 뒤에 누른다. 「앞으로 자동으로 받기」 칸은 꺼도 되고 켜도 새 판에선 무시된다.

최초 전달은 `python3 -m tools.release.bootstrap stage --app <설치본> --source <검증한 새 번들> --expected-machine <기기 id> --expected-sha256 <실행파일 SHA-256> --expected-version <버전>`으로 준비한다. 이 단계는 실행 중인 설치본을 바꾸지 않는다. 반환된 id를 `arm --plan <id> --python <Desktop 밖의 절대 Python 경로>`에 주면 전용 LaunchAgent가 프로세스 종료를 기다린다.

기기 id·bundle id·실행파일명·버전·서명 팀·해시를 다시 확인하고 macOS 원자 교환으로 두 번들을 바꾼다. 이전 번들은 전용 backup으로 보존하며 앱을 강제 종료하거나 재실행하지 않는다. `status --plan <id>`로 설치 여부를 확인한다. 등록 응답이 불확실하면 새 helper를 중복 등록하지 않고, `rollback_required`는 원본과 신규 번들을 보존한 채 운영자가 확인한다. Kasaterm의 정상 종료 후에만 위 두 설정을 기존 설정에 병합한다. Viewer만 갱신할 때는 공통 설정을 바꾸지 않는다.

preview 업데이터를 시작한 프로세스에서는 Sparkle이 설치를 전담한다. 표준 창·진행 막대·다시 켜기 확인을 바꿨으면 `docs/verify-app.md` 「업데이트 리그」로 실제 Sparkle 흐름을 돌려 본다. 로컬 dist 자기설치와 기존 원격 app-update 도우미를 함께 실행하지 않는다. 개발 실행·격리 검증·Lite는 이 자동 설치를 켜지 않는다. 기본 stable 사용자는 기존 업데이트 선택을 유지한다.

## 안정판 보호

- `.github/release-channel.json`은 버전 커밋의 채널·태그·부모 SHA·플랫폼을 고정한다. CI는 그 태그의 파일을 검증하고 분기한다.
- preview는 Mac DMG 하나, GitHub prerelease, `latest=false`, `appcast-preview.xml`만 사용한다. 안정판 피드·Windows 피드·일반 latest는 바꾸지 않는다.
- 기존 나쵸 단발 승인 방식은 그대로다. 자동 publisher는 별도 정책 허가이며 두 허가를 같은 작업에서 섞지 않는다.
- 버전·태그·파일 해시·서명·공증 검사를 낮추지 않는다. main이 움직이면 preview는 위 합침 커밋으로, stable은 재계획으로, 피드는 검증한 피드 커밋을 비교 후 다시 제출한다. 강제 push는 하지 않는다.
- 모바일 바이너리는 이 Mac 경로의 대상이 아니다. 폰 앱은 설치 링크 판(`mobile/tool/adhoc.sh`, [절차](ios-adhoc-install.md))으로 따로 올린다. TestFlight는 2026-10-01부터 쓰지 않는다.
