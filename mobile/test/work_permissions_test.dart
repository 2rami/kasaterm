import 'dart:convert';

import 'package:crypto/crypto.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/screens/work_permissions.dart';

final origin = Uri.parse('https://gateway.invalid');
final session = AccountSession(origin: origin, account: 'one', deviceId: 'dev_phone', token: 'kdt_phone');

Map<String, dynamic> pendingMail() => {
  'id': 'pw_a',
  'digest': 'digest-a',
  'provider': 'google',
  'display': 'me@example.com',
  'device_label': 'MacBook · 케이',
  'write': {
    'kind': 'mail',
    'to': ['team@example.com', 'lead@example.com'],
    'cc': [],
    'subject': '주간 보고',
    'body': '첫 줄\n둘째 줄',
  },
};

/// 관문 흉내 — 승인 열쇠를 한 번 잊은 척할 수 있다([forgetOnce]).
class FakeWork {
  FakeWork({this.forgetOnce = false});
  bool forgetOnce;
  final requests = <http.Request>[];
  final registered = <String>{};
  var pending = [pendingMail()];

  http.Client client() => MockClient((req) async {
    requests.add(req);
    expect(req.headers['authorization'], 'Bearer kdt_phone');
    final body = req.body.isEmpty ? const {} : jsonDecode(req.body) as Map;
    switch ((req.method, req.url.path)) {
      case ('GET', '/relay/connections'):
        return http.Response(
          jsonEncode({
            'ok': true,
            'available': {'google': true, 'github': false},
            'connections': [
              {'id': 'con_a', 'provider': 'google', 'display': 'me@example.com', 'features': ['mail.read', 'mail.send'], 'state': 'ok'},
            ],
            'pending': pending,
          }),
          200,
          headers: {'content-type': 'application/json; charset=utf-8'},
        );
      case ('POST', '/relay/connections/approver'):
        registered.add(body['key'] as String);
        return http.Response('{"ok":true}', 200);
      case ('POST', '/relay/connections/pending/pw_a/approve'):
        final key = req.headers['x-kasa-approver'];
        if (forgetOnce) {
          forgetOnce = false;
          registered.clear();
        }
        if (!registered.contains(key)) return http.Response('{"ok":false,"error":"approver_required"}', 403);
        expect(body, {'digest': 'digest-a'});
        pending = [];
        return http.Response('{"ok":true,"status":"sent","id":"m1"}', 200);
      case ('POST', '/relay/connections/pending/pw_a/reject'):
        pending = [];
        return http.Response('{"ok":true}', 200);
    }
    return http.Response('{"ok":false,"error":"not_found"}', 404);
  });
}

Future<void> until(WidgetTester tester, bool Function() done) async {
  for (var i = 0; i < 30 && !done(); i++) {
    await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 20)));
    await tester.pump(const Duration(milliseconds: 100));
  }
}

Widget host(FakeWork fake) => MaterialApp(
  home: WorkPermissionsScreen(
    api: () => RelayAccountApi(origin, client: fake.client(), session: session),
  ),
);

void main() {
  testWidgets('승인 대기를 펼치면 받는 사람·본문 전부를 보이고, 승인은 그 내용의 digest 와 앱 열쇠로만 간다', (tester) async {
    final fake = FakeWork(forgetOnce: true);
    await tester.pumpWidget(host(fake));
    await until(tester, () => find.byKey(const Key('pending-pw_a')).evaluate().isNotEmpty);
    expect(find.text('메일 · team@example.com 외 1 · 주간 보고'), findsOneWidget);
    expect(find.text('Gmail · me@example.com'), findsOneWidget);
    expect(find.text('메일 읽기 · 보내기 요청'), findsOneWidget);

    await tester.tap(find.byKey(const Key('pending-pw_a')));
    await tester.pumpAndSettle();
    expect(find.text('team@example.com, lead@example.com'), findsOneWidget);
    expect(find.text('첫 줄\n둘째 줄'), findsOneWidget);
    await tester.tap(find.byKey(const Key('approve-write')));
    await until(tester, () => find.text('메일을 보냈어요.').evaluate().isNotEmpty);
    await tester.pumpAndSettle();
    expect(find.text('메일을 보냈어요.'), findsOneWidget);

    final approves = fake.requests.where((r) => r.url.path.endsWith('/approve')).toList();
    expect(approves.length, 2, reason: '관문이 열쇠를 잊었을 때 한 번 다시 등록하고 다시 묻지 않았다');
    final key = approves.last.headers['x-kasa-approver']!;
    expect(key.length, 43);
    expect(fake.registered, {key});
    expect(find.byKey(const Key('pending-pw_a')), findsNothing);
  });

  testWidgets('버리기는 관문에 버리라고만 한다', (tester) async {
    final fake = FakeWork();
    await tester.pumpWidget(host(fake));
    await until(tester, () => find.byKey(const Key('pending-pw_a')).evaluate().isNotEmpty);
    await tester.tap(find.byKey(const Key('pending-pw_a')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('reject-write')));
    await until(tester, () => find.text('요청을 버렸어요.').evaluate().isNotEmpty);
    expect(fake.requests.map((r) => r.url.path), isNot(contains('/relay/connections/approver')));
    expect(fake.requests.map((r) => r.url.path), contains('/relay/connections/pending/pw_a/reject'));
  });

  test('일 권한 연결은 기기 자격과 앱 리다이렉트로만 시작한다', () async {
    late Map started;
    final api = RelayAccountApi(
      origin,
      session: session,
      client: MockClient((req) async {
        switch (req.url.path) {
          case '/relay/oauth/providers':
            return http.Response('{"ok":true,"redirect_login":true,"choose_account":true,"providers":[{"id":"google","enabled":true}],"connect":{"google":true}}', 200);
          case '/relay/oauth/start':
            started = jsonDecode(req.body) as Map;
            return http.Response(jsonEncode({'ok': true, 'request_id': 'r', 'expires_in': 600,
              'authorization_url': '${req.url.origin}/relay/oauth/authorize/r'}), 200);
          case '/relay/oauth/token':
            final body = jsonDecode(req.body) as Map;
            final challenge = base64Url.encode(sha256.convert(ascii.encode(body['code_verifier'] as String)).bytes).replaceAll('=', '');
            expect(challenge, started['code_challenge']);
            return http.Response('{"ok":true,"status":"connected","account":"one","connection":{"id":"con_a"}}', 200);
        }
        return http.Response('{}', 404);
      }),
    );
    final flow = await api.oauthStart(OAuthProvider.google, 'machine-phone-0001', redirect: true, connect: ['mail.read', 'mail.send']);
    expect(started['link'], isTrue);
    expect(started['connect'], ['mail.read', 'mail.send']);
    expect(started.containsKey('choose'), isFalse);
    final r = await api.oauthRedeem(flow, Uri.parse('kasaterm://oauth?code=c&state=${flow.redirect!.state}'));
    expect(r.connected, isTrue);
    expect(r.session, isNull);
    await expectLater(
      api.oauthStart(OAuthProvider.google, 'machine-phone-0001', connect: ['mail.read']),
      throwsA(isA<AccountException>()),
      reason: '리다이렉트 없이 연결을 시작했다',
    );
  });

  test('설정의 Google·GitHub 연결은 관문이 받는 한 일 권한도 같은 허용으로 붙인다', () async {
    final starts = <Map>[];
    var connect = '{"google":true,"github":false}';
    var tokenReply = '';
    final api = RelayAccountApi(
      origin,
      session: session,
      client: MockClient((req) async {
        switch (req.url.path) {
          case '/relay/oauth/providers':
            return http.Response('{"ok":true,"redirect_login":true,"providers":[{"id":"google","enabled":true},{"id":"github","enabled":true}],"connect":$connect}', 200);
          case '/relay/oauth/start':
            starts.add(jsonDecode(req.body) as Map);
            return http.Response(jsonEncode({'ok': true, 'request_id': 'r', 'expires_in': 600,
              'authorization_url': '${req.url.origin}/relay/oauth/authorize/r'}), 200);
          case '/relay/oauth/token':
            return http.Response(tokenReply, 200);
        }
        return http.Response('{}', 404);
      }),
    );
    final google = await api.oauthStart(OAuthProvider.google, 'machine-phone-0001', link: true, redirect: true, work: true);
    expect(starts.last['connect'], ['mail.read', 'mail.send']);
    expect(starts.last['link'], isTrue);
    // 관문이 GitHub 일 권한을 못 받으면 로그인 연결만 한다.
    await api.oauthStart(OAuthProvider.github, 'machine-phone-0001', link: true, redirect: true, work: true);
    expect(starts.last.containsKey('connect'), isFalse);
    connect = '{"google":true,"github":true}';
    final github = await api.oauthStart(OAuthProvider.github, 'machine-phone-0001', link: true, redirect: true, work: true);
    expect(starts.last['connect'], ['github.pr']);

    tokenReply = '{"ok":true,"status":"connected","account":"one","connection":{"id":"con_a"},"linked":true}';
    final linked = await api.oauthRedeem(google, Uri.parse('kasaterm://oauth?code=c&state=${google.redirect!.state}'));
    expect((linked.connected, linked.linked, linked.installUrl), (true, true, null));
    tokenReply = '{"ok":true,"status":"connected","account":"one","connection":{"id":"con_b"},"linked":false,'
        '"link_error":"already_linked","installed":false,"install_url":"https://github.com/apps/kasaterm/installations/new"}';
    final install = await api.oauthRedeem(github, Uri.parse('kasaterm://oauth?code=c&state=${github.redirect!.state}'));
    expect((install.connected, install.linked), (true, false));
    expect(install.installUrl, Uri.parse('https://github.com/apps/kasaterm/installations/new'));
    tokenReply = tokenReply.replaceFirst('https://github.com/apps/', 'https://evil.example/apps/');
    final odd = await api.oauthRedeem(github, Uri.parse('kasaterm://oauth?code=c&state=${github.redirect!.state}'));
    expect(odd.installUrl, isNull, reason: '관문의 GitHub 앱 주소 밖을 열려 했다');
  });
}
