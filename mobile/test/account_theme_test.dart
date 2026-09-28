import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/theme_prefs.dart';

import 'relay_account_test.dart' show session;

http.Response snapshot(int revision, String mode) => http.Response(
  jsonEncode({
    'revision': revision,
    'settings': {'mobile_theme_mode': mode, 'theme': 'graphite'},
    'machines': {},
  }),
  200,
);

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  setUp(() => FlutterSecureStorage.setMockInitialValues({}));

  test(
    'login reads account mode and persists only that account cache',
    () async {
      final sync = PhoneThemeSync(
        apiFactory: (account) => RelayAccountApi(
          account.origin,
          session: account,
          client: MockClient((_) async => snapshot(5, 'dark')),
        ),
      );
      await sync.bind(session());
      expect(phoneThemeMode.value, ThemeMode.dark);
      expect(await const ThemePrefs().load(), ThemeMode.system);
      expect(
        await ThemePrefs(scope: '${session().origin}|fixture').load(),
        ThemeMode.dark,
      );
      sync.unbind();
    },
  );

  test(
    'theme CAS retries conflict without writing unrelated desktop settings',
    () async {
      final revisions = <int>[];
      final sync = PhoneThemeSync(
        apiFactory: (account) => RelayAccountApi(
          account.origin,
          session: account,
          client: MockClient((request) async {
            if (request.method == 'GET') return snapshot(5, 'light');
            final body = jsonDecode(request.body);
            revisions.add(body['expected_revision'] as int);
            expect(body['settings'], {'mobile_theme_mode': 'dark'});
            expect(body['machines'], isEmpty);
            expect(
              request.headers['authorization'],
              'Bearer test-device-token',
            );
            expect(request.followRedirects, isFalse);
            if (revisions.length == 1) {
              return http.Response(
                jsonEncode({
                  'error': 'revision_conflict',
                  'current': jsonDecode(snapshot(6, 'system').body),
                }),
                409,
              );
            }
            return snapshot(7, 'dark');
          }),
        ),
      );
      await sync.bind(session());
      expect(await sync.setMode(ThemeMode.dark), isNull);
      expect(revisions, [5, 6]);
      expect(phoneThemeMode.value, ThemeMode.dark);
      sync.unbind();
    },
  );

  test(
    'late previous-account theme response cannot overwrite new account',
    () async {
      final delayed = Completer<http.Response>();
      final entered = Completer<void>();
      final sync = PhoneThemeSync(
        apiFactory: (account) => RelayAccountApi(
          account.origin,
          session: account,
          client: MockClient((_) async {
            if (account.account == 'fixture') {
              entered.complete();
              return delayed.future;
            }
            return snapshot(3, 'light');
          }),
        ),
      );
      final old = sync.bind(session());
      await entered.future;
      await sync.bind(session(account: 'other'));
      delayed.complete(snapshot(8, 'dark'));
      await old;
      expect(phoneThemeMode.value, ThemeMode.light);
      expect(sync.account!.account, 'other');
      sync.unbind();
    },
  );

  test('a late refresh cannot undo a newer explicit user selection', () async {
    final delayed = Completer<http.Response>();
    var gets = 0;
    final sync = PhoneThemeSync(
      apiFactory: (account) => RelayAccountApi(
        account.origin,
        session: account,
        client: MockClient((request) async {
          if (request.method == 'PATCH') return snapshot(3, 'dark');
          if (++gets == 2) return delayed.future;
          return snapshot(1, 'light');
        }),
      ),
    );
    await sync.bind(session());
    final refresh = sync.refresh();
    await sync.setMode(ThemeMode.dark);
    delayed.complete(snapshot(2, 'light'));
    await refresh;
    expect(phoneThemeMode.value, ThemeMode.dark);
    sync.unbind();
  });

  test(
    'unreachable sync remains a truthful local change, not a fake saved state',
    () async {
      final sync = PhoneThemeSync(
        apiFactory: (account) => RelayAccountApi(
          account.origin,
          session: account,
          client: MockClient((request) async {
            return request.method == 'PATCH'
                ? http.Response('', 503)
                : snapshot(1, 'light');
          }),
        ),
      );
      await sync.bind(session());
      expect(await sync.setMode(ThemeMode.dark), contains('계정에 저장하지 못했어요'));
      expect(phoneThemeMode.value, ThemeMode.dark);
      sync.unbind();
    },
  );

  test(
    'refresh started during pending PATCH cannot apply stale server mode',
    () async {
      final patch = Completer<http.Response>();
      final entered = Completer<void>();
      var gets = 0;
      final sync = PhoneThemeSync(
        apiFactory: (account) => RelayAccountApi(
          account.origin,
          session: account,
          client: MockClient((request) async {
            if (request.method == 'PATCH') {
              entered.complete();
              return patch.future;
            }
            gets++;
            return snapshot(1, 'light');
          }),
        ),
      );
      await sync.bind(session());
      final saving = sync.setMode(ThemeMode.dark);
      await entered.future;
      await sync.refresh();
      expect(gets, 1);
      expect(phoneThemeMode.value, ThemeMode.dark);
      patch.complete(snapshot(2, 'dark'));
      await saving;
      expect(phoneThemeMode.value, ThemeMode.dark);
      sync.unbind();
    },
  );

  test('older concurrent refresh cannot replace a newer response', () async {
    final old = Completer<http.Response>();
    var gets = 0;
    final sync = PhoneThemeSync(
      apiFactory: (account) => RelayAccountApi(
        account.origin,
        session: account,
        client: MockClient((_) async {
          gets++;
          if (gets == 2) return old.future;
          return snapshot(gets, gets == 1 ? 'system' : 'dark');
        }),
      ),
    );
    await sync.bind(session());
    final first = sync.refresh();
    await sync.refresh();
    old.complete(snapshot(2, 'light'));
    await first;
    expect(phoneThemeMode.value, ThemeMode.dark);
    sync.unbind();
  });
}
