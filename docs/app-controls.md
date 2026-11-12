# 에이전트의 앱 화면 조회·복원·재시작

`kasaterm-cli app`은 로컬 소켓과 인증된 `/app/control`을 같은 계약으로 쓴다. HTTP는 기존 소유자 인증·Origin 검사를 그대로 거치며 손님에게는 화면도 조작도 제공하지 않는다. 나쵸 승인 서버가 필요한 배포 조율용 `app-restart run`과 별도로, 사용자의 지시를 받은 에이전트가 앱을 조작하는 창구다.

```sh
kasaterm-cli app inspect --machine <등록된-machine_id>
kasaterm-cli app screen --machine <등록된-machine_id> --out /새/경로.jpg
kasaterm-cli app restore --machine <등록된-machine_id> --expect <inspect의-revision>
kasaterm-cli app restart --machine <등록된-machine_id> --expect <inspect의-revision>
kasaterm-cli app update-check --machine <등록된-machine_id> --expect <inspect의-revision>
```

기기를 생략하면 현재 앱이다. `--api http://…`도 지원하며 원격 경로는 카사넷을 따른다. HTTP에서 `--machine`을 함께 주면 그 HTTP 호스트의 신원과 같아야 한다(다른 기기는 `/m/~ID` 또는 대상 기기 API로 연결). `--machine`에는 보드에서 확인한 안정 ID를 준다. 등록되지 않은 호스트·실행 경로·셸 명령은 받지 않는다.

`inspect`는 관측 시각, 기기·앱 인스턴스, OS 초점·최소화, 현재 방·칸·탭, 복원 선택 목록·실제 진행, 재시작 영향(바쁜 작업·저장 안 된 문서·복원 가능한 대화·새로 뜰 셸)을 준다. 배경에 있는 앱의 활성 방을 사용자가 보고 있다고 단정하지 않는다. `screen`은 실제 모니터를 한 번 캡처한다. 여러 모니터는 응답의 `displays`를 보고 `--display ID`로 고른다. 화면 기록 권한이 없으면 오류로 끝나며 권한 창을 임의로 승인하지 않는다. JPEG는 새 파일에만 저장하고 응답에서 이미지 원문을 제거한다.

변경 명령은 30초 안의 `revision`이 필요하다. 다른 기기·다른 앱 인스턴스·다른 방·복원 선택·작업 상태가 되면 재조회가 필요하다. 이것은 사용자 승인 증명이 아니라 잘못된 현재 화면을 조작하지 않기 위한 조건이다. 재시작은 사용자가 요청한 기기와 시점에만 실행한다. 이미 받은 승인을 되묻는 절차는 없다.

- `restore`: 현재 복원 창의 선택된 작업을 같은 UI 경로로 복원한다. 새로 시작·기록 삭제는 제공하지 않는다. 반환값 `restoring`은 접수이며 완료는 다음 `inspect`의 실제 진행으로 확인한다.
- `restart`: 세션 저장 성공을 확인하고 앱 밖 도우미를 준비한다. 응답을 보낼 시간을 둔 뒤 GUI 스레드에서 화면·대상·미저장 문서를 다시 검사하고 정상 종료한다. 같은 설치본만 다시 열고, 새 앱이 저장된 재시작 작업에서 복원 의도를 읽어 자동으로 복원한다. 임의 환경변수나 에이전트 인증값을 새 앱으로 넘기지 않는다.
- 바쁜 작업이 있으면 기본적으로 거부한다. 영향 목록을 확인하고 사용자가 중단을 허락한 경우만 `restart … --interrupt`로 진행한다. 미저장 문서·새 자기설치·다른 진행 중 재시작은 이 옵션으로도 넘지 못한다. 일반 셸 명령은 이어지지 않으며 대화 복원이 작업의 자동 재개를 뜻하지 않는다.
- 응답의 `job_id`는 기존 `app-restart status JOB --machine ID`로 추적한다. 새 PID가 뜬 것과 복원 완료를 구분하며, 복원이 실패하면 작업도 실패로 남는다. 응답이 끊겼을 때 재시작을 다시 실행하지 않고 작업 상태와 `inspect`를 먼저 조회한다.
- `update-check`: 설치된 Sparkle/WinSparkle의 업데이트 창을 연다. 업데이트 설치 완료·OS 권한 승인을 뜻하지 않는다.

소스: `kasa-socket::app_control`(계약·관측 조건), `kasa-http::http::app_control`(소유자 문), 앱 `app_control.rs`(GUI 작업), `cli/app_control.rs`(명령·캡처 파일). 캡처는 기존 `kasa_remote::screen_mirror::Source`를 공유한다. 재기동 도우미·작업 기록은 기존 `app_restart`를 재사용한다.

일반 조작도 복원 상태를 확인한다. 복원 창에서 선택된 작업이 있으면 최초 조작이 같은 복원 경로를 시작하고
`app_restoring` 오류를 반환한다. **그 조작 자체는 실행하지 않는다.** `inspect.ready`가 true가 된 뒤 현재 판에서
대상 주소를 다시 찾고 요청한다. 복원 전의 `%N`에 대기 중 조작을 재생하지 않는다. 선택이 비었거나 복원이 실패하면
까닭을 돌려주며 새로 시작·기록 삭제로 대체하지 않는다. 실패한 복원은 새 revision으로 `app restore`를 다시 부르면
기존 UI의 다시 시도 경로를 타며 이미 준비된 칸은 유지한다. `where`의 `app`에도 준비·복원 상태가 실린다.
단순 조회·캡처·연결 확인·미러 크기 갱신은 자동 복원을 시작하지 않는다.
