import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/screens/settings.dart';
import 'package:kasaterm_mobile/server.dart';

/// 설정 화면이 묻는 관문 길만 답하는 가짜 — 프로필·로그인 공급자·데스크톱 외형. 나머지는 404.
class SettingsFixture {
  static final origin = Uri.parse('https://settings.fixture.test');
  static const _session = (account: 'fixture', device: 'phone-fixture', token: 'settings-fixture-token');

  final paths = <String>[];

  late final MockClient client = MockClient(answer);

  AccountSession get session => AccountSession(
    origin: origin,
    account: _session.account,
    deviceId: _session.device,
    token: _session.token,
  );

  late final Server server = Server.account(session, client: client, connectDirect: false);

  RelayAccountApi api() => RelayAccountApi(origin, session: session, client: client);

  Future<http.Response> answer(http.Request req) async {
    paths.add(req.url.path);
    final path = req.url.path;
    if (path == '/relay/profile') {
      return _json({
        'login': 'kasa',
        'has_password': true,
        'nickname': '카사',
        'identities': [
          {'provider': 'google', 'display': 'kasa@example.test'},
        ],
      });
    }
    if (path == '/relay/oauth/providers') {
      return _json({
        'providers': [
          {'id': 'google', 'enabled': true},
          {'id': 'github', 'enabled': true},
        ],
      });
    }
    if (path.endsWith('settings/values')) {
      return _json({
        'appearance': {
          'accent': 'Blue',
          'accents': [
            {'name': 'Blue', 'hex': '#5B9BF5'},
            {'name': 'Pink', 'hex': '#F28DB2'},
            {'name': 'Green', 'hex': '#6CC68A'},
            {'name': 'Amber', 'hex': '#F5C95A'},
            {'name': 'Purple', 'hex': '#A78BFA'},
          ],
          'shape': 'rounded',
          'shapes': [
            {'key': 'rounded', 'label': '둥글게'},
            {'key': 'sharp', 'label': '각지게'},
            {'key': 'pixel', 'label': '픽셀'},
          ],
        },
      });
    }
    return _json({'error': 'not found'}, 404);
  }

  Widget screen({ThemeData? theme}) => MaterialApp(
    theme: theme,
    home: SettingsScreen(
      server: server,
      onChangeAddress: () async {},
      installId: () async => 'fixture-install',
      accountApi: api,
    ),
  );
}

http.Response _json(Object body, [int status = 200]) =>
    http.Response.bytes(utf8.encode(jsonEncode(body)), status, headers: {'content-type': 'application/json'});
