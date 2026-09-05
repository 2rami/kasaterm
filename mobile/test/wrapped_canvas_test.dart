import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/grid.dart';
import 'package:kasaterm_mobile/grid_canvas.dart';
import 'package:kasaterm_mobile/reflow.dart';

void main() {
  testWidgets('느슨한 높이 안에서도 짧은 내용은 바닥에 앉는다', (tester) async {
    final g = Grid()
      ..apply({
        'cols': 60,
        'rows': 20,
        'dirty': [
          for (var r = 0; r < 20; r++)
            [
              r,
              if (r < 2)
                [
                  ['x' * 30, null, null, 0],
                ]
              else
                [],
            ],
        ],
        'cursor': [1, 31],
        'cursorVisible': true,
      });
    await tester.pumpWidget(
      MaterialApp(
        home: Center(
          child: SizedBox(
            width: 402,
            height: 500,
            // Row 는 자식 높이를 강제하지 않는다 — 실제 화면(학생색 리본 + 격자)과 같은 자리.
            child: Row(
              children: [
                Expanded(
                  child: Builder(
                    builder: (context) => WrappedCanvas(
                      grid: CombinedGrid(const [], g),
                      version: 1,
                      palette: TerminalPalette.of(context),
                    ),
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
    await tester.pump();
    final outer = tester.getRect(find.byType(WrappedCanvas));
    final paint = tester.getRect(find.byType(CustomPaint).last);
    expect(outer.height, 500);
    expect(paint.bottom, outer.bottom);
    expect(paint.height, lessThan(100));
  });
}
