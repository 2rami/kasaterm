import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/look.dart';
import 'package:kasaterm_mobile/status_style.dart';

/// 그린 그림의 (x, y) 알파. 칸 100×40 을 그 크기 그대로 굽는다.
Future<int Function(int, int)> _paint(double t, {bool still = false}) async {
  const size = Size(100, 40);
  final rec = ui.PictureRecorder();
  OrbitPainter(
    AlwaysStoppedAnimation(t),
    color: const Color(0xFF00FF00),
    radius: BorderRadius.zero,
    still: still,
  ).paint(Canvas(rec), size);
  final img = await rec.endRecording().toImage(100, 40);
  final data = (await img.toByteData(format: ui.ImageByteFormat.rawRgba))!;
  return (x, y) => data.getUint8((y * 100 + x) * 4 + 3);
}

void main() {
  testWidgets('쉬는 칸은 아무것도 안 두르고, 일하는 칸만 도는 윤곽을 단다', (tester) async {
    Widget edge(bool live) => MaterialApp(
      home: OrbitEdge(
        live: live,
        color: Colors.green,
        radius: Look.smallCorners,
        child: const SizedBox(width: 100, height: 40),
      ),
    );
    await tester.pumpWidget(edge(false));
    expect(
      find.byWidgetPredicate(
        (w) => w is CustomPaint && w.foregroundPainter is OrbitPainter,
      ),
      findsNothing,
    );
    await tester.pumpWidget(edge(true));
    expect(
      find.byWidgetPredicate(
        (w) => w is CustomPaint && w.foregroundPainter is OrbitPainter,
      ),
      findsOneWidget,
    );
    // 도는 컨트롤러가 살아 있으면 테스트가 안 끝난다 — 쉬게 돌려 거둔다.
    await tester.pumpWidget(edge(false));
  });

  test('빛 조각은 윤곽 일부만 — 머리 뒤만 빛나고 반대편은 비어 있다', () async {
    // 둘레 = 2(100 + 40) 근처. t = 0.25 면 머리가 위 변 오른쪽 끝 근처(70 쯤)에 있다.
    final a = await _paint(0.25);
    expect(a(60, 1), greaterThan(0), reason: '머리 바로 뒤(위 변)는 칠해진다');
    expect(a(50, 38), 0, reason: '아래 변 가운데는 꼬리가 닿지 않는다');
    expect(a(1, 20), 0, reason: '왼쪽 변도 비어 있다');
  });

  test('꼬리가 시작점(왼쪽 위)을 넘어도 끊기지 않는다', () async {
    // t = 0.02 면 머리가 위 변 왼쪽 끝 바로 지나고, 꼬리는 왼쪽 변 위쪽에 걸친다.
    final a = await _paint(0.02);
    expect(a(3, 1), greaterThan(0));
    expect(a(1, 12), greaterThan(0), reason: '꼬리가 왼쪽 변으로 이어진다');
  });

  test('동작 줄이기면 돌지 않고 윤곽 전체가 옅게 선다', () async {
    final a = await _paint(0.25, still: true);
    for (final (x, y) in [(50, 0), (50, 39), (0, 20), (99, 20)]) {
      expect(a(x, y), inExclusiveRange(0, 255), reason: '($x, $y)');
    }
  });
}
