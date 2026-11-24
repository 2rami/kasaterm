import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/screens/settings.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/wide_layout.dart';

import 'fixtures/settings.dart';

/// 폰 설정이 데스크톱 설정 옆 목록과 같은 묶음·이름인지(docs/design.md 「설정 화면」·「카사모바일 폰」 설정).
void main() {
  Future<void> phone(WidgetTester tester) async {
    tester.view.physicalSize = const Size(390 * 3, 1200 * 3);
    tester.view.devicePixelRatio = 3;
    addTearDown(tester.view.reset);
  }

  Finder inGroup(String group, Finder row) => find.descendant(of: find.byKey(Key(group)), matching: row);

  testWidgets('첫 화면은 데스크톱과 같은 주제별 묶음과 페이지 이름', (tester) async {
    await phone(tester);
    final fx = SettingsFixture();
    addTearDown(fx.server.close);
    await tester.pumpWidget(fx.screen());
    await tester.pumpAndSettle();

    final titles = ['화면', '에이전트', '계정·기기', '앱'];
    final ys = [for (final t in titles) tester.getTopLeft(find.text(t)).dy];
    expect(ys, orderedEquals([...ys]..sort()), reason: '묶음 차례는 화면 → 에이전트 → 계정·기기 → 앱');
    expect(find.text('데스크톱'), findsNothing, reason: '옛 「데스크톱」 묶음은 걷었다');

    for (final (group, rows) in [
      ('group-screen', ['settings-look']),
      ('group-agent', ['claude-accounts', 'character-picks']),
      ('group-connection', ['settings-account', 'settings-devices']),
      ('group-app', ['app-release']),
    ]) {
      final rowYs = [
        for (final key in rows) tester.getTopLeft(inGroup(group, find.byKey(Key(key)))).dy,
      ];
      expect(rowYs, orderedEquals([...rowYs]..sort()), reason: '$group 안의 차례');
    }
    expect(inGroup('group-connection', find.text('계정')), findsOneWidget);
    expect(inGroup('group-agent', find.text('코딩 에이전트')), findsOneWidget);
    expect(inGroup('group-agent', find.text('캐릭터')), findsOneWidget);
    expect(inGroup('group-connection', find.text('연결 기기')), findsOneWidget);
    expect(inGroup('group-app', find.byType(Divider)), findsNothing, reason: '설치 기록이 없으면 끝에 빈 줄 선을 남기지 않는다');
    expect(find.text('Google·아이디 로그인 · 일 권한'), findsOneWidget, reason: '계정 줄은 이어진 로그인 방법을 요약');

    // 페이지 안의 줄은 첫 화면에 없다.
    for (final key in ['change-login', 'work-permissions', 'approval-key', 'device-names', 'phone-brightness']) {
      expect(find.byKey(Key(key)), findsNothing, reason: key);
    }
  });

  testWidgets('계정 페이지는 로그인 방법과 일 권한, 비밀 승인은 연결 기기 페이지', (tester) async {
    await phone(tester);
    final fx = SettingsFixture();
    addTearDown(fx.server.close);
    await tester.pumpWidget(fx.screen());
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('settings-account')));
    await tester.pumpAndSettle();
    expect(find.widgetWithText(AppBar, '계정'), findsOneWidget);
    for (final key in ['link-google', 'link-github', 'change-login', 'change-password', 'work-permissions']) {
      expect(find.byKey(Key(key)), findsOneWidget, reason: key);
    }
    expect(find.byKey(const Key('linked-google')), findsOneWidget);
    expect(find.byKey(const Key('linked-github')), findsNothing);
    expect(
      tester.getTopLeft(find.text('로그인 방법')).dy,
      lessThan(tester.getTopLeft(find.text('일 권한')).dy),
      reason: '일 권한은 로그인 방법 아래',
    );
    expect(find.byKey(const Key('approval-key')), findsNothing, reason: 'Face ID 로 허락할 폰은 연결 기기');
    await tester.pageBack();
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('settings-devices')));
    await tester.pumpAndSettle();
    expect(find.widgetWithText(AppBar, '연결 기기'), findsOneWidget);
    final groups = ['기기', '이 폰', '브라우저'];
    final ys = [for (final t in groups) tester.getTopLeft(find.text(t)).dy];
    expect(ys, orderedEquals([...ys]..sort()), reason: '데스크톱 연결 기기처럼 기기 → 폰 → 브라우저');
    for (final key in ['device-names', 'desktop-tools', 'approval-key', 'browser-device', 'dev-server']) {
      expect(find.byKey(Key(key)), findsOneWidget, reason: key);
    }
    await tester.pageBack();
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('settings-look')));
    await tester.pumpAndSettle();
    expect(find.widgetWithText(AppBar, '외형'), findsOneWidget);
    expect(find.byKey(const Key('phone-brightness')), findsOneWidget);
    expect(find.text('강조색'), findsOneWidget);
    expect(find.text('둥글게 · 데스크톱 모양'), findsOneWidget);

    final preview = find.byKey(const Key('appearance-preview'));
    final before = tester.getRect(preview);
    final controls = find.descendant(
      of: find.byKey(const PageStorageKey('appearance-controls')),
      matching: find.byType(Scrollable),
    );
    await tester.scrollUntilVisible(find.text('날씨 켜기'), 160, scrollable: controls);
    expect(find.text('날씨 켜기').hitTestable(), findsOneWidget);
    expect(find.widgetWithText(AppBar, '외형'), findsOneWidget);
    expect(tester.getRect(preview), before);
    expect(find.text('색과 모양'), findsNothing);
  });

  testWidgets('계정 없이 붙은 주소 연결은 계정·캐릭터·기기 이름·비밀 승인을 감춘다', (tester) async {
    await phone(tester);
    final server = Server(Uri.parse('http://127.0.0.1:9/'));
    addTearDown(server.close);
    await tester.pumpWidget(MaterialApp(home: SettingsScreen(server: server, onChangeAddress: () async {})));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('settings-account')), findsNothing);
    expect(find.byKey(const Key('character-picks')), findsNothing);
    expect(find.byKey(const Key('claude-accounts')), findsOneWidget);
    expect(find.text('주소 바꾸기 · 지우기'), findsOneWidget);

    await tester.tap(find.byKey(const Key('settings-devices')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('device-names')), findsNothing);
    expect(find.byKey(const Key('approval-key')), findsNothing);
    expect(find.text('이 폰'), findsNothing);
    expect(find.byKey(const Key('desktop-tools')), findsOneWidget);
    expect(find.byKey(const Key('browser-device')), findsOneWidget);
  });

  Future<void> pad(WidgetTester tester, Size size) async {
    tester.view.physicalSize = size * 2;
    tester.view.devicePixelRatio = 2;
    addTearDown(tester.view.reset);
  }

  Finder chevronIn(String key) =>
      find.descendant(of: find.byKey(Key(key)), matching: find.byIcon(Icons.chevron_right_rounded));

  testWidgets('넓으면 왼쪽 목록 + 오른쪽 페이지 두 칸, 하위 화면도 오른쪽 칸 안에서', (tester) async {
    await pad(tester, const Size(1376, 1032)); // 아이패드 프로 13 가로 — 오른쪽 칸이 내용 열 상한보다 넓다
    final fx = SettingsFixture();
    addTearDown(fx.server.close);
    await tester.pumpWidget(fx.screen());
    await tester.pumpAndSettle();

    expect(tester.getRect(find.byKey(const Key('group-screen'))).right, lessThanOrEqualTo(settingsNavWidth));
    final brightness = find.byKey(const Key('phone-brightness'));
    expect(brightness, findsOneWidget, reason: '처음은 목록 맨 위 「외형」이 오른쪽에 열려 있다');
    final page = tester.getRect(brightness);
    expect(page.left, greaterThan(settingsNavWidth));
    expect(page.width, settingsPageMaxWidth, reason: '내용 열은 데스크톱 CONTENT_MAX_W 상한까지');
    final pane = (settingsNavWidth + 1 + 1376) / 2;
    expect(page.center.dx, closeTo(pane, 1), reason: '상한 폭 열은 오른쪽 칸 가운데');
    expect(chevronIn('settings-look'), findsNothing, reason: '두 칸이면 › 대신 연 페이지 강조');
    bool selected(String key) => find
        .descendant(
          of: find.byKey(Key(key)),
          matching: find.byWidgetPredicate((w) => w is Semantics && (w.properties.selected ?? false)),
        )
        .evaluate()
        .isNotEmpty;
    expect(selected('settings-look'), isTrue, reason: '연 페이지 줄은 선택 상태로 읽힌다');
    expect(selected('settings-account'), isFalse);

    await tester.tap(find.byKey(const Key('settings-account')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('change-login')), findsOneWidget);
    expect(brightness, findsNothing);
    expect(find.byKey(const Key('group-screen')), findsOneWidget, reason: '목록은 그대로 남는다');

    await tester.tap(find.byKey(const Key('work-permissions')));
    await tester.pumpAndSettle();
    final work = find.widgetWithText(AppBar, '일 권한');
    expect(work, findsOneWidget);
    expect(tester.getTopLeft(work).dx, greaterThan(settingsNavWidth), reason: '하위 화면도 오른쪽 칸 안');
    expect(find.byKey(const Key('group-screen')), findsOneWidget);

    await tester.tap(find.byKey(const Key('settings-look')));
    await tester.pumpAndSettle();
    final preview = find.byKey(const Key('appearance-preview'));
    expect(tester.getTopLeft(preview).dx, greaterThan(settingsNavWidth));
    final controls = find.descendant(
      of: find.byKey(const PageStorageKey('appearance-controls')),
      matching: find.byType(Scrollable),
    );
    await tester.scrollUntilVisible(find.text('날씨 켜기'), 160, scrollable: controls);
    expect(find.text('날씨 켜기').hitTestable(), findsOneWidget);
    expect(selected('settings-look'), isTrue);
    expect(find.byType(BottomSheet), findsNothing);
    expect(find.widgetWithText(AppBar, '일 권한'), findsNothing);
  });

  testWidgets('좁은 스플릿 뷰는 폰 판 그대로 — 줄을 누르면 들어간다', (tester) async {
    await pad(tester, Size(settingsNavWidth + roomColumnMinWidth - 1, 1376));
    final fx = SettingsFixture();
    addTearDown(fx.server.close);
    await tester.pumpWidget(fx.screen());
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('phone-brightness')), findsNothing);
    expect(chevronIn('settings-account'), findsOneWidget);
    await tester.tap(find.byKey(const Key('settings-account')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('change-login')), findsOneWidget);
    expect(find.byKey(const Key('group-screen')), findsNothing, reason: '페이지가 첫 화면을 덮는다');
  });
}
