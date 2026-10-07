import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/screens/conversation_view.dart';
import 'package:kasaterm_mobile/screens/shell_blocks_view.dart';
import 'package:kasaterm_mobile/screens/terminal.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/term_session.dart';

import 'mobile_terminal_qa_test.dart' show loadFonts;

const target = Pane(
  id: '%7',
  name: '세이아',
  title: '',
  status: 'idle',
  window: 0,
  cwd: '/',
  harness: 'claude',
);

/// 가로로 넘치는 코드 칸 하나가 든 대화.
final _transcript =
    '${jsonEncode({
      'type': 'assistant',
      'sessionId': 's1',
      'timestamp': '2026-10-02T01:00:05Z',
      'message': {
        'role': 'assistant',
        'content': [
          {'type': 'text', 'text': '이렇게 돌려요\n\n```\n${'cargo build --release --features kasanet ' * 4}\n```'},
        ],
      },
    })}\n';

class SwipeServer extends Server {
  SwipeServer({this.transcript}) : super(Uri.parse('https://example.com/'));
  final String? transcript;

  @override
  Future<({String raw, int offset, bool reset})?> transcriptRaw(
    String pane,
    int offset, {
    String? machine,
    int? waitMs,
  }) async {
    final t = transcript;
    if (t == null) return null;
    final end = utf8.encode(t).length;
    return offset == 0
        ? (raw: t, offset: end, reset: true)
        : (raw: '', offset: end, reset: false);
  }

  @override
  Future<List<Pane>> panes({String? machine}) async => const [target];

  @override
  Future<Map<String, Object?>?> shellBlocks(
    String pane, {
    String? machine,
    int? since,
    int have = 0,
    int? block,
    int? waitMs,
  }) async => null;
}

class SwipeSession extends TermSession {
  SwipeSession(super.server, super.pane) {
    state = TermState.connected;
    // 격자 그대로 보기의 채움 배율이 서려면 데스크톱 pane 만 한 격자가 있어야 한다.
    grid.apply({
      'cols': 96,
      'rows': 30,
      'dirty': [
        for (var i = 0; i < 30; i++)
          [
            i,
            [
              ['줄 $i ${'─' * 40}', null, null, 0],
            ],
          ],
      ],
      'cursor': [29, 0],
      'cursorVisible': false,
    });
  }
  @override
  void connect() {}
  @override
  void sendText(String text) {}
  @override
  void sendBytes(List<int> bytes) {}
}

double _page(WidgetTester tester) =>
    tester.widget<PageView>(find.byType(PageView)).controller!.page!;

/// 앱바 오른쪽 단추들의 툴팁 — 왼쪽부터(뒤로 가기는 뺀다).
List<String?> _actions(WidgetTester tester) => [
  for (final b in tester.widgetList<IconButton>(
    find.descendant(of: find.byType(AppBar), matching: find.byType(IconButton)),
  ))
    if (b.tooltip != 'Back') b.tooltip,
];

/// [view] 가 없으면 앱처럼 열린다(첫 쪽은 화면이 고른다).
Future<void> _open(
  WidgetTester tester, {
  PaneView? view = PaneView.terminal,
  Pane pane = target,
  String? transcript,
  bool still = false,
}) async {
  tester.view.physicalSize = const Size(390, 844);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  final server = SwipeServer(transcript: transcript);
  await tester.pumpWidget(
    MaterialApp(
      theme: ThemeData(platform: TargetPlatform.iOS),
      builder: (context, child) => MediaQuery(
        data: MediaQuery.of(context).copyWith(disableAnimations: still),
        child: child!,
      ),
      home: Builder(
        builder: (context) => Scaffold(
          body: Center(
            child: TextButton(
              onPressed: () => Navigator.of(context).push(
                MaterialPageRoute<void>(
                  builder: (_) => TerminalScreen(
                    server: server,
                    pane: pane,
                    session: SwipeSession(server, pane),
                    initialView: view,
                  ),
                ),
              ),
              child: const Text('열기'),
            ),
          ),
        ),
      ),
    ),
  );
  await tester.tap(find.text('열기'));
  await tester.pumpAndSettle();
}

Future<void> _close(WidgetTester tester) async {
  await tester.pumpWidget(const SizedBox());
  await tester.pump(const Duration(seconds: 3));
}

/// 손가락 하나로 [from] 에서 [by] 만큼 — 16ms 마다 나눠 움직여 판정 문턱과 튕김 속도를 실제처럼 낸다.
Future<void> _swipe(
  WidgetTester tester,
  Offset from,
  Offset by, {
  int steps = 10,
  bool settle = true,
}) async {
  var t = Duration.zero;
  const frame = Duration(milliseconds: 16);
  final g = await tester.createGesture();
  await g.down(from, timeStamp: t);
  for (var i = 1; i <= steps; i++) {
    t += frame;
    await g.moveBy(by / steps.toDouble(), timeStamp: t);
    await tester.pump(frame);
  }
  await g.up(timeStamp: t);
  if (settle) await tester.pumpAndSettle();
}

void main() {
  setUpAll(loadFonts);

  testWidgets('가로로 밀면 터미널 ↔ 대화, 전환 단추와 입력줄이 따라온다', (tester) async {
    await _open(tester);
    expect(_page(tester), 0);
    expect(find.byType(ChatComposer), findsNothing);

    await _swipe(tester, const Offset(300, 400), const Offset(-220, 0));
    expect(_page(tester), 1);
    expect(find.byType(ChatComposer), findsOneWidget);
    expect(_actions(tester), ['터미널로 보기', 'pane 닫기']);

    await _swipe(tester, const Offset(100, 400), const Offset(220, 0));
    expect(_page(tester), 0);
    expect(find.byType(ChatComposer), findsNothing);
    expect(_actions(tester), ['대화로 보기', 'pane 닫기']);

    await tester.tap(find.byTooltip('대화로 보기'));
    await tester.pumpAndSettle();
    expect(_page(tester), 1);
    await _close(tester);
  });

  testWidgets('34° 보다 비스듬한 밀기는 쪽을 넘기지 않는다', (tester) async {
    // 빈 대화 쪽 — 세로 스크롤이 없어 비스듬한 밀기를 가져갈 손이 없다. 그래도 안 넘긴다.
    await _open(tester, view: PaneView.chat);
    // 40° — 가로 18 을 먼저 지나지만 세로의 1.5배가 안 된다.
    await _swipe(tester, const Offset(100, 300), const Offset(200, 168));
    expect(_page(tester), 1);
    // 25° 는 넘긴다.
    await _swipe(tester, const Offset(100, 300), const Offset(220, 103));
    expect(_page(tester), 0);
    // 터미널 쪽 — 세로 스크롤이 있는 곳에서 40° 는 읽기 스크롤로 간다.
    await _swipe(tester, const Offset(300, 300), const Offset(-200, 168));
    expect(_page(tester), 0);
    await _close(tester);
  });

  testWidgets('왼쪽 가장자리에서 시작한 밀기는 뒤로 가기다', (tester) async {
    await _open(tester, view: PaneView.chat);
    await _swipe(tester, const Offset(6, 400), const Offset(300, 0));
    expect(find.byType(TerminalScreen), findsNothing);
    expect(find.text('열기'), findsOneWidget);
    await _close(tester);
  });

  testWidgets('코드 칸 안의 가로 밀기는 코드 칸이 가져간다', (tester) async {
    await _open(tester, view: PaneView.chat, transcript: _transcript);
    await tester.pump(const Duration(milliseconds: 100));
    final code = find.textContaining('cargo build', findRichText: true);
    expect(code, findsOneWidget);
    final scroll = find.ancestor(
      of: code,
      matching: find.byWidgetPredicate(
        (w) =>
            w is SingleChildScrollView && w.scrollDirection == Axis.horizontal,
      ),
    );
    expect(scroll, findsOneWidget);
    // 코드 글 전체는 화면보다 넓다 — 보이는 코드 칸의 가운데를 민다.
    final box = tester.getRect(scroll);
    final pos = Scrollable.of(tester.element(code)).position;
    expect(pos.maxScrollExtent, greaterThan(150));
    // 맨 왼쪽인 코드 칸을 오른쪽으로 — 더 갈 데가 없어도 코드 칸이 가져가 터미널로 안 넘어간다.
    await _swipe(tester, box.center, const Offset(150, 0));
    expect(_page(tester), 1, reason: '코드 칸을 밀었는데 쪽이 넘어갔다');
    await _swipe(tester, box.center, const Offset(-150, 0));
    expect(_page(tester), 1);
    expect(pos.axis, Axis.horizontal);
    expect(pos.pixels, greaterThan(0));
    await _close(tester);
  });

  testWidgets('쓰던 글과 붙인 사진은 쪽을 바꿔도 남고, 초점은 새 쪽 입력칸으로', (tester) async {
    await _open(tester, view: PaneView.chat);
    final chatField = find.descendant(
      of: find.byType(ChatComposer),
      matching: find.byType(TextField),
    );
    await tester.enterText(chatField, '아직 안 보낸 글');
    await tester.pump();
    expect(tester.widget<TextField>(chatField).focusNode!.hasFocus, isTrue);

    await _swipe(tester, const Offset(100, 400), const Offset(220, 0));
    expect(_page(tester), 0);
    final termField = find.byType(TextField);
    expect(termField, findsOneWidget);
    expect(tester.widget<TextField>(termField).focusNode!.hasFocus, isTrue);
    expect(
      tester.widget<TextField>(termField).controller!.text,
      isEmpty,
      reason: '대화 쪽 글이 바로 치기 칸으로 넘어가면 화면 입력상자에 들어간다',
    );

    await _swipe(tester, const Offset(300, 400), const Offset(-220, 0));
    expect(tester.widget<TextField>(chatField).controller!.text, '아직 안 보낸 글');
    expect(tester.widget<TextField>(chatField).focusNode!.hasFocus, isTrue);
    await _close(tester);
  });

  testWidgets('자판이 떠도 닫기 옆 보기 단추가 그 자리에서 바꾼다', (tester) async {
    await _open(tester);
    tester.view.viewInsets = const FakeViewPadding(bottom: 300);
    await tester.pumpAndSettle();
    expect(_actions(tester).last, 'pane 닫기');
    await tester.tap(find.byTooltip('대화로 보기'));
    await tester.pumpAndSettle();
    expect(_page(tester), 1);
    expect(find.byTooltip('터미널로 보기'), findsOneWidget);
    await _close(tester);
  });

  testWidgets('학생 칸은 열면 대화가 먼저, 닫기 옆 터미널 단추로 오간다', (tester) async {
    await _open(tester, view: null);
    expect(_page(tester), 1);
    expect(find.byType(ChatComposer), findsOneWidget);
    expect(_actions(tester), ['터미널로 보기', 'pane 닫기']);
    final toggle = tester.widget<IconButton>(
      find.widgetWithIcon(IconButton, Icons.terminal),
    );
    expect(toggle.tooltip, '터미널로 보기');
    final r = tester.getRect(find.widgetWithIcon(IconButton, Icons.terminal));
    expect(r.width, greaterThanOrEqualTo(44));
    expect(r.height, greaterThanOrEqualTo(44));
    // 닫기 바로 왼쪽.
    final close = tester.getRect(find.widgetWithIcon(IconButton, Icons.close));
    expect(close.left, closeTo(r.right, 1));

    await tester.tap(find.byTooltip('터미널로 보기'));
    await tester.pumpAndSettle();
    expect(_page(tester), 0);
    expect(find.byType(ChatComposer), findsNothing);
    expect(_actions(tester), ['대화로 보기', 'pane 닫기']);
    expect(find.widgetWithIcon(IconButton, Icons.forum_outlined), findsOneWidget);

    await tester.tap(find.byTooltip('대화로 보기'));
    await tester.pumpAndSettle();
    expect(_page(tester), 1);
    expect(find.byType(ChatComposer), findsOneWidget);
    await _close(tester);
  });

  testWidgets('고른 쪽은 다음 칸으로 넘어가지 않는다 — 다시 열면 대화부터', (tester) async {
    await _open(tester, view: null);
    await tester.tap(find.byTooltip('터미널로 보기'));
    await tester.pumpAndSettle();
    expect(_page(tester), 0);
    await tester.pageBack();
    await tester.pumpAndSettle();
    await tester.tap(find.text('열기'));
    await tester.pumpAndSettle();
    expect(_page(tester), 1);
    expect(find.byTooltip('터미널로 보기'), findsOneWidget);
    await _close(tester);
  });

  testWidgets('셸 칸은 명령 쪽이 먼저, 단추는 터미널 ↔ 명령', (tester) async {
    const shell = Pane(
      id: '%3',
      name: '',
      title: '',
      status: 'idle',
      window: 0,
      cwd: '/m',
    );
    await _open(tester, view: null, pane: shell);
    expect(_page(tester), 1);
    expect(find.byType(ShellBlocksView), findsOneWidget);
    expect(_actions(tester), ['터미널로 보기', 'pane 닫기']);
    await tester.tap(find.byTooltip('터미널로 보기'));
    await tester.pumpAndSettle();
    expect(_page(tester), 0);
    expect(find.widgetWithIcon(IconButton, Icons.view_agenda_outlined), findsOneWidget);
    await tester.tap(find.byTooltip('명령으로 보기'));
    await tester.pumpAndSettle();
    expect(_page(tester), 1);
    await _close(tester);
  });

  testWidgets('폰이 연 웹 셸은 터미널뿐, 보기 단추가 없다', (tester) async {
    const web = Pane(
      id: 'web-1',
      name: '',
      title: '',
      status: 'idle',
      window: 0,
      cwd: '/m',
    );
    await _open(tester, view: null, pane: web);
    expect(_page(tester), 0);
    expect(find.byTooltip('터미널로 보기'), findsNothing);
    expect(find.byTooltip('대화로 보기'), findsNothing);
    expect(_actions(tester).last, 'pane 닫기');
    await _close(tester);
  });

  testWidgets('동작 줄이기면 단추·밀기 모두 미끄러지지 않고 바로 바뀐다', (tester) async {
    await _open(tester, still: true);
    await tester.tap(find.byTooltip('대화로 보기'));
    await tester.pump();
    expect(_page(tester), 1);

    await _swipe(
      tester,
      const Offset(100, 400),
      const Offset(144, 0),
      steps: 8,
      settle: false,
    );
    await tester.pump();
    expect(_page(tester), 0, reason: '놓은 그 프레임에 이미 터미널이어야 한다');
    await _close(tester);
  });
}
