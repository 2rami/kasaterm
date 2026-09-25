import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/nacho.dart';
import 'package:kasaterm_mobile/screens/nacho_home.dart';
import 'package:kasaterm_mobile/screens/nacho_typing.dart';

import 'nacho_test.dart' show FakeNacho, settle;

Future<NachoDesk> open(WidgetTester tester, FakeNacho s, {bool still = false}) async {
  final d = NachoDesk(s);
  Widget home = NachoHome(server: s, onChangeAddress: () async {}, desk: d);
  if (still) {
    home = Builder(
      builder: (context) => MediaQuery(
        data: MediaQuery.of(context).copyWith(disableAnimations: true),
        child: NachoHome(server: s, onChangeAddress: () async {}, desk: d),
      ),
    );
  }
  await tester.pumpWidget(MaterialApp(home: home));
  await settle(tester);
  return d;
}

/// 원장에 새 줄을 넣고 매달려 있던 롱폴을 풀어 폰이 받게 한다.
Future<void> deliver(WidgetTester tester, FakeNacho s, List<Map<String, Object?>> rows) async {
  for (final r in rows) {
    s.add(r);
  }
  s.hold?.complete();
  await settle(tester);
}

Future<void> close(WidgetTester tester, NachoDesk d, FakeNacho s) async {
  d.stop();
  s.hold?.complete();
  await tester.pumpWidget(const SizedBox.shrink());
}

void main() {
  testWidgets('도는 동안 나쵸 자리에 점 하나 — 「답하는 중」 글은 없고 진행 한 줄은 점 밑에', (tester) async {
    final s = FakeNacho()
      ..add({'kind': 'message', 'id': 'a', 'text': '폰 화면 고쳐'})
      ..add({'kind': 'status', 'message': 'a', 'state': 'running'})
      ..add({'kind': 'progress', 'message': 'a', 'text': 'Bash flutter test'});
    final semantics = tester.ensureSemantics();
    final d = await open(tester, s);
    expect(find.byType(NachoTyping), findsOneWidget);
    expect(find.text('나쵸가 답하는 중'), findsNothing, reason: '글자 대신 점이 말한다');
    expect(find.bySemanticsLabel(RegExp('^나쵸가 답하는 중')), findsOneWidget, reason: '읽어 주기에는 남는다');
    expect(find.text('Bash flutter test'), findsOneWidget);
    final bubble = tester.getTopLeft(find.text('폰 화면 고쳐'));
    final dots = tester.getTopLeft(find.byType(NachoTyping));
    expect(dots.dy, greaterThan(bubble.dy), reason: '내 말 아래(가장 새 자리)');
    await close(tester, d, s);
    semantics.dispose();
  });

  testWidgets('첫 답이 오면 내리고 시계도 멈춘다', (tester) async {
    final s = FakeNacho()
      ..add({'kind': 'message', 'id': 'a', 'text': '하나'})
      ..add({'kind': 'status', 'message': 'a', 'state': 'running'});
    final d = await open(tester, s);
    expect(find.byType(NachoTyping), findsOneWidget);
    expect(tester.binding.transientCallbackCount, greaterThan(0), reason: '점이 뛰는 중');
    // 답 줄이 먼저 오고 끝 상태는 그 뒤에 온다(펫 전달을 기다리는 사이) — 답이 보이면 점은 내린다.
    await deliver(tester, s, [
      {'kind': 'reply', 'message': 'a', 'text': '고쳤어'},
    ]);
    expect(find.text('고쳤어'), findsOneWidget);
    expect(find.byType(NachoTyping), findsNothing);
    expect(tester.binding.transientCallbackCount, 0, reason: '내린 뒤 프레임을 더 요청하지 않는다');
    await close(tester, d, s);
  });

  for (final end in ['failed', 'refused', 'interrupted', 'restart', 'answered']) {
    testWidgets('끝 상태($end)면 답이 없어도 내린다', (tester) async {
      final s = FakeNacho()
        ..add({'kind': 'message', 'id': 'a', 'text': '하나'})
        ..add({'kind': 'status', 'message': 'a', 'state': 'running'});
      final d = await open(tester, s);
      expect(find.byType(NachoTyping), findsOneWidget);
      await deliver(tester, s, [
        {'kind': 'status', 'message': 'a', 'state': end, 'note': '이 턴은 끊겼다'},
      ]);
      expect(find.byType(NachoTyping), findsNothing);
      await close(tester, d, s);
    });
  }

  testWidgets('여러 말이 기다려도 점은 하나', (tester) async {
    final s = FakeNacho()
      ..add({'kind': 'message', 'id': 'a', 'text': '하나'})
      ..add({'kind': 'status', 'message': 'a', 'state': 'running'})
      ..add({'kind': 'message', 'id': 'b', 'text': '둘'})
      ..add({'kind': 'status', 'message': 'b', 'state': 'queued'});
    final d = await open(tester, s);
    expect(find.byType(NachoTyping), findsOneWidget);
    expect(find.text('앞 턴이 끝나면 이어서'), findsOneWidget, reason: '줄 선 말의 사정은 그 말 밑에 그대로');
    // 앞 말이 답을 받아도 뒤 말이 남아 있으면 점도 남는다.
    await deliver(tester, s, [
      {'kind': 'reply', 'message': 'a', 'text': '하나 답'},
      {'kind': 'status', 'message': 'a', 'state': 'answered'},
      {'kind': 'status', 'message': 'b', 'state': 'running'},
    ]);
    expect(find.byType(NachoTyping), findsOneWidget);
    await deliver(tester, s, [
      {'kind': 'reply', 'message': 'b', 'text': '둘 답'},
    ]);
    expect(find.byType(NachoTyping), findsNothing);
    await close(tester, d, s);
  });

  testWidgets('끝 상태를 못 적은 옛 말은 뒤 말이 답을 받았으면 점을 붙잡지 않는다', (tester) async {
    final s = FakeNacho()
      ..add({'kind': 'message', 'id': 'old', 'text': '옛 말'})
      ..add({'kind': 'status', 'message': 'old', 'state': 'running'})
      ..add({'kind': 'message', 'id': 'b', 'text': '새 말'})
      ..add({'kind': 'reply', 'message': 'b', 'text': '새 답'})
      ..add({'kind': 'status', 'message': 'b', 'state': 'answered'});
    final d = await open(tester, s);
    expect(find.byType(NachoTyping), findsNothing);
    await close(tester, d, s);
  });

  testWidgets('보내면 접수 뒤에 점이 서고, 대화 없이 새로 연 창구엔 점이 없다', (tester) async {
    final s = FakeNacho();
    final d = await open(tester, s);
    expect(find.byType(NachoTyping), findsNothing);
    await tester.enterText(find.byType(TextField), '안녕');
    await tester.tap(find.byTooltip('보내기'));
    await settle(tester);
    expect(find.text('안녕'), findsOneWidget);
    expect(find.byType(NachoTyping), findsOneWidget, reason: '서버가 받았다(접수됨)');
    await close(tester, d, s);
  });

  testWidgets('그려진 점이 실제로 차례로 뛴다 — 높이 차를 px 로 잰다', (tester) async {
    final s = FakeNacho()
      ..add({'kind': 'message', 'id': 'a', 'text': '하나'})
      ..add({'kind': 'status', 'message': 'a', 'state': 'running'});
    final d = await open(tester, s);
    final dots = find.descendant(
      of: find.byType(NachoTyping),
      matching: find.byWidgetPredicate((w) => w is Container && w.constraints?.maxWidth == 7),
    );
    expect(dots, findsNWidgets(3));
    final firstHighest = <int>[];
    var spread = 0.0;
    for (var i = 0; i < 24; i++) {
      await tester.pump(NachoTyping.period ~/ 24);
      final tops = [for (var k = 0; k < 3; k++) tester.getTopLeft(dots.at(k)).dy];
      final low = tops.reduce((a, b) => a > b ? a : b);
      final high = tops.reduce((a, b) => a < b ? a : b);
      spread = spread > low - high ? spread : low - high;
      final top = tops.indexOf(high);
      if (low - high > 3 && (firstHighest.isEmpty || firstHighest.last != top)) firstHighest.add(top);
    }
    expect(spread, inInclusiveRange(3.0, 4.01), reason: '최대 4px 뜬다');
    expect(firstHighest.toSet(), {0, 1, 2}, reason: '한 바퀴에 세 점이 모두 한 번씩 가장 높다');
    await close(tester, d, s);
  });

  testWidgets('동작 줄이기면 점은 서 있되 시계를 돌리지 않는다', (tester) async {
    final s = FakeNacho()
      ..add({'kind': 'message', 'id': 'a', 'text': '하나'})
      ..add({'kind': 'status', 'message': 'a', 'state': 'running'});
    final d = await open(tester, s, still: true);
    expect(find.byType(NachoTyping), findsOneWidget);
    expect(tester.binding.transientCallbackCount, 0);
    await close(tester, d, s);
  });

  test('점 위상 — 세 점이 차례로 뜨고 한 번에 하나만 가장 높다', () {
    // 한 바퀴를 잘게 돌며 가장 높은 점의 순서를 적는다.
    final order = <int>[];
    for (var i = 0; i < 100; i++) {
      final t = i / 100;
      final lifts = [for (var dot = 0; dot < 3; dot++) NachoTyping.lift(t, dot)];
      final top = lifts.indexOf(lifts.reduce((a, b) => a > b ? a : b));
      if (lifts[top] > 0.99 && (order.isEmpty || order.last != top)) order.add(top);
    }
    expect(order, [0, 1, 2]);
  });
}
