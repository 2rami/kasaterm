import 'dart:async';
import 'dart:convert';
import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:web_socket_channel/web_socket_channel.dart';

import 'server.dart';

/// 서버→폰 제어 프레임(`GET mobile/ws`, text 프레임 = JSON). 모르는 것은 null.
sealed class ControlFrame {
  const ControlFrame();

  static ControlFrame? parse(String text) {
    final Object? raw;
    try {
      raw = jsonDecode(text);
    } catch (_) {
      return null;
    }
    if (raw is! Map) return null;
    final m = raw.cast<String, Object?>();
    return switch (m['t']) {
      'hello' => HelloFrame(
        name: m['name'] as String? ?? '',
        id: m['id'] as String? ?? '',
      ),
      'open-url' => OpenUrlFrame.fromJson(m),
      _ => null,
    };
  }
}

/// 접속 직후 — 서버가 이 폰을 누구로 보는지.
class HelloFrame extends ControlFrame {
  const HelloFrame({required this.name, required this.id});
  final String name;

  /// `phone:<이름>` — 브라우징 기기 목록에서 이 폰의 자리.
  final String id;
}

/// 「이 주소를 이 폰에서 열어라」. `mode` 가 web 이면 앱 안 웹 화면, 아니면 브라우저.
class OpenUrlFrame extends ControlFrame {
  const OpenUrlFrame({required this.url, required this.web, required this.req});

  final Uri url;
  final bool web;

  /// 답할 때 되돌려 줄 번호. 서버가 수로 보내도 글자로 되돌린다.
  final String req;

  static OpenUrlFrame? fromJson(Map<String, Object?> m) {
    final u = m['url'];
    final url = u is String ? Uri.tryParse(u) : null;
    if (url == null || !url.hasScheme) return null;
    final req = m['req'];
    return OpenUrlFrame(
      url: url,
      web: m['mode'] == 'web',
      req: req == null ? '' : '$req',
    );
  }

  /// 폰→서버 `opened` 답장.
  String opened({required bool ok, String? error}) => jsonEncode({
    't': 'opened',
    'req': req,
    'ok': ok,
    if (!ok) 'error': error ?? '열지 못했다',
  });
}

enum ControlState { connecting, connected, reconnecting, paused }

/// 폰 제어 채널 — 앱이 앞에 있는 동안 서버에 붙어 `open-url` 을 받아 연다.
/// 백오프·pause/resume 규약은 [TermSession] 과 같다. text 프레임만 오간다.
class ControlSession extends ChangeNotifier {
  ControlSession(this.server, {required this.onOpenUrl});

  final Server server;

  /// 열고 나서 정상 반환하면 `ok:true`, 던지면 그 문구로 `ok:false` 를 답한다.
  final Future<void> Function(OpenUrlFrame frame) onOpenUrl;

  ControlState state = ControlState.connecting;

  /// 서버가 알려 준 이 폰의 이름·id(`hello`).
  String? name;
  String? id;

  /// iOS 가 조용한 소켓을 끊지 않게 살아 있음 표시 — 서버는 무시한다.
  static const pingEvery = Duration(seconds: 25);

  WebSocketChannel? _channel;
  StreamSubscription<Object?>? _sub;
  Timer? _retry;
  Timer? _ping;
  int _backoffSec = 1;
  bool _paused = false;
  bool _disposed = false;

  @protected
  WebSocketChannel openChannel() => WebSocketChannel.connect(server.controlUri());

  void connect() {
    _retry?.cancel();
    _retry = null;
    _closeChannel();
    if (_paused || _disposed) return;
    final ch = openChannel();
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
    handle(data);
  }

  /// 프레임 하나를 처리한다 — 테스트가 소켓 없이 부른다.
  @visibleForTesting
  void handle(String text) {
    final frame = ControlFrame.parse(text);
    switch (frame) {
      case HelloFrame():
        name = frame.name;
        id = frame.id;
        state = ControlState.connected;
        _backoffSec = 1;
        _ping ??= Timer.periodic(pingEvery, (_) => _send('{"t":"ping"}'));
        notifyListeners();
      case OpenUrlFrame():
        if (state != ControlState.connected) {
          state = ControlState.connected;
          notifyListeners();
        }
        _open(frame);
      case null:
        return;
    }
  }

  Future<void> _open(OpenUrlFrame frame) async {
    String reply;
    try {
      await onOpenUrl(frame);
      reply = frame.opened(ok: true);
    } catch (e) {
      reply = frame.opened(ok: false, error: '$e');
    }
    _send(reply);
  }

  void _send(String text) {
    final ch = _channel;
    if (ch == null || _disposed) return;
    ch.sink.add(text);
  }

  void _lost() {
    if (_disposed || _paused) return;
    if (_channel == null) return;
    _closeChannel();
    state = ControlState.reconnecting;
    notifyListeners();
    _retry = Timer(Duration(seconds: _backoffSec), connect);
    _backoffSec = math.min(_backoffSec * 2, 10);
  }

  /// 앱이 뒤로 가면 우리가 먼저 닫는다 — 서버의 「폰 online」 표시가 사실과 맞고,
  /// 복귀 때 죽은 소켓을 살아 있다고 믿는 일이 없다.
  void pause() {
    _paused = true;
    _retry?.cancel();
    _retry = null;
    _closeChannel();
    state = ControlState.paused;
  }

  void resume() {
    if (!_paused) return;
    _paused = false;
    _backoffSec = 1;
    state = ControlState.connecting;
    notifyListeners();
    connect();
  }

  void _closeChannel() {
    _ping?.cancel();
    _ping = null;
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
