import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/grid.dart';
import 'package:kasaterm_mobile/grid_canvas.dart';

Grid gridOf(int cols, int rows, int len) => Grid()
  ..apply({
    'cols': cols,
    'rows': rows,
    'dirty': [
      for (var r = 0; r < rows; r++)
        [
          r,
          [
            ['x' * len, null, null, 0],
          ],
        ],
    ],
    'cursor': [rows - 1, 0],
    'cursorVisible': false,
  });

Widget host(Grid g) => MaterialApp(
  home: Center(
    child: SizedBox(
      width: 402,
      height: 500,
      child: Builder(
        builder: (context) => WrappedCanvas(
          grid: g,
          version: 1,
          palette: TerminalPalette.of(context),
        ),
      ),
    ),
  ),
);

final paints = find.byWidgetPredicate(
  (w) => w is CustomPaint && w.painter.runtimeType.toString() == '_GridPainter',
);

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  setUpAll(() async {
    // Ahem 사각형은 폭이 딱 떨어져 접힘이 재현되지 않는다 — 실제 글꼴로 잰다.
    final mono = FontLoader('TermMono')
      ..addFont(
        rootBundle.load('assets/fonts/JetBrainsMonoNerdFontMono-Regular.ttf'),
      );
    await mono.load();
  });

  testWidgets('pane 에 맞춘 글꼴이면 pane 폭 꽉 찬 줄도 접히지 않는다', (tester) async {
    // 44열 pane 의 44칸 줄 다섯 — 열 수가 43으로 떨어지면 열 줄이 된다.
    await tester.pumpWidget(host(gridOf(44, 5, 44)));
    await tester.pump();
    final full = tester.getRect(paints.last).height;
    await tester.pumpWidget(host(gridOf(44, 5, 10)));
    await tester.pump();
    final short = tester.getRect(paints.last).height;
    expect(full, short);
  });
}
