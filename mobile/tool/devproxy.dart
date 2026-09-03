// 크롬에서 앱을 돌려 볼 때 쓰는 같은 출처 역프록시.
//
// 카사텀 서버는 Origin 이 Host 와 정확히 같아야 소켓을 열어 주고, /term/panes
// 같은 GET 에도 CORS 헤더가 없다. 그래서 Flutter 웹 개발 서버(다른 포트)에서 바로
// 붙일 수가 없다. 이 프록시가 한 주소에서 앱(Flutter dev 서버)과 서버(카사텀)를
// 함께 내주고, 서버로 넘기는 요청에서는 브라우저가 붙인 출처 흔적을 뗀다.
//
//   dart run tool/devproxy.dart --listen 8877 --upstream http://127.0.0.1:8765/ --web http://127.0.0.1:5555/
//
// 공용 주소(/u/<slug>/)를 상대로 볼 때는 argv 에 적지 말고 KASA_UPSTREAM 환경변수로 —
// argv 는 셸 히스토리와 ps 에 남는다.
import 'dart:io';

const _serverExact = {'send', 'sessions', 'machines', 'hub'};
const _serverPrefix = ['term/', 'mobile/', 'm/'];

/// 브라우저가 붙인 출처 흔적. 서버는 이걸로 「남의 페이지가 보낸 요청」을 가려내므로
/// 그대로 넘기면 우리 요청이 그 판정에 걸린다.
const _stripToUpstream = {
  'origin',
  'referer',
  'cookie',
  'host',
  'sec-fetch-site',
  'sec-fetch-mode',
  'sec-fetch-dest',
  'sec-fetch-user',
  'x-forwarded-for',
};

Future<void> main(List<String> args) async {
  final opts = _parse(args);
  final listen = int.tryParse(opts['listen'] ?? '') ?? 8877;
  final upstream = Uri.parse(
    opts['upstream'] ?? Platform.environment['KASA_UPSTREAM'] ?? 'http://127.0.0.1:8765/',
  );
  final web = Uri.parse(opts['web'] ?? 'http://127.0.0.1:5555/');
  final server = await HttpServer.bind(InternetAddress.loopbackIPv4, listen);
  stdout.writeln('devproxy http://127.0.0.1:$listen/  →  server ${upstream.host}:${upstream.port}  ·  web ${web.host}:${web.port}');
  await for (final req in server) {
    _handle(req, upstream, web).catchError((Object e) {
      try {
        req.response.statusCode = HttpStatus.badGateway;
        req.response.write('devproxy: $e');
        req.response.close();
      } catch (_) {}
    });
  }
}

Map<String, String> _parse(List<String> args) {
  final out = <String, String>{};
  for (var i = 0; i + 1 < args.length; i += 2) {
    if (args[i].startsWith('--')) out[args[i].substring(2)] = args[i + 1];
  }
  return out;
}

bool _isServerPath(String path) {
  final p = path.startsWith('/') ? path.substring(1) : path;
  return _serverExact.contains(p) || _serverPrefix.any(p.startsWith);
}

Future<void> _handle(HttpRequest req, Uri upstream, Uri web) async {
  final toServer = _isServerPath(req.uri.path);
  final base = toServer ? upstream : web;
  final rel = req.uri.path.startsWith('/') ? req.uri.path.substring(1) : req.uri.path;
  final target = base.resolve(rel).replace(query: req.uri.hasQuery ? req.uri.query : null);
  if (WebSocketTransformer.isUpgradeRequest(req)) {
    await _pipeWebSocket(req, target, strip: toServer);
    return;
  }
  await _forwardHttp(req, target, strip: toServer);
}

Future<void> _forwardHttp(HttpRequest req, Uri target, {required bool strip}) async {
  final client = HttpClient();
  try {
    final out = await client.openUrl(req.method, target);
    req.headers.forEach((name, values) {
      if (strip && _stripToUpstream.contains(name.toLowerCase())) return;
      if (name.toLowerCase() == 'host') return;
      for (final v in values) {
        out.headers.add(name, v);
      }
    });
    if (req.contentLength >= 0) out.contentLength = req.contentLength;
    await out.addStream(req);
    final res = await out.close();
    req.response.statusCode = res.statusCode;
    res.headers.forEach((name, values) {
      final n = name.toLowerCase();
      if (n == 'transfer-encoding' || n == 'content-length' || n == 'connection') return;
      for (final v in values) {
        req.response.headers.add(name, v);
      }
    });
    if (res.contentLength >= 0) req.response.contentLength = res.contentLength;
    await req.response.addStream(res);
    await req.response.close();
  } finally {
    client.close();
  }
}

/// 양쪽 소켓을 그대로 잇는다. String 과 `List<int>` 를 구별해 옮겨야 한다 — 키 입력은
/// binary 프레임이어야 서버가 키로 읽는다.
Future<void> _pipeWebSocket(HttpRequest req, Uri target, {required bool strip}) async {
  final wsTarget = target.replace(scheme: target.scheme == 'https' ? 'wss' : 'ws');
  final headers = <String, String>{};
  req.headers.forEach((name, values) {
    final n = name.toLowerCase();
    if (n.startsWith('sec-websocket') || n == 'upgrade' || n == 'connection') return;
    if (strip && _stripToUpstream.contains(n)) return;
    if (n == 'host' || n == 'content-length') return;
    headers[name] = values.join(',');
  });
  final protocols = req.headers['sec-websocket-protocol']?.expand((v) => v.split(',')).map((s) => s.trim()).toList();
  final remote = await WebSocket.connect(
    wsTarget.toString(),
    headers: headers,
    protocols: protocols == null || protocols.isEmpty ? null : protocols,
  );
  final local = await WebSocketTransformer.upgrade(req, protocolSelector: (_) => remote.protocol ?? '');
  local.listen(remote.add, onDone: () => remote.close(), onError: (Object _) => remote.close(), cancelOnError: true);
  remote.listen(local.add, onDone: () => local.close(), onError: (Object _) => local.close(), cancelOnError: true);
}
