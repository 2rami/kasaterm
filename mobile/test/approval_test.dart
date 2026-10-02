import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/approvals.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/screens/approval_screen.dart';

import 'relay_account_test.dart' show session;

Map<String, Object?> pending({String id = 'apv_0123456789abcdef0123456789abcdef', bool truncated = false, int expires = 120000}) => {
  'id': id,
  'state': 'pending',
  'machine': '건호의 MacBook Pro',
  'device': 'dev_book',
  'student': '유우카',
  'pane': '%3',
  'cwd': '/Users/kasa/Desktop/momewomo/kasaterm',
  'tool': 'Bash',
  'fields': [
    {'name': 'command', 'label': '명령', 'text': 'rm -rf target/debug && API_KEY=•••••• cargo build --release'},
    {'name': 'description', 'label': '설명', 'text': '빌드 폴더를 지우고 다시 굽기'},
  ],
  'truncated': truncated,
  'created': 0,
  'expires': expires,
  'now': 0,
  'digest': 'd1',
  'by': null,
};

void main() {
  Widget host(ApprovalCenter center, String id) => MaterialApp(
    theme: buildTheme(Brightness.light),
    home: ApprovalScreen(center: center, id: id),
  );

  ApprovalCenter centerWith(http.Client client, {DateTime Function()? clock}) {
    final c = ApprovalCenter(
      api: (s) => RelayAccountApi(s.origin, session: s, client: client),
      clock: clock ?? () => DateTime.fromMillisecondsSinceEpoch(0),
    );
    c.bind(session());
    c.setForeground(false);
    return c;
  }

  testWidgets('승인 화면은 기기·학생·도구와 입력 원문 전부를 보이고, 누른 결정을 그 지문과 함께 보낸다', (tester) async {
    final sent = <Map<String, Object?>>[];
    final headers = <String, String>{};
    final client = MockClient((req) async {
      if (req.url.path.endsWith('/approver')) return http.Response('{"ok":true}', 200);
      if (req.url.path.endsWith('/decide')) {
        sent.add(jsonDecode(req.body) as Map<String, Object?>);
        headers.addAll(req.headers);
        return http.Response(
          jsonEncode({
            'ok': true,
            'approval': {...pending(), 'state': 'allowed', 'by': {'label': '폰', 'kind': 'phone'}},
          }),
          200,
          headers: {'content-type': 'application/json; charset=utf-8'},
        );
      }
      return http.Response(jsonEncode({'ok': true, 'rev': 1, 'now': 0, 'approvals': [pending()]}), 200,
          headers: {'content-type': 'application/json; charset=utf-8'});
    });
    final center = centerWith(client);
    await center.refresh();
    final id = pending()['id'] as String;
    await tester.pumpWidget(host(center, id));
    await tester.pump();

    expect(find.text('유우카'), findsOneWidget);
    expect(find.text('건호의 MacBook Pro · %3'), findsOneWidget);
    expect(find.text('Bash'), findsOneWidget);
    expect(find.text('rm -rf target/debug && API_KEY=•••••• cargo build --release'), findsOneWidget);
    expect(find.text('빌드 폴더를 지우고 다시 굽기'), findsOneWidget);
    expect(find.text('/Users/kasa/Desktop/momewomo/kasaterm'), findsOneWidget);
    expect(find.textContaining('2:00 남음'), findsOneWidget);
    expect(find.textContaining('항상'), findsNothing, reason: '「항상 허락」은 없다');

    await tester.tap(find.byKey(const Key('approval-allow')));
    await tester.pump();
    await tester.pump();
    expect(sent.single, {'decision': 'allow', 'digest': 'd1'});
    expect(headers['x-kasa-approver'], approvalKey);
    expect(find.byKey(const Key('approval-allow')), findsNothing);
    expect(find.text('폰에서 허락했어요'), findsOneWidget);
  });

  testWidgets('다른 곳에서 먼저 닫힌 요청은 그 사실을 보이고 단추를 거둔다', (tester) async {
    var closed = false;
    final client = MockClient((req) async {
      if (req.url.path.endsWith('/approver')) return http.Response('{"ok":true}', 200);
      if (req.url.path.endsWith('/decide')) {
        closed = true;
        return http.Response(jsonEncode({'ok': false, 'error': 'already_closed'}), 409);
      }
      final item = closed ? {...pending(), 'state': 'denied', 'by': {'label': '맥미니', 'kind': 'desktop'}} : pending();
      return http.Response(jsonEncode({'ok': true, 'rev': closed ? 2 : 1, 'now': 0, 'approvals': [item]}), 200,
          headers: {'content-type': 'application/json; charset=utf-8'});
    });
    final center = centerWith(client);
    await center.refresh();
    await tester.pumpWidget(host(center, pending()['id'] as String));
    await tester.pump();
    await tester.tap(find.byKey(const Key('approval-allow')));
    await tester.pump();
    await tester.pump();
    expect(find.text('이미 다른 곳에서 처리됐어요.'), findsOneWidget);
    expect(find.text('맥미니에서 거절했어요'), findsOneWidget);
    expect(find.byKey(const Key('approval-deny')), findsNothing);
  });

  testWidgets('2분이 지나면 원래 창으로 — 결정 단추가 사라진다', (tester) async {
    var now = 0;
    final client = MockClient((req) async => http.Response(
          jsonEncode({'ok': true, 'rev': 1, 'now': 0, 'approvals': [pending()]}),
          200,
          headers: {'content-type': 'application/json; charset=utf-8'},
        ));
    final center = centerWith(client, clock: () => DateTime.fromMillisecondsSinceEpoch(now));
    await center.refresh();
    await tester.pumpWidget(host(center, pending()['id'] as String));
    await tester.pump();
    expect(find.byKey(const Key('approval-allow')), findsOneWidget);
    now = 120000;
    await tester.pump(const Duration(seconds: 1));
    expect(find.byKey(const Key('approval-allow')), findsNothing);
    expect(find.text('2분이 지나 원래 창으로 돌아갔어요'), findsOneWidget);
  });

  testWidgets('잘린 원문은 허락할 수 없고 거절만 된다', (tester) async {
    final client = MockClient((req) async => http.Response(
          jsonEncode({'ok': true, 'rev': 1, 'now': 0, 'approvals': [pending(truncated: true)]}),
          200,
          headers: {'content-type': 'application/json; charset=utf-8'},
        ));
    final center = centerWith(client);
    await center.refresh();
    await tester.pumpWidget(host(center, pending()['id'] as String));
    await tester.pump();
    expect(find.byKey(const Key('approval-truncated')), findsOneWidget);
    final allow = tester.widget<InkWell>(
      find.descendant(of: find.byKey(const Key('approval-allow')), matching: find.byType(InkWell)),
    );
    expect(allow.onTap, isNull);
    final deny = tester.widget<InkWell>(
      find.descendant(of: find.byKey(const Key('approval-deny')), matching: find.byType(InkWell)),
    );
    expect(deny.onTap, isNotNull);
  });

  test('앞에 있는 동안 기다리는 요청은 한 번씩만 알린다(켤 때 이미 있던 것도 — 2분짜리라 지금 봐야 한다)', () async {
    var round = 0;
    final client = MockClient((req) async {
      round++;
      final items = round == 1 ? [pending()] : [pending(), pending(id: 'apv_ffffffffffffffffffffffffffffffff')];
      return http.Response(jsonEncode({'ok': true, 'rev': round, 'now': 0, 'approvals': items}), 200,
          headers: {'content-type': 'application/json; charset=utf-8'});
    });
    final center = ApprovalCenter(
      api: (s) => RelayAccountApi(s.origin, session: s, client: client),
      clock: () => DateTime.fromMillisecondsSinceEpoch(0),
    );
    final announced = <String>[];
    center.onNew = (a) => announced.add(a.id);
    center.bind(session());
    await Future<void>.delayed(const Duration(milliseconds: 500));
    center.setForeground(false);
    expect(announced, ['apv_0123456789abcdef0123456789abcdef', 'apv_ffffffffffffffffffffffffffffffff']);
    expect(center.pending.length, 2);
  });
}
