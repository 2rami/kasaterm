import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/screens/controls.dart';

Widget _host(Widget child) => MaterialApp(home: Scaffold(body: child));

void main() {
  testWidgets('맥락 줄에 모델과 effort 가 브랜치 뒤에 선다', (tester) async {
    await tester.pumpWidget(_host(const PlaceLine(
      cwd: '/Users/me/repo',
      branch: 'main',
      model: 'Opus 5.5 1M',
      effort: 'xhigh',
      style: TextStyle(fontSize: 13),
    )));
    expect(find.text('main'), findsOneWidget);
    expect(find.text('Opus 5.5 1M · xhigh'), findsOneWidget);
  });

  testWidgets('모델 정보가 없으면 그 자리를 비운다', (tester) async {
    await tester.pumpWidget(_host(const PlaceLine(
      cwd: '/Users/me/repo',
      branch: 'main',
      model: ' ',
      style: TextStyle(fontSize: 13),
    )));
    expect(find.byIcon(Icons.auto_awesome_outlined), findsNothing);
  });

  testWidgets('effort 만 있어도 그것만 보인다', (tester) async {
    await tester.pumpWidget(_host(const PlaceLine(
      cwd: '/Users/me/repo',
      effort: 'high',
      style: TextStyle(fontSize: 13),
    )));
    expect(find.text('high'), findsOneWidget);
  });
}
