import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/screens/settings.dart';
import 'package:kasaterm_mobile/server.dart';

BrowseDevices sample({bool auto = false, String selected = 'phone:geono'}) =>
    BrowseDevices.fromJson({
      'ok': true,
      'open': 'chrome',
      'selected': selected,
      'auto': auto,
      'devices': [
        {'id': '', 'label': '이 기기', 'kind': 'desktop', 'online': true},
        {'id': '~1a2b', 'label': 'MacBook', 'kind': 'desktop', 'online': false},
        {
          'id': 'phone:geono',
          'label': 'geono 폰',
          'kind': 'phone',
          'online': true,
          'model': 'iPhone',
          'viewport': {'width': 393, 'height': 852, 'dpr': 3},
        },
      ],
    })!;

Future<List<String>> pump(
  WidgetTester tester,
  BrowseDevices data, {
  String myName = 'geono',
}) async {
  final picked = <String>[];
  await tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: SingleChildScrollView(
          child: BrowseCard(
            data: data,
            myName: myName,
            pending: null,
            onPick: (a, id) async => picked.add('$a=$id'),
          ),
        ),
      ),
    ),
  );
  return picked;
}

void main() {
  testWidgets('이 폰은 「이 폰」으로, 나머지는 라벨·종류·크기로 선다', (tester) async {
    await pump(tester, sample());
    expect(find.text('자동'), findsOneWidget);
    expect(find.text('이 폰'), findsOneWidget);
    expect(find.text('geono 폰'), findsNothing);
    expect(find.text('MacBook'), findsOneWidget);
    expect(find.text('폰 · iPhone · 393×852'), findsOneWidget);
    expect(find.text('데스크톱 · 오프라인'), findsOneWidget);
    expect(find.text('내장 웹'), findsOneWidget);
    expect(find.text('브라우저'), findsOneWidget);
  });

  testWidgets('다른 이름의 폰은 「이 폰」이 아니다', (tester) async {
    await pump(tester, sample(), myName: 'other');
    expect(find.text('이 폰'), findsNothing);
    expect(find.text('geono 폰'), findsOneWidget);
  });

  testWidgets('기기를 누르면 browse-device, 자동은 auto, 세그먼트는 browse-open', (
    tester,
  ) async {
    final picked = await pump(tester, sample());
    await tester.tap(find.text('MacBook'));
    await tester.tap(find.text('자동'));
    await tester.tap(find.text('내장 웹'));
    await tester.pump();
    expect(picked, [
      'browse-device=~1a2b',
      'browse-device=auto',
      'browse-open=web',
    ]);
  });

  testWidgets('auto 면 자동 항목이 켜지고 지금 고른 기기를 덧붙인다', (tester) async {
    final data = sample(auto: true, selected: 'phone:geono');
    expect(data.selectedItem, 'auto');
    await pump(tester, data);
    expect(find.textContaining('지금은 이 폰'), findsOneWidget);
  });
}
