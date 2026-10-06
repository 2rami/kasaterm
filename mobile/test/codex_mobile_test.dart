import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/claude_style.dart';
import 'package:kasaterm_mobile/conversation.dart';
import 'package:kasaterm_mobile/grid.dart';
import 'package:kasaterm_mobile/grid_canvas.dart';
import 'package:kasaterm_mobile/look.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/screens/conversation_view.dart';
import 'package:kasaterm_mobile/screens/terminal.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/term_session.dart';
import 'package:kasaterm_mobile/theme_prefs.dart';

import 'fixtures/codex_mobile.dart';
import 'mobile_terminal_qa_test.dart' show loadFonts;

Server fixtureServer(CodexScene scene) => Server(
  Uri.parse('https://codex-fixture.invalid/'),
  client: MockClient((request) async {
    final Object body;
    if (request.url.path.endsWith('/transcript-raw')) {
      body = {
        'ok': true,
        'raw': codexRollout(scene),
        'offset': 100,
        'reset': true,
      };
    } else if (request.url.path.endsWith('/term/panes')) {
      body = [codexPaneJson(scene)];
    } else {
      return http.Response('', 404);
    }
    return http.Response(
      jsonEncode(body),
      200,
      headers: {'content-type': 'application/json; charset=utf-8'},
    );
  }),
);

class CodexFixtureSession extends TermSession {
  CodexFixtureSession(super.server, super.pane, CodexScene scene) {
    state = TermState.connected;
    setRows(codexTerminalRows(scene));
  }
  final sent = <String>[];
  @override
  void connect() {}
  @override
  bool get canSend => state == TermState.connected && !server.isClosed;
  @override
  void sendText(String text) {
    if (canSend) sent.add(text);
  }

  void setRows(List<String> rows) {
    final wrapped = <String>[];
    for (final row in rows) {
      var width = 0;
      var line = StringBuffer();
      for (final rune in row.runes) {
        final cells = cellWidth(rune);
        if (width + cells > 96) {
          wrapped.add(line.toString());
          line = StringBuffer();
          width = 0;
        }
        line.writeCharCode(rune);
        width += cells;
      }
      wrapped.add(line.toString());
    }
    grid.apply({
      'cols': 96,
      'rows': wrapped.length,
      'dirty': [
        for (final (i, row) in wrapped.indexed)
          [
            i,
            [
              [row, null, null, 0],
            ],
          ],
      ],
      'cursor': [wrapped.length - 1, 0],
      'cursorVisible': false,
    });
  }
}

void viewport(WidgetTester tester, double width) {
  tester.view.physicalSize = Size(width, 844);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
}

Widget host(
  Server server,
  CodexFixtureSession session,
  Brightness brightness,
) => MaterialApp(
  debugShowCheckedModeBanner: false,
  theme: buildTheme(brightness),
  home: TickerMode(
    enabled: false,
    child: TerminalScreen(server: server, pane: session.pane, session: session),
  ),
);

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(loadFonts);

  test(
    'harness identity keeps unassigned Codex and Claude out of shell classification',
    () {
      for (final harness in ['codex', 'claude']) {
        final pane = Pane.fromJson({
          ...codexPaneJson(CodexScene.complete),
          'harness': harness,
        });
        expect(pane.isShell, isFalse);
        expect(pane.displayName, harness == 'codex' ? 'Codex' : 'Claude');
      }
      expect(
        Pane.fromJson({'id': '%0', 'name': null, 'harness': null}).isShell,
        isTrue,
      );
    },
  );

  test(
    'Codex rollout preserves progress, tool arguments/output and long Korean completion',
    () {
      final conversation = Conversation()
        ..apply(codexRollout(CodexScene.complete), reset: true, next: 100);
      final tool = conversation.items.whereType<ChatTool>().single;
      expect(tool.name, 'exec_command');
      expect(tool.summary, codexToolCommand);
      expect(tool.result, contains('12 tests passed'));
      expect(tool.error, isFalse);
      expect(
        conversation.items.whereType<ChatBubble>().last.text,
        contains(codexLongReply),
      );
    },
  );

  test(
    'Codex live approval marker is recognized but history below a composer is not',
    () {
      final menu = parsePromptMenu(codexApprovalRows)!;
      expect(menu.cursor, 0);
      expect(menu.options.map((o) => o.label), [
        'Yes, proceed (y)',
        'No, and tell Codex what to do differently (esc)',
      ]);
      expect(menu.title, contains(codexToolCommand));
      expect(parsePromptMenu([...codexApprovalRows, ...List.filled(30, '')]), isNotNull);
      for (final composer in [
        '› ',
        '› Explain this codebase',
        '› 계속 진행해줘',
        '❯ 다음 작업',
      ]) {
        expect(parsePromptMenu([...codexApprovalRows, composer]), isNull);
      }
      expect(
        parsePromptMenu(['The old choice was › 1. Yes, proceed', '2. No']),
        isNull,
      );
      expect(parsePromptMenu(['1. 구현', '2. 검사']), isNull);
    },
  );

  test(
    'wide Korean cells and repository Codex footer format retain the provider icon',
    () {
      expect(cellWidth('한'.runes.single), 2);
      final server = fixtureServer(CodexScene.complete);
      final session = CodexFixtureSession(
        server,
        codexPane(CodexScene.complete),
        CodexScene.complete,
      );
      final styled = restyleClaude(
        session.grid,
        const StudentStyle(
          slug: null,
          accent: Color(0xff326fb8),
          bg: Colors.white,
          codex: true,
        ),
        0,
        wrapCols: 42,
      );
      for (final row in session.grid.lines) {
        final columns = row.expand((run) => run.text.runes).fold<int>(0, (sum, rune) => sum + cellWidth(rune));
        expect(columns, lessThanOrEqualTo(session.grid.cols));
      }
      expect(styled.slots.map((s) => s.motion), contains('icon:codex'));
      expect(
        styled.lines.expand((r) => r).map((r) => r.text).join(),
        contains('GPT-5.6 Sol'),
      );
      session.dispose();
      server.close();
    },
  );

  for (final width in [320.0, 390.0, 430.0]) {
    for (final scene in CodexScene.values) {
      for (final brightness in Brightness.values) {
        testWidgets('Codex ${scene.name} ${brightness.name} $width terminal', (
          tester,
        ) async {
          viewport(tester, width);
          paneView.value = PaneView.terminal;
          phoneThemeMode.value = brightness == Brightness.dark
              ? ThemeMode.dark
              : ThemeMode.light;
          final server = fixtureServer(scene);
          final session = CodexFixtureSession(server, codexPane(scene), scene);
          await tester.pumpWidget(host(server, session, brightness));
          await tester.pump(const Duration(milliseconds: 200));
          expect(tester.takeException(), isNull);
          expect(find.text('Codex'), findsOneWidget);
          expect(find.byType(PaneViewSwitch), findsOneWidget);
          expect(
            find.text(switch (scene) {
              CodexScene.progress => '하는 중 · exec_command flutter test',
              CodexScene.complete => '쉬는 중',
              CodexScene.approval => '승인',
            }),
            findsOneWidget,
          );
          final rect = tester.getRect(find.byType(WrappedCanvas));
          expect(rect.left, greaterThanOrEqualTo(0));
          expect(rect.right, lessThanOrEqualTo(width));
          if (width == 390) {
            await expectLater(
              find.byType(MaterialApp),
              matchesGoldenFile(
                'goldens/codex_${scene.name}_${brightness.name}_390.png',
              ),
            );
          }
          expect(session.sent, isEmpty);
          await tester.pumpWidget(const SizedBox());
          server.close();
        });
      }
    }

    testWidgets(
      'Codex approval $width: native choices, 44px targets and stale-input rejection',
      (tester) async {
        viewport(tester, width);
        paneView.value = PaneView.chat;
        final server = fixtureServer(CodexScene.approval);
        final session = CodexFixtureSession(
          server,
          codexPane(CodexScene.approval),
          CodexScene.approval,
        );
        await tester.pumpWidget(host(server, session, Brightness.light));
        await tester.pump(const Duration(milliseconds: 200));
        expect(tester.takeException(), isNull);
        final decline = find.widgetWithText(
          OutlinedButton,
          '2. No, and tell Codex what to do differently (esc)',
        );
        final rect = tester.getRect(decline);
        expect(rect.height, greaterThanOrEqualTo(44));
        expect(rect.left, greaterThanOrEqualTo(0));
        expect(rect.right, lessThanOrEqualTo(width));
        if (width == 390) {
          await expectLater(
            find.byType(MaterialApp),
            matchesGoldenFile('goldens/codex_approval_chat_390.png'),
          );
        }
        session.setRows([...codexApprovalRows, '› 계속 진행해줘']);
        await tester.tap(decline);
        expect(
          session.sent,
          isEmpty,
          reason:
              'a choice that disappeared before the next paint cannot submit the new composer',
        );
        session.setRows(codexApprovalRows);
        await tester.tap(decline);
        expect(session.sent, ['\x1b[B'], reason: 'Enter waits until the cursor has moved');
        await tester.pump(TermSession.enterGap);
        expect(session.sent, ['\x1b[B', '\r']);
        await tester.pumpWidget(const SizedBox());
        server.close();
      },
    );

    testWidgets(
      'Codex complete $width chat shows rollout-shaped tool output and Korean response',
      (tester) async {
        viewport(tester, width);
        paneView.value = PaneView.chat;
        final server = fixtureServer(CodexScene.complete);
        final session = CodexFixtureSession(
          server,
          codexPane(CodexScene.complete),
          CodexScene.complete,
        );
        await tester.pumpWidget(host(server, session, Brightness.dark));
        await tester.pump(const Duration(milliseconds: 200));
        expect(tester.takeException(), isNull);
        expect(
          find.textContaining('로그인 상태와 데스크톱', findRichText: true),
          findsOneWidget,
        );
        final tool = find.text('exec_command');
        await tester.ensureVisible(tool);
        await tester.tap(tool);
        await tester.pump();
          expect(find.textContaining('12 tests passed'), findsOneWidget);
          // 흐르는 글의 한글은 고딕으로 — TermHangul 은 한글 진행 폭이 반 칸이라 포개진다.
          expect(tester.widget<Text>(find.textContaining('12 tests passed')).style?.fontFamilyFallback,
            Look.flowMonoFallback);
        expect(tester.takeException(), isNull);
        if (width == 390) {
          await expectLater(
            find.byType(MaterialApp),
            matchesGoldenFile('goldens/codex_complete_chat_390.png'),
          );
        }
        expect(session.sent, isEmpty);
        await tester.pumpWidget(const SizedBox());
        server.close();
      },
    );
  }
}
