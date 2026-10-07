import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/grid.dart';
import 'package:kasaterm_mobile/grid_canvas.dart';

/// Claude 전체 화면처럼 대체 화면에 마우스 보고를 켠 좁은 칸(24×15) — 스크롤백이 없다.
Grid claudeScreen({int rows = 15}) => Grid()
  ..apply({
    'cols': 24,
    'rows': rows,
    'dirty': [
      for (var r = 0; r < rows; r++)
        [
          r,
          [
            ['줄 $r 유우카가 확인', null, null, 0],
          ],
        ],
    ],
    'cursor': [rows - 1, 0],
    'cursorVisible': true,
    'alt': true,
    'mouse': true,
    'mouseSgr': true,
  });

Widget host(Widget Function(BuildContext) canvas) => MaterialApp(
  home: Center(
    child: SizedBox(width: 390, height: 600, child: Builder(builder: canvas)),
  ),
);

void main() {
  test('대체 화면·마우스 보고 깃발을 프레임에서 읽는다', () {
    final g = claudeScreen();
    expect((g.alt, g.mouse, g.mouseSgr), (true, true, true));
    g.apply({'alt': false});
    expect(g.alt, isFalse);
    expect(g.mouse, isTrue, reason: '빠진 필드는 그대로 둔다');
  });

  testWidgets('폰 폭 보기 — 아래로 끈 줄 수만큼 위로 굴린다', (tester) async {
    final g = claudeScreen();
    final wheels = <int>[];
    await tester.pumpWidget(
      host(
        (context) => WrappedCanvas(
          grid: g,
          version: g.version,
          palette: TerminalPalette.of(context),
          onWheel: wheels.add,
        ),
      ),
    );
    await tester.pump();
    final at = tester.getCenter(find.byType(WrappedCanvas));
    await tester.dragFrom(at, const Offset(0, 200));
    await tester.pump();
    final up = wheels.fold(0, (a, b) => a + b);
    expect(up, greaterThan(5), reason: '$wheels');
    expect(wheels.every((l) => l > 0), isTrue);
    wheels.clear();
    await tester.dragFrom(at, const Offset(0, -200));
    await tester.pump();
    expect(wheels.fold(0, (a, b) => a + b), lessThan(-5));
  });

  testWidgets('폰 폭 보기 — 화면보다 짧은 전체 화면은 바닥이 아니라 위에 붙는다', (tester) async {
    // 폰이 원본 크기를 안 쥔 동안(보기만)은 데스크톱 칸의 15줄뿐이다 — 바닥에 앉히면 위가 빈다.
    Future<Rect> body({required bool fullScreen}) async {
      final g = claudeScreen();
      await tester.pumpWidget(
        host(
          (context) => WrappedCanvas(
            grid: g,
            version: g.version,
            palette: TerminalPalette.of(context),
            fullScreen: fullScreen,
          ),
        ),
      );
      await tester.pump();
      return tester.getRect(
        find
            .descendant(
              of: find.byType(SingleChildScrollView),
              matching: find.byType(CustomPaint),
            )
            .first,
      );
    }

    final shell = await body(fullScreen: false);
    final top = tester.getRect(find.byType(WrappedCanvas)).top;
    expect(shell.top, greaterThan(top + 200), reason: '셸은 전처럼 바닥(입력줄)에 붙는다');
    final full = await body(fullScreen: true);
    expect(full.top, top, reason: '전체 화면은 첫 줄이 맨 위');
    expect(full.height, 600);
  });

  testWidgets('스크롤백이 있는 화면(셸)은 휠을 안 받는다', (tester) async {
    final g = claudeScreen()..apply({'alt': false});
    await tester.pumpWidget(
      host(
        (context) => WrappedCanvas(
          grid: g,
          version: g.version,
          palette: TerminalPalette.of(context),
          history: [
            for (var i = 0; i < 80; i++)
              [Run('지난 줄 $i', const DefaultColor(), const DefaultColor(), 0)],
          ],
          historyVersion: 1,
        ),
      ),
    );
    await tester.pump();
    final scroll = find.byType(SingleChildScrollView);
    await tester.drag(scroll, const Offset(0, 200));
    await tester.pump();
    expect(
      tester.widget<SingleChildScrollView>(scroll).controller!.offset,
      greaterThan(100),
    );
  });
}
