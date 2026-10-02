import 'dart:convert';

import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';

import 'package:kasaterm_mobile/app_link.dart';
import 'package:kasaterm_mobile/push.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/server.dart';
import 'relay_account_test.dart' show session;

class PushServer extends Server {
  PushServer(this.label, this.events, {this.ready = true})
    : super(Uri.parse('https://$label.invalid/'));

  final String label;
  final List<String> events;
  final bool ready;
  bool failRemoval = false;

  @override
  Future<bool> registerPushToken(String token, String env) async {
    events.add('$label:register');
    return ready;
  }

  @override
  Future<void> unregisterPushToken(String token) async {
    events.add('$label:remove');
    if (failRemoval) throw ServerException('offline');
  }
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  const channel = MethodChannel('kasaterm/push');
  final nativeCalls = <String>[];

  setUp(() {
    nativeCalls.clear();
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(channel, (call) async {
          nativeCalls.add(call.method);
          return call.method == 'token'
              ? {'token': 'test-device-token', 'env': 'dev'}
              : null;
        });
  });

  tearDown(() {
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(channel, null);
  });

  test('계정 폰은 토큰을 관문에 맡기고, 승인 알림을 누르면 그 승인 화면 링크가 간다', () async {
    final posted = <String, Object?>{};
    final bridge = PushBridge.forTesting();
    bridge.accountApi = (s) => RelayAccountApi(
      s.origin,
      session: s,
      client: MockClient((req) async {
        posted['path'] = req.url.path;
        posted['body'] = jsonDecode(req.body);
        return http.Response('{"ok":true,"ready":true}', 200);
      }),
    );
    final links = <AppLink>[];
    await bridge.bindAccount(session(), (link) async => links.add(link));
    expect(nativeCalls, contains('request'));
    expect(posted['path'], '/relay/approvals/push');
    expect(posted['body'], {'token': 'test-device-token', 'env': 'dev'});

    const codec = StandardMethodCodec();
    await TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger.handlePlatformMessage(
      'kasaterm/push',
      codec.encodeMethodCall(const MethodCall('onTap', {'kind': 'approval', 'approval': 'apv_1'})),
      (_) {},
    );
    expect(links.single.approval, 'apv_1');
    expect(links.single.pane, isNull);
  });

  test('server without a push key still owns its saved token', () async {
    final events = <String>[];
    final first = PushServer('first', events, ready: false);
    final second = PushServer('second', events);
    final bridge = PushBridge.forTesting();
    await bridge.bind(first, (_) async {});
    await bridge.bind(second, (_) async {});
    expect(events, ['first:register', 'first:remove', 'second:register']);
  });

  test(
    'failed disconnect cleanup is retried before another server registers',
    () async {
      final events = <String>[];
      final first = PushServer('first', events);
      final second = PushServer('second', events);
      final bridge = PushBridge.forTesting();
      await bridge.bind(first, (_) async {});
      first.failRemoval = true;
      bridge.unbind();
      await bridge.bind(second, (_) async {});
      expect(events, ['first:register', 'first:remove', 'first:remove']);
      first.failRemoval = false;
      await bridge.bind(second, (_) async {});
      expect(events, [
        'first:register',
        'first:remove',
        'first:remove',
        'first:remove',
        'second:register',
      ]);
    },
  );

  RelayAccountApi Function(AccountSession) gatewayOnly(List<String> paths) => (s) => RelayAccountApi(
    s.origin,
    session: s,
    client: MockClient((req) async {
      paths.add(req.url.path);
      return http.Response('{"ok":true}', 200);
    }),
  );

  test(
    'account mode drops desktop subscriptions and gives the token only to the gateway for approvals',
    () async {
      final paths = <String>[];
      final bridge = PushBridge.forTesting()..accountApi = gatewayOnly(paths);
      final account = Server.account(session());
      await bridge.bind(account, (_) async {});
      expect(nativeCalls.first, 'suspend');
      expect(nativeCalls, contains('request'));
      expect(paths, ['/relay/approvals/push']);
      account.close();
    },
  );

  test(
    'closed original connection is still removed before account mode',
    () async {
      final events = <String>[];
      final paths = <String>[];
      final old = PushServer('old', events);
      final bridge = PushBridge.forTesting()..accountApi = gatewayOnly(paths);
      await bridge.bind(old, (_) async {});
      old.close();
      final account = Server.account(session());
      await bridge.bind(account, (_) async {});
      expect(events, ['old:register', 'old:remove']);
      expect(nativeCalls, containsAllInOrder(['suspend', 'request']));
      expect(paths, ['/relay/approvals/push']);
      expect(bridge.cleanupPending, isFalse);
      account.close();
    },
  );

  test(
    'failed old subscription cleanup remains pending and retries on reconnect',
    () async {
      final events = <String>[];
      final old = PushServer('old', events);
      final bridge = PushBridge.forTesting();
      await bridge.bind(old, (_) async {});
      old.failRemoval = true;
      old.close();
      expect(await bridge.unbind(), isFalse);
      expect(bridge.cleanupPending, isTrue);
      old.failRemoval = false;
      await bridge.retryCleanup();
      expect(bridge.cleanupPending, isFalse);
      expect(events, ['old:register', 'old:remove', 'old:remove']);
    },
  );
}
