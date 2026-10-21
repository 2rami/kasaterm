import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:math';

import 'package:flutter/foundation.dart';

import 'inline_media.dart';
import 'server.dart';

Future<MediaLink> openMediaBridge({
  required Server server,
  required String id,
  required String? machine,
  required InlineMedia media,
  required int chunk,
}) => MediaBridge.open(
  server: server,
  id: id,
  machine: machine,
  media: media,
  chunk: chunk,
);

/// AVPlayer 가 http 로 읽는 루프백 창구. 요청받은 바이트 구간만 원본 기기에 조각(`Server.inlineMediaChunk`)으로
/// 물어 넘긴다 — 파일 전체를 받거나 디스크에 쓰지 않는다. 연 순간의 연결·기기·id·크기에 고정되고, 닫으면 받던
/// 요청과 연결을 모두 끊는다.
///
/// 응답은 소켓을 떼어 직접 쓴다. dart:io 의 HttpResponse 는 상대가 끊어도 쓰기 오류를 삼켜서, 탐색으로 버려진
/// 요청이 파일 끝까지 원본에서 계속 받아 온다.
class MediaBridge implements MediaLink {
  MediaBridge._(
    this._http,
    this._server,
    this._id,
    this._machine,
    this.media,
    this.chunk,
    this.uri,
  );

  static Future<MediaBridge> open({
    required Server server,
    required String id,
    required String? machine,
    required InlineMedia media,
    required int chunk,
  }) async {
    final http = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    http.defaultResponseHeaders.clear();
    final rand = Random.secure();
    final token = [
      for (var i = 0; i < 16; i++)
        rand.nextInt(256).toRadixString(16).padLeft(2, '0'),
    ].join();
    final bridge = MediaBridge._(
      http,
      server,
      id,
      machine,
      media,
      chunk,
      Uri(
        scheme: 'http',
        host: '127.0.0.1',
        port: http.port,
        path: '/$token/media.${media.extension}',
      ),
    );
    http.listen(bridge._serve, onError: (_) {});
    return bridge;
  }

  /// 다 받은 조각을 이만큼 들고 있는다 — AVPlayer 는 첫머리·끝(moov)을 몇 번씩 다시 묻는다.
  static const _keep = 8;

  final HttpServer _http;
  final Server _server;
  final String _id;
  final String? _machine;
  final InlineMedia media;
  final int chunk;
  final _blocks = <int, _Block>{};
  final _sockets = <Socket>{};
  bool _closed = false;

  /// 원본에서 받은 바이트 합 — 시험이 「다 받기 전에 재생」·「닫은 뒤 멈춤」을 잰다.
  @visibleForTesting
  int get received => _received;
  int _received = 0;

  @override
  String? error;

  @override
  String? get path => null;

  /// 루프백 임의 포트·짐작할 수 없는 경로. 닫은 뒤에도 같은 값이다.
  @override
  final Uri uri;

  @override
  Future<void> close() async {
    if (_closed) return;
    _closed = true;
    for (final b in _blocks.values) {
      b.cancel();
    }
    _blocks.clear();
    // 끊으면 그 소켓의 onDone 이 _sockets 에서 저를 뺀다.
    for (final s in _sockets.toList()) {
      s.destroy();
    }
    _sockets.clear();
    await _http.close(force: true);
  }

  _Block _hold(int index) {
    if (_closed) throw StateError('창구가 닫혔어요');
    var b = _blocks.remove(index);
    if (b == null) {
      final block = b = _Block(index);
      final offset = index * chunk;
      block.bytes = _server
          .inlineMediaChunk(
            _id,
            offset: offset,
            length: min(chunk, media.size - offset),
            machine: _machine,
            abort: block.abort.future,
          )
          .then(
            (bytes) {
              block.done = true;
              _received += bytes.length;
              error = null;
              return bytes;
            },
            onError: (Object e, StackTrace st) {
              if (identical(_blocks[index], block)) _blocks.remove(index);
              if (e is ServerException) error = e.message;
              Error.throwWithStackTrace(e, st);
            },
          );
      // 미리 받다 버린 조각의 실패는 아무도 기다리지 않는다.
      block.bytes.ignore();
    }
    _blocks[index] = b;
    b.holders++;
    _evict();
    return b;
  }

  void _release(_Block b) {
    b.holders--;
    if (b.holders > 0 || b.done) return;
    // 아무도 기다리지 않는 받던 조각 — 탐색·닫힘으로 버려진 구간이라 끊는다.
    b.cancel();
    if (identical(_blocks[b.index], b)) _blocks.remove(b.index);
  }

  void _evict() {
    while (_blocks.length > _keep) {
      final old = _blocks.values
          .where((b) => b.done && b.holders == 0)
          .firstOrNull;
      if (old == null) return;
      _blocks.remove(old.index);
    }
  }

  Future<void> _serve(HttpRequest req) async {
    final res = req.response;
    Future<void> refuse(int status, [Map<String, String> headers = const {}]) {
      res.statusCode = status;
      res.headers.set(HttpHeaders.cacheControlHeader, 'no-store');
      headers.forEach(res.headers.set);
      return res.close().then<void>((_) {}, onError: (_) {});
    }

    if (_closed || req.uri.path != uri.path) return refuse(404);
    // 웹 페이지가 루프백을 두드리면 Origin 이 실린다 — AVPlayer 는 싣지 않는다.
    if (req.headers.value('origin') != null) return refuse(403);
    if (req.method != 'GET' && req.method != 'HEAD') {
      return refuse(405, {'allow': 'GET, HEAD'});
    }
    final size = media.size;
    // Range 는 GET 에만 뜻이 있다(RFC 9110 §14.2) — HEAD 는 실려 와도 전체 길이로 답한다.
    final asked = req.method == 'GET'
        ? req.headers.value(HttpHeaders.rangeHeader)
        : null;
    final want = asked == null ? (0, size - 1) : byteRange(asked, size);
    if (want == null) {
      return refuse(416, {'content-range': 'bytes */$size'});
    }
    final (start, end) = want;
    final head = {
      'content-type': media.streamMime,
      'content-length': '${end - start + 1}',
      'accept-ranges': 'bytes',
      if (asked != null) 'content-range': 'bytes $start-$end/$size',
    };
    final ok = asked == null ? 200 : 206;
    if (req.method == 'HEAD') {
      res.statusCode = ok;
      res.contentLength = end - start + 1;
      res.headers.set(HttpHeaders.cacheControlHeader, 'no-store');
      head.forEach((k, v) {
        if (k != 'content-length') res.headers.set(k, v);
      });
      return res.close().then<void>((_) {}, onError: (_) {});
    }

    final Socket socket;
    try {
      socket = await res.detachSocket(writeHeaders: false);
    } catch (_) {
      return;
    }
    if (_closed) {
      socket.destroy();
      return;
    }
    _sockets.add(socket);
    final held = <_Block>[];
    var gone = false;
    void drop() {
      gone = true;
      for (final b in held) {
        _release(b);
      }
      held.clear();
      _sockets.remove(socket);
    }

    socket.listen(null, onDone: drop, onError: (_) => drop());
    _Block take(int index) {
      final b = _hold(index);
      held.add(b);
      return b;
    }

    void give(_Block b) {
      if (held.remove(b)) _release(b);
    }

    try {
      var cur = take(start ~/ chunk);
      Uint8List bytes;
      try {
        bytes = await cur.bytes;
      } catch (e) {
        if (gone || _closed) {
          socket.destroy();
          return;
        }
        // 머리를 보내기 전이라 원본의 거절을 상태로 돌려준다 — AVPlayer 가 같은 구간을 되묻지 않고 실패로 안다.
        final status = e is ServerException && (e.status ?? 500) < 500
            ? e.status!
            : 502;
        socket.add(_statusLine(status, {'content-length': '0'}));
        await socket.close();
        return;
      }
      socket.add(_statusLine(ok, head));
      var at = start;
      while (true) {
        final base = cur.index * chunk;
        // 쓰는 동안 다음 조각을 하나 앞서 묻는다 — 관문 왕복만큼 재생이 멎지 않게.
        final next = !gone && base + chunk <= end ? take(cur.index + 1) : null;
        final to = min(bytes.length, end - base + 1);
        socket.add(Uint8List.sublistView(bytes, at - base, to));
        give(cur);
        await socket.flush();
        at = base + to;
        if (next == null || gone) break;
        cur = next;
        bytes = await cur.bytes;
      }
      await socket.close();
    } catch (_) {
      // 머리를 이미 보냈다 — 연결을 끊어 AVPlayer 가 구간을 다시 묻거나 실패로 알게 한다.
      socket.destroy();
    } finally {
      for (final b in held) {
        _release(b);
      }
      held.clear();
    }
  }
}

List<int> _statusLine(int status, Map<String, String> headers) {
  final reason = switch (status) {
    200 => 'OK',
    206 => 'Partial Content',
    400 => 'Bad Request',
    403 => 'Forbidden',
    404 => 'Not Found',
    _ => 'Bad Gateway',
  };
  final b = StringBuffer('HTTP/1.1 $status $reason\r\n');
  headers.forEach((k, v) => b.write('$k: $v\r\n'));
  b.write('cache-control: no-store\r\nconnection: close\r\n\r\n');
  return ascii.encode(b.toString());
}

/// `Range: bytes=…` 하나를 [start, end] 로. 여러 구간·어긋난 꼴·파일 밖이면 null(416). 단위 이름은 대소문자를
/// 가리지 않는다(RFC 9110 §14.1).
@visibleForTesting
(int, int)? byteRange(String header, int size) {
  final m = RegExp(
    r'^bytes=(\d*)-(\d*)$',
    caseSensitive: false,
  ).firstMatch(header.trim());
  if (m == null) return null;
  final from = m[1]!, to = m[2]!;
  if (from.isEmpty) {
    final n = int.tryParse(to);
    if (n == null || n == 0) return null;
    return (max(0, size - n), size - 1);
  }
  final s = int.tryParse(from);
  if (s == null || s >= size) return null;
  if (to.isEmpty) return (s, size - 1);
  final e = int.tryParse(to);
  if (e == null || e < s) return null;
  return (s, min(e, size - 1));
}

class _Block {
  _Block(this.index);
  final int index;
  final abort = Completer<void>();
  late final Future<Uint8List> bytes;
  int holders = 0;
  bool done = false;

  void cancel() {
    if (!abort.isCompleted) abort.complete();
  }
}
