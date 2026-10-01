import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/term_session.dart';

class _NativeHttp extends HttpOverrides {}

/// 계정 관문 흉내 — 계정에 묶인 기본 기계만 소켓을 열고, 그 기계의 기계 목록에만 실려 오는 남의 기계(`m/~mini/`)는
/// 실제 관문처럼 업그레이드 전에 503 `account_device_unavailable` 로 거절한다.
Future<HttpServer> _gateway() async {
  final http = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
  http.listen((request) async {
    if (request.uri.path.startsWith('/relay/account/m/~mini/')) {
      request.response
        ..statusCode = 503
        ..headers.contentType = ContentType.json
        ..write(jsonEncode({'ok': false, 'error': 'account_device_unavailable'}));
      await request.response.close();
      return;
    }
    if (request.uri.path == '/relay/account/term/ws' && WebSocketTransformer.isUpgradeRequest(request)) {
      final socket = await WebSocketTransformer.upgrade(
        request,
        protocolSelector: (p) => p.contains('kasa-relay-account') ? 'kasa-relay-account' : null,
      );
      socket.add(jsonEncode({'t': 'size', 'cols': 29, 'rows': 22, 'mirror': true, 'id': '%3'}));
      socket.listen((_) {});
      return;
    }
    request.response.statusCode = 404;
    await request.response.close();
  });
  return http;
}

Future<void> _settle(TermSession s, bool Function() done) async {
  for (var i = 0; i < 60 && !done(); i++) {
    await Future<void>.delayed(const Duration(milliseconds: 50));
  }
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  Pane pane(String? machine) =>
      Pane(id: '%3', name: '이즈나', title: '', status: 'active', window: 0, cwd: '/', machine: machine);

  test(
    'a machine the account cannot reach says why instead of reconnecting silently',
    () => HttpOverrides.runWithHttpOverrides(() async {
      final gateway = await _gateway();
      final server = Server.account(AccountSession(
        origin: Uri.parse('http://127.0.0.1:${gateway.port}'),
        account: 'fixture', deviceId: 'phone-fixture', token: 'test-device-token',
      ));
      final s = TermSession(server, pane('~mini'))..connect();
      await _settle(s, () => s.note != null);
      expect(s.state, TermState.reconnecting);
      expect(s.note, contains('계정'));
      s.dispose();
      server.close();
      await gateway.close(force: true);
    }, _NativeHttp()),
  );

  test(
    'the account machine itself still opens with no note',
    () => HttpOverrides.runWithHttpOverrides(() async {
      final gateway = await _gateway();
      final server = Server.account(AccountSession(
        origin: Uri.parse('http://127.0.0.1:${gateway.port}'),
        account: 'fixture', deviceId: 'phone-fixture', token: 'test-device-token',
      ));
      final s = TermSession(server, pane(null))..connect();
      await _settle(s, () => s.state == TermState.connected);
      expect(s.state, TermState.connected);
      expect((s.grid.cols, s.grid.rows), (29, 22));
      expect(s.note, isNull);
      s.dispose();
      server.close();
      await gateway.close(force: true);
    }, _NativeHttp()),
  );
}
