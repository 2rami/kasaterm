import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/grid.dart';
import 'package:kasaterm_mobile/grid_canvas.dart';

Grid tall() {
  final rows = <String>[
    for (var i = 0; i < 60; i++) '줄 $i',
    '──────────────────────────────',
    '❯ 입력',
    '──────────────────────────────',
    'Fable ￼',
  ];
  return Grid()..apply({
    'cols': 40,
    'rows': rows.length,
    'dirty': [
      for (var r = 0; r < rows.length; r++)
        [
          r,
          [
            [rows[r], null, null, 0],
          ],
        ],
    ],
    'cursor': [61, 2],
    'cursorVisible': true,
  });
}

Widget host(Grid g, int tick) => MaterialApp(
  home: Center(
    child: SizedBox(
      width: 402,
      height: 500,
      child: Row(
        children: [
          Expanded(
            child: Builder(
              builder: (context) => WrappedCanvas(
                grid: g,
                version: 1,
                palette: TerminalPalette.of(context),
                bottomTick: tick,
              ),
            ),
          ),
        ],
      ),
    ),
  ),
);

final gridPaints = find.byWidgetPredicate(
  (w) => w is CustomPaint && w.painter.runtimeType.toString() == '_GridPainter',
);

void main() {
  testWidgets('위로 넘기면 입력상자부터 끝까지가 바닥에 붙고, 키를 보내면 다시 내려온다', (tester) async {
    final g = tall();
    await tester.pumpWidget(host(g, 0));
    await tester.pump();
    expect(gridPaints, findsOneWidget);
    final outer = tester.getRect(find.byType(WrappedCanvas));
    // 바닥에 있을 때는 붙잡을 것이 없다.
    await tester.drag(find.byType(SingleChildScrollView), const Offset(0, 200));
    await tester.pump();
    expect(gridPaints, findsNWidgets(2));
    final tail = tester.getRect(gridPaints.last);
    expect(tail.bottom, outer.bottom);
    // 입력상자 위 테두리부터 상태줄까지 네 줄.
    expect(tail.height, greaterThan(0));
    expect(tail.height, lessThan(outer.height / 2));
    await tester.pumpWidget(host(g, 1));
    await tester.pump();
    expect(gridPaints, findsOneWidget);
  });
}
