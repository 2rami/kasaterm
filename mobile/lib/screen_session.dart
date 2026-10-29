import 'dart:async';
import 'dart:convert';
import 'dart:math' as math;
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/painting.dart';
import 'package:flutter/scheduler.dart';
import 'package:web_socket_channel/web_socket_channel.dart';

import 'server.dart';
import 'term_socket.dart';

enum ScreenState { connecting, live, reconnecting, denied }

class ScreenDisplay {
  const ScreenDisplay({
    required this.id,
    required this.width,
    required this.height,
    required this.main,
  });

  final int id;
  final int width;
  final int height;
  final bool main;

  static ScreenDisplay? parse(Object? raw) {
    if (raw is! Map) return null;
    final id = raw['id'];
    if (id is! int) return null;
    return ScreenDisplay(
      id: id,
      width: (raw['width'] as num?)?.toInt() ?? 0,
      height: (raw['height'] as num?)?.toInt() ?? 0,
      main: raw['main'] == true,
    );
  }
}

/// 그림 상자 안의 자리를 그 화면 안의 비율(0–1)로 — 조작 좌표는 그림 크기와 상관없이 이것으로 오간다.
(double, double) screenFraction(Offset local, Size box) => (
  box.width <= 0 ? 0.5 : (local.dx / box.width).clamp(0.0, 1.0),
  box.height <= 0 ? 0.5 : (local.dy / box.height).clamp(0.0, 1.0),
);

/// 한 프레임 바이트에서 (seq, meta 끝) — 모자라면 null. 꼴은 `docs/screen-mirror.md`.
(int, int)? screenFrameHead(Uint8List b) {
  if (b.length < 6) return null;
  final head = ByteData.sublistView(b, 0, 6);
  final end = 6 + head.getUint16(4);
  return b.length < end ? null : (head.getUint32(0), end);
}

/// 기기 화면 하나와의 소켓 — JPEG 프레임을 [image] 로 풀고, 다 푼 뒤에 ack 한다.
/// ack 가 서버의 보낼 몫을 채워 주므로 폰이 그릴 수 있는 만큼만 온다(`docs/screen-mirror.md`).
class ScreenSession extends ChangeNotifier {
  ScreenSession(
    this.server, {
    this.machine,
    this.ping = const Duration(seconds: 15),
    this.connectWait = const Duration(seconds: 10),
  }) {
    server.addCloseListener(_serverClosed);
    server.routeChanges?.addListener(_routeChanged);
  }

  final Server server;
  final String? machine;
  final Duration ping;
  final Duration connectWait;

  ScreenState state = ScreenState.connecting;

  /// 서버가 못 뜬다고 한 까닭 — `permission`(맥 화면 기록 권한) · `unavailable`(화면 없는 기기·옛 판).
  String? denied;

  /// 조작을 못 넣은 까닭 — `permission`(맥 손쉬운 사용 권한) · `unavailable`(옛 판·지원 안 하는 OS). 넣히면 null.
  String? inputDenied;

  /// 관문이 거절한 까닭 같은, 다시 붙어도 안 풀리는 사정.
  String? note;
  List<ScreenDisplay> displays = const [];
  int? display;
  ui.Image? image;

  /// 지금 소켓이 카사넷 직통인가.
  bool get direct => _direct;

  /// 최근 2초에 받은 장 수로 센 초당 장 수. 화면이 멈추면 서버가 안 보내므로 0 으로 떨어진다.
  int get fps {
    final now = _clock.elapsedMilliseconds;
    _shownAt.removeWhere((t) => now - t > 2000);
    return (_shownAt.length / 2).round();
  }

  final Stopwatch _clock = Stopwatch()..start();
  final List<int> _shownAt = [];
  WebSocketChannel? _channel;
  StreamSubscription<Object?>? _sub;
  Timer? _retry;
  Timer? _opening;
  bool _direct = false;
  bool _gateway = false;
  bool _paused = false;
  bool _disposed = false;
  int _generation = 0;
  int _backoffSec = 1;
  int _shownSeq = 0;

  void _serverClosed() {
    pause();
    note = '로그인 연결이 종료되었어요.';
    if (!_disposed) notifyListeners();
  }

  /// 직통이면 크게, 관문이면 작게 — 관문은 큰 프레임 하나가 그 기기의 업링크를 붙든다.
  Uri _wsUri({bool gateway = false}) {
    Uri at(Map<String, String> query) => server.wsUri(
      'screen/ws',
      query: query,
      machine: machine,
      gateway: gateway,
    );
    final direct = server.isDirect(at(const {}).replace(scheme: 'http'));
    final (edge, quality) = direct ? (1920, 60) : (1280, 50);
    return at({
      'w': '$edge',
      'h': '$edge',
      'q': '$quality',
      'fps': '8',
      if (display != null) 'display': '$display',
    });
  }

  void connect() {
    _retry?.cancel();
    _retry = null;
    _closeChannel();
    if (_paused || _disposed || server.isClosed) return;
    if (state != ScreenState.connecting) state = ScreenState.reconnecting;
    final generation = _generation;
    final uri = _wsUri(gateway: _gateway);
    _direct = server.isDirect(uri.replace(scheme: 'http'));
    final ch = connectTermSocket(uri, protocols: server.wsProtocolsFor(uri), ping: ping);
    _channel = ch;
    // 악수가 끝나도 그 뒤 기기가 말을 안 하면 붙은 것이 아니다 — hello 를 기다린다.
    _opening = Timer(connectWait, () {
      if (generation == _generation) _lost();
    });
    var opened = false;
    _sub = ch.stream.listen(
      (data) {
        if (generation == _generation) _onData(data);
      },
      onError: (Object _) {
        if (generation == _generation) _lost(refused: opened ? null : uri);
      },
      onDone: () {
        if (generation == _generation) _lost(refused: opened ? null : uri);
      },
      cancelOnError: true,
    );
    ch.ready.then<void>(
      (_) => opened = true,
      onError: (Object _) {
        if (generation == _generation) _lost(refused: uri);
      },
    );
    notifyListeners();
  }

  void _routeChanged() {
    if (_channel == null || _paused || _disposed || state == ScreenState.connecting) return;
    if (server.isDirect(_wsUri().replace(scheme: 'http')) == _direct) return;
    _gateway = false;
    connect();
  }

  void _onData(Object? data) {
    if (data is String) {
      _control(data);
    } else if (data is Uint8List) {
      unawaited(_frame(data));
    } else if (data is List<int>) {
      unawaited(_frame(Uint8List.fromList(data)));
    }
  }

  void _control(String raw) {
    final Object? m;
    try {
      m = jsonDecode(raw);
    } catch (_) {
      return;
    }
    if (m is! Map) return;
    switch (m['t']) {
      case 'hello':
        _opening?.cancel();
        _opening = null;
        _backoffSec = 1;
        note = null;
        final list = m['displays'];
        displays = list is List
            ? list.map(ScreenDisplay.parse).whereType<ScreenDisplay>().toList()
            : const [];
        display = (m['display'] as num?)?.toInt() ?? display;
        if (state != ScreenState.live) state = ScreenState.connecting;
      case 'denied':
        denied = m['reason'] as String? ?? 'unavailable';
        state = ScreenState.denied;
      case 'input':
        inputDenied = m['ok'] == true ? null : (m['reason'] as String? ?? 'unavailable');
      default:
        return;
    }
    notifyListeners();
  }

  Future<void> _frame(Uint8List bytes) async {
    final head = screenFrameHead(bytes);
    if (head == null) return;
    final (seq, start) = head;
    final generation = _generation;
    ui.Image? decoded;
    try {
      final codec = await ui.instantiateImageCodec(Uint8List.sublistView(bytes, start));
      decoded = (await codec.getNextFrame()).image;
      codec.dispose();
    } catch (_) {}
    if (generation != _generation || _disposed) {
      decoded?.dispose();
      return;
    }
    // 다 풀고 나서 ack — 그래야 이 폰이 풀 수 있는 만큼만 온다.
    _sendJson({'t': 'ack', 'seq': seq});
    // 두 장을 함께 풀면 늦게 온 장이 먼저 끝날 수 있다 — 옛 장으로 되돌아가지 않는다.
    if (decoded == null || seq <= _shownSeq) {
      decoded?.dispose();
      return;
    }
    _shownSeq = seq;
    final old = image;
    image = decoded;
    // 그리는 쪽이 아직 옛 장을 들고 있을 수 있어 한 프레임 뒤에 놓는다.
    if (old != null) SchedulerBinding.instance.addPostFrameCallback((_) => old.dispose());
    _shownAt.add(_clock.elapsedMilliseconds);
    denied = null;
    state = ScreenState.live;
    notifyListeners();
  }

  void pickDisplay(int id) {
    if (id == display) return;
    display = id;
    _sendJson({'t': 'tune', 'display': id});
    notifyListeners();
  }

  /// 지금 화면의 포인트 크기 — 끌기 거리를 그 기기의 휠 픽셀로 바꿀 때. 모르면 받은 그림 크기.
  Size get displaySize {
    for (final d in displays) {
      if (d.id == display && d.width > 0 && d.height > 0) return Size(d.width.toDouble(), d.height.toDouble());
    }
    final i = image;
    return i == null ? const Size(1, 1) : Size(i.width.toDouble(), i.height.toDouble());
  }

  /// 단추 상태(1 왼쪽·2 오른쪽)를 통째로 — 서버가 앞 상태와 견줘 누름·뗌을 가른다.
  void pointer(double x, double y, int buttons) =>
      _sendJson({'t': 'pointer', 'x': x, 'y': y, 'b': buttons});

  void click(double x, double y, {bool right = false}) {
    pointer(x, y, right ? 2 : 1);
    pointer(x, y, 0);
  }

  /// 휠 — 그 기기 화면 포인트 단위, 양수 dy 는 위로 굴림.
  void scroll(double x, double y, int dx, int dy) {
    if (dx == 0 && dy == 0) return;
    _sendJson({'t': 'scroll', 'x': x, 'y': y, 'dx': dx, 'dy': dy});
  }

  /// 이름 붙은 키(웹 `KeyboardEvent.code`)를 눌렀다 뗀다.
  void key(String code) => _sendJson({'t': 'key', 'k': code});

  /// 폰 자판이 조합을 끝낸 글을 통째로 — 한글을 키로 풀어 보내면 그 기기 입력기가 다시 조합해 어긋난다.
  void typeText(String text) {
    if (text.isEmpty) return;
    _sendJson({'t': 'text', 's': text});
  }

  void _lost({Uri? refused}) {
    if (_disposed || _paused || _channel == null) return;
    final fallback = _direct && !_gateway;
    if (fallback) _gateway = true;
    _closeChannel();
    state = ScreenState.reconnecting;
    _retry = Timer(fallback ? Duration.zero : Duration(seconds: _backoffSec), connect);
    _backoffSec = math.min(_backoffSec * 2, 10);
    notifyListeners();
    if (refused != null && !fallback) unawaited(_explain(refused));
  }

  Future<void> _explain(Uri refused) async {
    final generation = _generation;
    final why = await server.socketRefusal(refused);
    if (_disposed || generation != _generation || why == null || why == note) return;
    note = why;
    notifyListeners();
  }

  /// 앱이 뒤로 가면 먼저 닫는다 — 보지도 않는 화면을 기기가 계속 뜨지 않게.
  void pause() {
    _paused = true;
    _retry?.cancel();
    _retry = null;
    _closeChannel();
  }

  void resume() {
    if (_disposed || server.isClosed) return;
    _paused = false;
    _backoffSec = 1;
    // 터미널과 같은 까닭으로 관문부터 — iOS 가 재운 직통은 한동안 살아 있는 척한다.
    _gateway = true;
    connect();
  }

  void _sendJson(Map<String, Object?> m) => _channel?.sink.add(jsonEncode(m));

  void _closeChannel() {
    _generation++;
    _shownSeq = 0;
    _opening?.cancel();
    _opening = null;
    _sub?.cancel();
    _sub = null;
    _channel?.sink.close();
    _channel = null;
  }

  @override
  void dispose() {
    _disposed = true;
    server.removeCloseListener(_serverClosed);
    server.routeChanges?.removeListener(_routeChanged);
    _retry?.cancel();
    _closeChannel();
    image?.dispose();
    image = null;
    super.dispose();
  }
}
