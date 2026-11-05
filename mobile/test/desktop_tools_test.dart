import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/hub_model.dart';
import 'package:kasaterm_mobile/screens/desktop_settings.dart';
import 'package:kasaterm_mobile/screens/desktop_tools.dart';
import 'package:kasaterm_mobile/screens/pane_actions.dart';
import 'package:kasaterm_mobile/server.dart';

http.Response reply(String body, int status) => http.Response.bytes(
  utf8.encode(body),
  status,
  headers: {'content-type': 'application/json'},
);

Map<String, Object?> fixture() => {
  'agents': {
    'preferred_agent': 'claude',
    'permissions': {'claude': 'default', 'codex': 'workspace'},
    'statusline': {
      'customized': true,
      'model': true,
      'usage': true,
      'cwd': true,
    },
  },
  'claude': {
    'model': 'sonnet',
    'effort': 'high',
    'extra': '',
    'shim_inject': true,
    'autoswitch': false,
    'autoswitch_pct': 90,
  },
  'general': {
    'cwd_mode': 'home',
    'file_tree_default': true,
    'file_open_mode': 'builtin',
    'autosave_ms': 3000,
    'tabs_on_top': true,
    'pane_header': 'bar',
    'cursor_shape': 'block',
    'cursor_thickness': 2,
    'mouse_cursor': 'arrow',
    'wheel_gain_x100': 100,
  },
  'shell': {'shell': '/bin/zsh'},
  'appearance': {
    'themes': [
      {'key': 'system', 'label': '시스템'},
      {'key': 'light', 'label': '밝게'},
    ],
    'theme': 'system',
    'font_size': 13,
    'ui_zoom': 1,
  },
};

void main() {
  test(
    'settings actions reject HTTP 200 failures and malformed replies',
    () async {
      for (final body in ['{"ok":false,"error":"거부됐어요"}', '{}', 'not json']) {
        final server = Server(
          Uri.parse('https://fixture.test/'),
          client: MockClient((_) async => reply(body, 200)),
        );
        await expectLater(
          server.settingsAction('preferred-agent', id: 'codex'),
          throwsA(isA<ServerException>()),
        );
        server.close();
      }
    },
  );

  testWidgets(
    'settings stay bound to the selected device and show server rejection',
    (tester) async {
      final posts = <http.Request>[];
      final server = Server(
        Uri.parse('https://fixture.test/'),
        client: MockClient((r) async {
          if (r.method == 'POST') {
            posts.add(r);
            return reply('{"ok":false,"error":"기기가 설정을 거부했어요"}', 200);
          }
          return reply(jsonEncode(fixture()), 200);
        }),
      );
      addTearDown(server.close);
      await tester.pumpWidget(
        MaterialApp(
          home: DesktopSettingsScreen(
            server: server,
            title: '미니',
            machine: '~mini-id',
          ),
        ),
      );
      await tester.pumpAndSettle();
      expect(
        find.text('기본 에이전트'),
        findsOneWidget,
        reason: tester
            .widgetList<Text>(find.byType(Text))
            .map((t) => t.data)
            .join(' | '),
      );
      await tester.tap(find.text('기본 에이전트'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Codex'));
      await tester.pumpAndSettle();
      expect(posts.single.url.path, '/m/~mini-id/settings/action');
      expect(jsonDecode(posts.single.body), {
        'action': 'preferred-agent',
        'id': 'codex',
      });
      expect(find.text('기기가 설정을 거부했어요'), findsOneWidget);
      expect(find.text('Claude'), findsOneWidget);
    },
  );

  for (final width in [320.0, 390.0, 430.0]) {
    testWidgets('settings search and large text fit $width px with keyboard', (
      tester,
    ) async {
      tester.view.physicalSize = Size(width, 844);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.reset);
      final server = Server(
        Uri.parse('https://fixture.test/'),
        client: MockClient((_) async => reply(jsonEncode(fixture()), 200)),
      );
      addTearDown(server.close);
      await tester.pumpWidget(
        MaterialApp(
          builder: (_, child) => MediaQuery(
            data: MediaQueryData(
              size: Size(width, 844),
              textScaler: const TextScaler.linear(2),
              viewInsets: const EdgeInsets.only(bottom: 300),
            ),
            child: child!,
          ),
          home: DesktopSettingsScreen(
            server: server,
            title: '긴 이름의 기기',
            machine: '~mini',
          ),
        ),
      );
      await tester.pumpAndSettle();
      await tester.enterText(find.byType(TextField), '한도');
      await tester.pumpAndSettle();
      expect(find.text('한도 도달 시 계정 자동 전환'), findsOneWidget);
      expect(find.text('기본 에이전트'), findsNothing);
      expect(tester.takeException(), isNull);
    });
  }

  testWidgets('history resumes exact Codex session and path in a new room', (
    tester,
  ) async {
    final posts = <http.Request>[];
    final server = Server(
      Uri.parse('https://fixture.test/'),
      client: MockClient((r) async {
        if (r.method == 'POST') {
          posts.add(r);
          return reply('{"ok":true}', 200);
        }
        return reply(
          jsonEncode({
            'ok': true,
            'sessions': [
              {
                'id': 'exact-session',
                'label': '지난 작업',
                'harness': 'codex',
                'cwd': '/작업/공백 & 문서',
              },
            ],
          }),
          200,
        );
      }),
    );
    addTearDown(server.close);
    await tester.pumpWidget(
      MaterialApp(
        home: DesktopHistoryScreen(
          server: server,
          title: '미니',
          machine: '~mini',
        ),
      ),
    );
    await tester.pumpAndSettle();
    await tester.tap(find.text('지난 작업'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('이어하기'));
    await tester.pumpAndSettle();
    expect(posts.single.url.path, '/m/~mini/session-resume');
    expect(posts.single.url.queryParameters, {
      'id': 'exact-session',
      'harness': 'codex',
      'newroom': 'true',
      'cwd': '/작업/공백 & 문서',
    });
  });

  testWidgets(
    'moving a pane uses chosen room and direction, not desktop focus',
    (tester) async {
      final posts = <http.Request>[];
      final server = Server(
        Uri.parse('https://fixture.test/'),
        client: MockClient((r) async {
          posts.add(r);
          return reply('{"ok":true,"result":{}}', 200);
        }),
      );
      addTearDown(server.close);
      final a = Pane.fromJson({'id': '%1', 'cwd': '/first', 'window': 0});
      final b = Pane.fromJson({'id': '%2', 'cwd': '/second', 'window': 1});
      final room = HubRoom(title: '첫 방', panes: [a]);
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: Builder(
              builder: (ctx) => TextButton(
                onPressed: () => showPaneSheet(
                  ctx,
                  server: server,
                  room: room,
                  pane: a,
                  rooms: [
                    room,
                    HubRoom(title: '둘째 방', panes: [b]),
                  ],
                  machine: '~mini',
                  onOpen: (_) {},
                  onChanged: () async {},
                ),
                child: const Text('메뉴'),
              ),
            ),
          ),
        ),
      );
      await tester.tap(find.text('메뉴'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('다른 자리로 이동'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('둘째 방 · second'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('아래'));
      await tester.pumpAndSettle();
      expect(posts.single.url.path, '/m/~mini/cmd');
      expect(jsonDecode(posts.single.body), {
        'method': 'surface.move',
        'params': {'surface_id': '%1', 'target': '%2', 'direction': 'down'},
      });
    },
  );
}
