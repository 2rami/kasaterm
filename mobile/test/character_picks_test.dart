import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/character_picks.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/screens/character_picks.dart';
import 'package:kasaterm_mobile/server.dart';

import 'relay_account_test.dart' show session;

// 데스크톱 session.rs `mixed_character_picks_replace_conflicts_without_changing_other_choices` 와 같은 명단.
Roster? _roster(String theme) => switch (theme) {
  'a' => Roster.fromJson({
    'members': [
      {'name': 'Alice', 'slug': 'alice'},
      {'name': 'Shared', 'slug': 'shared'},
      {'name': 'Old art', 'slug': 'same-art'},
    ],
  }),
  'b' => Roster.fromJson({
    'members': [
      {'name': 'Bob', 'slug': 'bob'},
      {'name': 'Shared', 'slug': 'shared'},
      {'name': 'New art', 'slug': 'same-art'},
    ],
  }),
  _ => null,
};

/// 기록은 안의 목록까지 같은지 보지 않는다 — 목록으로 펴서 비교한다.
List<Object> _p(Picks picks) => [for (final (t, n) in picks) [t, n]];

http.Response _json(Object body, [int status = 200]) =>
    http.Response.bytes(utf8.encode(jsonEncode(body)), status, headers: {'content-type': 'application/json'});

/// 데스크톱(관문 너머) 셋과 계정 동기화를 한 가짜로. [onPatch] 가 계정 쪽 응답을 정한다.
class _Fake {
  _Fake({Map<String, Object?>? settings, int revision = 3}) : snapshot = {'revision': revision, 'settings': settings ?? {}};

  Map<String, Object?> snapshot;
  final patches = <Map<String, dynamic>>[];
  final syncHeaders = <String?>[];
  http.Response Function(Map<String, dynamic> body)? onPatch;

  Future<http.Response> handle(http.Request req) async {
    switch (req.url.path) {
      case '/relay/account/settings/characters':
        return _json({
          'active_theme': '',
          'themes': [
            {'id': '', 'label': '블루 아카이브', 'count': 3, 'faces': ['alice'], 'picked': []},
            {'id': 'b', 'label': '두 번째', 'count': 3, 'faces': ['bob'], 'picked': []},
          ],
        });
      case '/relay/account/theme-roster':
        return switch (req.url.queryParameters['id']) {
          '__base' => _json({
            'members': [
              {'name': 'Alice', 'slug': 'alice'},
              {'name': 'Shared', 'slug': 'shared'},
              {'name': 'Old art', 'slug': 'same-art'},
            ],
          }),
          'b' => _json({
            'members': [
              {'name': 'Bob', 'slug': 'bob'},
              {'name': 'Shared', 'slug': 'shared'},
              {'name': 'New art', 'slug': 'same-art'},
            ],
          }),
          _ => _json({'error': 'theme roster not found'}, 404),
        };
      case '/relay/account/mobile/me':
        return _json({'name': 'owner', 'owner': true, 'machine': '맥북'});
      case '/relay/account-sync':
        syncHeaders.add(req.headers['x-kasa-sync-keys']);
        if (req.method == 'GET') return _json(snapshot);
        final body = jsonDecode(req.body) as Map<String, dynamic>;
        patches.add(body);
        final custom = onPatch?.call(body);
        if (custom != null) return custom;
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

  CharacterPicksStore store() {
    final client = MockClient(handle);
    return CharacterPicksStore(
      server: Server.account(session(), client: client),
      api: () => RelayAccountApi(session().origin, session: session(), client: client),
    );
  }
}

void main() {
  group('데스크톱과 같은 고르기 규칙', () {
    test('새로 고른 쪽이 같은 이름·같은 그림 키를 다른 테마에서 빼고, 남은 선택은 그대로 둔다', () {
      final old = <(String, List<String>)>[('a', ['Alice', 'Shared', 'Old art'])];
      var picks = pickOne(old, 'b', 'Bob', true, _roster);
      expect(_p(picks).first, _p(old).first);
      picks = pickOne(picks, 'b', 'Shared', true, _roster);
      expect(picks.first.$2, isNot(contains('Shared')));
      picks = pickOne(picks, 'b', 'New art', true, _roster);
      expect(picks.first.$2, ['Alice']);
      picks = pickOne(picks, 'a', 'Alice', false, _roster);
      expect([for (final e in picks) e.$1], ['b']);
      picks = pickOne(picks, 'a', 'Alice', false, _roster);
      expect(picks.length, 1, reason: '안 고른 테마를 끈다고 그 테마가 켜지면 안 된다');
      expect(() => pickOne([('b', ['Bob'])], 'b', 'Bob', false, _roster), throwsA(isA<PickError>()));
      expect(pickOne([], 'a', 'Alice', false, _roster).first.$2, ['Shared', 'Old art']);
    });

    test('켜기는 그 테마에 있는 이름만 받고 끄기는 유령도 지운다', () {
      expect(() => pickOne([], 'a', 'Nobody', true, _roster), throwsA(isA<PickError>()));
      expect(() => pickOne([], 'zz', 'Alice', true, _roster), throwsA(isA<PickError>()));
      expect(_p(pickOne([('a', ['Alice', 'Ghost'])], 'a', 'Ghost', false, _roster)), [['a', ['Alice']]]);
    });

    test('테마 전부 고르기는 그 테마를 차례 맨 뒤로 보내고 해제는 그 테마만 뺀다', () {
      final picks = pickAll([('b', ['Bob']), ('a', ['Alice'])], 'b', true, _roster);
      expect(_p(picks), [['a', ['Alice']], ['b', ['Bob', 'Shared', 'New art']]]);
      expect(_p(pickAll(picks, 'b', false, _roster)), [['a', ['Alice']]]);
      expect(() => pickAll([('b', ['Bob'])], 'b', false, _roster), throwsA(isA<PickError>()));
    });

    test('켜짐 표시는 차례가 앞선 테마 쪽이고, 고른 학생이 없으면 학생 테마 전원이다', () {
      final rosters = {'a': _roster('a')!, 'b': _roster('b')!};
      final picks = <(String, List<String>)>[('b', ['Bob', 'Shared']), ('a', ['Alice', 'Shared'])];
      for (final active in ['a', 'b', '']) {
        expect(isPicked(picks, rosters, active, 'a', 'Alice'), isTrue);
        expect(isPicked(picks, rosters, active, 'b', 'Bob'), isTrue);
        expect(isPicked(picks, rosters, active, 'a', 'Shared'), isFalse);
        expect(isPicked(picks, rosters, active, 'b', 'Shared'), isTrue);
      }
      expect(isPicked([], rosters, 'b', 'b', 'Bob'), isTrue);
      expect(isPicked([], rosters, 'b', 'a', 'Alice'), isFalse);
      expect(isPicked([('gone', ['Alice'])], rosters, 'b', 'b', 'Bob'), isTrue, reason: '이 기기에 없는 테마는 판정에서 빠진다');
    });

    test('차례는 적힌 순서 그대로, 겹친 이름은 한 번, 없는 이름은 건너뛴다', () {
      final rosters = {'a': _roster('a')!, 'b': _roster('b')!};
      final picks = parsePicks({'b': ['Shared', 'Bob'], 'gone': ['X'], 'a': ['Ghost', 'Shared', 'Alice']});
      expect(assignmentOrder(picks, rosters), [('b', 'Shared'), ('b', 'Bob'), ('a', 'Alice')]);
      expect(jsonEncode(encodePicks(picks)), '{"b":["Shared","Bob"],"gone":["X"],"a":["Ghost","Shared","Alice"]}');
    });
  });

  group('계정 동기화로 저장', () {
    test('학생을 켜면 옵트인 머리글을 달고 명단 전체를 차례대로 쓴다', () async {
      final fake = _Fake(settings: {'character_picks': {'b': ['Bob']}, 'character_theme': 'b'});
      final store = fake.store();
      await store.load();
      expect(store.error, isNull);
      expect(store.expanded, containsAll(['b']));
      expect(await store.pick('__base', 'Alice', true), isNull);
      expect(fake.patches.single, {
        'expected_revision': 3,
        'settings': {'character_picks': {'b': ['Bob'], '__base': ['Alice']}},
        'machines': {},
      });
      expect(fake.syncHeaders, everyElement('character_picks'));
      expect(store.order, [('b', 'Bob'), ('__base', 'Alice')]);
    });

    test('다른 기기가 먼저 바꿨으면 그 명단에 같은 바꿈을 다시 얹는다', () async {
      final fake = _Fake(settings: {'character_picks': {'b': ['Bob']}});
      final store = fake.store();
      await store.load();
      var first = true;
      fake.onPatch = (body) {
        if (first) {
          first = false;
          return _json({
            'error': 'revision_conflict',
            'current': {'revision': 9, 'settings': {'character_picks': {'b': ['Bob', 'New art']}}},
          }, 409);
        }
        return _json({'revision': 10, 'settings': body['settings']});
      };
      expect(await store.pick('__base', 'Old art', true), isNull);
      expect(fake.patches.last['expected_revision'], 9);
      expect(fake.patches.last['settings'], {
        'character_picks': {'b': ['Bob'], '__base': ['Old art']},
      }, reason: '같은 그림 키(New art)는 새로 고른 쪽이 이긴다');
    });

    test('관문이 키를 모르면(400) 까닭을 알리고 화면은 저장된 명단으로 돌아간다', () async {
      final fake = _Fake(settings: {'character_picks': {'b': ['Bob']}});
      final store = fake.store();
      await store.load();
      fake.onPatch = (_) => _json({'error': 'unsupported_setting'}, 400);
      final problem = await store.pick('b', 'Shared', true);
      expect(problem, contains('관문'));
      expect(_p(store.picks), [['b', ['Bob']]]);
      expect(store.saving, isFalse);
    });

    test('마지막 한 명은 끌 수 없다 — 계정에 쓰지 않는다', () async {
      final fake = _Fake(settings: {'character_picks': {'b': ['Bob']}});
      final store = fake.store();
      await store.load();
      expect(await store.pick('b', 'Bob', false), '최소 한 명은 선택해 주세요');
      expect(fake.patches, isEmpty);
    });

    test('번들 학생 테마는 계정에서 키를 지운다', () async {
      final fake = _Fake(settings: {'character_theme': 'b'});
      final store = fake.store();
      await store.load();
      expect(await store.selectTheme(''), isNull);
      expect(fake.patches.single['settings'], {'character_theme': null});
      expect(store.activeTheme, '');
    });
  });

  testWidgets('화면 — 펼친 테마의 학생 칸을 누르면 계정에 쓰고 차례가 바뀐다', (tester) async {
    final fake = _Fake(settings: {'character_picks': {'b': ['Bob']}});
    final client = MockClient(fake.handle);
    await tester.pumpWidget(
      MaterialApp(
        home: CharacterPicksScreen(
          server: Server.account(session(), client: client),
          api: () => RelayAccountApi(session().origin, session: session(), client: client),
        ),
      ),
    );
    await tester.pumpAndSettle();
    expect(find.text('Bob'), findsWidgets);
    expect(find.text('1번째'), findsOneWidget);
    final alice = find.byKey(const Key('pick-cell-__base-Alice'));
    await tester.ensureVisible(alice);
    await tester.tap(alice);
    await tester.pumpAndSettle();
    expect(fake.patches.single['settings'], {'character_picks': {'b': ['Bob'], '__base': ['Alice']}});
    expect(find.text('2번째'), findsOneWidget);
    await tester.scrollUntilVisible(find.text('Bob → Alice'), -200);
    expect(find.text('Bob → Alice'), findsOneWidget);
  });
}
