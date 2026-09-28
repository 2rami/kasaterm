import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/screens/share_screen.dart';
import 'package:kasaterm_mobile/server.dart';

const slug = 'abcdefghij0123456789abcde';
const root = 'https://kasaterm.debimarlene.com/u/$slug/';
const _json = {'content-type': 'application/json; charset=utf-8'};

const listing = {
  'ok': true,
  'name': 'KASA-share',
  'folders': [
    {
      'name': '2026-09-28-카사텀-쌍둥이-시안',
      'modified_ms': 1790550000000,
      'files': [
        {
          'path': '2026-09-28-카사텀-쌍둥이-시안/1-짝눈.png',
          'name': '1-짝눈.png',
          'size': 1205779,
          'modified_ms': 1790550000000,
          'kind': 'image',
          'origin': 'MacBook Pro',
        },
        {
          'path': '2026-09-28-카사텀-쌍둥이-시안/sub/노트.md',
          'name': 'sub/노트.md',
          'size': 42,
          'modified_ms': 1790550000000,
          'kind': 'markdown',
          'origin': 'Mac mini',
        },
      ],
    },
  ],
  'files': [
    {
      'path': '큰것.bin',
      'name': '큰것.bin',
      'size': 40000000,
      'modified_ms': 1790555000000,
      'kind': 'spreadsheet',
      'origin': '',
    },
  ],
};

void main() {
  test('shareList 는 폴더·하위 폴더 파일·폴더 밖 파일을 읽는다', () async {
    late Uri asked;
    final s = Server(
      Uri.parse(root),
      client: MockClient((req) async {
        asked = req.url;
        return http.Response(jsonEncode(listing), 200, headers: _json);
      }),
    );
    final l = await s.shareList();
    expect(asked.path, '/u/$slug/term/share/list');
    expect(l.name, 'KASA-share');
    expect(l.folders, hasLength(1));
    final f = l.folders.single;
    expect(f.topic, '카사텀-쌍둥이-시안');
    expect(f.day, '09-28');
    expect(f.modified.millisecondsSinceEpoch, 1790550000000);
    expect(f.files.map((e) => e.kind), [ShareKind.image, ShareKind.markdown]);
    expect(f.files[1].name, 'sub/노트.md');
    expect(f.files[1].origin, 'Mac mini');
    expect(f.firstImage?.name, '1-짝눈.png');
    final loose = l.files.single;
    expect(loose.kind, ShareKind.other, reason: '모르는 종류는 other');
    expect(loose.tooLarge, isTrue);
    expect(f.files.first.tooLarge, isFalse);
  });

  test('날짜 접두가 없는 폴더는 이름 그대로', () {
    final f = ShareFolder.fromJson({'name': '모아둔 것', 'files': []});
    expect(f.topic, '모아둔 것');
    expect(f.day, isNull);
  });

  test('shareFileUri 는 한글·슬래시·공백을 path 하나로 인코딩한다', () {
    final s = Server(Uri.parse(root));
    final u = s.shareFileUri('2026-09-28-시안/sub/a b.png');
    expect(u.path, '/u/$slug/term/share/file');
    expect(u.queryParameters['path'], '2026-09-28-시안/sub/a b.png');
    expect(u.query.contains('/'), isFalse);
  });

  test('shareText 거절은 사람 말로, slug 없이', () async {
    for (final (code, word) in [(403, '주인 주소'), (413, '32MB'), (503, '꺼져')]) {
      final s = Server(
        Uri.parse(root),
        client: MockClient((_) async => http.Response('nope', code)),
      );
      try {
        await s.shareText('a.md');
        fail('예외가 나야 한다');
      } on ServerException catch (e) {
        expect(e.status, code);
        expect(e.message, contains(word));
        expect(e.message.contains(slug), isFalse);
      }
    }
  });

  test('shareText 는 utf8 본문을 돌려준다', () async {
    final s = Server(
      Uri.parse(root),
      client: MockClient(
        (_) async => http.Response.bytes(utf8.encode('# 제목\n본문'), 200),
      ),
    );
    expect(await s.shareText('a.md'), '# 제목\n본문');
  });

  test('ok 가 아니면 서버 까닭을 그대로', () async {
    final s = Server(
      Uri.parse(root),
      client: MockClient(
        (_) async => http.Response(
          jsonEncode({'ok': false, 'error': '공유 폴더를 못 찾았다'}),
          200,
          headers: _json,
        ),
      ),
    );
    expect(
      s.shareList(),
      throwsA(
        isA<ServerException>().having(
          (e) => e.message,
          'message',
          '공유 폴더를 못 찾았다',
        ),
      ),
    );
  });

  test('formatBytes', () {
    expect(formatBytes(15), '15B');
    expect(formatBytes(3400), '3.3KB');
    expect(formatBytes(1205779), '1.1MB');
    expect(formatBytes(13000000), '12MB');
  });
}
