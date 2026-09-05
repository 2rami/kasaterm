import 'dart:async';
import 'dart:convert';
import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:web_socket_channel/web_socket_channel.dart';

import 'grid.dart';
import 'server.dart';

enum TermState { connecting, connected, reconnecting, gone }

/// pane 하나와의 소켓 — 격자 프레임을 받아 [grid] 에 반영하고, 키 바이트를 보낸다.
///
/// 규약: binary 프레임이 키 입력, text 프레임이 제어 JSON 이다. 키를 text 로
/// 보내면 서버가 JSON 으로 읽고 조용히 버린다.
class TermSession extends ChangeNotifier {
  TermSession(this.server, this.pane);

  final Server server;
  final Pane pane;
  final Grid grid = Grid();

  /// 살아 있는 화면 위의 지난 줄(오래된 순). 붙을 때 한 번 받고(`history`), 그 뒤로
  /// 화면이 위로 밀릴 때마다 서버가 흘려 준다(`scrolled`). 위로 넘겨 읽는 데 쓴다.
  final List<List<Run>> history = [];
  int historyVersion = 0;
  static const historyMax = 3000;
  static const historyAsk = 400;

  TermState state = TermState.connecting;
  String? note;
  bool mirror = true;

  /// 데스크톱 색. 세션마다 한 번 받는다 — 기계마다 테마가 다를 수 있어 pane 의 기계로 묻는다.
  DesignTokens? tokens;

  WebSocketChannel? _channel;
  StreamSubscription<Object?>? _sub;
  Timer? _retry;
  int _backoffSec = 1;
  bool _paused = false;
  bool _disposed = false;

  void connect() {
    _retry?.cancel();
    _retry = null;
    _closeChannel();
    if (_paused || _disposed || state == TermState.gone) return;
    if (tokens == null) {
      server.designTokens(machine: pane.machine).then((t) {
        if (t == null || _disposed) return;
        tokens = t;
        notifyListeners();
      });
    }
    final ch = WebSocketChannel.connect(
      server.wsUri(
        'term/ws',
        query: {'pane': pane.id, 'grid': '1'},
        machine: pane.machine,
      ),
    );
    _channel = ch;
    _sub = ch.stream.listen(
      _onData,
      onError: (Object _) => _lost(),
      onDone: _lost,
      cancelOnError: true,
    );
    ch.ready.catchError((Object _) => _lost());
  }

  void _onData(Object? data) {
    if (data is! String) return;
    final Object? m;
    try {
      m = jsonDecode(data);
    } catch (_) {
      return;
    }
    if (m is! Map) return;
    final msg = m.cast<String, Object?>();
    switch (msg['t']) {
      case 'size':
        grid.apply({'cols': msg['cols'], 'rows': msg['rows']});
        mirror = msg['mirror'] == true;
        state = TermState.connected;
        _backoffSec = 1;
        _sendJson({'t': 'history', 'rows': historyAsk});
      case 'history':
        history
          ..clear()
          ..addAll(_rows(msg['rows']));
        historyVersion++;
      case 'scrolled':
        history.addAll(_rows(msg['rows']));
        if (history.length > historyMax) {
          history.removeRange(0, history.length - historyMax);
        }
        historyVersion++;
      case 'grid':
        grid.apply(msg);
        if (state != TermState.connected) state = TermState.connected;
      case 'gone':
        // 세션이 진짜 끝났다 — 유실과 달리 다시 붙을 곳이 없다.
        state = TermState.gone;
        note = '이 학생의 화면이 끝났다';
        _closeChannel();
      default:
        return;
    }
    notifyListeners();
  }

  void _lost() {
    if (_disposed || _paused || state == TermState.gone) return;
    if (_channel == null) return;
    _closeChannel();
    state = TermState.reconnecting;
    notifyListeners();
    _retry = Timer(Duration(seconds: _backoffSec), connect);
    _backoffSec = math.min(_backoffSec * 2, 10);
  }

  bool get canSend => state == TermState.connected && _channel != null;

  void sendBytes(List<int> bytes) {
    final ch = _channel;
    if (ch == null || state != TermState.connected) return;
    ch.sink.add(Uint8List.fromList(bytes));
  }

  void sendText(String text) => sendBytes(utf8.encode(text));

  /// 방향키는 앱이 DECCKM 을 켰는지에 따라 SS3 여야 한다 — CSI 로 보내면 claude·vim
  /// 의 줄 이동이 조용히 무시된다.
  void arrow(String letter) =>
      sendText('${grid.appCursor ? '\x1bO' : '\x1b['}$letter');

  void ctrl(String key) {
    final c = key.toUpperCase().codeUnitAt(0);
    if (c >= 64 && c <= 95) sendBytes([c - 64]);
  }

  /// 답장 한 줄. 학생 pane 은 서버가 Enter 타이밍을 맡는 `send` 로, 웹 셸은
  /// 그 창구가 없어 소켓으로 직접.
  Future<void> reply(String text) async {
    if (pane.isWebShell) {
      sendText('$text\r');
      return;
    }
    await server.send(pane.id, text, machine: pane.machine);
  }

  /// 앱이 뒤로 가면 우리가 먼저 닫는다 — iOS 가 소켓을 죽인 채 두면 복귀 때
  /// 「끊김」인지 판단이 늦다.
  void pause() {
    _paused = true;
    _retry?.cancel();
    _retry = null;
    _closeChannel();
  }

  void resume() {
    if (!_paused) return;
    _paused = false;
    _backoffSec = 1;
    if (state == TermState.gone) return;
    state = TermState.connecting;
    notifyListeners();
    connect();
  }

  void _sendJson(Map<String, Object?> m) {
    _channel?.sink.add(jsonEncode(m));
  }

  static List<List<Run>> _rows(Object? raw) => [
    if (raw is List)
      for (final row in raw)
        if (row is List)
          [
            for (final r in row)
              if (r is List && r.length >= 4) Run.parse(r),
          ],
  ];

  void _closeChannel() {
    _sub?.cancel();
    _sub = null;
    _channel?.sink.close();
    _channel = null;
  }

  @override
  void dispose() {
    _disposed = true;
    _retry?.cancel();
    _closeChannel();
    super.dispose();
  }
}
