import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/kasanet.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/term_session.dart';

class _NativeHttp extends HttpOverrides {}

class _Native implements KasanetNative {
  _Native(this.port);
  final int port;
  @override
  String get id => 'fixture-phone';
  @override
  int open(String _) => port;
  @override
  KasanetPath state(int _) => const KasanetPath(direct: true);
  @override
  void networkChanged() {}
  @override
  String? get lastError => null;
}

class _Host {
  _Host(this.http);
  final HttpServer http;
  final sockets = <WebSocket>[];
  final addresses = <Uri>[];
  final keys = <String>[];
  bool frames = true;

  static Future<_Host> start() async {
    final host = _Host(await HttpServer.bind(InternetAddress.loopbackIPv4, 0));
    host.http.listen((request) async {
      if (WebSocketTransformer.isUpgradeRequest(request)) {
        final ws = await WebSocketTransformer.upgrade(request,
          protocolSelector: (p) => p.contains('kasa-relay-account') ? 'kasa-relay-account' : null);
        host.sockets.add(ws);
        host.addresses.add(request.uri);
        if (host.frames) host.frame(ws);
        ws.listen((data) {
          if (data is List<int>) host.keys.add(utf8.decode(data));
        }, onError: (Object _) {});
      } else {
        request.response.headers.contentType = ContentType.json;
        request.response.write(jsonEncode(request.uri.path.endsWith('/version')
            ? {'machine_id': 'mini', 'kasanet': {'id': 'fixture-desk'}}
            : request.uri.path.endsWith('/kasanet/phone')
            ? {'ok': true, 'ttl_secs': 900}
            : {}));
        await request.response.close();
      }
    });
    return host;
  }

  void frame(WebSocket ws) {
    ws.add(jsonEncode({'t': 'size', 'cols': 42, 'rows': 20, 'mirror': true}));
    ws.add(jsonEncode({'t': 'grid', 'dirty': [[0, [['same pane', null, null, 0]]]]}));
  }

  Future<void> close() async {
    for (final socket in sockets) { unawaited(socket.close()); }
    await http.close(force: true);
  }

  Server server({KasanetNative? native}) => Server.account(AccountSession(
    origin: Uri.parse('http://127.0.0.1:${http.port}'), account: 'fixture',
    deviceId: 'fixture-phone', token: 'fixture-token',
  ), kasanet: native);
}

const _pane = Pane(id: '%3', name: 'fixture', title: '', status: 'idle', window: 0,
  cwd: '/', machine: '~mini', session: 'same-session');

Future<void> _until(bool Function() ready) async {
  for (var n = 0; n < 150 && !ready(); n++) {
    await Future<void>.delayed(const Duration(milliseconds: 20));
  }
  expect(ready(), isTrue, reason: 'bounded reconnect');
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test('default hub rows keep their machine even when the account hub changes before the read', () async {
    var hub = 'first';
    final calls = <Uri>[];
    final server = Server.account(AccountSession(origin: Uri.parse('https://fixture.invalid'),
      account: 'fixture', deviceId: 'phone', token: 'fixture-token'), client: MockClient((r) async {
      calls.add(r.url);
      if (r.url.path == '/relay/account/version') {
        final id = hub;
        hub = 'second';
        return http.Response(jsonEncode({'machine_id': id}), 200);
      }
      if (r.url.path.endsWith('/term/panes')) {
        return http.Response(jsonEncode([{'id': '%3', 'session': 'same-session'}]), 200);
      }
      return http.Response('{}', 200);
    }));
    addTearDown(server.close);
    final pane = (await server.panes()).single;
    expect(pane.machine, '~first');
    expect(calls.last.path, '/relay/account/m/~first/term/panes');
    expect((await server.panes()).single.machine, '~second');
    expect(server.wsUri('term/ws', query: {'pane': pane.id}, machine: pane.machine, gateway: true).path,
      '/relay/account/m/~first/term/ws');
    await server.send(pane.id, 'after resume', machine: pane.machine);
    expect(calls.last.path, '/relay/account/m/~first/send');
  });

  test('unknown default-machine identity never falls back to a mutable hub pane', () async {
    final paths = <String>[];
    final server = Server.account(AccountSession(origin: Uri.parse('https://fixture.invalid'),
      account: 'fixture', deviceId: 'phone', token: 'fixture-token'), client: MockClient((r) async {
      paths.add(r.url.path);
      return http.Response('{}', 200);
    }));
    addTearDown(server.close);
    await expectLater(server.panes(), throwsA(isA<ServerException>()));
    expect(paths, ['/relay/account/version']);
  });

  test('short return replaces a silent socket, keeps its target and never replays keys',
      () => HttpOverrides.runWithHttpOverrides(() async {
    final host = await _Host.start();
    final server = host.server();
    final term = TermSession(server, _pane, ping: const Duration(minutes: 5))..connect();
    addTearDown(() async { term.dispose(); server.close(); await host.close(); });
    await _until(() => term.canSend);
    term.sendText('once');
    await _until(() => host.keys.length == 1);
    final old = host.sockets.single;
    host.frames = false;
    term.resume();
    expect(term.canSend, isFalse);
    term.sendText('must-not-send');
    await _until(() => host.sockets.length == 2);
    old.add(jsonEncode({'t': 'gone'}));
    await Future<void>.delayed(const Duration(milliseconds: 30));
    expect(term.state, isNot(TermState.gone));
    expect(term.grid.cols, 42, reason: 'last screen is not cleared while reconnecting');
    host.frame(host.sockets.last);
    await _until(() => term.canSend);
    term.sendText('after');
    await _until(() => host.keys.length == 2);
    expect(host.keys, ['once', 'after']);
    expect(host.addresses.map((u) => u.path).toSet(), {'/relay/account/m/~mini/term/ws'});
    expect(host.addresses.every((u) => u.queryParameters['pane'] == '%3'), isTrue);
    expect(term.pane.session, 'same-session');
    server.close();
    term.resume();
    await Future<void>.delayed(const Duration(milliseconds: 50));
    expect(host.sockets, hasLength(2), reason: 'logout cannot be revived');
  }, _NativeHttp()));

  test('a stale direct path falls back to the same device gateway when no frame arrives',
      () => HttpOverrides.runWithHttpOverrides(() async {
    final gateway = await _Host.start();
    final direct = await _Host.start()..frames = false;
    final server = gateway.server(native: _Native(direct.http.port));
    server.uri('term/panes', machine: '~mini');
    await _until(() => server.pathOf('~mini')?.$1 == true);
    final term = TermSession(server, _pane, connectWait: const Duration(milliseconds: 120))..connect();
    addTearDown(() async { term.dispose(); server.close(); await direct.close(); await gateway.close(); });
    await _until(() => term.canSend);
    expect(direct.sockets, hasLength(1));
    expect(gateway.sockets, hasLength(1));
    expect(gateway.addresses.single.path, '/relay/account/m/~mini/term/ws');
    expect(gateway.addresses.single.queryParameters['pane'], '%3');
    term.sendText('only once');
    await _until(() => gateway.keys.isNotEmpty);
    expect(gateway.keys, ['only once']);
    expect(direct.keys, isEmpty);
    term.resume();
    await _until(() => gateway.sockets.length == 2 && term.canSend);
    expect(direct.sockets, hasLength(1), reason: 'resume bypasses the still-stale native route');
  }, _NativeHttp()));

  test('attachment Enter cannot cross a reconnect or close',
      () => HttpOverrides.runWithHttpOverrides(() async {
    final host = await _Host.start();
    final server = host.server();
    final term = TermSession(server, _pane)..connect();
    addTearDown(() async { term.dispose(); server.close(); await host.close(); });
    await _until(() => term.canSend);
    final sending = term.replyAfterAttachment('draft');
    final rejected = expectLater(sending, throwsA(isA<ServerException>()));
    await _until(() => host.keys.isNotEmpty);
    term.resume();
    await _until(() => host.sockets.length == 2 && term.canSend);
    await rejected;
    expect(host.keys, ['\x1b[200~ draft\x1b[201~']);
    final enter = term.sendTextAfter('\n', TermSession.enterGap);
    final noEnter = expectLater(enter, throwsA(isA<ServerException>()));
    term.pause();
    term.resume();
    await _until(() => host.sockets.length == 3 && term.canSend);
    await noEnter;
    expect(host.keys, hasLength(1), reason: 'live-input delayed Enter has the same connection guard');
  }, _NativeHttp()));
}
