import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/fill_viewer.dart';

Future<double> pumpScale(WidgetTester tester, Size content, Size box) async {
  await tester.pumpWidget(
    Center(
      child: SizedBox(
        width: box.width,
        height: box.height,
        child: FillViewer(
          content: content,
          background: Colors.black,
          child: const ColoredBox(color: Colors.white),
        ),
      ),
    ),
  );
  await tester.pump();
  final viewer = tester.widget<InteractiveViewer>(
    find.byType(InteractiveViewer),
  );
  return viewer.transformationController!.value.getMaxScaleOnAxis();
}

void main() {
  testWidgets('넓은 내용은 높이를 채우되 1.3배에서 멈춘다', (tester) async {
    // fitW 0.5, fitH 3 → min(3, 1.3) = 1.3
    expect(
      await pumpScale(tester, const Size(400, 100), const Size(200, 300)),
      closeTo(1.3, 1e-6),
    );
  });

  testWidgets('아주 넓은 내용은 높이에 맞춘다', (tester) async {
    // fitW 0.125, fitH 0.75 → 0.75 — 폭에 맞추면 손톱만 해지는 경우
    expect(
      await pumpScale(tester, const Size(1600, 400), const Size(200, 300)),
      closeTo(0.75, 1e-6),
    );
  });

  testWidgets('좁고 긴 내용은 폭을 채운다', (tester) async {
    // fitW 2, fitH 0.3 → max(2, 0.3) = 2
    expect(
      await pumpScale(tester, const Size(100, 1000), const Size(200, 300)),
      closeTo(2, 1e-6),
    );
  });
}
