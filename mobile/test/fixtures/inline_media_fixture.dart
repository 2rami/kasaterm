import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:crypto/crypto.dart';

/// PC 등록소(`GET term/inline-media*`)를 흉내 내는 시험 고정물. 위젯 시험(MockClient)과 시뮬레이터 검증
/// (`tool/inline_media_fixture_server.dart`)이 같은 답을 쓴다. 판정 규칙은 계약 그대로 — 조각 8MiB 상한,
/// offset 은 0 이상 size 미만, 끝을 넘는 len 은 남은 만큼, 없는 id 404, 잘못된 입력 400.
class MediaFixture {
  MediaFixture({
    required this.id,
    required this.kind,
    required this.name,
    required this.mime,
    required this.bytes,
    this.poster,
    this.durationMs,
    this.width,
    this.height,
    this.claimedSha,
  });
  final String id;
  final String kind;
  final String name;
  final String mime;
  final Uint8List bytes;
  final Uint8List? poster;
  final int? durationMs;
  final int? width;
  final int? height;

  /// 일부러 틀린 해시를 알려 대조 실패를 본다.
  final String? claimedSha;

  Map<String, Object?> get info => {
    'version': 1,
    'id': id,
    'kind': kind,
    'name': name,
    'mime': mime,
    'size': bytes.length,
    'sha256': claimedSha ?? sha256.convert(bytes).toString(),
    'duration_ms': durationMs,
    'width': width,
    'height': height,
    'poster': poster != null,
  };
}

class FixtureReply {
  const FixtureReply(this.status, this.body, this.type);
  final int status;
  final List<int> body;
  final String type;
}

const fixtureChunkMax = 8 * 1024 * 1024;

FixtureReply inlineMediaReply(Map<String, MediaFixture> media, Uri url) {
  FixtureReply json(int status, Object body) =>
      FixtureReply(status, utf8.encode(jsonEncode(body)), 'application/json');
  final id = url.queryParameters['id'] ?? '';
  if (!RegExp(r'^[0-9a-f]{16}$').hasMatch(id)) {
    return json(400, {'error': 'bad_id'});
  }
  final m = media[id];
  if (m == null) return json(404, {'error': 'not_found'});
  final path = url.path;
  if (path.endsWith('/term/inline-media/poster')) {
    final p = m.poster;
    return p == null
        ? json(404, {'error': 'no_poster'})
        : FixtureReply(200, p, 'image/png');
  }
  if (path.endsWith('/term/inline-media/file')) {
    final offset = int.tryParse(url.queryParameters['offset'] ?? '');
    final len = int.tryParse(url.queryParameters['len'] ?? '');
    if (offset == null ||
        len == null ||
        offset < 0 ||
        offset >= m.bytes.length ||
        len < 1 ||
        len > fixtureChunkMax) {
      return json(400, {'error': 'bad_range'});
    }
    final end = offset + len > m.bytes.length ? m.bytes.length : offset + len;
    return FixtureReply(
      200,
      Uint8List.sublistView(m.bytes, offset, end),
      'application/octet-stream',
    );
  }
  if (path.endsWith('/term/inline-media')) return json(200, m.info);
  return json(404, {'error': 'no_route'});
}

/// `test/fixtures/media` 의 ffmpeg 산출물(3초 320x180 H.264+AAC · 90° 돌린 mov · 3초 AAC).
Map<String, MediaFixture> loadMediaFixtures(String dir) {
  Uint8List read(String name) => File('$dir/$name').readAsBytesSync();
  final clip = read('clip.mp4');
  return {
    for (final m in [
      MediaFixture(
        id: '1111111111111111',
        kind: 'video',
        name: 'demo clip.mp4',
        mime: 'video/mp4',
        bytes: clip,
        poster: read('poster.png'),
        durationMs: 3000,
        width: 320,
        height: 180,
      ),
      MediaFixture(
        id: '2222222222222222',
        kind: 'audio',
        name: 'tone.m4a',
        mime: 'audio/mp4',
        bytes: read('tone.m4a'),
        durationMs: 3000,
      ),
      MediaFixture(
        id: '3333333333333333',
        kind: 'video',
        name: 'portrait.mov',
        mime: 'video/quicktime',
        bytes: read('portrait.mov'),
        poster: read('portrait.png'),
        durationMs: 3000,
        width: 180,
        height: 320,
      ),
      MediaFixture(
        id: '4444444444444444',
        kind: 'video',
        name: 'tampered.mp4',
        mime: 'video/mp4',
        bytes: clip,
        durationMs: 3000,
        width: 320,
        height: 180,
        claimedSha: '0' * 64,
      ),
    ])
      m.id: m,
  };
}

String mediaBlock(String id, int rows) =>
    '```text\n${List.generate(rows, (i) => '[[km:$id:${i.toString().padLeft(2, '0')}/${rows.toString().padLeft(2, '0')}]]').join('\n')}\n```\n';
