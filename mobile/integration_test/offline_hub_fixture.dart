import 'dart:convert';
import 'dart:io';

import 'package:kasaterm_mobile/archive_model.dart';

/// 루프백 가짜 계정 관문과 그 뒤 데스크톱 하나 — 허브 목록·방 배치·계정 대화 기록을 준다. 내리면 폰에선 망이 끊긴 것과 같다.
class OfflineHubFixture {
  OfflineHubFixture._(this.http) {
    http.listen(_reply);
  }
  final HttpServer http;
  static const token = 'fixture-phone-token';
  static const machine = '검증 맥북';
  static const room = '오프라인 교실';
  static const session = '끊겨도 보이는 학생';
  static const answer = '기기가 끊겨도 이 답변은 폰에서 읽어요.';
  static const desktop = 'fixture-desktop';

  static String _line(String role, String text) =>
      '${jsonEncode({
        'type': role,
        'message': {'role': role, 'content': text},
      })}\n';
  static final raw = _line('user', '끊겨도 허브가 보이나요?') + _line('assistant', answer);

  static ArchiveEntry get entry => ArchiveEntry(
    id: List.filled(64, 'c').join(),
    machineId: desktop,
    machineName: machine,
    source: 'hub-fixture.jsonl',
    surfaceKey: 'hub-fixture-key',
    title: session,
    name: '하늘',
    harness: 'claude',
    cwd: '/workspace/offline',
    generation: List.filled(32, 'd').join(),
    revision: 1,
    cursor: 1000,
    bytes: utf8.encode(raw).length,
    updatedAt: DateTime.utc(2026, 11, 13, 6),
    ready: true,
  );

  static final panes = [
    {
      'id': '%1',
      'name': '하늘',
      'title': '◐ 오프라인 허브 확인',
      'status': 'working',
      'window': 0,
      'cwd': '/workspace/offline',
      'slug': 'kasa_sky',
      'harness': 'claude',
      'session': session,
      'surface_key': 'hub-fixture-key',
      'busy_secs': 400,
    },
    {
      'id': '%2',
      'name': '호박',
      'title': '쉬는 칸',
      'status': 'idle',
      'window': 0,
      'cwd': '/workspace/offline',
      'slug': 'kasa_amber',
      'harness': 'claude',
      'session': '쉬는 학생',
      'surface_key': 'hub-fixture-idle',
    },
  ];

  static Future<OfflineHubFixture> start({int port = 0}) async =>
      OfflineHubFixture._(await HttpServer.bind(InternetAddress.loopbackIPv4, port));

  Future<void> _reply(HttpRequest request) async {
    Object reply = {'error': 'fixture_route_missing'};
    var status = 404;
    final path = request.uri.path.replaceFirst('/relay/account/m/~$desktop/', '/relay/account/');
    if (request.headers.value(HttpHeaders.authorizationHeader) != 'Bearer $token') {
      status = 401;
    } else {
      status = 200;
      switch (path) {
        case '/relay/whoami':
          reply = {'ok': true, 'account': 'hub-fixture', 'device_id': 'fixture-phone', 'kind': 'phone'};
        case '/relay/devices':
          reply = {
            'devices': [
              {'device_id': desktop, 'label': machine, 'kind': 'desktop', 'online': true},
            ],
          };
        case '/relay/account-sync':
          reply = {'revision': 0, 'settings': {}};
        case '/relay/account/mobile/me':
          reply = {'name': 'fixture', 'owner': true, 'machine': machine};
        case '/relay/account/version':
          reply = {'machine_id': desktop};
        case '/relay/account/term/panes':
          reply = panes;
        case '/relay/account/sessions':
          reply = {'labels': [room]};
        case '/relay/account/windows':
          reply = {
            'windows': [
              {
                'idx': 0,
                'active': true,
                'aspect': 1.6,
                'name': room,
                'cwd': '~/offline',
                'panes': [
                  {'surface_id': '%1', 'x': 0, 'y': 0, 'w': 60, 'h': 100},
                  {'surface_id': '%2', 'x': 60, 'y': 0, 'w': 40, 'h': 100},
                ],
              },
            ],
          };
        case '/relay/account/machines':
          reply = {'machines': []};
        case '/relay/account/term/notes':
          reply = [];
        case '/relay/conversations':
          reply = {
            'enabled': true,
            'conversations': [entry.toJson()],
          };
        default:
          if (path == '/relay/conversations/${entry.id}') {
            final offset = int.parse(request.uri.queryParameters['offset']!);
            final end = int.parse(request.uri.queryParameters['end']!);
            reply = {
              'chunk': {
                'raw': utf8.decode(utf8.encode(raw).sublist(offset, end)),
                'next': end,
                'end': end,
                'generation': entry.generation,
              },
            };
          } else {
            status = 404;
          }
      }
    }
    await request.drain<void>();
    request.response.statusCode = status;
    request.response.headers.contentType = ContentType.json;
    request.response.write(jsonEncode(reply));
    await request.response.close();
  }

  Future<void> close() => http.close(force: true);
}
