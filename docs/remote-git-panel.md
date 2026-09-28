# 원격 창의 Git 패널

오른쪽 Git 열은 활성 pane 안의 실제 선택 탭을 따라간다. 로컬 경로 고정은 로컬 pane에만 적용한다. 원격 pane은 연결의 원본 pane ID·surface key와 기기 명부의 machine ID로 식별하고 원본 기기에서 Git 정보를 읽는다.

`GET /term/gitcol?schema=kasa.git-panel.v2&machine_id=…&pane=…&surface_key=…&commits=…`는 기존 인증·명부 경로를 사용한다. 원본 기기는 요청한 기기·surface key가 현재 원본과 일치하는지 확인하고 그 pane의 cwd를 조회한다. Git을 읽은 뒤 신원·cwd를 다시 확인한다. 보기 창의 로컬 cwd나 오래된 board cwd는 조회할 저장소로 쓰지 않는다. Windows의 보고 cwd도 동일한 원본 조회를 거친다.

응답은 `schema`, `ok`, `source`(machine_id, pane, surface_key, cwd), `view`로 구성한다. 클라이언트는 스키마·원본 식별자·절대경로·스냅샷 cwd를 검증한다. 구버전 응답은 업데이트 안내이며, 연결 실패·원본 변경·저장소 아님과 구분한다. 요청은 HTTP 10초/2MiB, Git 실행 전체 5초와 명령별 출력 상한을 가진다. Git 읽기는 선택적 index 잠금·fsmonitor·hook·외부 diff·lazy fetch를 끄고, 저장소 위치를 바꾸는 상속 Git 환경변수를 제거한다.

창·기기·폴더 전환은 표시 데이터·펼침 캐시·클릭 대상·커밋 입력을 비운다. 전환 세대와 읽기 요청 번호가 일치하는 응답만 반영하므로 이전 pane의 응답이나 같은 pane의 늦은 요청이 새 상태를 덮지 않는다. 원격 Git의 쓰기 버튼은 제공하지 않는다. 기존 로컬 변경·커밋 동작은 화면 스냅샷의 저장소 루트를 사용한다.

브랜치 목록은 조회 전용이다. 로컬·원격과 현재 항목을 표시하며 symbolic remote HEAD는 중복 항목에서 제외한다. 분리된 HEAD는 짧은 OID, 첫 커밋 전 브랜치는 해당 상태를 표시한다. 목록은 패널 높이에 맞춰 이전·다음 페이지로 이동한다. 원격 파일 diff·커밋 상세 확장은 아직 지원하지 않는다.

검증은 `cargo test -p kasa-mcp --lib git::panel_snapshot_tests`, `cargo test -p kasa-mcp --lib git_panel::tests`, `cargo test -p kasaterm --bin kasaterm git_panel --no-default-features`로 실행한다. 기존 전체 Git 테스트에는 저장소 변경 테스트가 있어 읽기 전용 검증에서는 지정한 필터만 사용한다. 실제 기기 간 원격 창과 Windows 서버의 통합 검증은 별도 대상이다.
