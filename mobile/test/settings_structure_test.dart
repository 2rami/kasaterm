import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/screens/settings.dart';
import 'package:kasaterm_mobile/server.dart';

import 'fixtures/settings.dart';

/// 폰 설정이 데스크톱 설정 옆 목록과 같은 묶음·이름인지(docs/design.md 「설정 화면」·「카사모바일 폰」 설정).
void main() {
  Future<void> phone(WidgetTester tester) async {
    tester.view.physicalSize = const Size(390 * 3, 1200 * 3);
    tester.view.devicePixelRatio = 3;
    addTearDown(tester.view.reset);
  }

  Finder inGroup(String group, Finder row) => find.descendant(of: find.byKey(Key(group)), matching: row);

  testWidgets('첫 화면은 화면·에이전트·앱 묶음 차례에 PC 와 같은 페이지 이름', (tester) async {
    await phone(tester);
    final fx = SettingsFixture();
    addTearDown(fx.server.close);
    await tester.pumpWidget(fx.screen());
    await tester.pumpAndSettle();

    final titles = ['화면', '에이전트', '앱'];
    final ys = [for (final t in titles) tester.getTopLeft(find.text(t)).dy];
    expect(ys, orderedEquals([...ys]..sort()), reason: '묶음 차례는 화면 → 에이전트 → 앱');
    expect(find.text('데스크톱'), findsNothing, reason: '옛 「데스크톱」 묶음은 걷었다');

    for (final (group, rows) in [
      ('group-screen', ['settings-look', 'settings-weather']),
      ('group-agent', ['settings-account', 'claude-accounts', 'character-picks']),
      ('group-app', ['settings-devices', 'app-release']),
    ]) {
      final rowYs = [
        for (final key in rows) tester.getTopLeft(inGroup(group, find.byKey(Key(key)))).dy,
      ];
      expect(rowYs, orderedEquals([...rowYs]..sort()), reason: '$group 안의 차례');
    }
    expect(inGroup('group-agent', find.text('계정')), findsOneWidget);
    expect(inGroup('group-agent', find.text('코딩 에이전트')), findsOneWidget);
    expect(inGroup('group-agent', find.text('캐릭터')), findsOneWidget);
    expect(inGroup('group-app', find.text('연결 기기')), findsOneWidget);
    expect(inGroup('group-app', find.byType(Divider)), findsOneWidget, reason: '설치 기록이 없으면 끝에 빈 줄 선을 남기지 않는다');
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
}
