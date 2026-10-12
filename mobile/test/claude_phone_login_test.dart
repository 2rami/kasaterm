import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/claude_login.dart';
import 'package:kasaterm_mobile/main.dart' show buildTheme;
import 'package:kasaterm_mobile/screens/claude_accounts.dart';
import 'package:kasaterm_mobile/screens/controls.dart' show SettingsRow;
import 'package:kasaterm_mobile/server.dart';

import 'relay_account_test.dart' show session;
import 'fixtures/claude_accounts.dart';

const _nonce = '10e6de91-11c8-477a-a384-b93fd905c2ed';
const _code = 'fixture-code-123456#fixture-state-123456';
const _authorizationUrl =
    'https://claude.ai/oauth/authorize?state=fixture-state-123456&code_challenge=fixture-pkce&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback';

http.Response _json(Object body, [int status = 200]) => http.Response.bytes(
  utf8.encode(jsonEncode(body)),
  status,
  headers: {'content-type': 'application/json'},
);

Map<String, Object?> _flow({
  String state = 'awaiting_code',
  String machine = 'book',
  String slot = 'personal',
  String flow = 'flow-fixture',
  String? url,
  String? error,
  bool? runtimeUpdated,
  String? runtimeErrorCode,
}) => {
  'flow_id': flow,
  'slot_id': slot,
  'machine_id': machine,
  'label': '개인',
  'email': 'person@example.test',
  'org': 'fixture-org',
  'state': state,
  'expires_at_ms': 1791831600000,
  'authorization_url': ?url,
  'error_code': ?error,
  'runtime_updated': ?runtimeUpdated,
  'runtime_error_code': ?runtimeErrorCode,
  if (state == 'needs_confirmation')
    'confirmation_nonce': 'fixture-confirmation',
};

class _Fake {
  final posts = <({String path, Map<String, dynamic> body})>[];
  final opened = <Uri>[];
  String state = 'awaiting_code';
  String? afterCode = 'succeeded';
  String? responseMachine;
  String? responseSlot;
  String? responseFlow;
  String? cancelError;
  String? error;
  bool empty = false;
  bool unavailable = false;
  bool missingIdentity = false;
  bool unknownAction = false;
  bool maliciousUrl = false;
  String? authorizationUrl;
  bool startFailure = false;
  bool remotes = true;
  bool? runtimeUpdated;
  String? runtimeErrorCode;
  Completer<void>? gate;
  Completer<http.Response>? statusReply;
  Completer<http.Response>? startReply;
  Completer<http.Response>? cancelReply;
  Future<http.Response> handle(http.Request request) async {
    if (request.url.path.endsWith('/version')) {
      return _json(missingIdentity ? {} : {'machine_id': 'book'});
    }
    if (request.url.path.endsWith('/mobile/me')) {
      return _json({'name': 'fixture', 'owner': true, 'machine': '맥북'});
    }
    if (request.url.path.endsWith('/machines')) {
      return _json({
        'machines': [
          if (remotes) ...[
            {'label': '미니', 'route': '~mini', 'online': true, 'panes': []},
            {
              'label': '이름만 있는 기기',
              'route': 'legacy',
              'online': true,
              'panes': [],
            },
            {
              'label': '꺼진 기기',
              'route': '~offline',
              'online': false,
              'panes': [],
            },
          ],
        ],
      });
    }
    if (request.url.path.endsWith('/account-machines')) {
      return _json({'machines': []});
    }
    if (request.url.path.endsWith('/settings/values')) {
      if (unavailable) return _json({'error': 'offline'}, 503);
      return _json({
        'claude': {
          'accounts': [
            {'id': '', 'label': '기본'},
            if (!empty) ...claudeSettingsAccounts.take(2),
          ],
        },
      });
    }
    if (request.url.path.endsWith('/settings/action')) {
      final body = jsonDecode(request.body) as Map<String, dynamic>;
      posts.add((path: request.url.path, body: body));
      if (unknownAction) {
        return _json({'ok': false, 'error_code': 'action_unknown'});
      }
      if (gate != null) await gate!.future;
      final action = body['action'];
      if (action == 'claude-phone-login-start') {
        final label = jsonDecode(body['label'] as String) as Map;
        if (label['machine_id'] !=
                (request.url.path.contains('~mini') ? 'mini' : 'book') ||
            !RegExp(
              r'^[0-9a-fA-F]{8}(-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}$',
            ).hasMatch(label['nonce'] as String)) {
          return _json({
            'ok': false,
            'error_code': 'phone_login_invalid_start',
          });
        }
        if (startFailure) throw http.ClientException('fixture offline');
        if (startReply != null) return startReply!.future;
      }
      if (action == 'claude-phone-login-status' && statusReply != null) {
        return statusReply!.future;
      }
      if (action == 'claude-phone-login-code' && afterCode != null) {
        if (!RegExp(
          r'^[A-Za-z0-9_-]{16,}#[A-Za-z0-9_-]{16,}$',
        ).hasMatch(body['label'] as String)) {
          return _json({'ok': false, 'error_code': 'phone_login_invalid_code'});
        }
        state = afterCode!;
      }
      if (action == 'claude-phone-login-cancel') {
        if (cancelError != null) {
          return _json({'ok': false, 'error_code': cancelError});
        }
        if (cancelReply != null) return cancelReply!.future;
        state = 'cancelled';
      }
      if (action == 'claude-phone-login-confirm') state = 'succeeded';
      return _json({
        'ok': true,
        'phone_login': _flow(
          state: state,
          machine:
              responseMachine ??
              (request.url.path.contains('~mini') ? 'mini' : 'book'),
          slot: responseSlot ?? 'personal',
          flow: responseFlow ?? 'flow-fixture',
          error: error,
          runtimeUpdated: runtimeUpdated,
          runtimeErrorCode: runtimeErrorCode,
          url: state == 'awaiting_code'
              ? maliciousUrl
                    ? 'https://claude.ai.evil.invalid/oauth/authorize'
                    : authorizationUrl ?? _authorizationUrl
              : null,
        ),
      });
    }
    return _json({'error': 'not_found'}, 404);
  }

  Server server() => Server.account(session(), client: MockClient(handle));
}

Future<void> _show(
  WidgetTester tester,
  _Fake fake, {
  Server? server,
  double width = 390,
  double scale = 1,
  Brightness brightness = Brightness.light,
  Duration every = const Duration(seconds: 2),
}) async {
  tester.view.physicalSize = Size(width, 844);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  final source = server ?? fake.server();
  addTearDown(source.close);
  await tester.pumpWidget(
    MaterialApp(
      theme: buildTheme(brightness),
      builder: (context, child) => MediaQuery(
        data: MediaQuery.of(
          context,
        ).copyWith(textScaler: TextScaler.linear(scale)),
        child: child!,
      ),
      home: RepaintBoundary(
        key: const Key('capture'),
        child: ClaudeAccountsScreen(
          server: source,
          every: every,
          open: (uri) async {
            fake.opened.add(uri);
            return true;
          },
        ),
      ),
    ),
  );
  await tester.pumpAndSettle();
}

Future<void> _start(WidgetTester tester) async {
  await tester.tap(find.byKey(const Key('claude-login-personal')));
  await tester.pumpAndSettle();
}

Future<void> _end(WidgetTester tester) async {
  await tester.pumpWidget(const SizedBox());
  await tester.pump();
}

void main() {
  test(
    'real settings rows distinguish logout, failed lookup and checking without repeated descriptions',
    () {
      final rows = ClaudeAccount.parse({
        'claude': {'accounts': claudeSettingsAccounts},
      });
      expect(rows[0].detail, '다시 로그인 필요');
      expect(rows[1].detail, 'work@example.test · Example Team');
      expect(rows[2].title, 'cached@example.test');
      expect(rows[2].detail, 'Example Team · 조회 실패');
      expect(rows[3].detail, '확인 중');
      for (final signal in [
        {'logged_in': false},
        {'usage_state': 'logged_out'},
        {'sub_code': 'account_login_required'},
      ]) {
        final account = ClaudeAccount.parse({
          'claude': {
            'accounts': [
              {
                ...claudeSettingsAccounts[1],
                'name': 'work@example.test',
                'sub': 'work@example.test · Example Team · 로그인 필요',
                ...signal,
              },
            ],
          },
        }).single;
        expect(account.detail, 'work@example.test · Example Team · 다시 로그인 필요');
      }
      final checking = ClaudeAccount.parse({
        'claude': {
          'accounts': [
            {...claudeSettingsAccounts[1], 'usage_state': 'checking'},
          ],
        },
      }).single;
      expect(checking.detail, 'work@example.test · Example Team · 확인 중');
    },
  );

  test('backend phone-login error codes use actionable safe messages', () {
    expect(
      claudeLoginError('phone_login_identity_mismatch'),
      contains('다른 Claude 계정'),
    );
    expect(claudeLoginError('phone_login_invalid_code'), contains('인증코드'));
    expect(claudeLoginError('phone_login_owner_changed'), contains('다시 연결'));
    expect(claudeLoginError('phone_login_machine_mismatch'), contains('다른 기기'));
    expect(claudeLoginError('phone_login_storage_failed'), contains('저장 공간'));
    expect(claudeLoginError('action_unknown'), contains('업데이트'));
    expect(claudeLoginError('secret://unexpected'), isNot(contains('secret')));
  });
  test('authorization URL accepts only official HTTPS authorization pages', () {
    for (final url in [
      _authorizationUrl,
      _authorizationUrl.replaceFirst('claude.ai', 'console.anthropic.com'),
      _authorizationUrl.replaceFirst('claude.ai', 'platform.claude.com'),
      _authorizationUrl.replaceFirst(
        'platform.claude.com',
        'console.anthropic.com',
      ),
    ]) {
      expect(claudeAuthorizationUrl(url), isNotNull);
    }
    for (final url in [
      'https://claude.ai.evil.test/oauth/authorize',
      'https://evil.test/oauth/authorize',
      'http://claude.ai/oauth/authorize',
      'https://claude.ai:444/oauth/authorize',
      'https://user@claude.ai/oauth/authorize',
      'https://claude.ai/oauth/authorize#code',
      'https://claude.ai/other',
      'javascript:alert(1)',
      'kasaterm://open',
      'https://claude.ai/oauth/authorize?state=abc',
      'https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A1234%2Fcallback',
      'https://claude.ai/oauth/authorize?redirect_uri=https%3A%2F%2Fevil.test%2Fcallback',
      '$_authorizationUrl&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback',
      '$_authorizationUrl&redirect_uri=%zz',
    ]) {
      expect(claudeAuthorizationUrl(url), isNull, reason: url);
    }
  });

  test(
    'typed API binds start to machine and slot and rejects ok false',
    () async {
      final fake = _Fake();
      final server = fake.server();
      addTearDown(server.close);
      final result = await server.claudePhoneLogin(
        action: 'start',
        id: 'personal',
        machineId: 'mini',
        slotId: 'personal',
        machine: '~mini',
        label: _nonce,
      );
      expect(result.machineId, 'mini');
      expect(fake.posts.single.path, '/relay/account/m/~mini/settings/action');
      expect(jsonDecode(fake.posts.single.body['label'] as String), {
        'nonce': _nonce,
        'machine_id': 'mini',
      });
      fake.unknownAction = true;
      await expectLater(
        server.claudePhoneLogin(
          action: 'status',
          id: 'flow-fixture',
          machineId: 'mini',
          slotId: 'personal',
          machine: '~mini',
        ),
        throwsA(
          isA<ServerException>().having(
            (e) => e.message,
            'message',
            contains('업데이트'),
          ),
        ),
      );
    },
  );

  test(
    'typed API rejects a different response machine, slot and flow',
    () async {
      for (final mismatch in ['machine', 'slot', 'flow']) {
        final fake = _Fake();
        if (mismatch == 'machine') fake.responseMachine = 'wrong';
        if (mismatch == 'slot') fake.responseSlot = 'wrong';
        if (mismatch == 'flow') fake.responseFlow = 'wrong';
        final server = fake.server();
        addTearDown(server.close);
        await expectLater(
          server.claudePhoneLogin(
            action: 'status',
            id: 'flow-fixture',
            machineId: 'book',
            slotId: 'personal',
            machine: '~book',
          ),
          throwsA(
            isA<ServerException>().having(
              (e) => e.message,
              'message',
              contains('달라요'),
            ),
          ),
        );
      }
    },
  );

  test(
    'typed API never sends a mutation to a name or a mismatched route',
    () async {
      final fake = _Fake();
      final server = fake.server();
      addTearDown(server.close);
      for (final route in ['미니', '~other']) {
        await expectLater(
          server.claudePhoneLogin(
            action: 'start',
            id: 'personal',
            machineId: 'mini',
            slotId: 'personal',
            machine: route,
            label: _nonce,
          ),
          throwsA(isA<ServerException>()),
        );
      }
      expect(fake.posts, isEmpty);
    },
  );

  test(
    'typed API rejects malformed nonces and manual codes before sending',
    () async {
      final fake = _Fake();
      final server = fake.server();
      addTearDown(server.close);
      for (final input in [
        (action: 'start', id: 'personal', label: 'fixture-nonce'),
        (action: 'code', id: 'flow-fixture', label: 'fixture-code'),
        (action: 'code', id: 'flow-fixture', label: '1234567890123456#short'),
        (action: 'code', id: 'flow-fixture', label: '$_code#extra'),
      ]) {
        await expectLater(
          server.claudePhoneLogin(
            action: input.action,
            id: input.id,
            machineId: 'book',
            slotId: 'personal',
            machine: '~book',
            label: input.label,
          ),
          throwsA(isA<ServerException>()),
        );
      }
      expect(fake.posts, isEmpty);
    },
  );

  testWidgets(
    'opening the list never starts login and remote selection pins every request',
    (tester) async {
      final fake = _Fake();
      await _show(tester, fake);
      expect(fake.posts, isEmpty);
      expect(find.text('기본'), findsNothing);
      expect(
        tester.getRect(find.byKey(const Key('claude-account-personal'))).top,
        lessThan(300),
      );
      await tester.tap(find.byKey(const Key('claude-device')));
      await tester.pumpAndSettle();
      expect(find.text('이름만 있는 기기'), findsNothing);
      await tester.tap(find.text('미니'));
      await tester.pumpAndSettle();
      await _start(tester);
      expect(fake.opened, isEmpty);
      await tester.tap(find.byKey(const Key('claude-open-browser')));
      await tester.pumpAndSettle();
      expect(fake.opened.single.toString(), _authorizationUrl);
      await tester.enterText(find.byKey(const Key('claude-code')), _code);
      await tester.tap(find.byKey(const Key('claude-submit-code')));
      await tester.pumpAndSettle();
      expect(fake.posts.last.body, {
        'action': 'claude-phone-login-code',
        'id': 'flow-fixture',
        'label': _code,
      });
      expect(fake.posts.every((p) => p.path.contains('/m/~mini/')), isTrue);
      expect(find.text('이 기기에 다시 로그인했어요.'), findsOneWidget);
      expect(find.byKey(const Key('claude-code')), findsNothing);
      await _end(tester);
    },
  );

  testWidgets(
    'unverified identity waits for explicit confirmation before succeeding',
    (tester) async {
      final fake = _Fake()..afterCode = 'needs_confirmation';
      await _show(tester, fake);
      await _start(tester);
      await tester.enterText(find.byKey(const Key('claude-code')), _code);
      await tester.tap(find.byKey(const Key('claude-submit-code')));
      await tester.pumpAndSettle();
      expect(find.text('이 기기에 다시 로그인했어요.'), findsNothing);
      expect(find.byKey(const Key('claude-confirm-email')), findsOneWidget);
      expect(
        fake.posts.where(
          (p) => p.body['action'] == 'claude-phone-login-confirm',
        ),
        isEmpty,
      );
      await tester.tap(find.byKey(const Key('claude-confirm')));
      await tester.pumpAndSettle();
      expect(fake.posts.last.body, {
        'action': 'claude-phone-login-confirm',
        'id': 'flow-fixture',
        'label': 'fixture-confirmation',
      });
      expect(find.text('이 기기에 다시 로그인했어요.'), findsOneWidget);
      await _end(tester);
    },
  );

  testWidgets(
    'mismatched response and unsafe URL expose no code or browser action',
    (tester) async {
      for (final mode in ['mismatch', 'url', 'manual-redirect']) {
        final fake = _Fake();
        if (mode == 'mismatch') fake.responseMachine = 'another-device';
        if (mode == 'url') fake.maliciousUrl = true;
        if (mode == 'manual-redirect') {
          fake.authorizationUrl =
              'https://claude.ai/oauth/authorize?state=fixture-state';
        }
        await _show(tester, fake);
        await _start(tester);
        expect(find.byKey(const Key('claude-open-browser')), findsNothing);
        expect(find.byKey(const Key('claude-code')), findsNothing);
        expect(fake.opened, isEmpty);
        await _end(tester);
      }
    },
  );

  testWidgets('only one poll runs and backgrounding pauses it until resume', (
    tester,
  ) async {
    final fake = _Fake();
    await _show(tester, fake);
    await _start(tester);
    fake.gate = Completer<void>();
    await tester.pump(const Duration(seconds: 2));
    expect(fake.posts.length, 2);
    expect(
      tester.widget<TextField>(find.byKey(const Key('claude-code'))).enabled,
      isTrue,
    );
    await tester.pump(const Duration(seconds: 8));
    expect(fake.posts.length, 2);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.hidden);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.paused);
    fake.gate!.complete();
    fake.gate = null;
    await tester.pump();
    await tester.pump(const Duration(seconds: 8));
    expect(fake.posts.length, 2);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.hidden);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pumpAndSettle();
    expect(fake.posts.length, 3);
    await _end(tester);
  });

  testWidgets('a slow status reply cannot restore a cancelled login', (
    tester,
  ) async {
    final fake = _Fake();
    await _show(tester, fake);
    await _start(tester);
    fake.statusReply = Completer<http.Response>();
    await tester.pump(const Duration(seconds: 2));
    await tester.tap(find.byKey(const Key('claude-cancel')));
    await tester.pumpAndSettle();
    expect(find.text('로그인을 취소했어요.'), findsOneWidget);
    fake.statusReply!.complete(_json({'ok': true, 'phone_login': _flow()}));
    await tester.pumpAndSettle();
    expect(find.text('로그인을 취소했어요.'), findsOneWidget);
    expect(find.byKey(const Key('claude-code')), findsNothing);
    await _end(tester);
  });

  testWidgets(
    'retrying an uncertain start keeps its nonce and selected target',
    (tester) async {
      final fake = _Fake()..startFailure = true;
      await _show(tester, fake);
      await _start(tester);
      expect(find.textContaining('연결을 확인'), findsOneWidget);
      final label = fake.posts.single.body['label'];
      fake.startFailure = false;
      await _start(tester);
      expect(fake.posts.length, 2);
      expect(fake.posts.last.body['label'], label);
      expect(fake.posts.last.path, fake.posts.first.path);
      await _end(tester);
    },
  );

  testWidgets(
    'a timed-out start pins its slot and device until the same nonce is recovered',
    (tester) async {
      final delayed = Completer<http.Response>();
      final fake = _Fake()..startReply = delayed;
      await _show(tester, fake);
      await _start(tester);
      await tester.pump(const Duration(seconds: 21));
      await tester.pumpAndSettle();
      expect(find.textContaining('같은 요청'), findsOneWidget);
      final request = fake.posts.single;
      expect(
        tester
            .widget<OutlinedButton>(find.byKey(const Key('claude-login-work')))
            .onPressed,
        isNull,
      );
      expect(
        tester
            .widget<IconButton>(
              find.byWidgetPredicate(
                (widget) => widget is IconButton && widget.tooltip == '새로고침',
              ),
            )
            .onPressed,
        isNull,
      );
      expect(
        tester
            .widget<SettingsRow>(find.byKey(const Key('claude-device')))
            .onTap,
        isNull,
      );
      expect(find.text('미니'), findsNothing);
      fake.startReply = null;
      await tester.ensureVisible(
        find.byKey(const Key('claude-login-personal')),
      );
      await _start(tester);
      expect(fake.posts.length, 2);
      expect(fake.posts.last.body, request.body);
      expect(fake.posts.last.path, request.path);
      expect(find.byKey(const Key('claude-code')), findsOneWidget);
      delayed.complete(
        _json({'ok': true, 'phone_login': _flow(url: _authorizationUrl)}),
      );
      await tester.pumpAndSettle();
      expect(
        fake.posts.where(
          (p) => p.body['action'] == 'claude-phone-login-cancel',
        ),
        isEmpty,
      );
      await _end(tester);
    },
  );

  testWidgets(
    'cancelling a timed-out start recovers the original nonce and cancels its flow',
    (tester) async {
      final delayed = Completer<http.Response>();
      final fake = _Fake()..startReply = delayed;
      await _show(tester, fake);
      await _start(tester);
      await tester.pump(const Duration(seconds: 21));
      await tester.pumpAndSettle();
      final request = fake.posts.single;
      fake.startReply = null;
      await tester.tap(find.byKey(const Key('claude-cancel-pending')));
      await tester.pumpAndSettle();
      expect(fake.posts[1].body, request.body);
      expect(fake.posts[1].path, request.path);
      expect(fake.posts.last.body, {
        'action': 'claude-phone-login-cancel',
        'id': 'flow-fixture',
      });
      expect(find.text('로그인을 취소했어요.'), findsOneWidget);
      delayed.complete(
        _json({'ok': true, 'phone_login': _flow(url: _authorizationUrl)}),
      );
      await tester.pumpAndSettle();
      expect(find.text('로그인을 취소했어요.'), findsOneWidget);
      expect(fake.posts.length, 3);
      await _end(tester);
    },
  );

  testWidgets(
    'dispose before the start reply still cancels only the captured target',
    (tester) async {
      final delayed = Completer<http.Response>();
      final fake = _Fake()..startReply = delayed;
      await _show(tester, fake);
      await tester.tap(find.byKey(const Key('claude-device')));
      await tester.pumpAndSettle();
      await tester.tap(find.text('미니'));
      await tester.pumpAndSettle();
      await _start(tester);
      await _end(tester);
      expect(fake.posts.length, 2);
      expect(fake.posts.first.body, fake.posts.last.body);
      expect(fake.posts.first.path, fake.posts.last.path);
      delayed.complete(
        _json({
          'ok': true,
          'phone_login': _flow(machine: 'mini', url: _authorizationUrl),
        }),
      );
      await tester.pumpAndSettle();
      expect(fake.posts.last.body, {
        'action': 'claude-phone-login-cancel',
        'id': 'flow-fixture',
      });
      expect(fake.posts.every((p) => p.path.contains('/m/~mini/')), isTrue);
      expect(tester.takeException(), isNull);
    },
  );

  testWidgets(
    'a late old start cannot cancel a later flow on the same server',
    (tester) async {
      final delayed = Completer<http.Response>();
      final fake = _Fake()..startReply = delayed;
      await _show(tester, fake);
      await _start(tester);
      await tester.pump(const Duration(seconds: 21));
      await tester.pumpAndSettle();
      fake.startReply = null;
      await tester.tap(find.byKey(const Key('claude-cancel-pending')));
      await tester.pumpAndSettle();
      await tester.tap(find.text('계정 목록으로'));
      await tester.pumpAndSettle();
      fake.state = 'awaiting_code';
      fake.responseFlow = 'new-flow';
      await _start(tester);
      final requests = fake.posts.length;
      delayed.complete(
        _json({'ok': true, 'phone_login': _flow(url: _authorizationUrl)}),
      );
      await tester.pumpAndSettle();
      expect(fake.posts.length, requests);
      expect(find.byKey(const Key('claude-code')), findsOneWidget);
      await tester.tap(find.byKey(const Key('claude-cancel')));
      await tester.pumpAndSettle();
      expect(fake.posts.last.body['id'], 'new-flow');
      await _end(tester);
    },
  );

  testWidgets(
    'server replacement cleans up the old server without cancelling its new flow',
    (tester) async {
      final delayed = Completer<http.Response>();
      final old = _Fake()..startReply = delayed;
      await _show(tester, old);
      await _start(tester);
      final next = _Fake()..responseFlow = 'new-flow';
      await _show(tester, next);
      await _start(tester);
      delayed.complete(
        _json({'ok': true, 'phone_login': _flow(url: _authorizationUrl)}),
      );
      await tester.pumpAndSettle();
      expect(old.posts.last.body, {
        'action': 'claude-phone-login-cancel',
        'id': 'flow-fixture',
      });
      expect(
        next.posts.where(
          (p) => p.body['action'] == 'claude-phone-login-cancel',
        ),
        isEmpty,
      );
      expect(find.byKey(const Key('claude-code')), findsOneWidget);
      await _end(tester);
    },
  );

  testWidgets(
    'previous connection warnings remain until both cancellations finish in either order',
    (tester) async {
      for (final reverse in [false, true]) {
        final firstCancellation = Completer<http.Response>();
        final secondCancellation = Completer<http.Response>();
        final first = _Fake()..cancelReply = firstCancellation;
        final second = _Fake()
          ..responseFlow = 'second-flow'
          ..cancelReply = secondCancellation;
        final third = _Fake();
        await _show(tester, first);
        await _start(tester);
        await _show(tester, second);
        await _start(tester);
        await _show(tester, third);
        final warning = find.textContaining('이전 연결의 로그인 취소 여부');
        expect(warning, findsOneWidget);
        expect(second.posts.last.body, {
          'action': 'claude-phone-login-cancel',
          'id': 'second-flow',
        });
        final cancellations = [
          (reply: firstCancellation, flow: 'flow-fixture'),
          (reply: secondCancellation, flow: 'second-flow'),
        ];
        final order = reverse ? cancellations.reversed.toList() : cancellations;
        order.first.reply.complete(
          _json({
            'ok': true,
            'phone_login': _flow(state: 'cancelled', flow: order.first.flow),
          }),
        );
        await tester.pumpAndSettle();
        expect(order.last.reply.isCompleted, isFalse);
        expect(warning, findsOneWidget);
        expect(third.posts, isEmpty);
        order.last.reply.complete(
          _json({
            'ok': true,
            'phone_login': _flow(state: 'cancelled', flow: order.last.flow),
          }),
        );
        await tester.pumpAndSettle();
        expect(warning, findsNothing);
        expect(third.posts, isEmpty);
        await _end(tester);
      }
    },
  );

  testWidgets(
    'an uncertain cancellation survives closing and reopening on the same server',
    (tester) async {
      final fake = _Fake()..startFailure = true;
      final server = fake.server();
      await _show(tester, fake, server: server);
      await _start(tester);
      final original = fake.posts.single;
      await _end(tester);
      await _show(tester, fake, server: server);
      expect(find.textContaining('최대 10분'), findsOneWidget);
      expect(find.byKey(const Key('claude-login-work')), findsNothing);
      fake.startFailure = false;
      await tester.tap(find.byKey(const Key('claude-cancel-pending')));
      await tester.pumpAndSettle();
      for (final request in fake.posts.where(
        (p) => p.body['action'] == 'claude-phone-login-start',
      )) {
        expect(request.body, original.body);
        expect(request.path, original.path);
      }
      expect(fake.posts.last.body, {
        'action': 'claude-phone-login-cancel',
        'id': 'flow-fixture',
      });
      expect(find.text('로그인을 취소했어요.'), findsOneWidget);
      await _end(tester);
    },
  );

  testWidgets(
    'reopening while cancellation is in flight reflects its terminal result',
    (tester) async {
      final delayed = Completer<http.Response>();
      final fake = _Fake();
      final server = fake.server();
      await _show(tester, fake, server: server);
      await _start(tester);
      fake.cancelReply = delayed;
      await _end(tester);
      await _show(tester, fake, server: server);
      expect(fake.posts.length, 2);
      delayed.complete(
        _json({'ok': true, 'phone_login': _flow(state: 'cancelled')}),
      );
      await tester.pumpAndSettle();
      expect(find.text('로그인을 취소했어요.'), findsOneWidget);
      expect(find.byKey(const Key('claude-cancel-pending')), findsNothing);
      expect(fake.posts.length, 2);
      await _end(tester);
    },
  );

  testWidgets(
    'confirmed absence after cancellation releases the pinned target without claiming cancellation',
    (tester) async {
      final fake = _Fake()..cancelError = 'phone_login_unknown';
      await _show(tester, fake);
      await _start(tester);
      await tester.tap(find.byKey(const Key('claude-cancel')));
      await tester.pumpAndSettle();
      expect(find.text('로그인을 취소했어요.'), findsNothing);
      expect(find.textContaining('요청이 없어졌어요'), findsOneWidget);
      expect(
        tester
            .widget<OutlinedButton>(find.byKey(const Key('claude-login-work')))
            .onPressed,
        isNotNull,
      );
      await _end(tester);
    },
  );

  testWidgets(
    'closing a server with an uncertain start explains unconfirmed cleanup and expiry',
    (tester) async {
      final delayed = Completer<http.Response>();
      final fake = _Fake()..startReply = delayed;
      final server = fake.server();
      await _show(tester, fake, server: server);
      await _start(tester);
      server.close();
      await tester.pumpAndSettle();
      expect(find.textContaining('취소 여부를 확인하지 못했어요'), findsOneWidget);
      expect(find.textContaining('최대 10분'), findsOneWidget);
      expect(
        tester
            .widget<TextButton>(find.byKey(const Key('claude-cancel-pending')))
            .onPressed,
        isNull,
      );
      delayed.complete(
        _json({'ok': true, 'phone_login': _flow(url: _authorizationUrl)}),
      );
      await tester.pumpAndSettle();
      expect(fake.posts.length, 1);
      expect(find.byKey(const Key('claude-code')), findsNothing);
      await _end(tester);
    },
  );

  testWidgets(
    'offline and missing stable identity never expose stale account actions',
    (tester) async {
      final fake = _Fake();
      await _show(tester, fake);
      fake.unavailable = true;
      await tester.tap(find.byTooltip('새로고침'));
      await tester.pumpAndSettle();
      expect(find.textContaining('연결하지 못했어요'), findsOneWidget);
      expect(find.byKey(const Key('claude-login-personal')), findsNothing);
      await _end(tester);
      final unknown = _Fake()
        ..missingIdentity = true
        ..remotes = false;
      await _show(tester, unknown);
      expect(find.textContaining('기기 식별자'), findsOneWidget);
      expect(find.byKey(const Key('claude-login-personal')), findsNothing);
      expect(unknown.posts, isEmpty);
      await _end(tester);
    },
  );

  testWidgets(
    'closing the connection clears code and ignores late successful replies',
    (tester) async {
      final fake = _Fake();
      final server = fake.server();
      await _show(tester, fake, server: server);
      await _start(tester);
      await tester.enterText(find.byKey(const Key('claude-code')), _code);
      fake.gate = Completer<void>();
      await tester.tap(find.byKey(const Key('claude-submit-code')));
      await tester.pump();
      expect(
        tester
            .widget<TextField>(find.byKey(const Key('claude-code')))
            .controller!
            .text,
        isEmpty,
      );
      server.close();
      fake.gate!.complete();
      fake.gate = null;
      await tester.pumpAndSettle();
      expect(find.text('이 기기에 다시 로그인했어요.'), findsNothing);
      expect(find.textContaining('연결이 바뀌었어요'), findsOneWidget);
      expect(
        tester.widget<TextField>(find.byKey(const Key('claude-code'))).enabled,
        isFalse,
      );
      await _end(tester);
    },
  );

  testWidgets('dispose ignores late replies and cancels an existing flow', (
    tester,
  ) async {
    final fake = _Fake();
    await _show(tester, fake);
    await _start(tester);
    await _end(tester);
    expect(fake.posts.last.body, {
      'action': 'claude-phone-login-cancel',
      'id': 'flow-fixture',
    });
    expect(tester.takeException(), isNull);
  });

  testWidgets(
    'saved login with pending runtime update does not claim full recovery',
    (tester) async {
      final fake = _Fake()
        ..state = 'succeeded'
        ..runtimeUpdated = false;
      await _show(tester, fake);
      await _start(tester);
      expect(find.textContaining('현재 선택된 계정과 실행 중인 칸은 그대로'), findsOneWidget);
      expect(find.text('이 기기에 다시 로그인했어요.'), findsNothing);
      await _end(tester);
    },
  );

  testWidgets(
    'unknown action, empty slots and missing machine identity explain recovery',
    (tester) async {
      for (final mode in ['old', 'empty', 'identity']) {
        final fake = _Fake();
        if (mode == 'old') fake.unknownAction = true;
        if (mode == 'empty') fake.empty = true;
        if (mode == 'identity') fake.missingIdentity = true;
        await _show(tester, fake);
        if (mode == 'old') await _start(tester);
        if (mode == 'empty') {
          expect(find.textContaining('기본 계정만'), findsOneWidget);
        } else if (mode == 'identity') {
          // Stable remote identities remain usable even if the hub is old.
          expect(find.text('미니'), findsOneWidget);
        } else {
          expect(find.textContaining('업데이트'), findsOneWidget);
        }
        expect(find.byKey(const Key('claude-code')), findsNothing);
        await _end(tester);
      }
    },
  );

  testWidgets('cancel, failure and expiry never look like a verified success', (
    tester,
  ) async {
    for (final state in ['failed', 'expired', 'cancelled']) {
      final fake = _Fake()
        ..state = state
        ..error = 'identity_mismatch';
      await _show(tester, fake);
      await _start(tester);
      expect(find.text('이 기기에 다시 로그인했어요.'), findsNothing);
      expect(find.byKey(const Key('claude-code')), findsNothing);
      expect(find.text('계정 목록으로'), findsOneWidget);
      await tester.pump(const Duration(seconds: 5));
      expect(fake.posts.length, 1);
      await _end(tester);
    }
  });

  for (final brightness in Brightness.values) {
    for (final width in [320.0, 390.0]) {
      testWidgets('$brightness ${width}px 200 percent text and keyboard fit', (
        tester,
      ) async {
        final fake = _Fake();
        await _show(
          tester,
          fake,
          width: width,
          scale: 2,
          brightness: brightness,
        );
        expect(tester.takeException(), isNull);
        final row = tester.getRect(
          find.byKey(const Key('claude-account-personal')),
        );
        expect(row.left, greaterThanOrEqualTo(0));
        expect(row.right, lessThanOrEqualTo(width));
        await tester.ensureVisible(
          find.byKey(const Key('claude-login-personal')),
        );
        await _start(tester);
        tester.view.viewInsets = const FakeViewPadding(bottom: 336);
        await tester.ensureVisible(find.byKey(const Key('claude-code')));
        await tester.pumpAndSettle();
        await tester.enterText(find.byKey(const Key('claude-code')), 'fixture');
        await tester.ensureVisible(find.byKey(const Key('claude-submit-code')));
        await tester.pumpAndSettle();
        expect(
          tester
              .widget<TextField>(find.byKey(const Key('claude-code')))
              .style!
              .fontSize,
          greaterThanOrEqualTo(16),
        );
        final submit = tester.getRect(
          find.byKey(const Key('claude-submit-code')),
        );
        expect(submit.bottom, lessThanOrEqualTo(508));
        expect(submit.height, greaterThanOrEqualTo(44));
        expect(submit.left, greaterThanOrEqualTo(0));
        expect(submit.right, lessThanOrEqualTo(width));
        expect(tester.takeException(), isNull);
        await _end(tester);
      });
    }
  }

  testWidgets(
    'runtime update failure preserves login success and explains the unchanged pane',
    (tester) async {
      final fake = _Fake()
        ..state = 'succeeded'
        ..runtimeUpdated = false
        ..runtimeErrorCode = 'runtime_update_pending';
      await _show(tester, fake);
      await _start(tester);
      expect(find.textContaining('작업대가 바뀌거나 갱신에 실패'), findsOneWidget);
      expect(find.text('이 기기에 다시 로그인했어요.'), findsNothing);
      await _end(tester);
    },
  );

  testWidgets('exports light and dark native screens with fixture accounts', (
    tester,
  ) async {
    final font = FontLoader('Pretendard')
      ..addFont(rootBundle.load('assets/fonts/Pretendard-Regular.otf'))
      ..addFont(rootBundle.load('assets/fonts/Pretendard-SemiBold.otf'));
    await font.load();
    await (FontLoader(
      'MaterialIcons',
    )..addFont(rootBundle.load('fonts/MaterialIcons-Regular.otf'))).load();
    for (final brightness in Brightness.values) {
      final fake = _Fake();
      await _show(
        tester,
        fake,
        brightness: brightness,
        every: const Duration(hours: 1),
      );
      final top = tester
          .getRect(find.byKey(const Key('claude-account-personal')))
          .top;
      expect(top, lessThan(300));
      await tester.runAsync(
        () => File('/tmp/kasa-claude-layout-${brightness.name}.json')
            .writeAsString(
              jsonEncode({
                'viewport': '390x844',
                'first_account_y': top,
                'login_button_height': tester
                    .getRect(find.byKey(const Key('claude-login-personal')))
                    .height,
                'input_font_min': 16,
                'tested_text_scale': 2,
                'tested_narrow_width': 320,
              }),
            ),
      );
      Future<void> capture(String name) async {
        final boundary = tester.renderObject<RenderRepaintBoundary>(
          find.byKey(const Key('capture')),
        );
        await tester.runAsync(() async {
          final image = await boundary.toImage();
          final data = await image.toByteData(format: ui.ImageByteFormat.png);
          await File(
            '/tmp/kasa-claude-$name-${brightness.name}.png',
          ).writeAsBytes(data!.buffer.asUint8List());
          image.dispose();
        });
      }

      await capture('accounts');
      await _start(tester);
      await capture('login');
      await _end(tester);
    }
  });
}
