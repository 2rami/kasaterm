import 'dart:async';
import 'dart:convert';
import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:web_socket_channel/web_socket_channel.dart';

import 'grid.dart';
import 'server.dart';
import 'term_socket.dart';

enum TermState { connecting, connected, reconnecting, gone }

/// pane 하나와의 소켓 — 격자 프레임을 받아 [grid] 에 반영하고, 키 바이트를 보낸다.
///
/// 규약: binary 프레임이 키 입력, text 프레임이 제어 JSON 이다. 키를 text 로
/// 보내면 서버가 JSON 으로 읽고 조용히 버린다.
class TermSession extends ChangeNotifier {
  TermSession(this.server, this.pane, {this.holdIdle = defaultHoldIdle}) {
    server.addCloseListener(_serverClosed);
    server.routeChanges?.addListener(_routeChanged);
  }

  /// 폰에서 마지막으로 친 뒤 이만큼 조용하면 쥔 원본 격자를 놓는다.
  static const defaultHoldIdle = Duration(seconds: 60);

  final Server server;
  final Pane pane;
  final Grid grid = Grid();

  /// 살아 있는 화면 위의 지난 줄(오래된 순). 붙을 때 한 번 받고(`history`), 그 뒤로
  /// 화면이 위로 밀릴 때마다 서버가 흘려 준다(`scrolled`). 위로 넘겨 읽는 데 쓴다.
  /// 대체 화면(Claude 전체 화면·vim)인 동안은 비운다 — 그 화면엔 지난 줄이 없고, 원본이 대체
  /// 화면을 줄일 때 밀려 올라간 윗줄이 `scrolled` 로 와서 전체 화면 위에 옛 머리말이 겹쳐 보였다
  /// (2026-10-05 「풀스크린으로 바뀌고 나오는 거」). 나오면 원본의 지난 줄을 다시 받는다.
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

  /// 지금 소켓이 카사넷 입구(직통)로 붙었나.
  bool _direct = false;
  StreamSubscription<Object?>? _sub;
  Timer? _retry;

  /// 폰 터미널 보기가 원본 격자를 쥘 크기. 원본 크기는 입력하는 쪽이 쥔다(docs/webterm-handoff.md
  /// 「원본 크기는 쓰는 쪽이 쥔다」) — 보기만 해서는 안 쥐고, 폰에서 키·답장을 보내면 그때 쥔다.
  /// 열면 바로 쥐던 때(10-05)는 폰으로 들여다보기만 해도 데스크톱 칸이 폰 크기로 줄었다(10-06 제보).
  (int, int)? _viewSize;
  bool _holdView = false;

  /// 서버가 `viewport_latest` 를 안다 — 모르는 옛 호스트엔 크기를 안 보낸다.
  bool _canHold = false;

  /// 이 연결이 쥐겠다고 보낸 크기. 연결이 끊기면 서버가 놓아 준다.
  (int, int)? _held;

  /// 폰에서 친 뒤 [holdIdle] 이 아직 안 지났다. 원본의 사람이나 더 늦게 친 거울이 가져가면
  /// (`lost`) 바로 내린다 — 폰에서 다시 칠 때까지 보기만으로는 도로 빼앗지 않는다.
  bool _typing = false;
  final Duration holdIdle;
  Timer? _idleTimer;
  Timer? _viewTimer;
  int _backoffSec = 1;
  bool _paused = false;
  bool _disposed = false;
  int _generation = 0;

  void _serverClosed() {
    pause();
    state = TermState.gone;
    note = '로그인 연결이 종료되었어요.';
    if (!_disposed) notifyListeners();
  }

  void connect() {
    _retry?.cancel();
    _retry = null;
    _closeChannel();
    if (_paused || _disposed || server.isClosed || state == TermState.gone) return;
    final generation = _generation;
    if (tokens == null) {
      server.designTokens(machine: pane.machine).then((t) {
        if (t == null || _disposed || server.isClosed || generation != _generation) return;
        tokens = t;
        notifyListeners();
      });
    }
    final uri = _wsUri();
    _direct = server.isDirect(uri.replace(scheme: 'http'));
    final ch = connectTermSocket(uri, protocols: server.wsProtocolsFor(uri));
    _channel = ch;
    var opened = false;
    _sub = ch.stream.listen(
      (data) { if (generation == _generation) _onData(data); },
      onError: (Object _) { if (generation == _generation) _lost(refused: opened ? null : uri); },
      onDone: () { if (generation == _generation) _lost(refused: opened ? null : uri); },
      cancelOnError: true,
    );
    ch.ready.then<void>(
      (_) { opened = true; },
      onError: (Object _) { if (generation == _generation) _lost(refused: uri); },
    );
  }

  Uri _wsUri() => server.wsUri(
    'term/ws',
    query: {'pane': pane.id, 'grid': '1'},
    machine: pane.machine,
  );

  /// 직통이 섰거나 잃었다. 잃은 쪽은 입구가 소켓을 끊어 [_lost] 로 관문에 다시 붙는다 — 여기서는 관문에 붙어 있던
  /// 소켓을 직통으로 옮긴다. 붙은 채로 두면 화면이 끝까지 관문 왕복(100ms 대)으로 온다.
  void _routeChanged() {
    if (_channel == null || _paused || _disposed || state != TermState.connected) return;
    final now = server.isDirect(_wsUri().replace(scheme: 'http'));
    if (now == _direct) return;
    _closeChannel();
    connect();
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
        note = null;
        _backoffSec = 1;
        _sendJson({'t': 'history', 'rows': historyAsk});
        // 크기가 바뀔 때마다 오는 size 에는 capabilities 가 없다 — 첫 악수에서만 판정한다.
        final caps = msg['capabilities'];
        if (caps is Map) {
          _canHold = caps['viewport_latest'] != null;
          _held = null;
          _syncView();
        }
      case 'viewport':
        if (msg['lost'] == true) {
          _held = null;
          _rest();
        } else if (msg['granted'] != true && _held != null) {
          _held = null;
        }
        return;
      case 'history':
        if (grid.alt) return;
        history
          ..clear()
          ..addAll(_rows(msg['rows']));
        historyVersion++;
      case 'scrolled':
        if (grid.alt) return;
        history.addAll(_rows(msg['rows']));
        if (history.length > historyMax) {
          history.removeRange(0, history.length - historyMax);
        }
        historyVersion++;
      case 'grid':
        final wasAlt = grid.alt;
        grid.apply(msg);
        if (state != TermState.connected) state = TermState.connected;
        if (grid.alt && !wasAlt && history.isNotEmpty) {
          history.clear();
          historyVersion++;
        } else if (wasAlt && !grid.alt) {
          _sendJson({'t': 'history', 'rows': historyAsk});
        }
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

  /// [refused] 는 악수조차 못 마친 소켓의 주소다. 망 유실과 달리 관문이 거절한 것이면(계정에 없는 기계 등) 몇 번을
  /// 다시 붙어도 같으니, 「다시 연결 중」만 띄우지 않고 그 까닭을 [note] 로 보인다.
  void _lost({Uri? refused}) {
    if (_disposed || _paused || state == TermState.gone) return;
    if (_channel == null) return;
    _closeChannel();
    state = TermState.reconnecting;
    notifyListeners();
    _retry = Timer(Duration(seconds: _backoffSec), connect);
    _backoffSec = math.min(_backoffSec * 2, 10);
    if (refused != null) _explain(refused);
  }

  Future<void> _explain(Uri refused) async {
    final why = await server.socketRefusal(refused);
    if (_disposed || state != TermState.reconnecting || why == note) return;
    note = why;
    notifyListeners();
  }

  bool get canSend => !server.isClosed && state == TermState.connected && _channel != null;

  void sendBytes(List<int> bytes) {
    final ch = _channel;
    if (ch == null || !canSend) return;
    _touched();
    ch.sink.add(Uint8List.fromList(bytes));
  }

  /// 터미널 보기가 접지 않고 담을 열·줄. 키보드가 오르내리는 동안 프레임마다 바뀌므로 멈춘 뒤에 보낸다.
  void setViewport(int cols, int rows) {
    if (_viewSize == (cols, rows)) return;
    _viewSize = (cols, rows);
    _viewTimer?.cancel();
    _viewTimer = Timer(const Duration(milliseconds: 250), _syncView);
  }

  /// 폰 폭으로 접은 터미널 보기가 보이는 동안만 쥘 수 있다 — 대화 보기·데스크톱 격자 그대로
  /// 보기에선 원본 격자가 폰 크기일 까닭이 없어 놓는다.
  set holdViewport(bool on) {
    if (_holdView == on) return;
    _holdView = on;
    _syncView();
  }

  void _touched() {
    _idleTimer?.cancel();
    _idleTimer = Timer(holdIdle, _rest);
    if (_typing) return;
    _typing = true;
    // 자판이 오르내리는 중이면 멈춘 크기로 쥔다 — 그 사이엔 터미널 칸이 한 줄짜리인 프레임도 있다
    // (가상 아이폰 실측: 첫 키에 42×1 을 쥐었다가 곧 42×33).
    if (_viewTimer == null) _syncView();
  }

  void _rest() {
    _idleTimer?.cancel();
    _idleTimer = null;
    if (!_typing) return;
    _typing = false;
    _syncView();
  }

  void _syncView() {
    _viewTimer?.cancel();
    _viewTimer = null;
    if (!_canHold || _channel == null || state != TermState.connected) return;
    final want = _holdView && _typing ? _viewSize : null;
    if (want == _held) return;
    if (want == null) {
      _sendJson({'t': 'viewport', 'op': 'release'});
    } else {
      _sendJson({
        't': 'viewport',
        'op': _held == null ? 'acquire' : 'resize',
        'cols': want.$1,
        'rows': want.$2,
      });
    }
    _held = want;
  }

  void sendText(String text) => sendBytes(utf8.encode(text));

  /// 지난 내용을 앱이 굴리는 화면인가 — 대체 화면에 마우스 보고(SGR)를 켠 앱(Claude 전체 화면)은
  /// 터미널 스크롤백이 0 이라(2026-10-05 실기 「스크롤이 안 돼」) 손가락 세로 끌기를 휠로 넘긴다.
  bool get scrollsApp => grid.alt && grid.mouse && grid.mouseSgr;

  /// 데스크톱 휠과 같은 SGR(64 위·65 아래, `input.rs handle_wheel`). [lines] 가 양수면 위(지난 내용).
  /// 휠은 보기라 원본 크기를 쥐는 입력으로 치지 않는다 — [sendBytes] 를 거치지 않는다.
  void wheel(int lines) {
    final ch = _channel;
    if (lines == 0 || ch == null || !canSend || !scrollsApp) return;
    // 줄이 아니라 그 pane 안인지가 중요하다 — 가운데는 Claude 의 대화 칸이다.
    final at = '${grid.cols ~/ 2 + 1};${grid.rows ~/ 2 + 1}';
    final one = '\x1b[<${lines > 0 ? 64 : 65};${at}M';
    ch.sink.add(Uint8List.fromList(utf8.encode(one * math.min(lines.abs(), 8))));
  }

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
    _touched();
    if (pane.isWebShell) {
      sendText('$text\r');
      return;
    }
    await server.send(pane.id, text, machine: pane.machine);
  }

  /// 글자 바로 뒤에 붙여 보내면 Ink 가 Enter 를 먹는다 — 서버 `send` 가 기다리는 것과 같은 틈.
  static const enterGap = Duration(milliseconds: 150);

  /// 사진을 붙여 둔 입력상자에 글을 더해 보낸다. [reply] 의 서버 `send` 는 상자를 Ctrl+U 로
  /// 비우고 붙이므로 먼저 들어간 `[Image #1]` 까지 지워 사진이 안 간다(2026-10-02 대화 보기 제보).
  Future<void> replyAfterAttachment(String text) async {
    if (state != TermState.connected) {
      throw const ServerException('연결이 끊겨 보내지 못했어요. 다시 연결되면 보내 주세요.');
    }
    _touched();
    // 앞 공백 — 붙인 자리표 바로 뒤에 붙으면 `[Image #1]글` 로 한 덩이가 된다.
    sendText('\x1b[200~ ${text.replaceAll('\x1b', '')}\x1b[201~');
    await Future<void>.delayed(enterGap);
    sendText('\r');
  }

  /// 앱이 뒤로 가면 우리가 먼저 닫는다 — iOS 가 소켓을 죽인 채 두면 복귀 때
  /// 「끊김」인지 판단이 늦다.
  void pause() {
    _paused = true;
    _retry?.cancel();
    _retry = null;
    // 폰을 떠났다 — 돌아와 다시 붙어도 칠 때까지는 보기만 한다. 쥔 크기는 소켓이 닫히며 서버가 놓는다.
    _typing = false;
    _idleTimer?.cancel();
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
    _generation++;
    _held = null;
    _viewTimer?.cancel();
    _viewTimer = null;
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
    _idleTimer?.cancel();
    _closeChannel();
    super.dispose();
  }
}
