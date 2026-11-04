import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:kasaterm_mobile/archive_store.dart';
import 'package:kasaterm_mobile/connection_store.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/screens/saved_conversations.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:path_provider/path_provider.dart';

import 'offline_archive_fixture.dart';

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  Future<void> until(
    WidgetTester tester,
    bool Function() ready,
    String label,
  ) async {
    final end = DateTime.now().add(const Duration(seconds: 70));
    while (!ready()) {
      if (DateTime.now().isAfter(end)) fail('Timed out: $label');
      await tester.pump(const Duration(milliseconds: 100));
      await Future<void>.delayed(const Duration(milliseconds: 100));
    }
    await tester.pump();
  }

  testWidgets(
    'native download, cold offline restart and incremental reconnect',
    (tester) async {
      // Push registration is outside this test; Keychain and file storage stay native.
      binding.defaultBinaryMessenger.setMockMethodCallHandler(
        const MethodChannel('kasaterm/push'),
        (_) async => null,
      );
      final support = await getApplicationSupportDirectory();
      final state = File('${support.path}/offline-archive-native-test.json');
      final previous = state.existsSync()
          ? jsonDecode(state.readAsStringSync()) as Map
          : null;
      if (previous == null) {
        final fixture = await OfflineArchiveFixture.start();
        final port = fixture.http.port;
        final account = AccountSession(
          origin: Uri.parse('http://127.0.0.1:$port'),
          account: 'archive-fixture',
          deviceId: 'fixture-phone',
          token: 'fixture-phone-token',
        );
        await const ConnectionStore().save(SavedConnection(account: account));
        final server = Server.account(account, connectDirect: false);
        await tester.pumpWidget(
          MaterialApp(
            theme: buildTheme(Brightness.light),
            home: SavedConversationsScreen(server: server),
          ),
        );
        await tester.tap(find.text('계정 기록'));
        await until(
          tester,
          () => find.text(OfflineArchiveFixture.title).evaluate().isNotEmpty,
          'cloud catalog',
        );
        await tester.tap(find.text(OfflineArchiveFixture.title));
        await until(
          tester,
          () => find.byType(SavedConversationScreen).evaluate().isNotEmpty,
          'saved reader',
        );
        expect(
          find.textContaining(
            OfflineArchiveFixture.originalText,
            findRichText: true,
          ),
          findsOneWidget,
        );
        final store = await openArchiveStore();
        final saved = await store.read(server.resumeScope, fixture.entry.id);
        expect(saved?.raw, fixture.raw);
        expect(fixture.reads, [0]);
        await binding.takeScreenshot('archive-downloaded');
        await state.writeAsString(
          jsonEncode({'phase': 'downloaded', 'port': port}),
          flush: true,
        );
        await fixture.close();
      } else {
        expect(previous['phase'], 'downloaded');
        final account = (await const ConnectionStore().load())!.account!;
        expect(account.account, 'archive-fixture');
        // No fixture listens until the persisted conversation is displayed after relaunch.
        await tester.pumpWidget(const KasatermApp());
        await until(
          tester,
          () => find.text('저장한 대화').evaluate().isNotEmpty,
          'offline entry from cached Keychain',
        );
        await tester.tap(find.text('저장한 대화'));
        await until(
          tester,
          () => find.text(OfflineArchiveFixture.title).evaluate().isNotEmpty,
          'persisted library',
        );
        await tester.tap(find.text(OfflineArchiveFixture.title));
        await until(
          tester,
          () => find.byType(SavedConversationScreen).evaluate().isNotEmpty,
          'offline reader',
        );
        expect(
          find.textContaining(
            OfflineArchiveFixture.originalText,
            findRichText: true,
          ),
          findsOneWidget,
        );
        expect(find.byType(TextField), findsNothing);
        expect(find.textContaining('읽기 전용'), findsOneWidget);
        await binding.takeScreenshot('archive-cold-offline');
        final fixture = await OfflineArchiveFixture.start(
          port: previous['port'] as int,
          advanced: true,
        );
        await until(
          tester,
          () => find
              .textContaining(
                OfflineArchiveFixture.appendedText,
                findRichText: true,
              )
              .evaluate()
              .isNotEmpty,
          'incremental refresh without closing reader',
        );
        expect(fixture.reads, [
          utf8.encode(OfflineArchiveFixture.initialRaw).length,
        ]);
        expect(find.byType(SavedConversationScreen), findsOneWidget);
        await binding.takeScreenshot('archive-reconnected');
        await state.writeAsString(
          jsonEncode({
            'phase': 'passed',
            'port': previous['port'],
            'incremental_offsets': fixture.reads,
          }),
          flush: true,
        );
        await fixture.close();
      }
    },
    timeout: const Timeout(Duration(minutes: 4)),
  );
}
