import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

import '../lib/push.dart';
import '../lib/server.dart';

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

  setUp(() {
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(channel, (call) async {
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

  test('failed disconnect cleanup is retried before another server registers', () async {
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
      'first:register', 'first:remove', 'first:remove',
      'first:remove', 'second:register',
    ]);
  });
}
