import 'dart:async';
import 'dart:io';

import 'package:web_socket_channel/web_socket_channel.dart';

import 'server.dart';
import 'term_socket.dart';

/// 데스크톱의 `127.0.0.1:<port>`(개발 서버)를 이 폰의 `localhost:<localPort>` 로 끌어온다 — 앱 안 웹뷰가
/// localhost 로 열어 개발 서버 bind·secure context·절대 주소가 그대로 돈다. 데스크톱과 같은 포트를 먼저 잡는다.
///
/// 연결마다 데스크톱 `/net/tcp?port=N` 웹소켓 하나(`docs/kasanet.md` P3). 길은 [Server.wsUri] 가 고른다 — 직통이면
/// 카사넷 입구, 아니면 관문. 틀: 바이너리 = 날 바이트, 텍스트 `eof` = 보낸 쪽이 쓰기를 닫음(반쯤 닫기).
class NetTcpBridge {
  NetTcpBridge._(this.server, this.port, this.machine, this._listeners);

  final Server server;
  final int port;
  final String? machine;
  final List<ServerSocket> _listeners;
  final Set<Socket> _open = {};
  final Set<WebSocketChannel> _channels = {};

  int get localPort => _listeners.first.port;

  static const _eof = 'eof';

  static Future<NetTcpBridge> start(
    Server server, {
    required int port,
    String? machine,
  }) async {
    ServerSocket v4;
    try {
      v4 = await ServerSocket.bind(InternetAddress.loopbackIPv4, port);
    } on SocketException {
      v4 = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
    }
    final listeners = [v4];
    // 웹뷰가 localhost 를 ::1 로 먼저 풀 수 있다 — 같은 포트로 v6 도 들어 둔다(못 들면 v4 만).
    try {
      listeners.add(
        await ServerSocket.bind(
          InternetAddress.loopbackIPv6,
          v4.port,
          v6Only: true,
        ),
      );
    } on SocketException {
      // v4 만으로도 WebKit 은 다음 주소로 넘어간다.
    }
    final bridge = NetTcpBridge._(server, port, machine, listeners);
    for (final l in listeners) {
      l.listen(bridge._pipe);
    }
    return bridge;
  }

  void _pipe(Socket local) {
    final uri = server.wsUri(
      'net/tcp',
      query: {'port': '$port'},
      machine: machine,
    );
    final ch = connectTermSocket(uri, protocols: server.wsProtocolsFor(uri));
    _open.add(local);
    _channels.add(ch);
    void done() {
      _open.remove(local);
      _channels.remove(ch);
      local.destroy();
      ch.sink.close();
    }

    local.listen(
      ch.sink.add,
      onDone: () => ch.sink.add(_eof),
      onError: (Object _) => done(),
      cancelOnError: true,
    );
    ch.stream.listen(
      (m) {
        if (m is List<int>) {
          local.add(m);
        } else if (m == _eof) {
          unawaited(local.close().catchError((Object _) => local));
        }
      },
      onDone: done,
      onError: (Object _) => done(),
      cancelOnError: true,
    );
    ch.ready.catchError((Object _) => done());
  }

  Future<void> close() async {
    for (final l in _listeners) {
      await l.close();
    }
    for (final s in _open.toList()) {
      s.destroy();
    }
    for (final c in _channels.toList()) {
      await c.sink.close();
    }
    _open.clear();
    _channels.clear();
  }
}
