import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/net_tcp.dart';
import 'package:kasaterm_mobile/server.dart';

/// 데스크톱 `/net/tcp` 흉내: 바이너리는 되돌려 주고, `eof` 를 받으면 제 `eof` 를 보낸 뒤 닫는다.
Future<(HttpServer, List<String>)> fakeDesktop() async {
  final seen = <String>[];
  final http = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
  http.listen((req) async {
    seen.add(req.uri.toString());
    if (req.uri.path != '/net/tcp') {
      req.response.statusCode = 404;
      await req.response.close();
      return;
    }
    final ws = await WebSocketTransformer.upgrade(req);
    ws.listen((m) {
      if (m is List<int>) {
        ws.add(m);
      } else if (m == 'eof') {
        ws.add('eof');
        ws.close();
      }
    });
  });
  return (http, seen);
}

void main() {
  test('폰 localhost 연결이 /net/tcp 웹소켓으로 가고 반쯤 닫기가 끝까지 간다', () async {
    final (desktop, seen) = await fakeDesktop();
    addTearDown(() => desktop.close(force: true));
    final server = Server(Uri.parse('http://127.0.0.1:${desktop.port}/'));
    addTearDown(server.close);
    final bridge = await NetTcpBridge.start(server, port: 4173);
    addTearDown(bridge.close);

    final s = await Socket.connect(
      InternetAddress.loopbackIPv4,
      bridge.localPort,
    );
    final got = <int>[];
    final closed = Completer<void>();
    s.listen(got.addAll, onDone: closed.complete);
    s.add(utf8.encode('GET / HTTP/1.0\r\n\r\n'));
    await s.close();
    await closed.future.timeout(const Duration(seconds: 5));
    expect(
      utf8.decode(got),
      'GET / HTTP/1.0\r\n\r\n',
      reason: '쓰기를 닫은 뒤에도 답을 끝까지 받는다',
    );
    expect(seen.single, '/net/tcp?port=4173');
    s.destroy();
  });

  test('아무도 안 들으면 폰 쪽 연결도 닫힌다', () async {
    final server = Server(Uri.parse('http://127.0.0.1:9/'));
    addTearDown(server.close);
    final bridge = await NetTcpBridge.start(server, port: 4174);
    addTearDown(bridge.close);
    final s = await Socket.connect(
      InternetAddress.loopbackIPv4,
      bridge.localPort,
    );
    final closed = Completer<void>();
    s.listen(
      (_) {},
      onDone: closed.complete,
      onError: (_) => closed.complete(),
    );
    await closed.future.timeout(const Duration(seconds: 5));
    s.destroy();
  });
}
