import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/twins_loading.dart';

Widget host({required bool still}) => MaterialApp(
  home: Builder(
    builder: (context) => MediaQuery(
      data: MediaQuery.of(context).copyWith(disableAnimations: still),
      child: const Scaffold(body: TwinsLoading()),
    ),
  ),
);

void main() {
  testWidgets('쌍둥이가 번갈아 뛰며 확인 중임을 알린다', (tester) async {
    await tester.pumpWidget(host(still: false));
    expect(find.byType(Image), findsNWidgets(2));
    expect(find.bySemanticsLabel('계정 확인 중'), findsOneWidget);
    expect(tester.hasRunningAnimations, isTrue);
    // 반 박 엇갈림 — 한쪽이 떠 있을 때 다른 쪽은 땅에 있다.
    await tester.pump(const Duration(milliseconds: 250));
    final ys = tester.widgetList<Image>(find.byType(Image))
        .map((i) => tester.getBottomLeft(find.byWidget(i)).dy)
        .toList();
    expect(ys[0], isNot(moreOrLessEquals(ys[1], epsilon: 1)));
  });

  testWidgets('동작 줄이기면 멈춘 그림', (tester) async {
    await tester.pumpWidget(host(still: true));
    expect(find.byType(Image), findsNWidgets(2));
    expect(find.bySemanticsLabel('계정 확인 중'), findsOneWidget);
    expect(tester.hasRunningAnimations, isFalse);
  });
}
