import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:kasaterm_mobile/hub_model.dart';
import 'package:kasaterm_mobile/main.dart' show buildTheme;
import 'package:kasaterm_mobile/screens/hub.dart';
import 'package:kasaterm_mobile/server.dart';

import '../test/fixtures/hub_interactions.dart';

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(() => Future<void>.delayed(const Duration(seconds: 1)));
  testWidgets(
    'native minimap drag actions, multi selection, undo and active subagents',
    (tester) async {
      HubModel.clearCache();
      final fixture = HubFixture();
      final alpha = fixture.pane(0), beta = fixture.pane(1);
      Finder tile(Pane pane) =>
          find.byKey(ValueKey('hub-drag-${pane.viewKey}'));
      Future<void> settle() async {
        // Pending confirmations intentionally retain the busy indicator behind the dialog.
        for (var i = 0; i < 6; i++) {
          await tester.pump(const Duration(milliseconds: 100));
        }
        await Future<void>.delayed(const Duration(milliseconds: 500));
      }

      Future<void> until(Finder finder) async {
        final end = DateTime.now().add(const Duration(seconds: 15));
        while (finder.evaluate().isEmpty) {
          if (DateTime.now().isAfter(end)) fail('native minimap timeout');
          await tester.pump(const Duration(milliseconds: 100));
          await Future<void>.delayed(const Duration(milliseconds: 100));
        }
        await settle();
      }

      Future<void> drag(Pane pane, String action, {String? shot}) async {
        final finger = await tester.startGesture(
          tester.getCenter(tile(pane).first),
        );
        await tester.pump(const Duration(milliseconds: 650));
        final target = find.byKey(ValueKey('hub-drop-$action'));
        await until(target);
        await finger.moveTo(tester.getCenter(target));
        await tester.pump(const Duration(milliseconds: 250));
        if (shot != null) {
          await Future<void>.delayed(const Duration(milliseconds: 500));
          await binding.takeScreenshot(shot);
        }
        await finger.up();
        await settle();
      }

      try {
        await tester.pumpWidget(
          MaterialApp(
            debugShowCheckedModeBanner: false,
            theme: buildTheme(Brightness.light),
            builder: (context, child) => MediaQuery(
              data: MediaQuery.of(context).copyWith(disableAnimations: true),
              child: child!,
            ),
            home: HubScreen(
              server: fixture.server,
              onChangeAddress: () async {},
            ),
          ),
        );
        await until(tile(alpha));
        await binding.takeScreenshot('minimap-subagents');
        await drag(alpha, 'select', shot: 'minimap-drag-select');
        await tester.tap(tile(beta).first);
        await settle();
        expect(find.text('2개 선택'), findsOneWidget);
        await binding.takeScreenshot('minimap-multi-select');
        await tester.tap(find.byKey(const ValueKey('hub-drop-hide')));
        await settle();
        expect(fixture.commands, isEmpty);
        await tester.tap(find.byKey(const Key('hub-hidden')));
        await settle();
        await binding.takeScreenshot('minimap-hidden');
        await tester.tap(find.text('전부 다시 보이기'));
        await settle();
        await tester.tapAt(const Offset(10, 100));
        await until(tile(alpha));
        await drag(alpha, 'trash');
        expect(fixture.commands, isEmpty);
        await until(find.widgetWithText(FilledButton, '닫기'));
        await binding.takeScreenshot('minimap-trash-confirm');
        await tester.tap(find.widgetWithText(FilledButton, '닫기'));
        await until(find.text('되살리기'));
        expect(fixture.commands.single['method'], 'surface.close_checked');
        await tester.tap(find.text('되살리기'));
        await until(tile(alpha));
        expect(fixture.commands.last['method'], 'surface.reopen_checked');
        await binding.takeScreenshot('minimap-restored');
        expect(tester.takeException(), isNull);
      } finally {
        await tester.pumpWidget(const SizedBox());
        await settle();
        fixture.server.close();
      }
    },
  );
}
