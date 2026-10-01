import 'dart:convert';

import 'package:crypto/crypto.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/screens/connect.dart';
import 'package:kasaterm_mobile/screens/oauth_sheet.dart';
import 'package:url_launcher_platform_interface/link.dart';
import 'package:url_launcher_platform_interface/url_launcher_platform_interface.dart';

final origin = Uri.parse('https://gateway.invalid');
const installId = '0123456789abcdef0123456789abcdef';

/// 관문 흉내 — 확인 화면을 연 뒤 [pendingPolls] 번은 기다리라 하고 그다음 [finish] 를 준다.
class FakeGateway {
  FakeGateway({this.pendingPolls = 1, Map<String, Object?>? finish, this.finishStatus = 200, this.redirect = false})
    : finish = finish ??
          {'ok': true, 'status': 'complete', 'account': 'oauth_ab12', 'display_name': 'me@example.test', 'device_id': 'dev-1', 'token': 'kdt_phone'};

  int pendingPolls;

  /// 확인 코드 없는 앱 리다이렉트를 아는 관문인가.
  final bool redirect;
  final Map<String, Object?> finish;
  final int finishStatus;
  final requests = <http.Request>[];

  http.Client client() => MockClient((req) async {
    requests.add(req);
    final body = req.body.isEmpty ? const {} : jsonDecode(req.body) as Map;
    switch (req.url.path) {
      case '/relay/oauth/providers':
        return http.Response(
          '{"ok":true,"signup_enabled":true,${redirect ? '"redirect_login":true,' : ''}"providers":[{"id":"google","enabled":true},{"id":"github","enabled":false}]}',
          200,
        );
      case '/relay/oauth/start':
        return http.Response(
          jsonEncode({
            'ok': true,
            'request_id': 'req-1',
            if (!body.containsKey('redirect_uri')) ...{'poll_token': 'poll-secret', 'user_code': 'ABCD-EFGH'},
            'expires_in': 600,
            'authorization_url': '${req.url.origin}/relay/oauth/authorize/req-1',
          }),
          200,
        );
      case '/relay/oauth/token':
        final challenge = base64Url.encode(sha256.convert(ascii.encode(body['code_verifier'] as String)).bytes).replaceAll('=', '');
        if (body['code'] != 'one-time' || challenge != start()['code_challenge'] || body['redirect_uri'] != start()['redirect_uri']) {
          return http.Response('{"ok":false,"error":"invalid_grant"}', 400);
        }
        return http.Response(jsonEncode(finish), finishStatus);
      case '/relay/oauth/poll':
        expect(body, {'request_id': 'req-1', 'poll_token': 'poll-secret', 'provider': 'google', 'kind': 'phone', 'machine_id': installId});
        if (pendingPolls-- > 0) return http.Response('{"ok":true,"status":"pending"}', 200);
        return http.Response(jsonEncode(finish), finishStatus);
      case '/relay/oauth/cancel':
        return http.Response('{"ok":true,"status":"cancelled"}', 200);
    }
    return http.Response('{}', 404);
  });

  Map start() => jsonDecode(requests.firstWhere((r) => r.url.path == '/relay/oauth/start').body) as Map;
}

class _Launcher extends UrlLauncherPlatform {
  final launched = <(String, PreferredLaunchMode)>[];

  @override
  LinkDelegate? get linkDelegate => null;

  @override
  Future<bool> launchUrl(String url, LaunchOptions options) async {
    launched.add((url, options.mode));
    return true;
  }
}

/// 가짜 관문의 응답(진짜 비동기)이 올 때까지 시계를 돌린다.
Future<void> until(WidgetTester tester, bool Function() done, {Duration step = Duration.zero}) async {
  for (var i = 0; i < 20 && !done(); i++) {
    await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 20)));
    await tester.pump(step);
  }
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  final clipboard = <String>[];
  late _Launcher launcher;

  setUp(() {
    launcher = _Launcher();
    UrlLauncherPlatform.instance = launcher;
    clipboard.clear();
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger.setMockMethodCallHandler(SystemChannels.platform, (call) async {
      if (call.method == 'Clipboard.setData') clipboard.add((call.arguments as Map)['text'] as String);
      return null;
    });
  });

  testWidgets('Google 로그인 — 코드를 보이고 복사한 채 관문 확인 화면을 열고, 끝나면 그 세션으로 들어간다', (tester) async {
    final gateway = FakeGateway();
    AccountSession? adopted;
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => ConnectScreen(
            onConnected: (_) async {},
            onLogin: (_, _, _) async {},
            relay: (o) => RelayAccountApi(o, client: gateway.client()),
            installId: () async => installId,
            onSession: (s) async => adopted = s,
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();
    // 꺼진 GitHub 는 단추를 세우지 않는다. 처음 신원이 새 계정이 된다는 것을 미리 말한다.
    expect(find.byKey(const Key('oauth-github')), findsNothing);
    expect(find.textContaining('새 계정이 돼요'), findsOneWidget);

    await tester.tap(find.byKey(const Key('oauth-google')));
    await until(tester, () => find.byKey(const Key('oauth-code')).evaluate().isNotEmpty && launcher.launched.isNotEmpty);
    expect(find.text('ABCD-EFGH'), findsOneWidget);
    // 확인 화면은 앱 안 웹뷰가 아니라 Safari 로 — 구글은 앱 안 로그인을 막고, 문서도 그렇게 정했다.
    expect(launcher.launched, [('$defaultGateway/relay/oauth/authorize/req-1', PreferredLaunchMode.externalApplication)]);
    expect(clipboard, ['ABCD-EFGH']);
    expect(gateway.start(), {'provider': 'google', 'kind': 'phone', 'label': '카사모바일', 'machine_id': installId, 'link': false});
    expect(gateway.requests.firstWhere((r) => r.url.path == '/relay/oauth/start').headers['authorization'], isNull);

    await until(tester, () => adopted != null, step: const Duration(seconds: 2));
    await tester.pumpAndSettle();
    expect(adopted?.account, 'oauth_ab12');
    expect(adopted?.label, 'me@example.test');
    expect(adopted?.token, 'kdt_phone');
    expect(adopted?.origin, Uri.parse(defaultGateway));
    expect(find.text('ABCD-EFGH'), findsNothing);
  });

  Future<OAuthResult?> runSheet(
    WidgetTester tester,
    FakeGateway gateway, {
    bool link = false,
    List<Uri>? opened,
    WebAuthenticate? authenticate,
  }) async {
    OAuthResult? result;
    var finished = false;
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => TextButton(
            onPressed: () async {
              result = await showOAuthSheet(
                context,
                api: RelayAccountApi(
                  origin,
                  client: gateway.client(),
                  session: link
                      ? AccountSession(origin: origin, account: 'fixture', deviceId: 'phone-1', token: 'kdt_existing')
                      : null,
                ),
                provider: OAuthProvider.google,
                machineId: installId,
                link: link,
                open: (u) async {
                  opened?.add(u);
                  return true;
                },
                authenticate: authenticate,
              );
              finished = true;
            },
            child: const Text('go'),
          ),
        ),
      ),
    );
    await tester.tap(find.text('go'));
    for (var i = 0; i < 6 && !finished; i++) {
      await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 50)));
      await tester.pump(const Duration(seconds: 2));
    }
    await tester.pumpAndSettle();
    return result;
  }

  testWidgets('연결은 기기 토큰을 싣고 시작해 linked 로 끝난다', (tester) async {
    final opened = <Uri>[];
    final gateway = FakeGateway(finish: {'ok': true, 'status': 'linked', 'account': 'fixture'});
    final r = await runSheet(tester, gateway, link: true, opened: opened);
    expect(r?.linked, isTrue);
    expect(r?.session, isNull);
    expect(opened, [Uri.parse('$origin/relay/oauth/authorize/req-1')]);
    expect(gateway.start()['link'], isTrue);
    expect(gateway.requests.firstWhere((r) => r.url.path == '/relay/oauth/start').headers['authorization'], 'Bearer kdt_existing');
  });

  testWidgets('관문이 거절하면 그 뜻을 말하고 세션을 주지 않는다', (tester) async {
    final gateway = FakeGateway(pendingPolls: 0, finish: {'ok': false, 'error': 'account_not_linked'}, finishStatus: 403);
    var finished = false;
    OAuthResult? result;
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => TextButton(
            onPressed: () async {
              result = await showOAuthSheet(
                context,
                api: RelayAccountApi(origin, client: gateway.client()),
                provider: OAuthProvider.google,
                machineId: installId,
                open: (_) async => true,
              );
              finished = true;
            },
            child: const Text('go'),
          ),
        ),
      ),
    );
    await tester.tap(find.text('go'));
    for (var i = 0; i < 4; i++) {
      await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 50)));
      await tester.pump(const Duration(seconds: 2));
    }
    expect(find.textContaining('연결된 KASA 계정이 없어요'), findsOneWidget);
    await tester.tap(find.text('닫기'));
    await tester.pumpAndSettle();
    expect(finished, isTrue);
    expect(result, isNull);
  });

  testWidgets('취소하면 관문의 요청도 거둔다', (tester) async {
    final gateway = FakeGateway(pendingPolls: 99);
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => TextButton(
            onPressed: () => showOAuthSheet(
              context,
              api: RelayAccountApi(origin, client: gateway.client()),
              provider: OAuthProvider.google,
              machineId: installId,
              open: (_) async => true,
            ),
            child: const Text('go'),
          ),
        ),
      ),
    );
    await tester.tap(find.text('go'));
    await until(tester, () => find.text('취소').evaluate().isNotEmpty);
    // 시트가 다 올라온 뒤에 누른다 — 기다리는 동안 도는 표시 때문에 pumpAndSettle 은 못 쓴다.
    await tester.pump(const Duration(seconds: 1));
    await tester.tap(find.text('취소'));
    await tester.pump();
    await tester.pump(const Duration(seconds: 1));
    await tester.pump();
    expect(find.text('취소'), findsNothing);
    await until(tester, () => gateway.requests.any((r) => r.url.path == '/relay/oauth/cancel'));
    expect(gateway.requests.map((r) => r.url.path), contains('/relay/oauth/cancel'));
  });

  testWidgets('확인 코드 없이 — 시스템 로그인 창이 돌려준 code 를 이 앱의 verifier 로 바꿔 세션을 받는다', (tester) async {
    final gateway = FakeGateway(redirect: true);
    final shown = <(Uri, String)>[];
    final r = await runSheet(
      tester,
      gateway,
      authenticate: (url, scheme) async {
        shown.add((url, scheme));
        return Uri.parse('kasaterm://oauth?code=one-time&state=${gateway.start()['state']}');
      },
    );
    expect(r?.session?.token, 'kdt_phone');
    expect(shown, [(Uri.parse('$origin/relay/oauth/authorize/req-1'), 'kasaterm')]);
    final start = gateway.start();
    expect(start['code_challenge_method'], 'S256');
    expect(start['redirect_uri'], 'kasaterm://oauth');
    expect((start['code_challenge'] as String).length, 43);
    // 코드를 보이지도, 복사하지도, Safari 로 따로 열지도 않고, 묻지도 않는다.
    expect(clipboard, isEmpty);
    expect(launcher.launched, isEmpty);
    expect(gateway.requests.map((r) => r.url.path), isNot(contains('/relay/oauth/poll')));
  });

  testWidgets('다른 요청의 state 로 돌아온 주소는 세션으로 바꾸지 않는다', (tester) async {
    final gateway = FakeGateway(redirect: true);
    var finished = false;
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => TextButton(
            onPressed: () async {
              await showOAuthSheet(
                context,
                api: RelayAccountApi(origin, client: gateway.client()),
                provider: OAuthProvider.google,
                machineId: installId,
                authenticate: (_, _) async => Uri.parse('kasaterm://oauth?code=one-time&state=forged'),
              );
              finished = true;
            },
            child: const Text('go'),
          ),
        ),
      ),
    );
    await tester.tap(find.text('go'));
    await until(tester, () => find.text('닫기').evaluate().isNotEmpty);
    await tester.pumpAndSettle();
    expect(find.textContaining('로그인 응답을 확인하지 못했어요'), findsOneWidget);
    expect(gateway.requests.map((r) => r.url.path), isNot(contains('/relay/oauth/token')));
    await tester.tap(find.text('닫기'));
    await tester.pumpAndSettle();
    expect(finished, isTrue);
  });

  testWidgets('시스템 로그인 창을 닫으면 시트도 닫고 아무것도 바꾸지 않는다', (tester) async {
    final gateway = FakeGateway(redirect: true);
    final r = await runSheet(tester, gateway, authenticate: (_, _) async => null);
    expect(r, isNull);
    expect(gateway.requests.map((r) => r.url.path), isNot(anyOf(contains('/relay/oauth/token'), contains('/relay/oauth/cancel'))));
  });

  testWidgets('리다이렉트를 모르는 옛 관문이면 확인 코드 길로 간다', (tester) async {
    final gateway = FakeGateway();
    final opened = <Uri>[];
    final r = await runSheet(tester, gateway, opened: opened, authenticate: (_, _) async => fail('옛 관문에서 시스템 창을 열었다'));
    expect(r?.session?.token, 'kdt_phone');
    expect(gateway.start().containsKey('code_challenge'), isFalse);
    expect(opened, [Uri.parse('$origin/relay/oauth/authorize/req-1')]);
  });

  test('관문 밖으로 보내는 확인 화면 주소는 받지 않는다', () async {
    final api = RelayAccountApi(
      origin,
      client: MockClient((_) async => http.Response(
        '{"ok":true,"request_id":"r","poll_token":"p","user_code":"C","expires_in":600,"authorization_url":"https://evil.invalid/x"}',
        200,
      )),
    );
    await expectLater(api.oauthStart(OAuthProvider.google, installId), throwsA(isA<AccountException>()));
  });

  test('세션은 표시 이름을 저장했다 되읽는다', () {
    final s = AccountSession(origin: origin, account: 'oauth_ab12', deviceId: 'd', token: 'kdt_x', displayName: 'me@example.test');
    final back = AccountSession.fromJson(s.toJson().cast<String, dynamic>());
    expect(back.label, 'me@example.test');
    expect(AccountSession(origin: origin, account: 'fixture', deviceId: 'd', token: 'kdt_x').label, 'fixture');
  });
}
