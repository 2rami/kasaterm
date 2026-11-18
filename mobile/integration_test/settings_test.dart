import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:kasaterm_mobile/main.dart' show buildTheme;
import 'package:kasaterm_mobile/wide_layout.dart';

import '../test/fixtures/settings.dart';

/// 설정 첫 화면과 페이지(계정·연결 기기·외형, 두 칸이면 날씨와 하위 화면까지)를 가상 기기의 진짜 화면으로 남긴다.
/// 폰이면 줄을 눌러 들어갔다 나오고, 아이패드처럼 넓으면 왼쪽 목록을 눌러 오른쪽 칸을 바꾼다.
/// 가짜 관문(MockClient)만 쓴다 — 사용자 계정·데스크톱에 닿지 않는다. 화면은 `KASA_SHOT_DIR`.
void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  Future<void> settle(WidgetTester tester) async {
    await tester.pumpAndSettle();
    await Future<void>.delayed(const Duration(milliseconds: 300));
    await tester.pumpAndSettle();
  }

  for (final brightness in Brightness.values) {
    final tag = brightness.name;
    testWidgets('설정 묶음 화면 · $tag', (tester) async {
      final fx = SettingsFixture();
      await tester.pumpWidget(fx.screen(theme: buildTheme(brightness)));
      await settle(tester);
      final split = settingsSplit(tester.view.physicalSize.width / tester.view.devicePixelRatio);
      final kind = split ? 'wide' : 'phone';

      Future<void> page(String key, String name) async {
        await tester.tap(find.byKey(Key(key)));
        await settle(tester);
        await binding.takeScreenshot('settings-$kind-$name-$tag');
        if (!split) {
          await tester.pageBack();
          await settle(tester);
        }
      }

      await binding.takeScreenshot('settings-$kind-top-$tag');
      if (!split) {
        await tester.drag(find.byType(Scrollable).first, const Offset(0, -600));
        await settle(tester);
        await binding.takeScreenshot('settings-$kind-top-end-$tag');
        await tester.drag(find.byType(Scrollable).first, const Offset(0, 600));
        await settle(tester);
      }
      await page('settings-account', 'account');
      if (split) {
        await tester.tap(find.byKey(const Key('work-permissions')));
        await settle(tester);
        await binding.takeScreenshot('settings-$kind-work-$tag');
        await page('settings-weather', 'weather');
      }
      await page('settings-devices', 'devices');
      await page('settings-look', 'look');
      await tester.pumpWidget(const SizedBox());
      fx.server.close();
    });
  }
}
