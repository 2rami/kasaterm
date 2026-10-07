import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/resume_spot.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/term_session.dart';

class _NativeHttp extends HttpOverrides {}

/// 연결마다 첫 요청엔 답하고, 같은 연결로 온 다음 요청은 답 없이 끊는다 — 앱이 멈춰 있던 사이 끊긴 연결을 다트가
/// 묶음에서 다시 집어 쓰는 모양 그대로다.
Future<ServerSocket> _staleKeepAlive() async {
  final server = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
  server.listen((sock) {
    var served = 0;
    var buf = '';
    sock.listen(
      (data) {
        buf += latin1.decode(data);
        while (buf.contains('\r\n\r\n')) {
          final end = buf.indexOf('\r\n\r\n') + 4;
          final head = buf.substring(0, end);
          buf = buf.substring(end);
          final length = RegExp(r'content-length: (\d+)', caseSensitive: false).firstMatch(head);
          if (length != null) buf = buf.substring(int.parse(length.group(1)!).clamp(0, buf.length));
          if (++served > 1) {
            sock.destroy();
            return;
          }
          sock.add(latin1.encode(
            'HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n'
            'connection: keep-alive\r\n\r\n[]',
          ));
        }
      },
      onError: (Object _) {},
    );
  });
  return server;
}

/// 사이에 끼어 바이트를 넘기다가 [cut] 이면 연결을 닫지 않고 삼킨다 — 망을 갈아탄 폰의 반쯤 열린 소켓.
class _Blackhole {
  _Blackhole._(this._socket, this.target);
  final ServerSocket _socket;
  final int target;
  bool cut = false;
  int get port => _socket.port;

  static Future<_Blackhole> to(int target) async {
    final b = _Blackhole._(await ServerSocket.bind(InternetAddress.loopbackIPv4, 0), target);
    b._socket.listen((client) async {
      final up = await Socket.connect(InternetAddress.loopbackIPv4, b.target);
      client.listen((d) { if (!b.cut) up.add(d); }, onDone: up.destroy, onError: (Object _) => up.destroy());
      up.listen((d) { if (!b.cut) client.add(d); }, onDone: client.destroy, onError: (Object _) => client.destroy());
    });
    return b;
  }

  Future<void> close() => _socket.close();
}

class _CountingClient extends http.BaseClient {
  _CountingClient(this.name, this.log);
  final String name;
  final List<String> log;
  @override
  Future<http.StreamedResponse> send(http.BaseRequest request) async {
    log.add('$name ${request.method}');
    return http.StreamedResponse(Stream.value(utf8.encode('[]')), 200);
  }

  @override
  void close() => log.add('$name close');
}

Future<void> _until(bool Function() done, {int tries = 80}) async {
  for (var i = 0; i < tries && !done(); i++) {
    await Future<void>.delayed(const Duration(milliseconds: 50));
  }
}

void main() {
  test(
    '끊긴 묶음 연결을 집은 GET 은 「닿지 못했다」 대신 새 연결로 한 번 더 간다',
    () => HttpOverrides.runWithHttpOverrides(() async {
      final stale = await _staleKeepAlive();
      final root = Uri.parse('http://127.0.0.1:${stale.port}/');

      // 고치기 전 길 그대로 — 묶음의 연결을 다시 쓴 둘째 요청이 서버에 닿지도 못한다.
      final bare = http.Client();
      await bare.get(root.resolve('term/panes'));
      await expectLater(bare.get(root.resolve('term/panes')), throwsA(isA<http.ClientException>()));
      bare.close();

      final server = Server(root);
      expect(await server.panes(), isEmpty);
      expect(await server.panes(), isEmpty);
      server.close();
      await stale.close();
    }, _NativeHttp()),
  );

  test('POST 는 다시 보내지 않는다 — 데스크톱이 받고 답만 끊겼으면 두 번 들어간다', () async {
    var posts = 0;
    final client = OriginClient(
      Uri.parse('https://gateway.invalid'),
      client: MockClient((request) async {
        if (request.method == 'POST') {
          posts++;
          throw http.ClientException('Connection closed before full header was received');
        }
        return http.Response('[]', 200);
      }),
    );
    await expectLater(
      client.post(Uri.parse('https://gateway.invalid/term/send'), body: 'x'),
      throwsA(isA<http.ClientException>()),
    );
    expect(posts, 1);
    client.close();
  });

  test('돌아오면 연결 묶음을 갈아 끼우고 옛 묶음은 닫는다', () async {
    final log = <String>[];
    var made = 0;
    final client = OriginClient(
      Uri.parse('https://gateway.invalid'),
      connect: () => _CountingClient('c${made++}', log),
    );
    await client.get(Uri.parse('https://gateway.invalid/a'));
    client.freshConnections();
    await client.get(Uri.parse('https://gateway.invalid/b'));
    expect(log, ['c0 GET', 'c0 close', 'c1 GET']);
    client.close();
    expect(log.last, 'c1 close');
  });

  test(
    '반쯤 열린 터미널 소켓은 핑으로 알아채 다시 붙는다',
    () => HttpOverrides.runWithHttpOverrides(() async {
      var opened = 0;
      final desk = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
      desk.listen((request) async {
        if (request.uri.path != '/term/ws') {
          request.response.statusCode = 404;
          await request.response.close();
          return;
        }
        final ws = await WebSocketTransformer.upgrade(request);
        opened++;
        ws.add(jsonEncode({'t': 'size', 'cols': 40, 'rows': 10, 'mirror': true}));
        ws.listen((_) {}, onError: (Object _) {});
      });
      final hole = await _Blackhole.to(desk.port);
      final server = Server(Uri.parse('http://127.0.0.1:${hole.port}/'));
      const pane = Pane(id: '%3', name: '이즈나', title: '', status: 'idle', window: 0, cwd: '/');
      final s = TermSession(server, pane, ping: const Duration(milliseconds: 300))..connect();
      await _until(() => s.state == TermState.connected);
      expect(s.state, TermState.connected);

      hole.cut = true;
      await _until(() => s.state == TermState.reconnecting);
      expect(s.state, TermState.reconnecting);

      hole.cut = false;
      await _until(() => s.state == TermState.connected && opened == 2);
      expect(s.state, TermState.connected);
      expect(opened, 2);

      s.dispose();
      server.close();
      await hole.close();
      await desk.close(force: true);
    }, _NativeHttp()),
  );

  test('보던 자리는 같은 연결·한 시간 안에만 이어받는다', () {
    final at = DateTime(2026, 10, 7, 12);
    final spot = ResumeSpot(
      scope: 'https://kasaterm.debimarlene.com|2rami',
      pane: '%12',
      machine: '~book',
      view: 'chat',
      chatDraft: '이어서 써',
      at: at,
    );
    final back = ResumeSpot.fromJson(jsonDecode(jsonEncode(spot.toJson())))!;
    expect(back.scope, spot.scope);
    expect(back.pane, '%12');
    expect(back.machine, '~book');
    expect(back.view, 'chat');
    expect(back.chatDraft, '이어서 써');
    expect(back.termDraft, '');
    expect(back.freshAt(at.add(const Duration(minutes: 59))), isTrue);
    expect(back.freshAt(at.add(ResumeSpot.keep)), isFalse);
    expect(ResumeSpot.fromJson({'scope': 'x', 'pane': '', 'at': 1}), isNull);
    expect(ResumeSpot.fromJson('garbage'), isNull);
  });
}
