import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/machine_look.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/screens/device_names.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/status_style.dart';

import 'relay_account_test.dart' show session;

http.Response _json(Object body, [int status = 200]) =>
    http.Response.bytes(utf8.encode(jsonEncode(body)), status, headers: {'content-type': 'application/json'});

/// 기준 기기(맥북, id `book`)와 `~mini` 로 닿는 맥미니, 계정 동기화를 한 가짜로.
class _Fake {
  _Fake({Map<String, Object?>? settings}) : snapshot = {'revision': 4, 'settings': settings ?? {}};

  Map<String, Object?> snapshot;
  final patches = <Map<String, dynamic>>[];
  final syncHeaders = <String?>[];
  int patchStatus = 200;

  Future<http.Response> handle(http.Request req) async {
    switch (req.url.path) {
      case '/relay/account/version':
        return _json({'machine_id': 'book'});
      case '/relay/account/mobile/me':
        return _json({'name': 'owner', 'owner': true, 'machine': '건호의 MacBook Pro'});
      case '/relay/account/machines':
        return _json({
          'machines': [
            {'label': 'nachoneko', 'route': '~mini', 'online': true, 'panes': []},
            {'label': '옛 기계', 'route': '옛 기계', 'online': false, 'panes': []},
          ],
        });
      case '/relay/account-sync':
        syncHeaders.add(req.headers['x-kasa-sync-keys']);
        if (req.method == 'GET') return _json(snapshot);
        final body = jsonDecode(req.body) as Map<String, dynamic>;
        patches.add(body);
        if (patchStatus != 200) return _json({'error': 'unsupported_setting'}, patchStatus);
        final settings = {...snapshot['settings'] as Map<String, Object?>};
        for (final e in (body['settings'] as Map<String, dynamic>).entries) {
          if (e.value == null) {
            settings.remove(e.key);
          } else {
            settings[e.key] = e.value;
          }
        }
        snapshot = {'revision': (snapshot['revision'] as int) + 1, 'settings': settings};
        return _json(snapshot);
    }
    return _json({'error': 'not found'}, 404);
  }
}

Widget _screen(_Fake fake) {
  final client = MockClient(fake.handle);
  return MaterialApp(
    home: DeviceNamesScreen(
      server: Server.account(session(), client: client),
      api: () => RelayAccountApi(session().origin, session: session(), client: client),
    ),
  );
}

void main() {
  tearDown(() => machineLooks.value = const MachineLooks());

  group('이름은 기기 id 에 붙고 보여 줄 때만 쓴다', () {
    final looks = MachineLooks.parse(
      deviceNames: {'book': ' 회사 맥북 ', 'mini': '거실', 'blank': '  ', 'bad': 3},
      rootId: 'book',
    ).copyWith(ids: {normalizeDevice('nachoneko'): 'mini'});

    test('id 를 아는 길·기준 기기·이름 표가 계정 이름을 찾고, 모르면 제 이름', () {
      expect(looks.names, {'book': '회사 맥북', 'mini': '거실'});
      expect(looks.name('아무 이름', route: '~mini'), '거실');
      expect(looks.name('건호의 MacBook Pro', local: true), '회사 맥북');
      expect(looks.name('NachoNeko'), '거실', reason: '거울 칩은 이름만 든다');
      expect(looks.name('옛 기계', route: '옛 기계'), '옛 기계');
      expect(looks.name('~'), '~');
    });

    test('색·아이콘은 제 이름으로 찾는다 — 이름을 바꿔도 안 바뀐다', () {
      expect(looks.color('nachoneko'), looks.color('nachoneko'));
      expect(looks.icon('nachoneko'), MachineLooks.parse().icon('nachoneko'));
    });

    test('계정 값을 못 받은 폰은 기준 기기가 든 사본을 쓰고, 받았으면 계정이 이긴다', () {
      final appearance = {'device_names': {'mini': '데스크톱 사본'}};
      expect(MachineLooks.parse(appearance: appearance).names, {'mini': '데스크톱 사본'});
      expect(MachineLooks.parse(appearance: appearance, deviceNames: const <String, String>{}).names, isEmpty);
    });

    test('바꾸기는 그 id 만, 빈 이름은 지운다', () {
      expect(renameDevice({'a': '하나'}, 'b', ' 둘 '), {'a': '하나', 'b': '둘'});
      expect(renameDevice({'a': '하나', 'b': '둘'}, 'b', ''), {'a': '하나'});
    });

    test('기기 줄은 기준 기기 먼저, ~id 로 닿는 기계만, 같은 id 는 한 줄', () {
      final rows = deviceEntries(rootId: 'book', rootName: '맥북', machines: const [
        Machine(label: 'nachoneko', route: '~mini', online: true, panes: []),
        Machine(label: 'mini again', route: '~mini', online: true, panes: []),
        Machine(label: '옛 기계', online: false, panes: []),
        Machine(label: '자기 자신', route: '~book', online: true, panes: []),
      ]);
      expect([for (final r in rows) (r.id, r.label, r.local)], [('book', '맥북', true), ('mini', 'nachoneko', false)]);
    });
  });

  testWidgets('화면 — 기기를 눌러 이름을 바꾸면 그 id 만 계정에 쓰고 거울 칩까지 바뀐다', (tester) async {
    final fake = _Fake(settings: {'device_names': {'book': '회사 맥북'}});
    await tester.pumpWidget(_screen(fake));
    await tester.pumpAndSettle();
    expect(find.text('회사 맥북'), findsOneWidget);
    expect(find.text('원래 이름 건호의 MacBook Pro · 이 폰이 붙은 기기'), findsOneWidget);
    expect(find.text('nachoneko'), findsOneWidget);
    expect(find.text('이름을 안 붙였어요'), findsOneWidget);
    expect(find.text('옛 기계'), findsNothing, reason: 'id 를 모르는 기계엔 이름을 못 붙인다');

    await tester.tap(find.byKey(const Key('device-mini')));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('device-name-field')), '거실 맥미니');
    await tester.tap(find.text('바꾸기'));
    await tester.pumpAndSettle();

    expect(fake.patches.single['settings'], {'device_names': {'book': '회사 맥북', 'mini': '거실 맥미니'}});
    expect(fake.syncHeaders, everyElement('device_names'));
    expect(find.text('거실 맥미니'), findsOneWidget);
    expect(find.text('원래 이름 nachoneko'), findsOneWidget);
    expect(machineLooks.value.names['mini'], '거실 맥미니');

    machineLooks.value = machineLooks.value.copyWith(ids: {'nachoneko': 'mini'});
    await tester.pumpWidget(const MaterialApp(home: Scaffold(body: Center(child: MirrorTag('nachoneko')))));
    expect(find.text('거실 맥미니'), findsOneWidget);
  });

  testWidgets('화면 — 원래 이름으로 돌리면 계정에서 그 이름을 지운다', (tester) async {
    final fake = _Fake(settings: {'device_names': {'book': '회사 맥북'}});
    await tester.pumpWidget(_screen(fake));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('device-book')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('원래 이름으로'));
    await tester.pumpAndSettle();
    expect(fake.patches.single['settings'], {'device_names': null});
    expect(find.text('건호의 MacBook Pro'), findsOneWidget);
  });

  testWidgets('화면 — 옛 관문이 키를 모르면 까닭을 말하고 이름은 그대로', (tester) async {
    final fake = _Fake()..patchStatus = 400;
    await tester.pumpWidget(_screen(fake));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('device-mini')));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('device-name-field')), '거실');
    await tester.tap(find.text('바꾸기'));
    await tester.pumpAndSettle();
    expect(find.text('관문이 아직 기기 이름을 몰라요. 관문 새 판이 올라간 뒤 다시 해 주세요.'), findsOneWidget);
    expect(find.text('nachoneko'), findsOneWidget);
  });
}
