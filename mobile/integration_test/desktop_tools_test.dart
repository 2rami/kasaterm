import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:kasaterm_mobile/main.dart' show buildTheme;
import 'package:kasaterm_mobile/screens/desktop_tools.dart';
import 'package:kasaterm_mobile/server.dart';

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(() => Future<void>.delayed(const Duration(seconds: 1)));
  testWidgets('native PC settings, device routing, search and Codex history', (
    tester,
  ) async {
    final received = <(String, Map<String, dynamic>)>[];
    var provider = 'claude';
    final http = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    http.listen((request) async {
      Object body;
      if (request.method == 'POST') {
        final text = await utf8.decoder.bind(request).join();
        final args = text.isEmpty
            ? <String, dynamic>{}
            : jsonDecode(text) as Map<String, dynamic>;
        received.add((
          request.uri.path,
          {...args, ...request.uri.queryParameters},
        ));
        if (args['action'] == 'preferred-agent') {
          provider = args['id'] as String;
        }
        body = {'ok': true};
      } else if (request.uri.path.endsWith('recent-sessions')) {
        body = {
          'ok': true,
          'sessions': [
            {
              'id': 'native-codex-session',
              'harness': 'codex',
              'cwd': '/tmp/mobile-parity',
              'label': '모바일 기능 검증',
            },
          ],
        };
      } else {
        body = {
          'agents': {
            'preferred_agent': provider,
            'permissions': {'claude': 'default', 'codex': 'workspace'},
          },
          'claude': {
            'model': 'sonnet',
            'effort': 'high',
            'autoswitch': false,
            'autoswitch_pct': 90,
          },
          'general': {
            'cwd_mode': 'home',
            'autosave_ms': 3000,
            'cursor_shape': 'block',
            'tabs_on_top': true,
          },
          'appearance': {
            'theme': 'system',
            'font_size': 13,
            'ui_zoom': 1,
            'themes': [
              {'key': 'system', 'label': '시스템'},
              {'key': 'light', 'label': '밝게'},
            ],
          },
        };
      }
      request.response.headers.contentType = ContentType.json;
      request.response.write(jsonEncode(body));
      await request.response.close();
    });
    final server = Server(Uri.parse('http://127.0.0.1:${http.port}/'));
    Future<void> until(Finder finder) async {
      final until = DateTime.now().add(const Duration(seconds: 15));
      while (finder.evaluate().isEmpty) {
        if (DateTime.now().isAfter(until)) fail('native UI timeout: $finder');
        await tester.pump(const Duration(milliseconds: 100));
        await Future<void>.delayed(const Duration(milliseconds: 100));
      }
      await tester.pump(const Duration(milliseconds: 500));
      await tester.pumpAndSettle();
      await Future<void>.delayed(const Duration(milliseconds: 500));
    }

    try {
      await tester.pumpWidget(
        MaterialApp(
          debugShowCheckedModeBanner: false,
          theme: buildTheme(Brightness.light),
          home: DesktopToolsScreen(
            server: server,
            title: '검증용 미니',
            machine: '~fixture-mini',
          ),
        ),
      );
      await tester.pumpAndSettle();
      await binding.takeScreenshot('mobile-pc-tools');
      await tester.tap(find.text('PC 설정'));
      await until(find.text('기본 에이전트'));
      await binding.takeScreenshot('mobile-pc-settings');
      await tester.tap(find.text('기본 에이전트'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Codex'));
      await until(find.text('Codex'));
      await tester.enterText(find.byType(TextField), '한도');
      await tester.pumpAndSettle();
      await binding.takeScreenshot('mobile-pc-settings-keyboard');
      expect(find.text('한도 도달 시 계정 자동 전환'), findsOneWidget);
      FocusManager.instance.primaryFocus?.unfocus();
      await tester.pumpAndSettle();
      await tester.pageBack();
      await tester.pumpAndSettle();
      await tester.tap(find.text('대화 기록 이어하기'));
      await until(find.text('모바일 기능 검증'));
      await binding.takeScreenshot('mobile-pc-history');
      await tester.tap(find.text('모바일 기능 검증'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('이어하기'));
      await until(find.text('새 방에서 대화를 이어했어요. 허브에서 열 수 있어요.'));
      expect(received.map((r) => r.$1), [
        '/m/~fixture-mini/settings/action',
        '/m/~fixture-mini/session-resume',
      ]);
      expect(received.map((r) => r.$2), [
        {'action': 'preferred-agent', 'id': 'codex'},
        {
          'id': 'native-codex-session',
          'harness': 'codex',
          'newroom': 'true',
          'cwd': '/tmp/mobile-parity',
        },
      ]);
      expect(tester.takeException(), isNull);
    } finally {
      await tester.pumpWidget(const SizedBox());
      await tester.pumpAndSettle();
      server.close();
      await http.close(force: true);
    }
  });
}
