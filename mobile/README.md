# KASA Mobile — 원격 작업을 확인하는 Flutter 앱

카사텀 서버가 폰 웹 화면에 내주는 통로(`term/panes` · `term/ws?grid=1` · `send` · `term/shot`)에
그대로 붙는 네이티브 앱이다. 학생 목록을 보고, 한 학생의 화면을 격자로 보고, 답장을 보내고,
허락 대기가 생기면 배지·햅틱으로 알린다. 서버 규약은 `docs/webterm-handoff.md`.

## 계정 로그인과 연결 대기

첫 화면에서 데스크톱과 같은 아이디·비밀번호로 로그인한다. 계정 서버 기본값은
`https://kasaterm.debimarlene.com`이며 「고급 설정」에서 바꿀 수 있다. 기존 `/u/<slug>/`
폰 주소 연결도 고급 설정에 남아 있다. 로그인 성공과 데스크톱 연결은 별도 상태다.
온라인 기기가 없거나 구형 데스크톱이면 로그인은 유지하고 연결 대기·업데이트 안내를 보인다.

- 비밀번호는 저장하지 않는다. origin·account·device_id·device token은 Keychain의 단일 항목에 저장한다.
- HTTP Bearer와 WebSocket subprotocol 인증은 저장된 origin에서만 쓰고 redirect는 따르지 않는다.
  토큰은 URL·쿼리·오류 메시지에 넣지 않는다. 앱 링크는 현재 세션의 화면만 열며 자격을 바꾸지 않는다.
- 복원 시 whoami를 검증한다. 401이면 재로그인하며, 로그아웃·계정 전환은 이전 HTTP/WS와 캐시·데스크톱 색을 폐기한다.
- 「밝게/어둡게/데스크톱 따라감」은 계정별로 보관하며 `/relay/account-sync`의 `mobile_theme_mode`로 동기화한다.
  revision 충돌 시 최신 revision에 그 키만 다시 적용한다. 저장 실패는 로컬 적용과 계정 저장을 구분해 알린다.
- 계정 연결의 APNs 등록은 기기별 서버 lease가 마련될 때까지 보류한다. 기존 폰 주소의 푸시는 유지한다.
  전환 시 native 등록을 중지하고 이전 등록 해제를 3초 내 시도한다. 실패는 세션 중 기억하고 재연결·복귀 때 재시도한다.
  앱 종료 뒤의 원격 해제 재시도와 이미 전달된 OS 알림 철회까지 보장하지 않는다.

UI는 Pretendard 400/600(OFL)을 사용하고 터미널은 TermMono/TermHangul/TermSymbol을 유지한다.
기본 로고는 자체 우산 이미지이며 이전 로고·교실 배경은 표시·번들 목록에서 제외했다.
모바일 번들에는 자체 Sky/Amber 정지 그림과 쌍둥이 그림만 포함한다. 과거 캐릭터 그림 폴더는
소스 이력에 남기되 앱 에셋 목록에서는 제외한다. 사용자 프사는 기존 인증된 서버 경로로 읽으며,
없는 그림은 중립 아이콘으로 표시한다. 새 애니메이션을 제공하는 것은 아니다.

iOS 빌드 전에 `tool/kasanet.sh` 가 카사넷 정적 라이브러리(`ios/KasaNet/KasaNet.xcframework`, 커밋하지 않음)를 굽는다 —
`sim.sh`·`phone.sh`·`adhoc.sh` 는 먼저 부른다. 손으로 `flutter build ios` 할 때도 먼저 부른다.

폰·아이패드 배포는 설치 링크 `tool/adhoc.sh`([절차](../docs/ios-adhoc-install.md))다. `testflight.sh` 는 2026-10-01부터 쓰지 않는다.

시뮬레이터 검증은 `flutter build ios --simulator --debug`를 사용한다. `--no-codesign`은
Keychain 접근이 거부될 수 있어 로그인 저장소 검증용으로 쓰지 않는다.
`mobile_terminal_qa_test.dart`의 320/390/430px 위젯 검사는 합성 Codex/Claude fixture이며 실계정 메시지를 보내지 않는다.
물리 iPhone의 OS 한글 키보드·푸시 수신은 별도 실기 검증이 필요하다.

## 학생 목록이 빨리 뜨고 바로 바뀌는 길

관문 너머 요청 하나가 0.7~1.4초라, 목록을 단계마다 차례로 기다리면 첫 화면까지 3~6초가 걸렸다
(2026-09-25 실측). 지금은 이렇게 간다.

- 주소 기계 목록·방 이름·배치·명부·쪽지를 **한 번에** 묻고, 온 것부터 그린다. 명부를 기억하면
  원격 기계도 같은 차례에 묻는다. 요청마다 10초 상한이 있어 느린 기계는 그 절만 직전 것으로 남는다.
- 앱이 살아 있는 동안 마지막 목록을 기억한다. 허브를 다시 열면 곧바로 그려지고 새 목록이 닿을
  때까지 위에 얇은 진행 막대가 선다.
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
  main.dart            Pretendard UI 테마(시안 A 「한 얼굴」 — 플랫·테만·모서리 6) · 첫 화면 분기
  look.dart            단추·배치 값 한 곳(docs/design.md 「카사모바일 폰 — 단추·배치」)
  server.dart          Server(root) — uri/wsUri/me/panes/sessions/machines/shot/send · describe() 는 slug 를 가린다
  connection.dart     로그인·복원·기기 연결 대기·로그아웃 상태
  connection_store.dart 계정/기존 주소 → 단일 Keychain 항목
  relay_account.dart  origin 고정 계정 API · 인증 전송 · 테마 revision CAS
  hub_model.dart       기계→방→학생 트리 · 기계마다 따로 받아 온 것부터 · `term/changes` 롱폴 · 대기 전이 배지+햅틱
  grid.dart            순수 Dart 격자 모델 — dirty 행 교체 · 글자 폭 표 · 256 팔레트
  term_session.dart    WS 수명(백오프·gone·pause/resume) · 키 바이트 · 답장 · 그림 폴링
  grid_canvas.dart     CustomPainter 렌더러(행 캐시) + InteractiveViewer 폭 맞춤·핀치
  wide_layout.dart     아이패드·가로 화면 — 방 상자 여러 열(벽돌 쌓기). 값은 docs/design.md 「태블릿」
  hardware_keys.dart   하드웨어 키보드 → pane 바이트(Esc·⌘.·Ctrl·방향·Shift+Tab). 글자는 입력칸이 받는다
  kasanet.dart         데스크톱 직통(카사넷) 길 고르기 — 직통이면 앱 안 입구, 아니면 관문. 등록은 관문으로만(docs/kasanet.md P5)
  kasanet_native*.dart 앱에 링크된 kasa-net-ffi(ios/KasaNet) 바인딩. 웹·시험은 없음(늘 관문)
  net_tcp*.dart        데스크톱 개발 서버를 폰 localhost 로 끌어오는 다리(/net/tcp 웹소켓)
  conversation.dart    학생 대화 모델 — transcript-raw(claude)·rollout(codex) 줄 → 말풍선·도구 묶음 · 화면의 선택 메뉴 읽기
  screens/             connect · hub(첫 화면) · share_screen · terminal(「터미널|대화」 전환) · conversation_view · settings · dev_server(앱 안 Safari·크롬 커스텀 탭)
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
- 번들 id 는 `com.debimarlene.kasaterm`. 로컬 서버(`http://127.0.0.1:8765/` · LAN)에
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
