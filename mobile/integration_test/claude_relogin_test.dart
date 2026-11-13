import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:kasaterm_mobile/claude_login.dart';
import 'package:kasaterm_mobile/main.dart' show buildTheme;
import 'package:kasaterm_mobile/screens/claude_accounts.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:path_provider/path_provider.dart';
import 'package:url_launcher/url_launcher.dart';

const code = 'native-fixture-code-123456#native-fixture-state-123456';
const authorization =
    'https://claude.com/cai/oauth/authorize?state=native-fixture-state-123456&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback';

class FixtureServer extends Server {
  FixtureServer() : super(Uri.parse('http://127.0.0.1:9/'));
  bool failCode = true;
  var state = ClaudeLoginState.awaitingCode;
  @override
  Future<String?> machineId({String? machine}) async => 'fixture-book';
  @override
  Future<Me> me() async =>
      const Me(name: 'fixture', owner: true, machine: '검증 기기');
  @override
  Future<List<Machine>> machines() async => [];
  @override
  Future<ClaudeDeviceAccounts> claudeDeviceAccounts({String? machine}) async =>
      const ClaudeDeviceAccounts(
        activeId: 'personal',
        accounts: [
          ClaudeAccount(
            id: 'personal',
            label: '개인 계정',
            name: 'person@example.test',
            subtitle: '',
            usageState: 'expired',
          ),
        ],
      );
  @override
  Future<ClaudePhoneLogin> claudePhoneLogin({
    required String action,
    required String id,
    required String machineId,
    required String slotId,
    String? label,
    String? machine,
    Duration? timeout = const Duration(seconds: 20),
  }) async {
    if (action == 'code') {
      expect(label, code);
      if (failCode) {
        failCode = false;
        throw const ServerException('연결이 잠시 끊겼어요. 다시 시도해 주세요.');
      }
      state = ClaudeLoginState.succeeded;
    }
    if (action == 'cancel') state = ClaudeLoginState.cancelled;
    return ClaudePhoneLogin(
      flowId: 'fixture-flow',
      slotId: slotId,
      machineId: machineId,
      label: '개인 계정',
      email: 'person@example.test',
      org: '검증',
      state: state,
      expiresAtMs: DateTime.now()
          .add(const Duration(minutes: 10))
          .millisecondsSinceEpoch,
      authorizationUrl: state.terminal ? null : Uri.parse(authorization),
      runtimeUpdated: true,
    );
  }
}

class Lifecycle extends WidgetsBindingObserver {
  bool left = false, returned = false;
  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state != AppLifecycleState.resumed) left = true;
    if (left && state == AppLifecycleState.resumed) returned = true;
  }
}

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(() => Future<void>.delayed(const Duration(seconds: 1)));
  testWidgets(
    'Safari return, explicit native paste and retry without losing code',
    (tester) async {
      final server = FixtureServer();
      final browser = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
      browser.listen((request) async {
        request.response.headers.contentType = ContentType.html;
        request.response.write(
          '<html><meta name="viewport" content="width=device-width"><body><h1>Login fixture</h1><p>Return to KASA to continue the native test.</p></body></html>',
        );
        await request.response.close();
      });
      final support = await getApplicationSupportDirectory();
      final marker = File('${support.path}/claude-login-native.json');
      final lifecycle = Lifecycle();
      binding.addObserver(lifecycle);
      Future<void> until(bool Function() ready) async {
        final end = DateTime.now().add(const Duration(seconds: 90));
        while (!ready()) {
          if (DateTime.now().isAfter(end)) fail('native login timeout');
          await tester.pump(const Duration(milliseconds: 100));
          await Future<void>.delayed(const Duration(milliseconds: 100));
        }
        await tester.pump(const Duration(milliseconds: 500));
      }

      await tester.pumpWidget(
        MaterialApp(
          debugShowCheckedModeBanner: false,
          theme: buildTheme(Brightness.light),
          home: ClaudeAccountsScreen(
            server: server,
            open: (uri) async {
              expect(uri.toString(), authorization);
              await marker.writeAsString(
                jsonEncode({'phase': 'browser'}),
                flush: true,
              );
              return launchUrl(
                Uri.parse('http://127.0.0.1:${browser.port}/'),
                mode: LaunchMode.externalApplication,
              );
            },
          ),
        ),
      );
      await until(
        () => find
            .byKey(const Key('claude-login-personal'))
            .evaluate()
            .isNotEmpty,
      );
      // 「다시 로그인」 한 번으로 승인 페이지가 열린다 — 따로 누를 Safari 단추를 기다리지 않는다.
      await tester.tap(find.byKey(const Key('claude-login-personal')));
      await until(() => lifecycle.returned);
      await Clipboard.setData(const ClipboardData(text: code));
      await tester.ensureVisible(find.byKey(const Key('claude-paste-code')));
      await tester.tap(find.byKey(const Key('claude-paste-code')));
      await tester.pumpAndSettle();
      final field = find.byKey(const Key('claude-code'));
      expect(tester.widget<TextField>(field).controller!.text, code);
      await binding.takeScreenshot('claude-login-pasted');
      final send = find.byKey(const Key('claude-submit-code'));
      await tester.ensureVisible(send);
      await tester.tap(send);
      await tester.pumpAndSettle();
      expect(tester.widget<TextField>(field).controller!.text, code);
      expect(tester.widget<TextField>(field).enabled, isTrue);
      await binding.takeScreenshot('claude-login-retry');
      await tester.ensureVisible(send);
      await tester.tap(send);
      await until(() => find.text('이 기기에 다시 로그인했어요.').evaluate().isNotEmpty);
      await binding.takeScreenshot('claude-login-complete');
      await marker.writeAsString(
        jsonEncode({
          'phase': 'passed',
          'returned_from_safari': lifecycle.returned,
        }),
        flush: true,
      );
      binding.removeObserver(lifecycle);
      await tester.pumpWidget(const SizedBox());
      server.close();
      await browser.close(force: true);
    },
    timeout: const Timeout(Duration(minutes: 4)),
  );
}
