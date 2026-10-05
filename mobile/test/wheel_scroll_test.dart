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

  testWidgets('격자 그대로 보기 — 세로는 휠, 가로는 그대로 민다', (tester) async {
    final g = claudeScreen();
    final wheels = <int>[];
    await tester.pumpWidget(
      host(
        (context) => GridCanvas(
          grid: g,
          version: g.version,
          palette: TerminalPalette.of(context),
          onWheel: wheels.add,
        ),
      ),
    );
    await tester.pump();
    await tester.pump();
    final view = find.byType(InteractiveViewer);
    final before = tester
        .widget<InteractiveViewer>(view)
        .transformationController!
        .value
        .clone();
    final at = tester.getCenter(view);
    await tester.dragFrom(at, const Offset(0, 240));
    await tester.pumpAndSettle();
    expect(wheels.fold(0, (a, b) => a + b), greaterThan(3), reason: '$wheels');
    final after = tester
        .widget<InteractiveViewer>(view)
        .transformationController!
        .value;
    expect(
      after.getTranslation().y,
      before.getTranslation().y,
      reason: '세로로 밀어 빈 여백이 나오면 안 된다',
    );
    wheels.clear();
    await tester.dragFrom(at, const Offset(-160, 0));
    await tester.pumpAndSettle();
    expect(wheels, isEmpty);
  });

  testWidgets('격자 그대로 보기 — 세로로 넘치는 칸은 끝까지 밀고 넘는 몫만 굴린다', (tester) async {
    // 24×40 은 폭에 맞추면 세로가 화면을 넘친다 — 먼저 격자 끝까지 민다.
    final g = claudeScreen(rows: 40);
    final wheels = <int>[];
    await tester.pumpWidget(
      host(
        (context) => GridCanvas(
          grid: g,
          version: g.version,
          palette: TerminalPalette.of(context),
          onWheel: wheels.add,
        ),
      ),
    );
    await tester.pump();
    await tester.pump();
    final view = find.byType(InteractiveViewer);
    Matrix4 m() =>
        tester.widget<InteractiveViewer>(view).transformationController!.value;
    final box = tester.getSize(view);
    final contentH = box.width / (24 * 0.6) * 40 * 1.2;
    expect(contentH, greaterThan(box.height));
    final at = tester.getCenter(view);
    // 위로 끌면 아래 끝까지 밀리고, 남는 몫은 아래로 굴린다.
    await tester.dragFrom(at, const Offset(0, -2000));
    await tester.pumpAndSettle();
    final bottom = m().getTranslation().y;
    expect(bottom, lessThan(-100));
    expect(bottom, greaterThanOrEqualTo(box.height - contentH - 30));
    expect(wheels.fold(0, (a, b) => a + b), lessThan(0));
    wheels.clear();
    // 아래로 조금 끌면 격자만 밀리고 굴리지 않는다.
    await tester.dragFrom(at, const Offset(0, 120));
    await tester.pumpAndSettle();
    expect(m().getTranslation().y, greaterThan(bottom));
    expect(wheels, isEmpty);
    // 위 끝을 넘으면 빈 여백 대신 지난 내용으로 굴린다.
    await tester.dragFrom(at, const Offset(0, 2000));
    await tester.pumpAndSettle();
    expect(m().getTranslation().y, 0);
    expect(wheels.fold(0, (a, b) => a + b), greaterThan(0));
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
