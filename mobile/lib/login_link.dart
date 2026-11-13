import 'dart:async';
import 'dart:io';

import 'relay_account.dart' show oauthRedirectScheme;

/// 학생이 사람에게 맡기는 CLI 로그인 — 「이 주소를 열고, 돌아온 localhost:포트/?code=… 를 보내 달라」
/// (docs/conversation-asks.md). 사람이 열 [url] 과, 로그인 끝에 브라우저가 넘어올 루프백 자리.
class LoginAsk {
  const LoginAsk(this.url, this.port, {this.host = 'localhost', this.path = ''});

  final Uri url;
  final int port;

  /// 돌아올 주소의 호스트 — 학생 CLI 가 기다리는 꼴 그대로 돌려준다(localhost·127.0.0.1·::1).
  final String host;

  /// 돌아올 경로 — 그 경로로 온 것만 받는다. 비면 아무 경로.
  final String path;

  @override
  bool operator ==(Object other) =>
      other is LoginAsk && other.url == url && other.port == port && other.host == host && other.path == path;

  @override
  int get hashCode => Object.hash(url, port, host, path);
}

final _urlPattern = RegExp(r'''https?://[^\s<>"'`()\[\]{}]+''');

/// 학생 말에 적은 돌아올 자리 — `localhost:53682/?code=…` 처럼 포트와 `code` 가 함께 있어야 한다.
final _backHint = RegExp(r'(?:https?://)?(localhost|127\.0\.0\.1|\[::1\]):(\d{2,5})(/[^\s`"<>]*)');

const _redirectKeys = {'redirect_uri', 'redirect_url', 'redirecturi', 'redirect', 'callback', 'callback_url'};

bool _loopback(String host) => host == 'localhost' || host == '127.0.0.1' || host == '::1' || host == '[::1]';

/// 대화 글 꼬리의 `.`·`,`·마크다운 굵게 따위는 주소가 아니다.
String _trimUrl(String s) {
  var end = s.length;
  while (end > 0 && '.,;:!?*_~>\''.contains(s[end - 1])) {
    end--;
  }
  return s.substring(0, end);
}

/// 쿼리(또는 그 안에 한두 겹 싸인 주소의 쿼리)에서 루프백으로 돌아오는 redirect 를 찾는다.
Uri? _loopbackRedirect(Uri u, int depth) {
  final Map<String, List<String>> query;
  try {
    query = u.queryParametersAll;
  } on FormatException {
    return null;
  }
  for (final e in query.entries) {
    for (final v in e.value) {
      if (_redirectKeys.contains(e.key.toLowerCase())) {
        final back = Uri.tryParse(v);
        if (back != null && back.scheme == 'http' && _loopback(back.host)) return back;
      }
      if (depth < 2 && (v.contains('?') || v.contains('://'))) {
        final inner = Uri.tryParse(v);
        final back = inner == null ? null : _loopbackRedirect(inner, depth + 1);
        if (back != null) return back;
      }
    }
  }
  return null;
}

/// 학생 말 한 덩이에서 로그인 요청을 읽는다. 신호는 둘(새 명령·표식 없이 학생이 이미 쓰는 말 그대로):
/// 1. 주소 쿼리의 `redirect_uri` 가 루프백(`http://localhost:<포트>/…`) — OAuth 루프백 흐름(RFC 8252)은 이것을 꼭 싣는다.
/// 2. 주소를 줄여 redirect 가 안 보이면, 같은 말에 돌아올 자리를 `localhost:<포트>/?code=…` 꼴로 적은 것.
LoginAsk? findLoginAsk(String text) {
  final urls = <Uri>[];
  for (final m in _urlPattern.allMatches(text)) {
    final u = Uri.tryParse(_trimUrl(m.group(0)!));
    if (u != null && u.host.isNotEmpty && !_loopback(u.host)) urls.add(u);
  }
  if (urls.isEmpty) return null;
  for (final u in urls) {
    final back = _loopbackRedirect(u, 0);
    if (back != null) {
      return LoginAsk(u, back.port, host: back.host, path: back.path == '/' ? '' : back.path);
    }
  }
  for (final m in _backHint.allMatches(text)) {
    if (!m.group(3)!.contains('code')) continue;
    final port = int.tryParse(m.group(2)!);
    if (port == null || port <= 0 || port > 65535) continue;
    final host = m.group(1) == '[::1]' ? '::1' : m.group(1)!;
    return LoginAsk(urls.first, port, host: host);
  }
  return null;
}

class LoginCatchException implements Exception {
  const LoginCatchException(this.message);
  final String message;

  @override
  String toString() => message;
}

/// 그 포트의 루프백에서 로그인 끝에 넘어오는 주소를 받는다. 폰에는 학생의 CLI 가 없으니 시스템 로그인 창이
/// `http://localhost:<포트>/…` 로 넘어오는 순간 이 앱이 그 자리에서 받는다. 루프백에만 열어 다른 기기는 못 넣는다.
class LoopbackCatch {
  LoopbackCatch._(this.ask, this._servers);

  final LoginAsk ask;
  final List<HttpServer> _servers;
  final _caught = Completer<Uri>();

  Future<Uri> get caught => _caught.future;

  static Future<LoopbackCatch> open(LoginAsk ask) async {
    final servers = <HttpServer>[];
    // localhost 는 ::1 로 먼저 갈 수 있다 — 두 루프백에 다 연다.
    for (final address in [InternetAddress.loopbackIPv4, InternetAddress.loopbackIPv6]) {
      try {
        servers.add(await HttpServer.bind(address, ask.port, v6Only: address.type == InternetAddressType.IPv6));
      } on SocketException {
        // 한쪽만 열려도 된다.
      }
    }
    if (servers.isEmpty) {
      throw LoginCatchException('이 폰에서 ${ask.port} 번 자리를 열지 못했어요. 돌아온 주소를 복사해 붙여 넣어 주세요.');
    }
    final c = LoopbackCatch._(ask, servers);
    for (final s in servers) {
      s.listen((r) => unawaited(c._serve(r)), onError: (Object _) {});
    }
    return c;
  }

  Future<void> _serve(HttpRequest req) async {
    final uri = req.uri;
    final ours = req.method == 'GET' &&
        uri.hasQuery &&
        !_caught.isCompleted &&
        (ask.path.isEmpty || uri.path == ask.path || uri.path.startsWith('${ask.path}/'));
    req.response.headers.contentType = ContentType.html;
    if (!ours) {
      req.response.statusCode = HttpStatus.notFound;
      await req.response.close();
      return;
    }
    req.response.write(_donePage);
    await req.response.close();
    final host = ask.host.contains(':') ? '[${ask.host}]' : ask.host;
    _caught.complete(Uri.parse('http://$host:${ask.port}$uri'));
  }

  Future<void> close() async {
    for (final s in _servers) {
      await s.close(force: true);
    }
  }
}

const _donePage = '<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">'
    '<title>KASA</title><body style="font:17px -apple-system,sans-serif;padding:48px 24px;text-align:center">'
    '돌아온 주소를 받았어요.<br>카사텀으로 돌아가요.</body>';

/// 로그인 창을 열고 돌아온 주소를 받는다. 받으면 창을 거두고 그 주소를, 사람이 먼저 창을 닫으면 null.
/// [authenticate] 는 시스템 로그인 창(`systemWebAuthenticate`), [cancel] 은 그 창 거두기.
Future<Uri?> catchLoginRedirect(
  LoginAsk ask, {
  required Future<Uri?> Function(Uri url, String scheme) authenticate,
  required Future<void> Function() cancel,
}) async {
  final net = await LoopbackCatch.open(ask);
  try {
    final window = authenticate(ask.url, oauthRedirectScheme);
    final first = await Future.any<Object?>([
      net.caught,
      window.then<Object?>((_) => const _Closed(), onError: (Object e) => _Failed(e)),
    ]);
    if (first is Uri) {
      unawaited(cancel().catchError((Object _) {}));
      unawaited(window.then((_) {}, onError: (Object _) {}));
      return first;
    }
    if (first is _Failed) {
      throw const LoginCatchException('로그인 창을 열지 못했어요. 다시 시도해 주세요.');
    }
    return null;
  } finally {
    await net.close();
  }
}

class _Closed {
  const _Closed();
}

class _Failed {
  const _Failed(this.error);
  final Object error;
}

/// 사람이 복사해 온 돌아온 주소 — 루프백 http 이고 쿼리가 있어야 한다.
Uri? pastedLoginRedirect(String? text) {
  final t = text?.trim() ?? '';
  final u = Uri.tryParse(t);
  if (u == null || u.scheme != 'http' || !_loopback(u.host) || !u.hasQuery) return null;
  return u;
}

/// 카드에 보일 꼴 — `code` 같은 일회용 값은 앞뒤만.
String maskLoginRedirect(Uri u) {
  final parts = [
    for (final p in u.query.split('&'))
      if (p.indexOf('=') case final at when at > 0 && p.length - at > 11 && p.substring(0, at) != 'scope')
        '${p.substring(0, at + 5)}…${p.substring(p.length - 3)}'
      else
        p,
  ];
  final host = u.host.contains(':') ? '[${u.host}]' : u.host;
  return '$host:${u.port}${u.path}?${parts.join('&')}';
}
