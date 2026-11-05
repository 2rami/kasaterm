import 'dart:convert';

import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/relay_account.dart';

class HubFixture {
  late final Server server = Server.account(
    AccountSession(
      origin: Uri.parse('https://fixture.test'),
      account: 'fixture',
      deviceId: 'phone-fixture',
      token: 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
    ),
    client: MockClient(answer),
    connectDirect: false,
  );
  final commands = <Map<String, dynamic>>[];
  final List<Map<String, dynamic>> rows = [
    {
      'id': '%1',
      'surface_key': 'alpha-key',
      'name': '알파',
      'session': '계획 검토',
      'harness': 'claude',
      'status': 'waiting',
      'kind': 'question',
      'window': 0,
      'cwd': '/fixture',
      'subagents': ['구조 검토', '테스트 조사'],
    },
    {
      'id': '%2',
      'surface_key': 'beta-key',
      'name': '베타',
      'session': '화면 구현',
      'harness': 'codex',
      'status': 'idle',
      'window': 0,
      'cwd': '/fixture',
    },
  ];
  final Map<String, Map<String, dynamic>> closed = {};
  String? replacedKey;
  bool requireConfirmation = false;
  bool failClose = false;
  bool partialClose = false;
  Pane pane(int index) => Pane.fromJson(rows[index], machine: '~fixture');
  List<Map<String, dynamic>> get addresses => [
    for (final row in rows)
      {
        'machine_id': 'fixture',
        'surface_id': row['id'],
        'surface_key': replacedKey ?? row['surface_key'],
        'instance_id': 'fixture-instance',
        'session_id': '${row['surface_key']}-session',
      },
  ];
  Future<http.Response> answer(http.Request request) async {
    final path = request.url.path;
    Object value = {};
    var status = 200;
    if (path.endsWith('term/panes')) {
      value = rows;
    } else if (path.endsWith('mobile/me')) {
      value = {'owner': true, 'machine': '검증 기기'};
    } else if (path.endsWith('version')) {
      value = {'machine_id': 'fixture'};
    } else if (path.endsWith('machines')) {
      value = {'machines': []};
    } else if (path.endsWith('sessions')) {
      value = {
        'labels': ['검증 방'],
      };
    } else if (path.endsWith('windows')) {
      value = {
        'windows': [
          {
            'idx': 0,
            'active': true,
            'aspect': 1.8,
            'panes': [
              for (final (i, row) in rows.indexed)
                {
                  'surface_id': row['id'],
                  'x': i * 50,
                  'y': 0,
                  'w': 50,
                  'h': 100,
                },
            ],
          },
        ],
      };
    } else if (path.endsWith('settings/values')) {
      value = {'appearance': {}};
    } else if (path.endsWith('term/notes')) {
      value = {'notes': []};
    } else if (path.endsWith('collab/board')) {
      value = {
        'panes': [
          for (final address in addresses) {'address': address},
        ],
      };
    } else if (path.endsWith('cmd')) {
      final body = jsonDecode(request.body) as Map<String, dynamic>;
      commands.add({...body, 'path': path});
      final method = body['method'];
      final params = body['params'] as Map<String, dynamic>;
      if (method == 'surface.close_idle_checked' && requireConfirmation) {
        value = {
          'ok': true,
          'result': {'requires_confirmation': true},
        };
      } else if (method == 'surface.close_checked' ||
          method == 'surface.close_idle_checked') {
        if (failClose) {
          value = {'ok': false, 'error': '세션이 바뀌었어요'};
        } else {
          final keys = [
            for (final a in params['addresses'] as List) a['surface_key'],
          ];
          final records = <Map<String, dynamic>>[];
          rows.removeWhere((row) {
            if (partialClose && records.isNotEmpty) return false;
            if (!keys.contains(row['surface_key'])) return false;
            final key = row['surface_key'] as String;
            closed[key] = row;
            records.add({
              'machine_id': 'fixture',
              'source_key': key,
              'restore_key': 'restore-$key',
            });
            return true;
          });
          value = {
            'ok': true,
            'result': {
              'closed': !partialClose,
              'error': partialClose ? '일부 칸을 닫지 못했어요.' : null,
              'records': records,
            },
          };
        }
      } else if (method == 'surface.reopen_checked') {
        final row = closed.remove(params['source_key']);
        if (row != null) rows.add(row);
        value = {'ok': true, 'result': {}};
      } else {
        value = {'ok': true, 'result': {}};
      }
    } else {
      status = 404;
    }
    return http.Response.bytes(
      utf8.encode(jsonEncode(value)),
      status,
      headers: {'content-type': 'application/json'},
    );
  }
}
