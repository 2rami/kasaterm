import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/screens/controls.dart';
import 'package:kasaterm_mobile/screens/engine_sheet.dart';

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
    expect(find.text('Opus 5.5 1M'), findsOneWidget);
    expect(find.text('xhigh'), findsOneWidget);
  });

  testWidgets('대화 쪽은 모델 칸이 단추고, 셸 쪽은 단추가 없다', (tester) async {
    var taps = 0;
    await tester.pumpWidget(_host(PlaceLine(
      cwd: '/Users/me/repo',
      model: 'GPT-5.6 Sol',
      effort: 'high',
      harness: 'codex',
      colored: true,
      onEngine: () => taps++,
      style: const TextStyle(fontSize: 13),
    )));
    await tester.tap(find.text('GPT-5.6 Sol'));
    expect(taps, 1);
    expect(tester.getSize(find.byType(PlaceLine)).height, greaterThanOrEqualTo(44));
    await tester.pumpWidget(_host(const PlaceLine(cwd: '/x', style: TextStyle(fontSize: 13))));
    expect(find.byIcon(Icons.expand_more), findsNothing);
  });

  test('effort 색은 데스크톱 상태줄과 같다', () {
    expect(effortColor('xhigh'), const Color(0xfff7768e));
    expect(effortColor('high'), const Color(0xffe0af68));
    expect(effortColor('low'), const Color(0xff565f89));
  });

  test('고른 것을 학생 칸 슬래시 명령으로 바꾼다', () {
    expect(engineCommand(harness: 'claude', model: 'opus'), '/model opus');
    expect(engineCommand(harness: 'claude', effort: 'xhigh'), '/effort xhigh');
    expect(engineCommand(harness: 'codex', model: 'gpt-5.6'), '/model');
  });

  testWidgets('모델 정보가 없으면 그 자리를 비운다', (tester) async {
    await tester.pumpWidget(_host(const PlaceLine(
      cwd: '/Users/me/repo',
      branch: 'main',
      model: ' ',
      style: TextStyle(fontSize: 13),
    )));
    expect(find.byType(Image), findsNothing);
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
