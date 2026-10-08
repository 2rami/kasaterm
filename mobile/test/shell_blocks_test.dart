import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/grid.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/screens/shell_blocks_view.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/shell_blocks.dart';
import 'package:kasaterm_mobile/term_session.dart';

import 'package:kasaterm_mobile/screens/controls.dart';
import 'package:kasaterm_mobile/screens/terminal.dart';

import 'mobile_terminal_qa_test.dart' show FixtureServer, FixtureSession, loadFonts;

Map<String, Object?> _answer(int since, int oldest, List<Map<String, Object?>> blocks, {int? newest}) => {
  'ok': true,
  'since': since,
  'integration': true,
  'alt': false,
  'oldest': oldest,
  'newest': newest ?? blocks.fold<int>(oldest, (m, b) => (b['id'] as int) > m ? b['id'] as int : m),
  'blocks': blocks,
};

Map<String, Object?> _block(int id, String cmd, {bool running = false, int? exit, List<Object?> lines = const []}) => {
  'id': id,
  'cmd': cmd,
  'running': running,
  'exit': ?exit,
  'start_ms': 1,
  'lines': lines,
};

void main() {
  group('ShellFeed', () {
    test('끝난 블록은 두고 도는 블록만 갈아 끼운다', () {
      final f = ShellFeed()
        ..merge(_answer(4, 1, [
          _block(1, 'ls', exit: 0, lines: [
            [
              {'t': 'docs', 'f': 4, 's': 1},
            ],
          ]),
          _block(2, 'make', running: true),
        ]));
      expect(f.have, 1);
      expect(f.running, isTrue);
      final run = f.blocks.first.lines.first.first;
      expect((run.fg as IndexColor).index, 4);
      expect(run.flags & flagBold, flagBold);
      f.merge(_answer(6, 1, [_block(2, 'make', exit: 2)]));
      expect(f.blocks.map((b) => b.exit), [0, 2]);
      expect(f.have, 2);
      expect(f.running, isFalse);
      // 바뀜 없이 기다림이 끝난 빈 답은 가진 것을 지우지 않는다.
      f.merge(_answer(6, 1, [], newest: 2));
      expect(f.blocks, hasLength(2));
    });

    test('밀려난 블록을 버리고 새 셸이면 처음부터', () {
      final f = ShellFeed()
        ..merge(_answer(9, 3, [_block(3, 'a', exit: 0), _block(4, 'b', exit: 0)]))
        ..merge(_answer(11, 4, [_block(5, 'c', exit: 0)]));
      expect(f.blocks.map((b) => b.id), [4, 5]);
      f.merge(_answer(2, 1, [_block(1, 'fresh', running: true)]));
      expect(f.blocks.map((b) => b.cmd), ['fresh']);
    });

    test('트루컬러 수를 푼다', () {
      final c = wireColor(0x1000000 | 0x0a0b0c) as RgbColor;
      expect([c.r, c.g, c.b], [10, 11, 12]);
      expect(wireColor(null), isA<DefaultColor>());
    });

    test('걸린 시간 말', () {
      expect(tookLabel(4), '');
      expect(tookLabel(11), '0.01초');
      expect(tookLabel(12000), '12초');
      expect(tookLabel(184000), '3분 4초');
    });
  });

  testWidgets('결과 속 폴더를 누르면 그 폴더로 cd 한다 — 파일은 고리가 아니다', (tester) async {
    expect(await _tapFolder(tester, running: false), [
      "cd -- '/w/docs'",
      "cd -- '/w/it'\\''s dir'",
    ]);
  });

  testWidgets('명령이 도는 동안은 폴더를 눌러도 안 보낸다', (tester) async {
    expect(await _tapFolder(tester, running: true), isEmpty);
  });

  testWidgets('추천 칩은 입력칸을 채우고, 길게 누르면 바로 친다 — 최근 명령이 먼저', (tester) async {
    final (ran, drafted) = await _pumpChips(tester, [
      _block(1, 'make test', exit: 0),
      _block(2, 'ls', exit: 0),
    ], branch: 'main');
    expect(find.text('main'), findsOneWidget, reason: '맥락 줄의 브랜치');
    final row = find.ancestor(of: find.text('git status'), matching: find.byType(ListView));
    expect(
      tester.widgetList<Text>(find.descendant(of: row, matching: find.byType(Text))).map((t) => t.data),
      ['make test', 'cd ..', 'ls', 'git status', 'git pull'],
    );
    await tester.tap(find.descendant(of: row, matching: find.text('make test')));
    await tester.longPress(find.descendant(of: row, matching: find.text('cd ..')));
    expect(drafted, ['make test']);
    expect(ran, ['cd ..']);
    await tester.tap(find.byTooltip('다시 실행').first);
    await tester.tap(find.text('make test').first);
    expect(ran.last, 'ls', reason: '맨 아래 카드가 첫 다시 실행');
    expect(drafted.last, 'make test', reason: '카드 명령 줄을 누르면 입력칸으로');
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('빈 셸은 시작 칩을 누르면 바로 친다', (tester) async {
    final (ran, _) = await _pumpChips(tester, const []);
    expect(find.text('git status'), findsNothing, reason: '저장소 밖이면 git 칩이 없다');
    await tester.tap(find.text('claude'));
    expect(ran, ['claude']);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('셸 명령 묶음 보기', (tester) async {
    await loadFonts();
    tester.view.physicalSize = const Size(390 * 3, 844 * 3);
    tester.view.devicePixelRatio = 3;
    addTearDown(tester.view.reset);
    final fixture = File('test/fixtures/shell_blocks.json').readAsStringSync();
    final asked = <Uri>[];
    final server = Server(
      Uri.parse('http://127.0.0.1:1/'),
      client: MockClient((req) async {
        if (!req.url.path.endsWith('term/blocks')) return http.Response('', 404);
        asked.add(req.url);
        // 첫 답 뒤의 기다림은 바뀐 것이 없다 — 같은 도장을 돌려준다.
        return http.Response(
          fixture,
          200,
          headers: {'content-type': 'application/json; charset=utf-8'},
        );
      }),
    );
    const pane = Pane(id: '%0', name: '', title: '', status: 'idle', window: 0, cwd: '/m');
    final session = TermSession(server, pane)..state = TermState.connected;
    await tester.pumpWidget(
      MaterialApp(
        theme: buildTheme(Brightness.dark),
        home: Scaffold(
          appBar: AppBar(
            title: const Text('~/Desktop'),
          ),
          body: ShellBlocksView(
            server: server,
            pane: pane,
            session: session,
            onTerminal: () {},
          ),
        ),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));
    expect(asked.first.queryParameters['pane'], '%0');
    expect(find.text('cargo build --release'), findsOneWidget);
    expect(find.text('도는 중'), findsOneWidget);
    expect(find.textContaining('전체 화면 프로그램이었어요'), findsOneWidget);
    expect(find.textContaining('from-mirror 거울에서', findRichText: true), findsWidgets);
    // 위로 올리면 실패한 명령이 종료 코드와 함께 있다(목록은 아래부터 쌓인다).
    await tester.drag(find.byType(ListView), const Offset(0, 600));
    await tester.pump();
    expect(find.text('ls /nope-dir'), findsOneWidget);
    expect(find.text('종료 1'), findsOneWidget);
    await tester.drag(find.byType(ListView), const Offset(0, -2000));
    await tester.pump();
    await expectLater(
      find.byType(Scaffold),
      matchesGoldenFile('goldens/shell_blocks_390.png'),
    );
    await tester.pumpWidget(const SizedBox());
    session.dispose();
  });

  testWidgets('셸 칸은 원본 크기를 쥐지 않고 명령 쪽에서 보낸다', (tester) async {
    tester.view.physicalSize = const Size(390 * 3, 844 * 3);
    tester.view.devicePixelRatio = 3;
    addTearDown(tester.view.reset);
    final server = _BlocksServer();
    const pane = Pane(id: '%3', name: '', title: '', status: 'idle', window: 0, cwd: '/m');
    final session = _HoldSession(server, pane);
    await tester.pumpWidget(
      MaterialApp(
        theme: buildTheme(Brightness.light),
        home: TerminalScreen(server: server, pane: pane, session: session),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    expect(session.held, isFalse, reason: '셸 칸은 명령 묶음·줄여 보기로 원본 크기 없이 그린다');
    expect(find.byType(ShellBlocksView), findsOneWidget);
    expect(find.byTooltip('터미널로 보기'), findsOneWidget);
    expect(find.text('cargo build --release'), findsWidgets, reason: '카드와 추천 칩');
    await tester.enterText(find.widgetWithText(TextField, '명령 보내기'), 'git status');
    await tester.tap(find.byType(SendButton).last);
    await tester.pump(const Duration(milliseconds: 50));
    expect(server.replies, ['git status']);
    // 세션은 화면이 닫으며 놓는다.
    await tester.pumpWidget(const SizedBox());
  });
}

class _HoldSession extends FixtureSession {
  _HoldSession(super.server, super.pane);

  bool? held;

  @override
  set holdViewport(bool value) {
    held = value;
    super.holdViewport = value;
  }
}

Future<List<String>> _tapFolder(WidgetTester tester, {required bool running}) async {
  final answer = _answer(3, 1, [
    {
      ..._block(2, 'ls', running: running, exit: running ? null : 0, lines: [
        [
          {'t': 'docs  '},
          {'t': 'a.txt  ', 'f': 2},
          {'t': "it's dir"},
        ],
      ]),
      'cwd': '/w',
      'links': [
        [0, 0, 4, '/w/docs'],
        [0, 13, 8, "/w/it's dir"],
      ],
    },
  ]);
  final server = Server(
    Uri.parse('http://127.0.0.1:1/'),
    client: MockClient((req) async => http.Response(
      jsonEncode(answer),
      200,
      headers: {'content-type': 'application/json; charset=utf-8'},
    )),
  );
  const pane = Pane(id: '%0', name: '', title: '', status: 'idle', window: 0, cwd: '/w');
  final session = TermSession(server, pane)..state = TermState.connected;
  final sent = <String>[];
  await tester.pumpWidget(
    MaterialApp(
      theme: buildTheme(Brightness.dark),
      home: Scaffold(
        body: ShellBlocksView(
          server: server,
          pane: pane,
          session: session,
          onTerminal: () {},
          onCommand: (c) async => sent.add(c),
        ),
      ),
    ),
  );
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 50));
  // SelectableText 는 RichText 가 아니라 글자 범위 찾기가 안 닿는다 — 글자 자리를 재서 진짜로 누른다.
  final RenderEditable edit = tester
      .state<EditableTextState>(
        find.descendant(of: find.byType(SelectableText), matching: find.byType(EditableText)),
      )
      .renderEditable;
  Future<void> tapWord(String word) async {
    final at = edit.text!.toPlainText().indexOf(word);
    final caret = edit.getLocalRectForCaret(TextPosition(offset: at + 1));
    await tester.tapAt(edit.localToGlobal(caret.center));
    await tester.pump(const Duration(milliseconds: 400));
  }

  await tapWord('docs');
  await tapWord("it's dir");
  await tapWord('a.txt');
  await tester.pumpWidget(const SizedBox());
  session.dispose();
  return sent;
}

Future<(List<String>, List<String>)> _pumpChips(
  WidgetTester tester,
  List<Map<String, Object?>> blocks, {
  String? branch,
}) async {
  final answer = {..._answer(3, 1, blocks), 'cwd': '/w', 'branch': ?branch};
  final server = Server(
    Uri.parse('http://127.0.0.1:1/'),
    client: MockClient((req) async => http.Response(
      jsonEncode(answer),
      200,
      headers: {'content-type': 'application/json; charset=utf-8'},
    )),
  );
  const pane = Pane(id: '%0', name: '', title: '', status: 'idle', window: 0, cwd: '/w');
  final session = TermSession(server, pane)..state = TermState.connected;
  final ran = <String>[];
  final drafted = <String>[];
  addTearDown(session.dispose);
  await tester.pumpWidget(
    MaterialApp(
      theme: buildTheme(Brightness.dark),
      home: Scaffold(
        body: ShellBlocksView(
          server: server,
          pane: pane,
          session: session,
          onTerminal: () {},
          onCommand: (c) async => ran.add(c),
          onDraft: drafted.add,
        ),
      ),
    ),
  );
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 50));
  return (ran, drafted);
}

class _BlocksServer extends FixtureServer {
  final replies = <String>[];

  @override
  Future<Map<String, Object?>?> shellBlocks(
    String pane, {
    String? machine,
    int? since,
    int have = 0,
    int? block,
    int? waitMs,
  }) async =>
      (jsonDecode(File('test/fixtures/shell_blocks.json').readAsStringSync()) as Map).cast<String, Object?>();

  @override
  Future<void> send(String pane, String text, {String? machine}) async {
    replies.add(text);
  }

  /// 명령 쪽 입력줄은 대화 보기의 것이라 원본이 넣는 창구로 간다.
  @override
  Future<void> chatSend(String pane, String text, {String? machine}) async {
    replies.add(text);
  }
}
