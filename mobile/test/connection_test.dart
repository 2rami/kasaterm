import 'dart:async';
import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/connection.dart';
import 'package:kasaterm_mobile/connection_store.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/server.dart';

import 'relay_account_test.dart' show session;

class MemoryStore extends ConnectionStore {
  SavedConnection? value;
  @override
  Future<SavedConnection?> load() async => value;
  @override
  Future<void> save(SavedConnection connection) async {
    value = connection;
  }
}

ConnectionController controller(
  MemoryStore store, {
  int authStatus = 200,
  int desktopStatus = 200,
  Completer<http.Response>? pendingLogin,
  void Function(http.Request)? observe,
}) => ConnectionController(
  store: store,
  relayFactory: (origin, credentials) => RelayAccountApi(
    origin,
    session: credentials,
    client: MockClient((request) async {
      observe?.call(request);
      if (request.url.path == '/relay/login') {
        if (pendingLogin != null) return pendingLogin.future;
        return http.Response(jsonEncode(session().toJson()), 200);
      }
      if (request.url.path == '/relay/whoami') {
        return http.Response(
          '{"ok":true,"account":"fixture","device_id":"phone-fixture","kind":"phone"}',
          authStatus,
        );
      }
      return http.Response('{"ok":true,"devices":[]}', 200);
    }),
  ),
  serverFactory: (session) => Server.account(
    session,
    client: MockClient(
      (_) async =>
          http.Response('{"name":"desktop","owner":true}', desktopStatus),
    ),
  ),
);

void main() {
  test(
    'login remains successful when there is no online or updated desktop',
    () async {
      final store = MemoryStore();
      final connection = controller(store, desktopStatus: 503);
      await connection.login(session().origin, 'fixture', 'test-password');
      expect(connection.phase, ConnectionPhase.waiting);
      expect(connection.account!.account, 'fixture');
      expect(store.value!.account, isNotNull);
      expect(connection.server, isNull);
      expect(connection.message, contains('데스크톱'));
      connection.dispose();
    },
  );

  test('restored invalid token is removed and requests login again', () async {
    final store = MemoryStore()..value = SavedConnection(account: session());
    final connection = controller(store, authStatus: 401);
    await connection.restore();
    expect(connection.phase, ConnectionPhase.signedOut);
    expect(connection.account, isNull);
    expect(store.value!.account, isNull);
    expect(connection.message, contains('만료'));
    connection.dispose();
  });

  test('account root 401 after verification also expires the login', () async {
    final store = MemoryStore()..value = SavedConnection(account: session());
    final connection = controller(store, desktopStatus: 401);
    await connection.restore();
    expect(connection.phase, ConnectionPhase.signedOut);
    expect(store.value!.account, isNull);
    connection.dispose();
  });

  test(
    'explicit logout closes old server and revokes only this device token',
    () async {
      final requests = <http.Request>[];
      final store = MemoryStore()..value = SavedConnection(account: session());
      final connection = controller(store, observe: requests.add);
      await connection.restore();
      final old = connection.server!;
      var closed = false;
      old.addCloseListener(() => closed = true);
      final logout = connection.logout();
      expect(old.isClosed, isTrue);
      expect(closed, isTrue);
      expect(connection.account, isNull);
      await logout;
      await Future<void>.delayed(Duration.zero);
      final revoke = requests.singleWhere((r) => r.url.path == '/relay/logout');
      expect(revoke.headers['authorization'], 'Bearer test-device-token');
      expect(revoke.method, 'POST');
      expect(store.value!.account, isNull);
      connection.dispose();
    },
  );

  test('계정 확인을 기다리는 동안 기기 목록·데스크톱 확인이 이미 떠 있다', () async {
    final whoami = Completer<http.Response>();
    final asked = <String>[];
    final store = MemoryStore()..value = SavedConnection(account: session());
    final connection = ConnectionController(
      store: store,
      relayFactory: (origin, credentials) => RelayAccountApi(
        origin,
        session: credentials,
        client: MockClient((request) async {
          asked.add(request.url.path);
          if (request.url.path == '/relay/whoami') return whoami.future;
          return http.Response('{"ok":true,"devices":[{"id":"mac"}]}', 200);
        }),
      ),
      serverFactory: (session) => Server.account(
        session,
        client: MockClient((request) async {
          asked.add('desktop:${request.url.path}');
          return http.Response('{"name":"desktop","owner":true}', 200);
        }),
      ),
    );
    final restoring = connection.restore();
    await Future<void>.delayed(const Duration(milliseconds: 10));
    expect(asked, containsAll(['/relay/whoami', '/relay/devices', 'desktop:/relay/account/mobile/me']));
    expect(connection.phase, ConnectionPhase.checking);
    whoami.complete(http.Response(
      '{"ok":true,"account":"fixture","device_id":"phone-fixture","kind":"phone"}',
      200,
    ));
    await restoring;
    expect(connection.phase, ConnectionPhase.ready);
    expect(connection.devices.single['id'], 'mac');
    connection.dispose();
  });

  test('logout wins over a late successful login response', () async {
    final pending = Completer<http.Response>();
    final store = MemoryStore();
    final connection = controller(store, pendingLogin: pending);
    final login = connection.login(
      session().origin,
      'fixture',
      'test-password',
    );
    await connection.logout();
    pending.complete(http.Response(jsonEncode(session().toJson()), 200));
    await login;
    expect(connection.phase, ConnectionPhase.signedOut);
    expect(connection.server, isNull);
    expect(store.value!.account, isNull);
    connection.dispose();
  });

  test(
    'logout sentinel prevents baked legacy address from silently reconnecting',
    () async {
      final store = MemoryStore()..value = const SavedConnection();
      final connection = controller(store);
      await connection.restore(bakedRoot: 'https://old.invalid/u/secret/');
      expect(connection.phase, ConnectionPhase.signedOut);
      expect(connection.server, isNull);
      connection.dispose();
    },
  );

  test('two accounts at the same gateway never share cache identity', () {
    final a = Server.account(session());
    final b = Server.account(session(account: 'other'));
    expect(a.root, b.root);
    expect(a.cacheIdentity, isNot(b.cacheIdentity));
    a.close();
    b.close();
  });

  test(
    'local close is immediate but remote revoke follows bounded push cleanup',
    () async {
      final done = Completer<bool>();
      final requests = <http.Request>[];
      final store = MemoryStore()..value = SavedConnection(account: session());
      final connection = controller(store, observe: requests.add);
      await connection.restore();
      final old = connection.server!;
      connection.beforeDisconnect = () => done.future;
      await connection.logout();
      expect(old.isClosed, isTrue);
      expect(store.value!.account, isNull);
      expect(requests.where((r) => r.url.path == '/relay/logout'), isEmpty);
      done.complete(false);
      await Future<void>.delayed(Duration.zero);
      expect(requests.where((r) => r.url.path == '/relay/logout').length, 1);
      expect(connection.message, contains('알림 해제'));
      connection.dispose();
    },
  );
}
