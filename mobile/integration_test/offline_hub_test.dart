import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:kasaterm_mobile/approvals.dart';
import 'package:kasaterm_mobile/archive_model.dart';
import 'package:kasaterm_mobile/archive_store.dart';
import 'package:kasaterm_mobile/connection_store.dart';
import 'package:kasaterm_mobile/hub_snapshot.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/screens/saved_conversations.dart';
import 'package:path_provider/path_provider.dart';

import 'offline_hub_fixture.dart';

/// 두 번 돈다(docs/offline-conversations.md 「검증과 배포」와 같은 손). 첫 실행은 루프백 계정에 붙어 산 허브를 보고
/// 지난 허브·대화를 실제 Application Support 에 남긴다. 앱을 끄고 재설치 없이 다시 켜면 서버가 없다 — 망이 끊긴 폰과 같다.
/// 그 상태로 지난 허브가 뜨는지, 끊긴 학생이 저장한 대화로 열리는지, 같은 주소에 서버가 돌아오면 저절로 산 허브인지 본다.
void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(() => Future<void>.delayed(const Duration(seconds: 1)));
  Future<void> until(WidgetTester tester, bool Function() ready, String label) async {
    final end = DateTime.now().add(const Duration(seconds: 90));
    while (!ready()) {
      if (DateTime.now().isAfter(end)) fail('Timed out: $label');
      await tester.pump(const Duration(milliseconds: 100));
      await Future<void>.delayed(const Duration(milliseconds: 100));
    }
    await tester.pump(const Duration(milliseconds: 500));
    await Future<void>.delayed(const Duration(milliseconds: 500));
    await tester.pump();
  }

  bool shown(Finder f) => f.evaluate().isNotEmpty;

  testWidgets(
    'last hub survives a cold offline restart and goes live on reconnect',
    (tester) async {
      binding.defaultBinaryMessenger.setMockMethodCallHandler(const MethodChannel('kasaterm/push'), (_) async => null);
      final support = await getApplicationSupportDirectory();
      final state = File('${support.path}/offline-hub-native-test.json');
      final previous = state.existsSync() ? jsonDecode(state.readAsStringSync()) as Map : null;
      final status = find.textContaining(RegExp('(끊김|확인 중) · '), findRichText: true);
      final room = find.text(OfflineHubFixture.room);
      if (previous == null) {
        final fixture = await OfflineHubFixture.start();
        final account = AccountSession(
          origin: Uri.parse('http://127.0.0.1:${fixture.http.port}'),
          account: 'hub-fixture',
          deviceId: 'fixture-phone',
          token: OfflineHubFixture.token,
        );
        await const ConnectionStore().save(SavedConnection(account: account));
        await tester.pumpWidget(const KasatermApp());
        await until(tester, () => shown(room) && shown(find.text(OfflineHubFixture.session)), 'live hub');
        expect(shown(status), isFalse);
        // 허브가 띄운 자동 받기로 계정 기록이 폰에 남는다 — 버튼 없이.
        final scope = '${account.origin}|${account.account}';
        await until(tester, () => HubSnapshots.shared.peek(scope) != null, 'hub snapshot');
        final store = await openArchiveStore();
        SavedConversation? saved;
        await until(tester, () {
          store.read(scope, OfflineHubFixture.entry.id).then((v) => saved = v);
          return saved != null;
        }, 'automatic conversation save');
        await HubSnapshots.shared.flush();
        await Future<void>.delayed(const Duration(seconds: 3));
        await binding.takeScreenshot('hub-live');
        await state.writeAsString(jsonEncode({'phase': 'live', 'port': fixture.http.port}), flush: true);
        await fixture.close();
      } else {
        expect(previous['phase'], 'live');
        // 서버 없이 켠다 — 대기 화면 대신 지난 허브가 서야 한다.
        await tester.pumpWidget(const KasatermApp());
        await until(tester, () => shown(room) && shown(status), 'offline hub after cold restart');
        expect(find.text('기기 연결'), findsNothing);
        expect(find.byKey(const Key('hub-offline-band')), findsOneWidget);
        expect(find.text(OfflineHubFixture.machine), findsOneWidget);
        await until(tester, () => shown(find.textContaining('끊김 · ', findRichText: true)), 'disconnected header');
        await binding.takeScreenshot('hub-offline');

        await tester.tap(find.text(OfflineHubFixture.session).last);
        await until(tester, () => shown(find.byType(SavedConversationScreen)), 'saved conversation from frozen pane');
        expect(shown(find.textContaining(OfflineHubFixture.answer, findRichText: true)), isTrue);
        expect(find.byType(TextField), findsNothing);
        expect(find.textContaining('기기가 끊겨 보내기'), findsOneWidget);
        await binding.takeScreenshot('hub-offline-saved');
        await tester.pageBack();
        await until(tester, () => shown(room), 'back to offline hub');

        final fixture = await OfflineHubFixture.start(port: previous['port'] as int);
        await tester.tap(find.text('다시 확인'));
        await until(
          tester,
          () => !shown(find.byKey(const Key('hub-offline-band'))) && !shown(status) && shown(room),
          'live hub after reconnect',
        );
        expect(find.text(OfflineHubFixture.session), findsWidgets);
        await binding.takeScreenshot('hub-reconnected');
        await state.writeAsString(jsonEncode({'phase': 'passed', 'port': previous['port']}), flush: true);
        await fixture.close();
      }
      ApprovalCenter.instance.bind(null);
      await tester.pumpWidget(const SizedBox());
      await tester.pump(const Duration(seconds: 3));
    },
    timeout: const Timeout(Duration(minutes: 5)),
  );
}
