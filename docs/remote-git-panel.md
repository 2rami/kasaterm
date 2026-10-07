# 원격 창의 Git 패널

오른쪽 Git 열은 활성 pane 안의 실제 선택 탭을 따라간다. 로컬 경로 고정은 로컬 pane에만 적용한다. 원격 pane은 연결의 원본 pane ID·surface key와 기기 명부의 machine ID로 식별하고 원본 기기에서 Git 정보를 읽는다.

`GET /term/gitcol?schema=kasa.git-panel.v2&machine_id=…&pane=…&surface_key=…&commits=…`는 기존 인증·명부 경로를 사용한다. 원본 기기는 요청한 기기·surface key가 현재 원본과 일치하는지 확인하고 그 pane의 cwd를 조회한다. Git을 읽은 뒤 신원·cwd를 다시 확인한다. 보기 창의 로컬 cwd나 오래된 board cwd는 조회할 저장소로 쓰지 않는다. Windows의 보고 cwd도 동일한 원본 조회를 거친다.

응답은 `schema`, `ok`, `source`(machine_id, pane, surface_key, cwd), `view`로 구성한다. 클라이언트는 스키마·원본 식별자·절대경로·스냅샷 cwd를 검증한다. 구버전 응답은 업데이트 안내이며, 연결 실패·원본 변경·저장소 아님과 구분한다. 요청은 HTTP 10초/2MiB, 원본의 Git 실행 전체 8초(로컬 열은 20초)와 명령별 출력 상한을 가진다. status 로 저장소인지 정한 뒤 나머지 git 은 한꺼번에 돌린다. 읽기가 실패하면 그 칸의 마지막 열을 지키고 「다시 읽는 중」을 달아 2·4·8·15초 뒤 다시 읽는다. git 자식에는 도구 PATH(`reposync::tool_path`)를 준다 — Finder 로 띄운 앱은 PATH 에 `git-lfs` 가 없어 LFS 레포의 status 가 늘 실패했다(2026-10-07). Git 읽기는 선택적 index 잠금·fsmonitor·hook·외부 diff·lazy fetch를 끄고, 저장소 위치를 바꾸는 상속 Git 환경변수를 제거한다.

창·기기·폴더 전환은 표시 데이터·펼침 캐시·클릭 대상·커밋 입력을 비운다. 전환 세대와 읽기 요청 번호가 일치하는 응답만 반영하므로 이전 pane의 응답이나 같은 pane의 늦은 요청이 새 상태를 덮지 않는다. 원격 Git의 쓰기 버튼은 제공하지 않는다. 기존 로컬 변경·커밋 동작은 화면 스냅샷의 저장소 루트를 사용한다.

원본이 바뀐 순간은 `GET /term/gitcol/wait?schema=…&machine_id=…&pane=…&surface_key=…&since=<번호>&wait_ms=<≤8000>`로 받는다. 원본은 같은 신원 확인을 거친 뒤 그 pane의 작업 트리(부모 폴더 칸이면 그 아래 저장소들)에서 파일이 바뀔 때까지(`kasa_mcp::git_watch` — FSEvents·ReadDirectoryChangesW, claude·codex·셸·편집기 누가 고쳤든) 쥐고 `{schema, ok, seq, changed, live}`로 답한다. `since`가 없으면 지금 번호를 바로 준다. 원본 앱이 다시 떠 번호가 줄었으면 `changed`다. 바뀐 경로는 내보내지 않는다. 보기 기기는 감시 스레드 하나가 이 길에 매달려 `changed`면 Git 열 일꾼을 깨우고, `live`(원본이 그 작업 트리를 감시한다)면 그 칸의 지문 조회를 5초로 늦춘다. 이 길이 없는 옛 원본(404)이면 물러나 옛 1.2초 주기로 읽는다.

## 부모 폴더 칸

학생은 칸 cwd 를 여러 레포를 담은 부모(`~/…/opengateway`)에 두고 하위 레포에서 일하곤 한다. 칸 폴더가 저장소 안이 아니면 그 아래 두 단까지의 저장소(숨은·빌드 폴더 제외, 저장소 안으로는 안 내려감)를 「최근에 만진 때」 순으로 놓고 맨 위를 읽는다. 「최근」은 git 이 남기는 HEAD·reflog·인덱스 시각, 이 앱이 감시한 작업 트리 변경, 그 칸 셸 아래에서 도는 명령의 폴더(명령이 시작된 때로 센다 — 오래 도는 개발 서버가 방금 고친 다른 레포를 이기지 않게) 가운데 늦은 것이다. 에이전트 종류는 보지 않는다. 머리에는 `기기 / 칸 폴더 › 레포` 로, 메뉴에는 「자동 추적」 아래 하위 레포가 최근 순으로 뜨고 고르면 그 칸만 그 레포에 머문다. 열의 `cwd` 는 칸 폴더 그대로이고 `repo_root` 가 보이는 레포, `repos` 가 고를 거리다. 원격 칸은 원본이 같은 규칙으로 자동으로 고른다(고르기 메뉴는 로컬 칸만).

파일이 바뀌면 250ms 조용함·최대 1초로 몰아 한 번 읽되, 읽었는데 열이 그대로면(gitignore 산출물·로그) 간격을 0.8·1.6·3.2·6.4초로 늘린다. 작업 트리만 바뀐 읽기(지문 그대로)는 ref 쪽 재료(브랜치·그래프·log)를 앞 읽기에서 가져와 git 네 개만 돌린다. 지문이 그대로이고 변경도 없으면 감시하는 칸은 30초, 감시 못 하는 곳(리눅스 등)은 10초마다만 다시 읽는다. 격리 리그 실측(셸 칸, 디버그): 쉴 때 git 실행 방 8·칸 16 1.75→0.85회/초, 2초마다 편집하면 반영 20/20번 약 350ms(전에는 셸 칸 편집이 10초 주기로만 잡혔다).

브랜치 목록은 조회 전용이다. 로컬·원격과 현재 항목을 표시하며 symbolic remote HEAD는 중복 항목에서 제외한다. 분리된 HEAD는 짧은 OID, 첫 커밋 전 브랜치는 해당 상태를 표시한다. 목록은 패널 높이에 맞춰 이전·다음 페이지로 이동한다. 원격 파일 diff·커밋 상세 확장은 아직 지원하지 않는다.

검증은 `cargo test -p kasa-mcp --lib git::panel_snapshot_tests`, `cargo test -p kasa-mcp --lib git_panel::tests`, `cargo test -p kasa-mcp --lib git_watch`, `cargo test -p kasaterm --bin kasaterm git_panel --no-default-features`로 실행한다. 기존 전체 Git 테스트에는 저장소 변경 테스트가 있어 읽기 전용 검증에서는 지정한 필터만 사용한다. 실제 기기 간 원격 창과 Windows 서버의 통합 검증은 별도 대상이다.
