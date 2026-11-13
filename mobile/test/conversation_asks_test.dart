import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/approvals.dart';
import 'package:kasaterm_mobile/conversation.dart';
import 'package:kasaterm_mobile/login_link.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/screens/conversation_asks.dart';
import 'package:kasaterm_mobile/server.dart';

import 'approval_secret_test.dart' show FakeSigner, challenge, secretPending;
import 'approval_test.dart' show pending;
import 'relay_account_test.dart' show session;

const _route = '~machine-book';

Pane _pane({String id = '%3', String? machine = _route}) =>
    Pane(id: id, name: '유우카', title: '', status: 'idle', window: 0, cwd: '/repo', machine: machine);

http.Response _json(Object body, [int status = 200]) =>
    http.Response(jsonEncode(body), status, headers: {'content-type': 'application/json; charset=utf-8'});

Future<int> _freePort() async {
  final s = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
  final port = s.port;
  await s.close();
  return port;
}

/// 위젯 시험 묶음은 HttpClient 를 400 만 주는 가짜로 바꿔 둔다 — 루프백 자리에는 진짜로 닿아야 한다.
class _RealHttp extends HttpOverrides {}

Future<int> _get(Uri url) async {
  final client = HttpOverrides.runWithHttpOverrides(HttpClient.new, _RealHttp());
  try {
    final res = await (await client.getUrl(url)).close();
    await res.drain<void>();
    return res.statusCode;
  } finally {
    client.close(force: true);
  }
}

void main() {
  group('로그인 요청 알아보기', () {
    test('루프백 redirect_uri 가 실린 주소 — 포트·호스트·경로를 거기서 읽는다', () {
      final netbird = findLoginAsk(
        '넷버드 로그인이 필요해요. 이 주소를 Safari 에 붙여 넣어 주세요:\n'
        'https://login.netbird.io/authorize?client_id=abc&redirect_uri=http%3A%2F%2Flocalhost%3A53000%2F&state=s1&code_challenge=x\n'
        '돌아온 주소를 보내 주세요.',
      );
      expect(netbird?.port, 53000);
      expect(netbird?.host, 'localhost');
      expect(netbird?.path, '');
      expect(netbird?.url.host, 'login.netbird.io');

      final loopback = findLoginAsk('[로그인](https://auth.example.com/oauth/authorize?redirect_uri=http://127.0.0.1:8085/callback&state=z).');
      expect(loopback?.port, 8085);
      expect(loopback?.host, '127.0.0.1');
      expect(loopback?.path, '/callback');
      expect(loopback?.url.toString(), 'https://auth.example.com/oauth/authorize?redirect_uri=http://127.0.0.1:8085/callback&state=z');

      final nested = findLoginAsk(
        'https://id.example.com/login?return_to=%2Foauth%2Fauthorize%3Fredirect_uri%3Dhttp%253A%252F%252F%5B%3A%3A1%5D%253A4567%252Fcb',
      );
      expect(nested?.port, 4567);
      expect(nested?.host, '::1');
      expect(nested?.path, '/cb');
    });

    test('줄인 주소면 학생이 적은 돌아올 자리(localhost:포트/?code=…)로 안다', () {
      final ask = findLoginAsk('https://sho.rt/AbC 를 열고, 돌아온 `http://localhost:53682/?code=…` 주소를 보내 주세요.');
      expect(ask?.url.toString(), 'https://sho.rt/AbC');
      expect(ask?.port, 53682);
    });

    test('로그인이 아닌 주소는 카드가 아니다', () {
      expect(findLoginAsk('문서는 https://docs.rs/tokio 에 있어요.'), isNull);
      expect(findLoginAsk('개발 서버는 http://localhost:3000 에서 떠요. 자세한 건 https://vitejs.dev 참고.'), isNull);
      expect(findLoginAsk('http://localhost:8080/?code=abc 로 열어 보세요.'), isNull, reason: '열 주소가 루프백뿐');
      expect(findLoginAsk('redirect 없음 https://example.com/authorize?client_id=1'), isNull);
    });

    test('붙여 넣은 글은 루프백 http 이고 쿼리가 있을 때만 돌아온 주소다', () {
      expect(pastedLoginRedirect(' http://localhost:53000/?code=abc&state=s '), Uri.parse('http://localhost:53000/?code=abc&state=s'));
      expect(pastedLoginRedirect('https://localhost:53000/?code=abc'), isNull);
      expect(pastedLoginRedirect('http://evil.example/?code=abc'), isNull);
      expect(pastedLoginRedirect('http://127.0.0.1:53000/'), isNull);
    });

    test('카드에 보이는 돌아온 주소는 일회용 값의 앞뒤만', () {
      expect(
        maskLoginRedirect(Uri.parse('http://localhost:53000/cb?code=4%2F0AbCdEfGhIjK123&state=short&scope=openid%20email%20profile')),
        'localhost:53000/cb?code=4%2F…123&state=short&scope=openid%20email%20profile',
      );
    });
  });

  group('루프백에서 돌아온 주소 받기', () {
    test('그 경로로 온 GET 하나를 학생이 기다리는 꼴(호스트·포트) 그대로 받고, 딴 요청은 404', () async {
      final port = await _freePort();
      final net = await LoopbackCatch.open(LoginAsk(Uri.parse('https://x.example/a'), port, path: '/callback'));
      try {
        expect(await _get(Uri.parse('http://127.0.0.1:$port/favicon.ico')), 404);
        expect(await _get(Uri.parse('http://127.0.0.1:$port/callback')), 404, reason: '쿼리 없는 것은 돌아온 주소가 아니다');
        expect(await _get(Uri.parse('http://127.0.0.1:$port/callback?code=a%2Fb&state=s')), 200);
        expect(await net.caught, Uri.parse('http://localhost:$port/callback?code=a%2Fb&state=s'));
      } finally {
        await net.close();
      }
    });

    test('창 안에서 넘어오면 창을 거두고 그 주소를, 사람이 먼저 닫으면 null', () async {
      final port = await _freePort();
      final ask = LoginAsk(Uri.parse('https://x.example/authorize'), port);
      final closed = Completer<Uri?>();
      var cancelled = 0;
      final back = await catchLoginRedirect(
        ask,
        authenticate: (url, scheme) {
          expect(url, ask.url);
          unawaited(_get(Uri.parse('http://127.0.0.1:$port/?code=abc&state=s')));
          return closed.future;
        },
        cancel: () async {
          cancelled++;
          closed.complete(null);
        },
      );
      expect(back, Uri.parse('http://localhost:$port/?code=abc&state=s'));
      expect(cancelled, 1);
      // 자리를 거뒀다 — 같은 포트를 다시 열 수 있다.
      await (await ServerSocket.bind(InternetAddress.loopbackIPv4, port)).close();

      final none = await catchLoginRedirect(ask, authenticate: (_, _) async => null, cancel: () async => cancelled++);
      expect(none, isNull);
      expect(cancelled, 1);
    });

    test('그 포트를 못 열면 붙여 넣기로 안내한다', () async {
      final busy = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
      final busy6 = await ServerSocket.bind(InternetAddress.loopbackIPv6, busy.port, v6Only: true).then<ServerSocket?>((s) => s, onError: (_) => null);
      try {
        await expectLater(
          LoopbackCatch.open(LoginAsk(Uri.parse('https://x.example/a'), busy.port)),
          throwsA(isA<LoginCatchException>().having((e) => e.message, 'message', contains('붙여 넣어'))),
        );
      } finally {
        await busy.close();
        await busy6?.close();
      }
    });
  });

  group('대화 안 승인 카드', () {
    ApprovalCenter centerWith(http.Client client, FakeSigner signer, List<Map<String, Object?>> items) {
      final c = ApprovalCenter(
        api: (s) => RelayAccountApi(s.origin, session: s, client: client),
        clock: () => DateTime.fromMillisecondsSinceEpoch(0),
        signer: signer,
      );
      c.bind(session());
      c.setForeground(false);
      c.debugSeed([for (final i in items) Approval.fromJson(i)!], routes: {'dev_book': _route});
      return c;
    }

    Widget host(ApprovalCenter center, {Pane? pane, bool active = true, bool toolsAsked = false, List<Object> rows = const []}) =>
        MaterialApp(
          theme: buildTheme(Brightness.light),
          home: Scaffold(
            body: Column(
              children: [
                const Expanded(child: SizedBox()),
                ConversationAsks(
                  pane: pane ?? _pane(),
                  rows: rows,
                  active: active,
                  toolsAsked: toolsAsked,
                  onSend: (_) async {},
                  center: center,
                ),
              ],
            ),
          ),
        );

    testWidgets('그 칸 학생의 비밀 요청은 대화 안에서 Face ID 로 허락하고 결과를 말한 뒤 걷힌다', (tester) async {
      final sent = <Map<String, Object?>>[];
      final client = MockClient((req) async {
        if (req.url.path.endsWith('/approver')) return http.Response('{"ok":true}', 200);
        if (req.url.path.endsWith('/decide')) {
          sent.add(jsonDecode(req.body) as Map<String, Object?>);
          return _json({'ok': true, 'approval': {...secretPending(), 'state': 'allowed', 'by': {'label': '폰', 'kind': 'phone'}}});
        }
        return _json({'ok': true, 'approvals': []});
      });
      final signer = FakeSigner();
      final center = centerWith(client, signer, [secretPending()]);
      await tester.pumpWidget(host(center));
      await tester.pump();

      expect(find.text('키체인 승인 요청'), findsOneWidget);
      expect(find.text('keychain://db.example#deploy'), findsOneWidget);
      expect(find.text('Face ID 로 허락'), findsOneWidget);
      expect(find.text('2:00'), findsOneWidget);
      final a = center.pending.single;
      expect(center.shownInPlace(a), isTrue, reason: '보이는 동안 위쪽 알림 띠는 겹쳐 세우지 않는다');

      await tester.tap(find.byKey(const Key('ask-allow')));
      await tester.pump();
      await tester.pump();
      expect(signer.signed, [challenge()], reason: '도전값 그대로에 Face ID 서명');
      expect(sent.single['decision'], 'allow');
      expect(sent.single['sig'], 'MEUCIQ==');
      expect(find.text('허락했어요 · 이번 한 번만 꺼내요'), findsOneWidget);
      expect(find.byKey(const Key('ask-allow')), findsNothing);

      await tester.pump(const Duration(seconds: 5));
      await tester.pump();
      expect(find.byKey(const Key('ask-result')), findsNothing);
    });

    testWidgets('Face ID 를 취소하면 아무것도 안 보내고 카드는 그대로 남는다', (tester) async {
      final sent = <String>[];
      final client = MockClient((req) async {
        sent.add(req.url.path);
        return _json({'ok': true, 'approvals': []});
      });
      final center = centerWith(client, FakeSigner(answer: null), [secretPending()]);
      await tester.pumpWidget(host(center));
      await tester.pump();
      await tester.tap(find.byKey(const Key('ask-allow')));
      await tester.pump();
      await tester.pump();
      expect(sent.where((p) => p.endsWith('/decide')), isEmpty);
      expect(find.text('Face ID 를 취소했어요. 아무것도 보내지 않았어요.'), findsOneWidget);
      expect(find.byKey(const Key('ask-allow')), findsOneWidget);
    });

    testWidgets('승인 열쇠가 없는 폰은 허락 대신 「Face ID 로 연결」', (tester) async {
      final client = MockClient((req) async => _json({'ok': true, 'approvals': []}));
      final center = centerWith(client, FakeSigner(public: null), [secretPending()]);
      await tester.pumpWidget(host(center));
      await tester.pump();
      await tester.pump();
      expect(find.byKey(const Key('ask-make-key')), findsOneWidget);
      expect(find.byKey(const Key('ask-allow')), findsNothing);
    });

    testWidgets('다른 칸·다른 기계의 요청은 이 대화에 안 뜬다', (tester) async {
      final client = MockClient((req) async => _json({'ok': true, 'approvals': []}));
      final center = centerWith(client, FakeSigner(), [secretPending()]);
      for (final pane in [_pane(id: '%4'), _pane(machine: '~machine-mini'), _pane(machine: null)]) {
        await tester.pumpWidget(host(center, pane: pane));
        await tester.pump();
        expect(find.byKey(const Key('ask-allow')), findsNothing, reason: '${pane.id} ${pane.machine}');
        expect(center.shownInPlace(center.pending.single), isFalse);
      }
      await tester.pumpWidget(host(center, active: false));
      await tester.pump();
      expect(find.byKey(const Key('ask-allow')), findsNothing);
      expect(center.shownInPlace(center.pending.single), isFalse, reason: '터미널 쪽을 보는 중이면 알림 띠가 선다');
    });

    testWidgets('다른 곳에서 답하면 단추를 거두고 어디서 닫혔는지 말한다', (tester) async {
      var closed = false;
      final client = MockClient((req) async {
        final item = closed ? {...pending(), 'state': 'denied', 'by': {'label': '맥미니', 'kind': 'desktop'}} : pending();
        return _json({'ok': true, 'rev': closed ? 2 : 1, 'now': 0, 'approvals': [item]});
      });
      final center = centerWith(client, FakeSigner(), [pending()]);
      await tester.pumpWidget(host(center));
      await tester.pump();
      expect(find.text('승인 요청 · Bash'), findsOneWidget);
      expect(find.text('허락'), findsOneWidget);

      closed = true;
      await tester.runAsync(center.refresh);
      await tester.pump();
      expect(find.byKey(const Key('ask-allow')), findsNothing);
      expect(find.text('맥미니에서 거절했어요'), findsOneWidget);
      await tester.pump(const Duration(seconds: 5));
      await tester.pump();
      expect(find.text('맥미니에서 거절했어요'), findsNothing);
    });

    testWidgets('화면 선택지 카드가 이미 묻는 도구 요청은 두 번 세우지 않는다 — 비밀 요청은 그대로', (tester) async {
      final client = MockClient((req) async => _json({'ok': true, 'approvals': []}));
      final center = centerWith(client, FakeSigner(), [pending(), secretPending()]);
      await tester.pumpWidget(host(center, toolsAsked: true));
      await tester.pump();
      expect(find.text('승인 요청 · Bash'), findsNothing);
      expect(find.text('키체인 승인 요청'), findsOneWidget);
    });
  });

  group('대화 안 로그인 카드', () {
    testWidgets('학생이 준 로그인 주소를 앱 안 창으로 열고, 돌아온 주소를 확인하면 그 학생에게 보낸다', (tester) async {
      final port = await tester.runAsync(_freePort) ?? 0;
      final text = '로그인해 주세요: https://login.netbird.io/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A$port%2F&state=s1';
      final sent = <String>[];
      var rows = <Object>[ChatBubble(user: true, text: '넷버드 붙여 줘'), ChatBubble(user: false, text: text)];
      final closed = Completer<Uri?>();
      Widget host() => MaterialApp(
        theme: buildTheme(Brightness.light),
        home: Scaffold(
          body: Column(
            children: [
              const Expanded(child: SizedBox()),
              ConversationAsks(
                pane: _pane(),
                rows: rows,
                active: true,
                toolsAsked: false,
                onSend: (t) async => sent.add(t),
                center: ApprovalCenter(),
                authenticate: (url, scheme) {
                  unawaited(_get(Uri.parse('http://127.0.0.1:$port/?code=4%2F0AbCdEfGhIjK&state=s1')));
                  return closed.future;
                },
                cancelAuthenticate: () async {
                  if (!closed.isCompleted) closed.complete(null);
                },
              ),
            ],
          ),
        ),
      );
      await tester.pumpWidget(host());
      expect(find.text('로그인 요청'), findsOneWidget);
      expect(find.text('login.netbird.io'), findsOneWidget);

      await tester.runAsync(() async {
        await tester.tap(find.byKey(const Key('login-open')));
        for (var i = 0; i < 50 && find.byKey(const Key('login-send')).evaluate().isEmpty; i++) {
          await Future<void>.delayed(const Duration(milliseconds: 20));
          await tester.pump();
        }
      });
      expect(find.text('돌아온 주소를 유우카에게 보낼까요?'), findsOneWidget);
      expect(find.text('localhost:$port/?code=4%2F…IjK&state=s1'), findsOneWidget);
      expect(sent, isEmpty, reason: '사람이 보내기를 누르기 전엔 안 간다');

      await tester.tap(find.byKey(const Key('login-send')));
      await tester.pump();
      expect(sent, ['http://localhost:$port/?code=4%2F0AbCdEfGhIjK&state=s1']);

      // 보낸 말이 대화에 서면 그 요청은 끝난 것이다.
      rows = [...rows, ChatBubble(user: true, text: sent.single)];
      await tester.pumpWidget(host());
      expect(find.text('로그인 요청'), findsNothing);
      expect(find.byKey(const Key('login-send')), findsNothing);
    });

    testWidgets('사람이 이미 답한 뒤의 옛 로그인 주소·다른 학생이 넣은 말은 카드가 아니다', (tester) async {
      const url = 'https://login.netbird.io/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A53000%2F';
      Future<void> pumpRows(List<Object> rows) => tester.pumpWidget(MaterialApp(
        theme: buildTheme(Brightness.light),
        home: Scaffold(
          body: ConversationAsks(
            pane: _pane(),
            rows: rows,
            active: true,
            toolsAsked: false,
            onSend: (_) async {},
            center: ApprovalCenter(),
          ),
        ),
      ));
      await pumpRows([ChatBubble(user: false, text: url), ChatBubble(user: true, text: '했어')]);
      expect(find.text('로그인 요청'), findsNothing);
      await pumpRows([ChatBubble(user: true, from: '아로나', text: url)]);
      expect(find.text('로그인 요청'), findsNothing);
      await pumpRows([ChatBubble(user: false, text: url)]);
      expect(find.text('로그인 요청'), findsOneWidget);
      await tester.tap(find.byKey(const Key('login-dismiss')));
      await tester.pump();
      expect(find.text('로그인 요청'), findsNothing, reason: '접은 요청은 다시 안 선다');
    });
  });
}
