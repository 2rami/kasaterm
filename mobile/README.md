# 카사모바일 — 나쵸와 얘기하고 맡긴 일을 보는 아이폰 앱 (Flutter)

카사텀 서버가 폰 웹 화면에 내주는 통로(`term/panes` · `term/ws?grid=1` · `send` · `term/shot`)에
그대로 붙는 네이티브 앱이다. 학생 목록을 보고, 한 학생의 화면을 격자로 보고, 답장을 보내고,
허락 대기가 생기면 배지·햅틱으로 알린다. 서버 규약은 `docs/webterm-handoff.md`.

## 학생 목록이 빨리 뜨고 바로 바뀌는 길

관문 너머 요청 하나가 0.7~1.4초라, 목록을 단계마다 차례로 기다리면 첫 화면까지 3~6초가 걸렸다
(2026-09-25 실측). 지금은 이렇게 간다.

- 주소 기계 목록·방 이름·배치·명부·쪽지를 **한 번에** 묻고, 온 것부터 그린다. 명부를 기억하면
  원격 기계도 같은 차례에 묻는다. 요청마다 10초 상한이 있어 느린 기계는 그 절만 직전 것으로 남는다.
- 앱이 살아 있는 동안 마지막 목록을 기억한다. 첫 화면(나쵸 창구)이 뜰 때 미리 한 바퀴 받아 두어,
  허브를 열면 곧바로 그려지고 새 목록이 닿을 때까지 위에 얇은 진행 막대가 선다.
- 기계마다 `term/changes?since=N&wait=15` 에 매달려 있다가 번호가 오르면 그 기계만 다시 읽는다.
  답의 `status:true` 는 그 서버가 **학생 상태 전이**(작업 중·기다림·쉼)에도 번호를 올린다는 뜻이고,
  그때 폴링은 도구 이름·컨텍스트 % 용으로 15초에 한 번만 한다. `status` 가 없는 옛 판은 5초 폴링,
  길 자체가 없으면(404) 폴링만 한다. 끊기면 2→30초로 물러서며 다시 붙는다.
- 읽는 중에 또 부르면 겹쳐 쏘지 않고 끝난 뒤 한 번만 더 읽는다. 앱이 쉬면 멈추고 돌아오면 바로 다시 읽는다.

## 사진 첨부와 화면 밝기

학생 화면 아래 사진 버튼에서 사진 한 장을 고르면 현재 학생의 입력창에 첨부된다.
선택한 사진만 읽으며, 첨부만으로 메시지를 보내지는 않는다. 보내기 또는 Enter로 전달한다.
사진은 실제 픽셀을 PNG로 변환해 해당 학생이 실행 중인 기기로 보내므로 미러링된 Claude와
Codex에도 같은 방식으로 붙는다. 변환 뒤 32 MiB를 넘는 사진은 전송하지 않는다.
사진 선택을 취소하거나 선택 중 다른 학생 화면으로 이동하면 전송하지 않는다.

설정에서 「밝게」·「어둡게」를 고르면 원본 기기의 밝기와 무관하게 폰의 터미널에도 적용된다.
「데스크톱 따라감」을 고른 경우에만 원본의 화면 색을 따른다. 학생색과 코드·변경점의 색은 유지한다.

## 구조

```
lib/
  main.dart            테마(SCHALE 흰/연하늘 표면·네이비 잉크) · 첫 화면 분기
  server.dart          Server(root) — uri/wsUri/me/panes/sessions/machines/shot/send · describe() 는 slug 를 가린다
  address_store.dart   주소(slug 포함) → Keychain(flutter_secure_storage)
  nacho.dart           나쵸 창구 — 대화 원장 이어 받기(순번) · 같은 id 재전송 · 작업 장부 읽기
  nacho_reply.dart     나쵸 답 가르기 — 실행·진단 줄과 사용량 꼬리를 접힌 상세로(원문 보존) · 학생 링크 읽기
  nacho_student.dart   장부의 맡은 학생(surface·host·machine_id) → 실제 pane. 기계를 못 정하면 짐작 안 함
  hub_model.dart       기계→방→학생 트리 · 기계마다 따로 받아 온 것부터 · `term/changes` 롱폴 · 대기 전이 배지+햅틱
  grid.dart            순수 Dart 격자 모델 — dirty 행 교체 · 글자 폭 표 · 256 팔레트
  term_session.dart    WS 수명(백오프·gone·pause/resume) · 키 바이트 · 답장 · 그림 폴링
  grid_canvas.dart     CustomPainter 렌더러(행 캐시) + InteractiveViewer 폭 맞춤·핀치
  conversation.dart    학생 대화 모델 — transcript-raw(claude)·rollout(codex) 줄 → 말풍선·도구 묶음 · 화면의 선택 메뉴 읽기
  screens/             nacho_home(첫 화면: 대화·작업) · nacho_task · connect · hub · terminal(「터미널|대화」 전환) · conversation_view · settings
tool/devproxy.dart     크롬 개발용 같은 출처 역프록시
test/                  유닛 · 골든(goldens/) · live/(실서버, KASA_ROOT 있을 때만)
```

## 돌리기

```bash
flutter pub get
flutter analyze
NO_PROXY='127.0.0.1,localhost' flutter test            # 유닛 + 골든
KASA_ROOT=http://127.0.0.1:8765/ NO_PROXY='127.0.0.1,localhost' flutter test test/live/
```

`flutter test` 는 `flutter_tester` 와 로컬 소켓으로 붙는데, 셸에 프록시 변수가 있으면 그 연결이
프록시로 새어 "Invalid WebSocket upgrade request" 로 죽는다 — `NO_PROXY` 가 그걸 막는다.

크롬에서 보기 — 서버는 Origin 이 Host 와 같아야 소켓을 열어 주고 CORS 헤더도 없으므로, 앱과
서버를 한 주소로 내주는 역프록시가 필요하다:

```bash
flutter build web
python3 -m http.server 5555 --directory build/web &
dart run tool/devproxy.dart --listen 8877 --upstream http://127.0.0.1:8765/ --web http://127.0.0.1:5555/ &
# 크롬에서 http://127.0.0.1:8877/ 를 열고 연결 화면에 같은 주소를 넣는다
```

공용 주소(`/u/<slug>/`)를 상대로 볼 때는 `--upstream` 대신 `KASA_UPSTREAM` 환경변수로 준다 —
argv 는 셸 히스토리와 `ps` 에 남는다.

## 실기(아이폰)

Xcode 가 있어야 한다. 처음 한 번:

```bash
sudo xcode-select -s /Applications/Xcode.app
sudo xcodebuild -runFirstLaunch
brew install cocoapods
flutter precache --ios
```

그 다음은 한 줄이다. 아이폰을 케이블로 붙이고(폰의 개발자 모드가 켜져 있어야 한다):

```bash
tool/phone.sh
```

이 맥의 카사텀이 받은 폰 주소(`GET /mobile/users` 의 주인 항목)를 앱 안에 구워 넣은
릴리스판을 만들어 폰에 설치하고 켠다. 앱은 주소를 처음부터 알고 있어 **폰에서 아무것도
입력하지 않는다** — 연결 화면은 남의 기계에 붙을 때만 나온다. 주소는 `--dart-define-from-file`
로 넘긴다(argv 에 자격이 안 남는다). Xcode 화면은 열지 않는다: 팀 ID 를 Xcode 계정 설정에서
읽어 환경변수로 주면 flutter 가 인증서 검사를 건너뛰고 xcodebuild 에 `-allowProvisioningUpdates`
를 붙여 인증서·프로필을 스스로 만든다. 팀 ID 는 사람마다 달라 프로젝트 파일에 적지 않는다.

- 첫 설치 뒤 폰의 설정 → 일반 → VPN 및 기기 관리에서 개발자 앱을 한 번 「신뢰」해야 켜진다.
- 무료 Apple ID(Personal Team)는 7일마다 `tool/phone.sh` 를 다시 돌린다.
- 개발자 모드가 꺼져 있으면 xcodebuild 가 "Developer Mode disabled" 로 선다.
- 번들 id 는 `com.debimarlene.kasatermMobile`. 로컬 서버(`http://127.0.0.1:8765/` · LAN)에
  붙이려면 `Info.plist` 의 `NSAppTransportSecurity` 에 `NSAllowsLocalNetworking` 이 켜져 있어야 한다.

## 웹에서 앱으로

폰의 사파리로 들어온 웹 허브·학생 화면(슬랙 알림 링크 포함)은 `kasaterm://open?root=…&machine=…&pane=…`
로 앱을 부른다. 처음엔 「앱」 단추, 한 번 성공한 뒤부터는 저절로 넘어간다(앱 없는 폰에 대고 무작정
가면 사파리가 경고를 띄운다). 앱은 `root` 를 **주소가 하나도 없을 때만** 받는다 — 링크 한 줄이
저장된 자격을 갈아치우면 안 된다. 애플의 유니버설 링크는 유료 계정이 필요해 쓰지 않는다.

## 지킬 것

- 키는 `Uint8List` 로만 소켓에 넣는다 — text 프레임은 서버가 제어 JSON 으로 읽고 조용히 버린다.
- 미러 세션에는 `resize` 를 보내지 않는다(원본 기계 화면이 바뀐다). 멈추는 이유는 `gone` 뿐.
- slug 는 자격이다. `Uri.toString()`·예외 문구·argv 에 싣지 말고, 표시는 `Server.describe()` 로만.
- `lib/` 에서 `dart:io` 를 import 하지 않는다 — 크롬 개발 루프가 통째로 깨진다.
- 글꼴은 D2CodingLigatureNerdFontMono(OFL, `assets/fonts/OFL-D2Coding.txt`). 한글이 정확히 두 칸이라
  격자가 어긋나지 않는다.
