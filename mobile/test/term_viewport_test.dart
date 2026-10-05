import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/term_session.dart';

class _NativeHttp extends HttpOverrides {}

/// 원본 칸 흉내 — 악수 `size` 를 보내고, 폰이 보낸 제어(JSON)를 쌓는다.
class _Host {
  _Host(this.http);
  final HttpServer http;
  final control = <Map<String, Object?>>[];
  WebSocket? socket;

  static Future<_Host> start({required bool latest}) async {
    final host = _Host(await HttpServer.bind(InternetAddress.loopbackIPv4, 0));
    host.http.listen((request) async {
      if (!WebSocketTransformer.isUpgradeRequest(request)) {
        request.response.statusCode = 404;
        await request.response.close();
        return;
      }
      final socket = await WebSocketTransformer.upgrade(
        request,
        protocolSelector: (p) =>
            p.contains('kasa-relay-account') ? 'kasa-relay-account' : null,
      );
      host.socket = socket;
      socket.add(
        jsonEncode({
          't': 'size',
          'cols': 24,
          'rows': 15,
          'mirror': true,
          'id': '%3',
          if (latest)
            'capabilities': {'mirror_viewport': 1, 'viewport_latest': 1},
        }),
      );
      socket.listen((m) {
        if (m is String) {
          final v = (jsonDecode(m) as Map).cast<String, Object?>();
          if (v['t'] == 'viewport') host.control.add(v);
        }
      });
    });
    return host;
  }

  void send(Map<String, Object?> m) => socket!.add(jsonEncode(m));
}

Future<void> _until(bool Function() done) async {
  for (var i = 0; i < 80 && !done(); i++) {
    await Future<void>.delayed(const Duration(milliseconds: 25));
  }
}

Future<void> _quiet() => Future<void>.delayed(const Duration(milliseconds: 400));

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  final pane = Pane(
    id: '%3',
    name: '유우카',
    title: '',
    status: 'active',
    window: 0,
    cwd: '/',
  );

  Server serverFor(_Host host) => Server.account(
    AccountSession(
      origin: Uri.parse('http://127.0.0.1:${host.http.port}'),
      account: 'fixture',
      deviceId: 'phone-fixture',
      token: 'test-device-token',
    ),
  );

  test(
    '열면 원본을 폰 크기로 쥐고, 원본 쪽이 되찾으면 폰에서 칠 때까지 기다린다',
    () => HttpOverrides.runWithHttpOverrides(() async {
      final host = await _Host.start(latest: true);
      final s = TermSession(serverFor(host), pane)
        ..holdViewport = true
        ..setViewport(42, 30)
        ..connect();
      await _until(() => host.control.isNotEmpty);
      expect(host.control, [
        {'t': 'viewport', 'op': 'acquire', 'cols': 42, 'rows': 30},
      ]);

      // 자판이 오르내리는 동안의 중간 크기는 모아서 마지막 하나만.
      s
        ..setViewport(42, 24)
        ..setViewport(42, 18);
      await _until(() => host.control.length > 1);
      await _quiet();
      expect(host.control.skip(1), [
        {'t': 'viewport', 'op': 'resize', 'cols': 42, 'rows': 18},
      ]);

      host.send({'t': 'viewport', 'granted': false, 'lost': true});
      await _quiet();
      s.setViewport(42, 30);
      await _quiet();
      expect(host.control, hasLength(2), reason: '원본에서 만진 뒤엔 보기만으로 도로 빼앗지 않는다');

      s.sendText('a');
      await _until(() => host.control.length > 2);
      expect(host.control.last, {
        't': 'viewport',
        'op': 'acquire',
        'cols': 42,
        'rows': 30,
      });

      s.holdViewport = false;
      await _until(() => host.control.length > 3);
      expect(host.control.last, {'t': 'viewport', 'op': 'release'});

      s.dispose();
      await host.http.close(force: true);
    }, _NativeHttp()),
  );

  test(
    '크기 계약을 모르는 옛 호스트엔 크기를 보내지 않는다',
    () => HttpOverrides.runWithHttpOverrides(() async {
      final host = await _Host.start(latest: false);
      final s = TermSession(serverFor(host), pane)
        ..holdViewport = true
        ..setViewport(42, 30)
        ..connect();
      await _until(() => s.state == TermState.connected);
      await _quiet();
      expect(host.control, isEmpty);
      s.dispose();
      await host.http.close(force: true);
    }, _NativeHttp()),
  );
}
