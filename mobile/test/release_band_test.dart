import 'inline_result_fonts.dart';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/app_release.dart';
import 'package:kasaterm_mobile/main.dart';

void main() {
  setUpAll(loadInlineResultFonts);

  Future<ScaffoldMessengerState> host(WidgetTester tester, Brightness brightness) async {
    tester.view.physicalSize = const Size(390 * 3, 360 * 3);
    tester.view.devicePixelRatio = 3;
    addTearDown(tester.view.reset);
    final key = GlobalKey<ScaffoldMessengerState>();
    await tester.pumpWidget(
      MaterialApp(
        theme: buildTheme(brightness),
        scaffoldMessengerKey: key,
        home: Scaffold(
          appBar: AppBar(title: const Text('학생')),
          body: ListView(
            children: [for (final n in ['아리스', '유즈', '모모이']) ListTile(title: Text(n), subtitle: const Text('일하는 중'))],
          ),
        ),
      ),
    );
    return key.currentState!;
  }

  SnackBar offer(void Function(String) pressed) => releaseBand(
    title: '새 판 1.0.0 있어요',
    body: '빌드 2611180700 · 설치하는 동안 앱이 잠깐 비켜서요',
    plain: '나중에',
    primary: '설치',
    duration: const Duration(seconds: 12),
    onPressed: pressed,
  );

  for (final brightness in Brightness.values) {
    testWidgets('새 판 띠는 앱의 판으로 선다 · ${brightness.name}', (tester) async {
      final messenger = await host(tester, brightness);
      messenger.showSnackBar(offer((_) {}));
      await tester.pumpAndSettle();
      await expectLater(find.byType(Scaffold), matchesGoldenFile('goldens/release_band_${brightness.name}.png'));
    });
  }

  testWidgets('어느 단추든 띠를 action 으로 닫고 누른 이름을 넘긴다', (tester) async {
    final messenger = await host(tester, Brightness.light);
    for (final label in ['나중에', '설치']) {
      final pressed = <String>[];
      final closed = messenger.showSnackBar(offer(pressed.add)).closed;
      await tester.pumpAndSettle();
      await tester.tap(find.text(label));
      await tester.pumpAndSettle();
      expect(pressed, [label]);
      expect(await closed, SnackBarClosedReason.action);
      expect(find.text('새 판 1.0.0 있어요'), findsNothing);
    }
  });

  testWidgets('단추가 있어도 시간이 다 되면 걷힌다 — 비켜서기 띠가 남으면 앱이 영영 안 비켜선다', (tester) async {
    final messenger = await host(tester, Brightness.light);
    final closed = messenger.showSnackBar(offer((_) {})).closed;
    await tester.pumpAndSettle();
    await tester.pump(const Duration(seconds: 12));
    await tester.pumpAndSettle();
    expect(await closed, SnackBarClosedReason.timeout);
  });
}
