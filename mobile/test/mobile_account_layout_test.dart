import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:kasaterm_mobile/app_link.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/screens/connect.dart';
import 'package:kasaterm_mobile/server.dart';

Widget loginHost({Brightness brightness = Brightness.light}) => MaterialApp(
  theme: buildTheme(brightness),
  home: ConnectScreen(onConnected: (_) async {}, onLogin: (_, _, _) async {}),
);

void main() {
  testWidgets('a deep link cannot install account or legacy credentials', (
    tester,
  ) async {
    FlutterSecureStorage.setMockInitialValues({});
    await tester.pumpWidget(
      MaterialApp(
        navigatorKey: navigatorKey,
        theme: buildTheme(Brightness.light),
        home: const RootScreen(),
      ),
    );
    await tester.pumpAndSettle();
    await AppLinkObserver.instance.didPushRouteInformation(
      RouteInformation(
        uri: Uri.parse(
          'kasaterm://open?root=https%3A%2F%2Fother.invalid%2Fu%2Fsecret%2F&pane=%257&token=untrusted',
        ),
      ),
    );
    await tester.pumpAndSettle();
    expect(find.byType(ConnectScreen), findsOneWidget);
    expect(await const FlutterSecureStorage().readAll(), isEmpty);
    expect(tester.takeException(), isNull);
    await tester.pumpWidget(const SizedBox());
  });

  for (final width in [320.0, 390.0, 430.0]) {
    for (final brightness in Brightness.values) {
      testWidgets(
        'login $width $brightness fits with keyboard and exposes first input',
        (tester) async {
          tester.view.physicalSize = Size(width, 844);
          tester.view.devicePixelRatio = 1;
          addTearDown(tester.view.resetPhysicalSize);
          addTearDown(tester.view.resetDevicePixelRatio);
          addTearDown(tester.view.resetViewInsets);
          await tester.pumpWidget(loginHost(brightness: brightness));
          final input = find.byKey(const Key('account-input'));
          final first = tester.getRect(input);
          expect(first.top, lessThanOrEqualTo(300));
          expect(first.left, greaterThanOrEqualTo(0));
          expect(first.right, lessThanOrEqualTo(width));
          expect(
            tester.widget<TextField>(input).style!.fontSize,
            greaterThanOrEqualTo(16),
          );
          final button = tester.getRect(find.byKey(const Key('account-login')));
          expect(button.height, greaterThanOrEqualTo(44));
          expect(button.width, greaterThanOrEqualTo(44));
          tester.view.viewInsets = const FakeViewPadding(bottom: 336);
          await tester.tap(find.byKey(const Key('password-input')));
          await tester.pumpAndSettle();
          await tester.ensureVisible(find.byKey(const Key('account-login')));
          await tester.pumpAndSettle();
          expect(tester.takeException(), isNull);
          expect(
            tester.getRect(find.byKey(const Key('account-login'))).bottom,
            lessThanOrEqualTo(844 - 336),
          );
          await tester.tap(find.text('고급 설정'));
          await tester.pumpAndSettle();
          await tester.ensureVisible(find.text('폰 주소로 연결'));
          await tester.pumpAndSettle();
          expect(tester.takeException(), isNull);
        },
      );
    }
  }

  testWidgets(
    'login sends only explicit form submission; password remains obscured',
    (tester) async {
      final submitted = <String>[];
      await tester.pumpWidget(
        MaterialApp(
          theme: buildTheme(Brightness.light),
          home: ConnectScreen(
            onConnected: (_) async {},
            onLogin: (origin, account, password) async {
              submitted.add('$origin/$account');
              expect(password, 'fixture-password');
            },
          ),
        ),
      );
      await tester.enterText(find.byKey(const Key('account-input')), 'fixture');
      await tester.enterText(
        find.byKey(const Key('password-input')),
        'fixture-password',
      );
      expect(submitted, isEmpty);
      expect(
        tester
            .widget<TextField>(find.byKey(const Key('password-input')))
            .obscureText,
        isTrue,
      );
      await tester.tap(find.byKey(const Key('account-login')));
      await tester.pump();
      expect(submitted, ['https://kasaterm.debimarlene.com/fixture']);
    },
  );

  test('light/dark button label contrast is at least 4.5 to 1', () {
    for (final brightness in Brightness.values) {
      final scheme = buildTheme(brightness).colorScheme;
      final a = scheme.primary.computeLuminance();
      final b = scheme.onPrimary.computeLuminance();
      final ratio = ((a > b ? a : b) + .05) / ((a < b ? a : b) + .05);
      expect(ratio, greaterThanOrEqualTo(4.5));
    }
  });

  test('desktop theme tokens are applied without modifying source', () {
    final tokens = DesignTokens(
      dark: true,
      fg: 0xffdddddd,
      bg: 0xff101010,
      accent: 0xff88bbff,
      ansi: const [],
      surface: 0xff202020,
      surfaceHover: 0xff303030,
      border: 0xff444444,
      text: 0xffdddddd,
      textDim: 0xff999999,
      onAccent: 0xff101010,
      danger: 0xffff8888,
    );
    final theme = themeFromTokens(tokens);
    expect(theme.scaffoldBackgroundColor, const Color(0xff101010));
    expect(theme.colorScheme.primary, const Color(0xff88bbff));
    expect(theme.brightness, Brightness.dark);
  });
}
