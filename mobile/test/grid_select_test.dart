import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/grid.dart';
import 'package:kasaterm_mobile/grid_select.dart';
import 'package:kasaterm_mobile/reflow.dart';
import 'package:kasaterm_mobile/screens/conversation_view.dart';
import 'package:kasaterm_mobile/screens/terminal.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/term_session.dart';

import 'mobile_terminal_qa_test.dart' show loadFonts, FixtureServer;

List<Run> _row(String t) => [Run(t, const DefaultColor(), const DefaultColor(), 0)];

/// [s] 안 [sub] 의 시작 칸 — 한글은 두 칸.
int _colOf(String s, String sub) {
  var col = 0;
  for (final r in s.substring(0, s.indexOf(sub)).runes) {
    col += cellWidth(r);
  }
  return col;
}

String _all(GridText t) =>
    t.text(const GridSpot(0, 0), GridSpot(t.rows - 1, t.width(t.rows - 1)));

GridLines _grid(List<String> rows, int cols) {
  final g = Grid();
  g.apply({
    'cols': cols,
    'rows': rows.length,
    'dirty': [
      for (final (i, row) in rows.indexed)
        [
          i,
          [
            [row, null, null, 0],
          ],
        ],
    ],
    'cursor': [rows.length - 1, 0],
    'cursorVisible': false,
  });
  return g;
}

const _pane = Pane(
  id: '%7',
  name: '세이아',
  title: '',
  status: 'idle',
  window: 0,
  cwd: '/',
  harness: 'claude',
);

/// 화면보다 긴 터미널 — 가장자리로 끌면 굴러야 한다.
class _LongSession extends TermSession {
  _LongSession(super.server, super.pane) {
    state = TermState.connected;
    final rows = [
      for (var i = 1; i <= 80; i++) '검증 $i 한글과 English 줄',
      '❯ ',
    ];
    grid.apply({
      'cols': 40,
      'rows': rows.length,
      'dirty': [
        for (final (i, row) in rows.indexed)
          [
            i,
            [
              [row, null, null, 0],
            ],
          ],
      ],
      'cursor': [rows.length - 1, 2],
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

Future<void> _open(WidgetTester tester) async {
  tester.view.physicalSize = const Size(390, 844);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  final server = FixtureServer();
  await tester.pumpWidget(
    MaterialApp(
      theme: ThemeData(platform: TargetPlatform.iOS),
      home: TerminalScreen(
        server: server,
        pane: _pane,
        session: _LongSession(server, _pane),
        initialView: PaneView.terminal,
      ),
    ),
  );
  await tester.pumpAndSettle();
}

Future<void> _close(WidgetTester tester) async {
  await tester.pumpWidget(const SizedBox());
  await tester.pump(const Duration(seconds: 5));
}

RenderGridSelectable _grid0(WidgetTester tester) =>
    tester.renderObject<RenderGridSelectable>(
      find.byType(GridSelectable, skipOffstage: false),
    );

/// [text] 가 든 줄의 가운데(전역 좌표). [dx] 는 그 줄 안 칸 자리.
Offset _rowAt(WidgetTester tester, String text, {double dx = 1.5}) {
  final box = _grid0(tester);
  final lines = box.text.lines;
  final r = lines.indexWhere((l) => l.map((x) => x.text).join().contains(text));
  expect(r, isNonNegative, reason: '$text 가 든 줄이 없다');
  return box.localToGlobal(
    Offset(dx * box.cell.width, (r + 0.5) * box.cell.height),
  );
}

String? _selected(WidgetTester tester) =>
    _grid0(tester).getSelectedContent()?.plainText;

List<String?> _actions(WidgetTester tester) => [
  for (final b in tester.widgetList<IconButton>(
    find.descendant(of: find.byType(AppBar), matching: find.byType(IconButton)),
  ))
    if (b.tooltip != 'Back') b.tooltip,
];

/// 길게 눌러(600ms) 잡고 [to] 까지 끈 뒤 놓는다.
Future<void> _pressDrag(WidgetTester tester, Offset from, Offset to) async {
  final g = await tester.startGesture(from);
  await tester.pump(const Duration(milliseconds: 600));
  for (var i = 1; i <= 8; i++) {
    await g.moveTo(Offset.lerp(from, to, i / 8)!);
    await tester.pump(const Duration(milliseconds: 16));
  }
  await g.up();
  await tester.pumpAndSettle();
}

void main() {
  group('GridText', () {
    test('두 칸 글자는 가운데에서 갈리지 않는다', () {
      final t = GridText([_row('ab가나cd')]);
      expect(t.width(0), 8);
      // 가 = 2..4 — 왼쪽 반이면 앞, 오른쪽 반이면 뒤.
      expect(t.boundary(0, 2.9), 2);
      expect(t.boundary(0, 3.0), 4);
      expect(t.boundary(0, 3.9), 4);
      expect(t.boundary(0, 99), 8);
      expect(t.text(const GridSpot(0, 2), const GridSpot(0, 6)), '가나');
    });

    test('낱말 — 한글 낱말, 경로는 한 덩어리', () {
      const s = '수정한 파일: lib/grid_select.dart 끝';
      final t = GridText([_row(s)]);
      String word(String at) {
        final w = t.word(0, _colOf(s, at) + 0.5)!;
        return t.text(GridSpot(0, w.$1), GridSpot(0, w.$2));
      }

      expect(word('정'), '수정한');
      expect(word('grid'), 'lib/grid_select.dart');
      // 빈칸이면 왼쪽 낱말, 글 뒤 빈 곳이면 그 줄 마지막 낱말.
      expect(word(' lib'), '파일:');
      final end = t.word(0, 200)!;
      expect(t.text(GridSpot(0, end.$1), GridSpot(0, end.$2)), '끝');
      expect(GridText([_row('')]).word(0, 3), isNull);
    });

    test('폰 폭으로 접힌 줄은 한 줄로, 줄 끝 빈칸은 걷고 복사한다', () {
      const long = '한글과 English가 섞인 아주 긴 문장은 폰 폭에서 여러 줄로 접힌다';
      const bullet = '- 글머리 아래로 이어지는 긴 목록 항목도 들여쓰기 없이 이어진다';
      const url = 'https://example.com/a/very/long/path/that/breaks/mid/word';
      final view = Reflow().apply(
        _grid([long, bullet, url, '끝   '], 80),
        20,
      );
      expect(view.rows, greaterThan(6), reason: '접혀야 시험이 된다');
      expect(_all(GridText(view.lines, folds: view.folds)), [
        long,
        bullet,
        url,
        '끝',
      ].join('\n'));
      // 접힘을 모르면 줄마다 끊긴다 — 이음새 정보가 실제로 쓰였다.
      expect(_all(GridText(view.lines)).split('\n').length, view.rows);
    });
  });

  group('터미널 고르기', () {
    setUpAll(loadFonts);

    testWidgets('앱바는 보기 단추와 닫기 둘뿐', (tester) async {
      await _open(tester);
      expect(_actions(tester), ['대화로 보기', 'pane 닫기']);
      expect(find.byTooltip('글자 선택·복사'), findsNothing);
      expect(find.byTooltip('데스크톱 격자 그대로 보기'), findsNothing);
      await _close(tester);
    }, variant: TargetPlatformVariant.only(TargetPlatform.iOS));

    testWidgets('길게 눌러 끌면 여러 줄을 고르고, 메뉴의 복사로 담는다', (tester) async {
      String? clip;
      tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        SystemChannels.platform,
        (call) async {
          if (call.method == 'Clipboard.setData') {
            clip = (call.arguments as Map)['text'] as String?;
          }
          return null;
        },
      );
      addTearDown(
        () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
          SystemChannels.platform,
          null,
        ),
      );
      await _open(tester);
      await _pressDrag(
        tester,
        _rowAt(tester, '검증 70 '),
        _rowAt(tester, '검증 73 ', dx: 12),
      );
      final text = _selected(tester)!;
      expect(text.split('\n').length, 4, reason: text);
      expect(text, startsWith('검증 70 한글과 English 줄'));
      expect(text, contains('검증 71 한글과 English 줄\n검증 72'));
      expect(text, endsWith('검증 73 한글과'));

      // 놓으면 메뉴 — 복사·전체 선택 둘.
      expect(find.text('복사'), findsOneWidget);
      expect(find.text('전체 선택'), findsOneWidget);
      await tester.tap(find.text('복사'));
      await tester.pump();
      expect(clip, text);
      expect(find.text('복사했어요'), findsOneWidget);
      await _close(tester);
    }, variant: TargetPlatformVariant.only(TargetPlatform.iOS));

    testWidgets('가장자리로 끌면 지난 줄로 굴러 올라가며 넓힌다', (tester) async {
      await _open(tester);
      final scroll = Scrollable.of(
        tester.element(find.byType(GridSelectable)),
      ).position;
      expect(scroll.pixels, 0);
      final g = await tester.startGesture(_rowAt(tester, '검증 80 '));
      await tester.pump(const Duration(milliseconds: 600));
      // 터미널 위 가장자리를 넘겨 앱바까지 끌고 붙들고 있는다 — 붙든 손가락도 조금씩 떨린다.
      final edge = Offset(40, tester.getRect(find.byType(PageView)).top - 30);
      await g.moveTo(edge);
      for (var i = 0; i < 40; i++) {
        await g.moveBy(Offset(0, i.isEven ? 0.5 : -0.5));
        await tester.pump(const Duration(milliseconds: 16));
      }
      await g.up();
      await tester.pumpAndSettle();
      expect(scroll.pixels, greaterThan(0));
      final text = _selected(tester)!;
      expect(text.split('\n').length, greaterThan(50), reason: text);
      // 위로 넓혀도 길게 누른 낱말은 고른 안에 남는다.
      expect(text, endsWith('검증 79 한글과 English 줄\n검증'));
      await _close(tester);
    }, variant: TargetPlatformVariant.only(TargetPlatform.iOS));

    testWidgets('누르면 고름이 걷히고, 쪽을 넘기면 고름도 같이 걷힌다', (tester) async {
      await _open(tester);
      await _pressDrag(
        tester,
        _rowAt(tester, '검증 75 '),
        _rowAt(tester, '검증 76 ', dx: 8),
      );
      expect(_selected(tester), isNotEmpty);
      await tester.tapAt(_rowAt(tester, '검증 60 ', dx: 30));
      await tester.pumpAndSettle();
      expect(_selected(tester) ?? '', isEmpty);
      expect(find.text('복사'), findsNothing);

      await _pressDrag(
        tester,
        _rowAt(tester, '검증 75 '),
        _rowAt(tester, '검증 76 ', dx: 8),
      );
      expect(_selected(tester), isNotEmpty);
      await tester.tap(find.byTooltip('대화로 보기'));
      await tester.pumpAndSettle();
      expect(_selected(tester), isNull);
      expect(find.text('복사'), findsNothing);
      await _close(tester);
    }, variant: TargetPlatformVariant.only(TargetPlatform.iOS));

    // 플랫폼을 가리지 않는다 — 고르기 영역의 가로 끌기가 먼저 이기는 플랫폼(안드로이드·웹)에서도.
    for (final platform in [TargetPlatform.iOS, TargetPlatform.android]) {
      testWidgets('고른 채 가로로 밀어도 쪽이 넘어가고 고름은 걷힌다 ($platform)', (
        tester,
      ) async {
        await _open(tester);
        await _pressDrag(
          tester,
          _rowAt(tester, '검증 75 '),
          _rowAt(tester, '검증 76 ', dx: 8),
        );
        expect(_selected(tester), isNotEmpty);
        // 16ms 마다 시각을 붙여 문턱과 튕김 속도를 실제처럼 낸다.
        var t = Duration.zero;
        const frame = Duration(milliseconds: 16);
        final g = await tester.createGesture();
        await g.down(_rowAt(tester, '검증 70 ', dx: 30), timeStamp: t);
        for (var i = 0; i < 10; i++) {
          t += frame;
          await g.moveBy(const Offset(-22, 0), timeStamp: t);
          await tester.pump(frame);
        }
        await g.up(timeStamp: t);
        await tester.pumpAndSettle();
        final pages = tester.widget<PageView>(find.byType(PageView));
        expect(pages.controller!.page, 1);
        expect(_selected(tester), isNull);
        await _close(tester);
      }, variant: TargetPlatformVariant.only(platform));
    }

    testWidgets('짧게 끄는 세로 손가락은 고르지 않고 스크롤한다', (tester) async {
      await _open(tester);
      final scroll = Scrollable.of(
        tester.element(find.byType(GridSelectable)),
      ).position;
      await tester.dragFrom(_rowAt(tester, '검증 70 '), const Offset(0, 200));
      await tester.pumpAndSettle();
      expect(scroll.pixels, greaterThan(100));
      expect(_selected(tester), isNull);
      await _close(tester);
    }, variant: TargetPlatformVariant.only(TargetPlatform.iOS));
  });
}
