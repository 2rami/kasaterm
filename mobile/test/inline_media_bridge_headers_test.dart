import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/inline_media.dart';
import 'package:kasaterm_mobile/inline_media_bridge_io.dart';
import 'package:kasaterm_mobile/server.dart';

const _id = '3333333333333333';
const _chunk = 16 * 1024;
final _bytes = Uint8List.fromList(
  List.generate(40000, (i) => (i * 13 + (i >> 7)) & 0xff),
);

class _Answer {
  _Answer(this.status, this.headers, this.body);
  final int status;
  final Map<String, String> headers;
  final List<int> body;
}

/// 머리를 손으로 적어 보낸다 — dart:io·package:http 클라이언트는 같은 머리를 두 줄로 보내 주지 않는다.
/// 창구가 응답을 끝내지 않으면 3초 뒤 시험이 실패한다.
Future<_Answer> _raw(Uri uri, String method, List<String> headers) async {
  final s = await Socket.connect(uri.host, uri.port);
  s.write(
    '$method ${uri.path} HTTP/1.1\r\nhost: ${uri.host}:${uri.port}\r\n'
    '${headers.map((h) => '$h\r\n').join()}connection: close\r\n\r\n',
  );
  final all = <int>[];
  await s
      .listen(all.addAll)
      .asFuture<void>()
      .timeout(
        const Duration(seconds: 3),
        onTimeout: () => fail('$method $headers — 창구가 응답을 끝내지 않았다'),
      );
  s.destroy();
  final split = _indexOf(all, ascii.encode('\r\n\r\n'));
  final lines = latin1.decode(all.sublist(0, split)).split('\r\n');
  return _Answer(int.parse(lines.first.split(' ')[1]), {
    for (final l in lines.skip(1))
      l.substring(0, l.indexOf(':')).toLowerCase(): l
          .substring(l.indexOf(':') + 1)
          .trim(),
  }, all.sublist(split + 4));
}

int _indexOf(List<int> hay, List<int> needle) {
  for (var i = 0; i + needle.length <= hay.length; i++) {
    var j = 0;
    while (j < needle.length && hay[i + j] == needle[j]) {
      j++;
    }
    if (j == needle.length) return i;
  }
  throw StateError('응답에 머리 끝이 없다');
}

void main() {
  test('Range 꼴 — 파일 밖 단일 구간만 416, 무시할 꼴은 null(전체)', () {
    expect(byteRange('bytes=0-1', 100), (0, 1));
    expect(byteRange('bytes=10-', 100), (10, 99));
    expect(byteRange('bytes=90-500', 100), (90, 99));
    expect(byteRange('bytes=-30', 100), (70, 99));
    expect(byteRange('bytes=-300', 100), (0, 99));
    expect(byteRange(' BYTES= 2 - 3 ', 100), (2, 3), reason: '단위는 대소문자를 안 가린다');
    expect(byteRange('Bytes=-1', 100), (99, 99));
    for (final out in ['bytes=100-', 'bytes=150-200', 'bytes=-0']) {
      expect(byteRange(out, 100), unsatisfiable, reason: out);
    }
    for (final ignored in [
      'items=0-1',
      'bytes',
      'bytes=',
      'bytes=-',
      'bytes=5-1',
      'bytes=0-1,4-5',
      'bytes=a-b',
      'bytes=+1-2',
      'bytes=1-2-3',
      'bytes=99999999999999999999-',
      'bytes=0-99999999999999999999',
      'bytes=-99999999999999999999',
      '바이트=1-2',
    ]) {
      expect(byteRange(ignored, 100), isNull, reason: ignored);
    }
  });

  group('실소켓', () {
    late MediaBridge bridge;
    late List<(int, int)> asked;
    late List<Object> zoneErrors;
    final size = _bytes.length;

    setUp(() async {
      asked = [];
      zoneErrors = [];
      final server = Server(
        Uri.parse('https://example.test/u/fixture/'),
        client: MockClient((req) async {
          final q = req.url.queryParameters;
          final offset = int.parse(q['offset']!), len = int.parse(q['len']!);
          asked.add((offset, len));
          return http.Response.bytes(_bytes.sublist(offset, offset + len), 200);
        }),
      );
      addTearDown(server.close);
      // 창구의 요청 처리는 여기서 연 zone 에서 돈다 — 던진 것이 있으면 시험 zone 대신 여기 모인다.
      final opened = Completer<MediaBridge>();
      runZonedGuarded(
        () async => opened.complete(
          await MediaBridge.open(
            server: server,
            id: _id,
            machine: null,
            media: InlineMedia(
              id: _id,
              kind: InlineMediaKind.video,
              name: 'clip.mp4',
              mime: 'video/mp4',
              size: size,
              sha256: '0' * 64,
            ),
            chunk: _chunk,
          ),
        ),
        (e, _) => zoneErrors.add(e),
      );
      bridge = await opened.future;
      addTearDown(bridge.close);
    });

    test('같은 머리가 두 줄이면 예외 없이 403·400 으로 끝내고 원본에 묻지 않는다', () async {
      var r = await _raw(bridge.uri, 'GET', [
        'origin: https://a.test',
        'origin: https://b.test',
      ]);
      expect(r.status, 403);
      r = await _raw(bridge.uri, 'GET', [
        'range: bytes=0-1',
        'range: bytes=2-3',
      ]);
      expect(r.status, 400);
      expect(r.headers['cache-control'], 'no-store');
      r = await _raw(bridge.uri, 'GET', [
        'range: bytes=0-1',
        'origin: https://a.test',
        'origin: https://b.test',
      ]);
      expect(r.status, 403);
      r = await _raw(bridge.uri, 'HEAD', [
        'range: bytes=0-1',
        'range: bytes=2-3',
      ]);
      expect(r.status, 200, reason: 'HEAD 는 Range 를 읽지 않는다');
      expect(r.headers['content-length'], '$size');
      expect(r.body, isEmpty);
      expect(asked, isEmpty);
      expect(zoneErrors, isEmpty);

      // 거절 뒤에도 창구는 산다.
      r = await _raw(bridge.uri, 'GET', ['range: bytes=0-1']);
      expect(r.status, 206);
      expect(r.body, _bytes.sublist(0, 2));
      expect(zoneErrors, isEmpty);
    });

    test('모르는 단위·어긋난 꼴·여러 구간·넘치는 숫자는 Range 를 무시하고 200 전체', () async {
      for (final ignored in [
        'items=0-1',
        'bytes=5-1',
        'bytes=0-1,4-5',
        'bytes=a-b',
        'x',
        'bytes=99999999999999999999-',
        'bytes=0-99999999999999999999',
      ]) {
        final r = await _raw(bridge.uri, 'GET', ['range: $ignored']);
        expect(r.status, 200, reason: ignored);
        expect(r.headers['content-length'], '$size', reason: ignored);
        expect(
          r.headers.containsKey('content-range'),
          isFalse,
          reason: ignored,
        );
        expect(r.body, _bytes, reason: ignored);
      }
      expect(zoneErrors, isEmpty);
    });

    test('파일 밖 단일 구간만 416 이고 원본에 묻지 않는다', () async {
      for (final out in [
        'bytes=$size-',
        'bytes=${size + 10}-${size + 20}',
        'bytes=-0',
      ]) {
        final r = await _raw(bridge.uri, 'GET', ['range: $out']);
        expect(r.status, 416, reason: out);
        expect(r.headers['content-range'], 'bytes */$size', reason: out);
      }
      expect(asked, isEmpty);
      final r = await _raw(bridge.uri, 'GET', ['range: BYTES=${size - 3}-']);
      expect(r.status, 206);
      expect(r.headers['content-range'], 'bytes ${size - 3}-${size - 1}/$size');
      expect(r.body, _bytes.sublist(size - 3));
      expect(zoneErrors, isEmpty);
    });
  });
}
