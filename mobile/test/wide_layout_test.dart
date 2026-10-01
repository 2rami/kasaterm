import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/wide_layout.dart';

void main() {
  test('열 수 — 폰 세로는 1열, 아이패드 세로 2열, 가로 3열', () {
    // 허브 ListView 좌우 여백 12 씩을 뺀 폭.
    expect(columnsFor(402 - 24), 1);
    expect(columnsFor(320 - 24), 1);
    expect(columnsFor(744 - 24), 2, reason: 'iPad mini 세로');
    expect(columnsFor(834 - 24), 2);
    expect(columnsFor(1032 - 24), 2);
    expect(columnsFor(1210 - 24), 3);
    expect(columnsFor(1376 - 24), 3);
  });

  test('벽돌 쌓기 — 차례대로 가장 낮은 열에, 같으면 왼쪽', () {
    expect(masonryPlace([300, 100, 100, 100], 2), [
      (0, 0.0),
      (1, 0.0),
      (1, 100.0),
      (1, 200.0),
    ]);
    expect(masonryPlace([100, 100, 100], 1), [(0, 0.0), (0, 100.0), (0, 200.0)]);
    expect(masonryPlace([50, 50, 50], 3), [(0, 0.0), (1, 0.0), (2, 0.0)]);
  });

  testWidgets('Masonry — 받은 폭으로 열을 나누고, 높이는 가장 긴 열', (tester) async {
    // 시험 화면이 800 폭이다.
    Widget box(double h, String k) => SizedBox(key: ValueKey(k), height: h);
    await tester.pumpWidget(
      Directionality(
        textDirection: TextDirection.ltr,
        child: Align(
          alignment: Alignment.topLeft,
          child: SizedBox(
            width: 800,
            child: Masonry(children: [box(300, 'a'), box(100, 'b'), box(100, 'c')]),
          ),
        ),
      ),
    );
    expect(tester.getRect(find.byKey(const ValueKey('a'))), const Rect.fromLTWH(0, 0, 395, 300));
    expect(tester.getRect(find.byKey(const ValueKey('b'))), const Rect.fromLTWH(405, 0, 395, 100));
    expect(tester.getRect(find.byKey(const ValueKey('c'))), const Rect.fromLTWH(405, 100, 395, 100));
    expect(tester.getSize(find.byType(Masonry)), const Size(800, 300));
  });
}
