// dart run tool/vault_fixture_server.dart [port]
// Build the simulator with KASA_ROOT and KASA_VAULT_FIXTURE set to this URL.
import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';

const _research = '리서치/검색 용어.md';

const _source = '''
# 검색 용어

검색 시스템에서 자주 쓰는 개념을 정리한 노트입니다.

## 의미를 비교하는 검색

의미 검색은 표현이 달라도 질문과 같은 뜻을 담은 문서를 찾습니다. 문장 임베딩을 벡터로 만들고, 벡터 사이 거리를 비교합니다.

| 용어 | 뜻 | 예시 |
| --- | --- | --- |
| 임베딩 | 텍스트의 특징을 수치 벡터로 나타낸 값 | 문장 하나를 1,024차원 벡터로 표현 |
| 코사인 유사도 | 두 벡터가 가리키는 방향이 얼마나 비슷한지 나타내는 값 | 비슷한 문장의 벡터는 같은 방향을 향함 |
| 재순위화 | 후보 문서를 질문과 다시 비교해 순서를 매기는 단계 | 검색 후보 중 가장 관련 있는 문서를 맨 위로 올림 |
''';

String _sha(String text) => sha256.convert(utf8.encode(text)).toString();
String _title(String path) =>
    path.split('/').last.replaceFirst(RegExp(r'\.md$'), '');

Future<void> main(List<String> args) async {
  final port = args.isEmpty ? 8788 : int.parse(args.single);
  final files = {
    '메모리 안내.md': '# 메모리 안내\n\n리서치 노트와 공부 덱을 찾아보세요.',
    _research: _source,
    '리서치/평가 지표.md': '# 평가 지표\n\n검색 품질을 판단할 때 재현율과 정밀도를 봅니다.',
    '언어/일본어 표현.md': '# 일본어 표현\n\n| 일본어 | 뜻 |\n| --- | --- |\n| 調べる | 알아보다 |',
    '언어/영어 어휘.md': '# 영어 어휘\n\n| 단어 | 뜻 |\n| --- | --- |\n| retrieve | 찾아내다 |',
    '_study/리서치 기초.md':
        '---\ndeck: true\nfront: 용어\nback: 설명\n---\n'
        '# 리서치 기초\n\n| 용어 | 설명 |\n| --- | --- |\n| 정밀도 | 검색 결과 중 관련 문서의 비율 |',
  };
  final server = await HttpServer.bind(InternetAddress.loopbackIPv4, port);
  stdout.writeln('Vault fixture at http://127.0.0.1:${server.port}/');
  final started = DateTime.now().toUtc().toIso8601String();

  await for (final request in server) {
    final input = request.uri.queryParameters;
    final path = input['path'];
    final existing = files[path];
    (int, Object?) result;

    Future<Map<String, dynamic>> body() async =>
        jsonDecode(await utf8.decoder.bind(request).join())
            as Map<String, dynamic>;

    try {
      switch ((request.method, request.uri.path)) {
        case ('GET', '/status'):
          result = (
            200,
            {
              'vault': 'fixture',
              'files': files.length,
              'chunks': 8,
              'points': 8,
              'pending': 0,
              'last_sync': started,
              'last_error': null,
            },
          );
        case ('GET', '/list'):
          final dir = input['dir'] ?? '';
          final prefix = dir.isEmpty ? '' : '$dir/';
          final dirs = <String>{};
          final entries = <Object>[];
          for (final entry in files.entries.where(
            (e) => e.key.startsWith(prefix),
          )) {
            final relative = entry.key.substring(prefix.length);
            if (relative.contains('/')) {
              dirs.add(relative.split('/').first);
            } else {
              entries.add({
                'path': entry.key,
                'title': _title(entry.key),
                'mtime': started,
                'size': utf8.encode(entry.value).length,
              });
            }
          }
          result = (200, {'dir': dir, 'dirs': dirs.toList(), 'files': entries});
        case ('GET', '/file'):
          result = existing == null
              ? (404, {'error': 'not_found'})
              : (
                  200,
                  {
                    'path': path,
                    'text': existing,
                    'sha': _sha(existing),
                    'mtime': started,
                  },
                );
        case ('PUT', '/file'):
          final data = await body();
          final target = data['path'] as String;
          final text = data['text'] as String;
          final previous = files[target];
          final sha = previous == null ? null : _sha(previous);
          if (sha != data['base_sha'] && previous != null) {
            result = (409, {'error': 'conflict', 'sha': sha, 'text': previous});
          } else {
            files[target] = text;
            result = (
              200,
              {'path': target, 'sha': _sha(text), 'commit': 'fixture'},
            );
          }
        case ('POST', '/search'):
          final data = await body();
          final q = data['q'] as String;
          final include = [
            '뜻',
            '의미',
            'similar',
            'meaning',
            '검색',
            '벡터',
          ].any(q.contains);
          result = (
            200,
            {
              'query': q,
              'mode': 'semantic',
              'results': [
                if (include)
                  {
                    'path': _research,
                    'score': 0.89,
                    'cos': 0.82,
                    'snippets': [
                      {
                        'head': '의미를 비교하는 검색',
                        'text': '의미 검색은 표현이 달라도 질문과 같은 뜻을 담은 문서를 찾습니다.',
                      },
                    ],
                  },
              ],
            },
          );
        case ('POST', '/quiz'):
          final data = await body();
          final name = _title(data['path'] as String);
          final deck = '_study/$name 덱.md';
          files[deck] =
              '---\ndeck: true\nfront: 질문\nback: 답\n---\n'
              '# $name 덱\n\n| 질문 | 답 |\n| --- | --- |\n'
              '| 의미가 비슷한 문장을 어떻게 찾나요? | 문장 임베딩을 비교합니다. |';
          result = (200, {'deck_path': deck, 'cards': 1});
        case ('POST', '/__external_edit'):
          files[_research] = '${files[_research]}\n다른 기기에서 검색 노트를 보완했습니다.\n';
          result = (200, {'ok': true});
        case ('GET', '/version'):
          result = (200, {'version': 'fixture', 'machine_id': 'fixture'});
        case ('GET', '/term/panes'):
          result = (200, []);
        case ('GET', '/sessions'):
          result = (
            200,
            {
              'labels': ['문서 QA'],
            },
          );
        case ('GET', '/machines'):
          result = (200, {'machines': []});
        default:
          result = (404, {'error': 'not_found'});
      }
    } catch (_) {
      result = (400, {'error': 'invalid_request'});
    }
    stdout.writeln('${request.method} ${request.uri.path} ${result.$1}');
    request.response
      ..statusCode = result.$1
      ..headers.contentType = ContentType.json
      ..write(jsonEncode(result.$2));
    await request.response.close();
  }
}
