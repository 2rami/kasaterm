import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:http/http.dart' as http;

import '../server.dart';

typedef VaultMachine = ({String route, String label, bool preferred});
typedef VaultRequest =
    Future<http.Response> Function(
      String method,
      String path, {
      Map<String, String>? query,
      String? body,
      required Duration timeout,
    });
typedef VaultDirectory = ({List<String> dirs, List<VaultEntry> files});
typedef VaultEntry = ({String path, String title});
typedef VaultFile = ({String path, String text, String sha});
typedef VaultHit = ({String path, List<({String head, String text})> snippets});

enum VaultQuizKind { auto, terms, vocab }

class VaultConflict implements Exception {
  const VaultConflict(this.sha, this.text);
  final String? sha;
  final String? text;
}

class VaultException implements Exception {
  const VaultException(this.message);
  final String message;
}

String vaultError(Object error) => switch (error) {
  VaultException(:final message) || ServerException(:final message) => message,
  TimeoutException() => '볼트 응답이 늦어요. 잠시 후 다시 시도해 주세요.',
  _ => '볼트 요청을 처리하지 못했어요.',
};

String vaultTitle(String path) => path
    .split('/')
    .last
    .replaceFirst(RegExp(r'\.md$', caseSensitive: false), '');

bool vaultWritable(String path) {
  final parts = path.split('/');
  return path.endsWith('.md') &&
      !path.contains('\\') &&
      !path.runes.any((rune) => rune < 32 || (rune >= 127 && rune < 160)) &&
      (!parts.first.startsWith('_') || parts.first == '_study') &&
      !parts.any((part) => part.isEmpty || part.startsWith('.'));
}

class VaultApi {
  const VaultApi(this._request);
  final VaultRequest _request;

  factory VaultApi.kasanet(Server server, String machine) => VaultApi(
    (method, path, {query, body, required timeout}) => server.vaultRequest(
      machine,
      method,
      path,
      query: query,
      body: body,
      timeout: timeout,
    ),
  );

  static const fixture = String.fromEnvironment('KASA_VAULT_FIXTURE');

  /// The simulator fixture can only use this computer's loopback and debug builds.
  static VaultApi? debugFixture(http.Client client) {
    if (!kDebugMode || fixture.isEmpty) return null;
    final base = Uri.tryParse(fixture);
    if (base == null ||
        base.scheme != 'http' ||
        base.host != '127.0.0.1' ||
        base.userInfo.isNotEmpty ||
        base.path != '/' ||
        base.hasQuery ||
        base.hasFragment) {
      return null;
    }
    return VaultApi((method, path, {query, body, required timeout}) async {
      final stop = Completer<void>();
      final request = http.AbortableRequest(
        method,
        base.resolve(path).replace(queryParameters: query),
        abortTrigger: stop.future,
      )..followRedirects = false;
      if (body != null) {
        request.headers['content-type'] = 'application/json; charset=utf-8';
        request.bodyBytes = utf8.encode(body);
      }
      try {
        return await client
            .send(request)
            .then(http.Response.fromStream)
            .timeout(timeout);
      } finally {
        stop.complete();
      }
    });
  }

  Future<Map<String, dynamic>> _json(
    String method,
    String path, {
    Map<String, String>? query,
    Map<String, Object?>? body,
    Duration timeout = const Duration(seconds: 25),
  }) async {
    final res = await _request(
      method,
      path,
      query: query,
      body: body == null ? null : jsonEncode(body),
      timeout: timeout,
    );
    if (res.statusCode == 409 && method == 'PUT' && path == 'file') {
      final json = jsonDecode(utf8.decode(res.bodyBytes));
      if (json case {
        'error': 'conflict',
        'sha': final String sha,
        'text': final String text,
      }) {
        throw VaultConflict(sha, text);
      }
      if (json case {'error': 'conflict', 'sha': null, 'text': null}) {
        throw const VaultConflict(null, null);
      }
    }
    if (res.statusCode == 503 &&
        method == 'PUT' &&
        path == 'file' &&
        utf8
            .decode(res.bodyBytes, allowMalformed: true)
            .contains('git busy, retry')) {
      throw const VaultException('볼트가 다른 변경을 저장 중이에요. 잠시 뒤 다시 저장해 주세요.');
    }
    if (path == 'quiz' && res.statusCode != 200) {
      throw VaultException(switch (res.statusCode) {
        400 => '문서 경로나 덱 종류를 확인해 주세요.',
        403 => '이 폴더의 문서로는 덱을 만들 수 없어요.',
        404 => '덱의 원본 문서를 찾지 못했어요.',
        503 => '볼트가 다른 변경을 저장 중이에요. 잠시 뒤 다시 시도해 주세요.',
        _ => '볼트 기기에서 덱을 만들지 못했어요. 공부 덱 목록을 확인해 주세요.',
      });
    }
    if (res.statusCode != 200) {
      throw VaultException(switch (res.statusCode) {
        401 || 403 => '이 볼트에 접근할 권한이 없어요.',
        404 => '볼트에서 요청한 항목을 찾지 못했어요.',
        413 => '문서가 너무 커요.',
        502 || 503 => '기기에서 볼트 서버에 연결하지 못했어요.',
        _ => '볼트 요청을 처리하지 못했어요. (${res.statusCode})',
      });
    }
    final result = jsonDecode(utf8.decode(res.bodyBytes));
    if (result is! Map<String, dynamic>) {
      throw const FormatException('vault response');
    }
    return result;
  }

  Future<VaultDirectory> list(String dir) async {
    final json = await _json('GET', 'list', query: {'dir': dir});
    return (
      dirs: (json['dirs'] as List).cast<String>(),
      files: [
        for (final f in json['files'] as List)
          (path: f['path'] as String, title: f['title'] as String),
      ],
    );
  }

  Future<VaultFile> file(String path) async {
    final json = await _json('GET', 'file', query: {'path': path});
    return (
      path: json['path'] as String,
      text: json['text'] as String,
      sha: json['sha'] as String,
    );
  }

  Future<VaultFile> save(String path, String text, String? baseSha) async {
    final json = await _json(
      'PUT',
      'file',
      body: {'path': path, 'text': text, 'base_sha': baseSha},
      timeout: const Duration(seconds: 75),
    );
    return (
      path: json['path'] as String,
      text: text,
      sha: json['sha'] as String,
    );
  }

  Future<List<VaultHit>> search(String query) async {
    final json = await _json('POST', 'search', body: {'q': query, 'limit': 30});
    return [
      for (final hit in json['results'] as List)
        (
          path: hit['path'] as String,
          snippets: [
            for (final snippet in hit['snippets'] as List)
              (
                head: snippet['head'] as String,
                text: snippet['text'] as String,
              ),
          ],
        ),
    ];
  }

  Future<String> createQuiz(String path, VaultQuizKind kind) async {
    final json = await _json(
      'POST',
      'quiz',
      body: {'path': path, 'kind': kind.name, 'n': 30},
      timeout: const Duration(seconds: 645),
    );
    final deck = json['deck_path'] as String;
    if (!deck.startsWith('_study/') || !vaultWritable(deck)) {
      throw const FormatException('vault deck path');
    }
    return deck;
  }
}
