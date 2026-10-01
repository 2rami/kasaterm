import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/connection_store.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/term_socket_io.dart';

AccountSession session({String account = 'fixture'}) => AccountSession(
  origin: Uri.parse('https://gateway.invalid'),
  account: account,
  deviceId: 'phone-fixture',
  token: 'test-device-token',
);

class _NativeHttp extends HttpOverrides {}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test(
    'gateway excludes credentials, paths, query tokens and cleartext remotes',
    () {
      for (final raw in [
        'http://example.com',
        'https://name:secret@example.com',
        'https://example.com/relay/',
        'https://example.com/?token=secret',
        'https://example.com/#token',
        'file:///tmp',
      ]) {
        expect(parseGateway(raw), isNull, reason: raw);
      }
      expect(
        parseGateway('https://example.com/').toString(),
        'https://example.com',
      );
      expect(parseGateway('http://127.0.0.1:8765'), isNotNull);
    },
  );

  test(
    'login is phone kind and password exists only in the request body',
    () async {
      final api = RelayAccountApi(
        session().origin,
        client: MockClient((request) async {
          expect(request.url.path, '/relay/login');
          expect(request.url.query, isEmpty);
          expect(request.followRedirects, isFalse);
          expect(request.headers['authorization'], isNull);
          expect(jsonDecode(request.body), {
            'account': 'fixture',
            'password': 'test-password',
            'kind': 'phone',
            'label': '카사모바일',
          });
          return http.Response(
            jsonEncode({
              'account': 'fixture',
              'device_id': 'phone-fixture',
              'token': 'test-device-token',
            }),
            200,
          );
        }),
      );
      final value = await api.login(' fixture ', 'test-password');
      expect(jsonEncode(value.toJson()), isNot(contains('password')));
      expect(value.deviceId, 'phone-fixture');
      api.close();
    },
  );

  for (final status in [401, 404, 429, 503]) {
    test(
      'status $status is safe and actionable without echoing response data',
      () async {
        final api = RelayAccountApi(
          session().origin,
          client: MockClient(
            (_) async =>
                http.Response('test-device-token test-password', status),
          ),
        );
        await expectLater(
          api.login('fixture', 'test-password'),
          throwsA(
            isA<AccountException>()
                .having((e) => e.status, 'status', status)
                .having(
                  (e) => e.message,
                  'redaction',
                  isNot(contains('test-')),
                ),
          ),
        );
        api.close();
      },
    );
  }

  test('관문의 Ad Hoc 판은 기기 토큰으로만 묻고 설치 주소는 itms-services 만 받는다', () async {
    final seen = <Uri>[];
    final s = Server.account(
      session(),
      client: MockClient((req) async {
        seen.add(req.url);
        expect(req.headers['authorization'], 'Bearer test-device-token');
        return http.Response(
          jsonEncode({
            'ok': true,
            'release': {
              'version': '1.0.0',
              'build': '2610011039',
              'install': 'itms-services://?action=download-manifest&url=https%3A%2F%2Fgateway.invalid%2Fm',
            },
          }),
          200,
        );
      }),
    );
    final r = await s.latestRelease();
    expect(seen.single.toString(), 'https://gateway.invalid/relay/install/latest');
    expect(r?.build, '2610011039');
    // 시험 판은 KASA_BUILD 가 없다 — 개발 설치처럼 새 판을 알리지 않는다.
    expect(r?.newer, isFalse);
    s.close();

    final odd = Server.account(
      session(),
      client: MockClient((_) async => http.Response(
        '{"ok":true,"release":{"version":"1","build":"2","install":"https://other.invalid/x"}}',
        200,
      )),
    );
    expect(await odd.latestRelease(), isNull);
    odd.close();
    final denied = Server.account(session(), client: MockClient((_) async => http.Response('{}', 403)));
    expect(await denied.latestRelease(), isNull);
    denied.close();
    expect(await Server(Uri.parse('https://gateway.invalid/u/x/')).latestRelease(), isNull);
  });

  test(
    'Bearer is origin-bound, redirects disabled, WS token is never a URL',
    () async {
      var requests = 0;
      final value = session();
      final s = Server.account(
        value,
        client: MockClient((request) async {
          requests++;
          expect(request.headers['authorization'], 'Bearer ${value.token}');
          expect(request.followRedirects, isFalse);
          expect(request.url.toString(), isNot(contains(value.token)));
          return http.Response('{"name":"desktop","owner":true}', 200);
        }),
      );
      await s.me();
      expect(s.wsProtocols, [
        'kasa-relay-account',
        'kasa-auth.test-device-token',
      ]);
      expect(
        s.wsUri('term/ws', query: {'pane': '%7'}).toString(),
        isNot(contains(value.token)),
      );
      await expectLater(
        s.imageBytes(Uri.parse('https://other.invalid/image')),
        throwsA(isA<ServerException>()),
      );
      expect(() => s.uri('../outside'), throwsA(isA<ServerException>()));
      expect(requests, 1);
      s.close();
      await expectLater(s.me(), throwsA(isA<ServerException>()));
      expect(requests, 1);
    },
  );

  test('HTTP redirect is not accepted as a valid session', () async {
    final api = RelayAccountApi(
      session().origin,
      session: session(),
      client: MockClient((request) async {
        expect(request.followRedirects, isFalse);
        return http.Response(
          '',
          302,
          headers: {'location': 'https://other.invalid'},
        );
      }),
    );
    await expectLater(api.verify(session()), throwsA(isA<AccountException>()));
    api.close();
  });

  test(
    'late HTTP completion from a closed account cannot populate data',
    () async {
      final response = Completer<http.Response>();
      final s = Server.account(
        session(),
        client: MockClient((_) => response.future),
      );
      final pending = s.me();
      final check = expectLater(pending, throwsA(isA<ServerException>()));
      s.close();
      response.complete(http.Response('{"name":"old","owner":true}', 200));
      await check;
    },
  );

  test(
    'whoami validates account and device_id instead of accepting any 200',
    () async {
      final api = RelayAccountApi(
        session().origin,
        session: session(),
        client: MockClient(
          (_) async => http.Response(
            '{"ok":true,"account":"another","device_id":"phone-fixture","kind":"phone"}',
            200,
          ),
        ),
      );
      await expectLater(
        api.verify(session()),
        throwsA(isA<AccountException>().having((e) => e.status, 'status', 401)),
      );
      api.close();
    },
  );

  test(
    'one secure record persists credentials, logout suppresses legacy restore',
    () async {
      FlutterSecureStorage.setMockInitialValues({
        'root': 'https://old.invalid/u/old/',
      });
      const store = ConnectionStore();
      await store.save(SavedConnection(account: session()));
      final all = await const FlutterSecureStorage().readAll();
      expect(all.keys, [ConnectionStore.key]);
      expect(all.values.single, isNot(contains('password')));
      expect((await store.load())!.account!.account, 'fixture');
      await store.save(const SavedConnection());
      final loggedOut = await store.load();
      expect(loggedOut, isNotNull);
      expect(loggedOut!.account, isNull);
      expect(loggedOut.legacyRoot, isNull);
    },
  );

  test(
    'native WS uses subprotocol auth without redirect credential leakage',
    () => HttpOverrides.runWithHttpOverrides(() async {
      final origin = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
      final destination = await HttpServer.bind(
        InternetAddress.loopbackIPv4,
        0,
      );
      var leaked = false;
      destination.listen((request) {
        leaked = true;
        request.response.close();
      });
      origin.listen((request) {
        expect(request.uri.queryParameters, {'pane': '%7'});
        expect(
          request.headers.value('sec-websocket-protocol'),
          contains('kasa-auth.test-device-token'),
        );
        request.response.statusCode = 302;
        request.response.headers.set(
          'location',
          'http://127.0.0.1:${destination.port}/',
        );
        request.response.close();
      });
      final channel = connectTermSocket(
        Uri.parse('ws://127.0.0.1:${origin.port}/term/ws?pane=%257'),
        protocols: session().protocols,
      );
      final done = channel.stream.drain<void>().catchError((Object _) {});
      await expectLater(channel.ready, throwsA(anything));
      await done;
      expect(leaked, isFalse);
      await origin.close(force: true);
      await destination.close(force: true);
    }, _NativeHttp()),
  );

  test(
    'native WS successful upgrade remains open after handshake client closes',
    () => HttpOverrides.runWithHttpOverrides(() async {
      final origin = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
      origin.listen((request) async {
        final socket = await WebSocketTransformer.upgrade(
          request,
          protocolSelector: (protocols) =>
              protocols.contains('kasa-relay-account')
              ? 'kasa-relay-account'
              : null,
        );
        socket.listen((data) => socket.add(data));
      });
      final channel = connectTermSocket(
        Uri.parse('ws://127.0.0.1:${origin.port}/term/ws'),
        protocols: session().protocols,
      );
      await channel.ready;
      final first = channel.stream.first;
      channel.sink.add('fixture');
      expect(await first, 'fixture');
      await channel.sink.close();
      await origin.close(force: true);
    }, _NativeHttp()),
  );
}
