import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:kasaterm_mobile/main.dart' show buildTheme;

import '../test/fixtures/settings.dart';

/// 설정 첫 화면과 세 페이지(계정·연결 기기·외형)를 가상 아이폰의 진짜 화면으로 남긴다.
/// 가짜 관문(MockClient)만 쓴다 — 사용자 계정·데스크톱에 닿지 않는다. 화면은 `KASA_SHOT_DIR`.
void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  Future<void> settle(WidgetTester tester) async {
    await tester.pumpAndSettle();
    await Future<void>.delayed(const Duration(milliseconds: 300));
    await tester.pumpAndSettle();
  }

  Future<void> page(WidgetTester tester, String key, String shot) async {
    await tester.tap(find.byKey(Key(key)));
    await settle(tester);
    await binding.takeScreenshot(shot);
    await tester.pageBack();
    await settle(tester);
  }

  for (final brightness in Brightness.values) {
    final tag = brightness.name;
    testWidgets('설정 묶음 화면 · $tag', (tester) async {
      final fx = SettingsFixture();
      await tester.pumpWidget(fx.screen(theme: buildTheme(brightness)));
      await settle(tester);
      await binding.takeScreenshot('settings-top-$tag');
      await tester.drag(find.byType(Scrollable).first, const Offset(0, -600));
      await settle(tester);
      await binding.takeScreenshot('settings-top-end-$tag');
      await tester.drag(find.byType(Scrollable).first, const Offset(0, 600));
      await settle(tester);

      await page(tester, 'settings-account', 'settings-account-$tag');
      await page(tester, 'settings-devices', 'settings-devices-$tag');
      await page(tester, 'settings-look', 'settings-look-$tag');
      await tester.pumpWidget(const SizedBox());
      fx.server.close();
    });
  }
}
