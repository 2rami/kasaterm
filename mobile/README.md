# kasaterm mobile — 아이폰 원격 조종 앱 (Flutter)

카사텀 서버가 폰 웹 화면에 내주는 통로(`term/panes` · `term/ws?grid=1` · `send` · `term/shot`)에
그대로 붙는 네이티브 앱이다. 학생 목록을 보고, 한 학생의 화면을 격자로 보고, 답장을 보내고,
허락 대기가 생기면 배지·햅틱으로 알린다. 서버 규약은 `docs/webterm-handoff.md`.

## 구조

```
lib/
  main.dart            테마(SCHALE 흰/연하늘 표면·네이비 잉크) · 첫 화면 분기
  server.dart          Server(root) — uri/wsUri/me/panes/sessions/machines/shot/send · describe() 는 slug 를 가린다
  address_store.dart   주소(slug 포함) → Keychain(flutter_secure_storage)
  hub_model.dart       기계→방→학생 트리 · 5초 폴링 · 대기→작업중→쉼 정렬 · 대기 전이 배지+햅틱
  grid.dart            순수 Dart 격자 모델 — dirty 행 교체 · 글자 폭 표 · 256 팔레트
  term_session.dart    WS 수명(백오프·gone·pause/resume) · 키 바이트 · 답장 · 그림 폴링
  grid_canvas.dart     CustomPainter 렌더러(행 캐시) + InteractiveViewer 폭 맞춤·핀치
  screens/             connect · hub · terminal · settings
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

## 지킬 것

- 키는 `Uint8List` 로만 소켓에 넣는다 — text 프레임은 서버가 제어 JSON 으로 읽고 조용히 버린다.
- 미러 세션에는 `resize` 를 보내지 않는다(원본 기계 화면이 바뀐다). 멈추는 이유는 `gone` 뿐.
- slug 는 자격이다. `Uri.toString()`·예외 문구·argv 에 싣지 말고, 표시는 `Server.describe()` 로만.
- `lib/` 에서 `dart:io` 를 import 하지 않는다 — 크롬 개발 루프가 통째로 깨진다.
- 글꼴은 D2CodingLigatureNerdFontMono(OFL, `assets/fonts/OFL-D2Coding.txt`). 한글이 정확히 두 칸이라
  격자가 어긋나지 않는다.
