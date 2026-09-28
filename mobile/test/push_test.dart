import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:kasaterm_mobile/push.dart';
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

  test(
    'account mode suspends native push and never requests or registers APNs',
    () async {
      final bridge = PushBridge.forTesting();
      final account = Server.account(session());
      await bridge.bind(account, (_) async {});
      expect(nativeCalls, ['suspend']);
      expect(nativeCalls, isNot(contains('request')));
      account.close();
    },
  );

  test(
    'closed original connection is still removed before account mode',
    () async {
      final events = <String>[];
      final old = PushServer('old', events);
      final bridge = PushBridge.forTesting();
      await bridge.bind(old, (_) async {});
      old.close();
      final account = Server.account(session());
      await bridge.bind(account, (_) async {});
      expect(events, ['old:register', 'old:remove']);
      expect(nativeCalls.last, 'suspend');
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
