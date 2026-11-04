import 'dart:convert';
import 'dart:io';

import 'package:kasaterm_mobile/archive_model.dart';

class OfflineArchiveFixture {
  OfflineArchiveFixture._(this.http, this.advanced) {
    http.listen(_reply);
  }
  final HttpServer http;
  final bool advanced;
  final reads = <int>[];
  static const title = '오프라인 재시작 확인';
  static const originalText = '연결 없이도 이 답변을 읽어요.';
  static const appendedText = '다시 연결한 뒤 새 내용이 이어졌어요.';
  static String line(String role, String text) =>
      '${jsonEncode({
        'type': role,
        'message': {'role': role, 'content': text},
      })}\n';
  static final initialRaw =
      line('user', '저장한 대화를 다시 열어 주세요.') +
      line('assistant', '$originalText\n\n```dart\nfinal offline = true;\n```');
  String get raw =>
      initialRaw + (advanced ? line('assistant', appendedText) : '');
  ArchiveEntry get entry => ArchiveEntry(
    id: List.filled(64, 'a').join(),
    machineId: 'fixture-desktop',
    machineName: '검증 기기',
    source: 'fixture.jsonl',
    surfaceKey: 'fixture-key',
    title: title,
    name: 'Assistant',
    harness: 'claude',
    cwd: '/workspace/fixture',
    generation: List.filled(32, 'b').join(),
    revision: advanced ? 2 : 1,
    cursor: advanced ? 2000 : 1000,
    bytes: utf8.encode(raw).length,
    updatedAt: DateTime.utc(2026, 11, 4, 3, advanced ? 1 : 0),
    ready: true,
  );
  static Future<OfflineArchiveFixture> start({
    int port = 0,
    bool advanced = false,
  }) async => OfflineArchiveFixture._(
    await HttpServer.bind(InternetAddress.loopbackIPv4, port),
    advanced,
  );

  Future<void> _reply(HttpRequest request) async {
    Object reply = {'error': 'fixture_route_missing'};
    var status = 404;
    if (request.headers.value(HttpHeaders.authorizationHeader) !=
        'Bearer fixture-phone-token') {
      status = 401;
    } else if (request.uri.path == '/relay/whoami') {
      status = 200;
      reply = {
        'ok': true,
        'account': 'archive-fixture',
        'device_id': 'fixture-phone',
        'kind': 'phone',
      };
    } else if (request.uri.path == '/relay/devices') {
      status = 200;
      reply = {'devices': []};
    } else if (request.uri.path == '/relay/account-sync') {
      status = 200;
      reply = {'revision': 0, 'settings': {}};
    } else if (request.uri.path == '/relay/account/mobile/me') {
      status = 200;
      reply = {'name': 'fixture', 'owner': true, 'machine': 'fixture-desktop'};
    } else if (request.uri.path == '/relay/conversations') {
      status = 200;
      reply = {
        'enabled': true,
        'conversations': [entry.toJson()],
      };
    } else if (request.uri.path == '/relay/conversations/${entry.id}') {
      final offset = int.parse(request.uri.queryParameters['offset']!);
      final end = int.parse(request.uri.queryParameters['end']!);
      reads.add(offset);
      status = 200;
      reply = {
        'chunk': {
          'raw': utf8.decode(utf8.encode(raw).sublist(offset, end)),
          'next': end,
          'end': end,
          'generation': entry.generation,
        },
      };
    }
    await request.drain<void>();
    request.response.statusCode = status;
    request.response.headers.contentType = ContentType.json;
    request.response.write(jsonEncode(reply));
    await request.response.close();
  }

  Future<void> close() => http.close(force: true);
}
