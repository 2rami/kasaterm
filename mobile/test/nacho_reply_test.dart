import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/nacho.dart';
import 'package:kasaterm_mobile/nacho_reply.dart';
import 'package:kasaterm_mobile/screens/nacho_home.dart';
import 'package:kasaterm_mobile/screens/nacho_reply_view.dart';
import 'package:kasaterm_mobile/screens/terminal.dart';
import 'package:kasaterm_mobile/server.dart';

import 'nacho_test.dart' show FakeNacho, card, settle;

final root = Uri.parse('http://127.0.0.1:1/u/slug/');

/// 나쵸가 학생을 띄운 첫 보고 — 앱 창구엔 상태 카드가 없어 도구 결과를 본문에 그대로 싣는다.
const startReply = '미도리에게 학생 목록 속도 조사를 맡겼어.\n'
    '\n'
    '[작업 화면 보기](http://127.0.0.1:1/u/slug/term?pane=%42)\n'
    '기기: 맥미니(nachoneko · 이 기계) — board --all: machine_label=nachoneko is_local=true\n'
    '실행: POST http://127.0.0.1:8765/spawn-student?character=미도리 → cd /Users/nachoneko/x && claude\n'
    '\n'
    '_94s · 9턴 · gpt-6 · 추론 high · 12k/350k (3%)_';

Pane pane(String id, String name, {String? machine, String status = 'working', String? slug}) => Pane(
  id: id, name: name, title: '', status: status, window: 0, cwd: '/x', slug: slug, machine: machine,
);

/// 나쵸 창구에 더해 카사텀 목록(`/machines`·`/term/panes`·`/mobile/me`)도 흉내 낸다.
class FakeKasa extends FakeNacho {
  final Map<String, List<Pane>> panesOf = {};
  List<Machine> roster = const [];
  final List<String?> panesAsked = [];

  @override
  Future<List<Pane>> panes({String? machine}) async {
    panesAsked.add(machine);
    return panesOf[machine ?? ''] ?? const [];
  }

  @override
  Future<List<Machine>> machines() async => roster;

  @override
  Future<Me> me() async => const Me(name: 'miku', owner: true, machine: '맥미니');
}

Map<String, Object?> seated(String id, Map<String, Object?> student) => {
  ...card(id, '학생 목록 속도', 'active'),
  'step': '학생 %42 이 일하는 중',
  'student': student,
};

Future<NachoDesk> open(WidgetTester tester, FakeKasa s) async {
  final d = NachoDesk(s);
  await tester.pumpWidget(MaterialApp(home: NachoHome(server: s, onChangeAddress: () async {}, desk: d)));
  await settle(tester);
  await settle(tester);
  return d;
}

Future<void> close(WidgetTester tester, NachoDesk d, FakeNacho s) async {
  d.stop();
  s.hold?.complete();
  await tester.pumpWidget(const SizedBox.shrink());
  await tester.pump(const Duration(seconds: 1));
}

void main() {
  group('답 가르기', () {
    test('학생 띄운 보고 — 기기·실행 줄은 상세로, 사용량 꼬리는 잔글씨로, 원문은 그대로', () {
      final v = splitReply(startReply, root: root);
      expect(v.body, '미도리에게 학생 목록 속도 조사를 맡겼어.\n\n[작업 화면 보기](http://127.0.0.1:1/u/slug/term?pane=%42)');
      expect(v.details, [
        '기기: 맥미니(nachoneko · 이 기계) — board --all: machine_label=nachoneko is_local=true',
        '실행: POST http://127.0.0.1:8765/spawn-student?character=미도리 → cd /Users/nachoneko/x && claude',
      ]);
      expect(v.meta!.parts, ['94s', '9턴', 'gpt-6', '추론 high', '12k/350k (3%)']);
      expect(v.meta!.brief, '94s · 9턴 · gpt-6');
      expect(v.original, startReply);
    });

    test('카드가 서는 답이면 그 학생 링크 한 줄도 상세로 — 다른 pane 링크는 본문에 남는다', () {
      expect(splitReply(startReply, root: root, seatPane: '%42').body, '미도리에게 학생 목록 속도 조사를 맡겼어.');
      expect(splitReply(startReply, root: root, seatPane: '%7').body, contains('작업 화면 보기'));
    });

    test('빠지는 글자가 없다 — 모든 줄이 본문·상세·꼬리 중 한 곳에', () {
      for (final text in [startReply, '- **기기**: 맥북\n그냥 말\n_1s · 2턴 · x_ 가운데', '실행: 테스트\n']) {
        final v = splitReply(text, root: root, seatPane: '%42');
        for (final line in text.split('\n').where((l) => l.trim().isNotEmpty)) {
          final there = v.body.contains(line.trim()) || v.details.contains(line.trim()) || v.meta?.raw == line.trim();
          expect(there, isTrue, reason: '「$line」이 사라졌다');
        }
      }
    });

    test('꼬리는 마지막 줄의 그 문법만 — 가운데 줄이나 비슷한 글은 본문에 남는다', () {
      final v = splitReply('_3s · 2턴 · x_\n본문', root: root);
      expect(v.meta, isNull);
      expect(v.body, '_3s · 2턴 · x_\n본문');
      expect(splitReply('3초 걸렸다 _hi_', root: root).meta, isNull);
    });

    test('목록·굵게로 감싼 진단 줄도 알아본다', () {
      final v = splitReply('맡겼어\n- **실행**: `POST http://x`\n> 기기: 맥북', root: root);
      expect(v.body, '맡겼어');
      expect(v.details, ['- **실행**: `POST http://x`', '> 기기: 맥북']);
    });

    test('인라인 — 링크는 라벨만, 굵게·코드는 기호 없이, 짝 없는 기호는 글자로', () {
      final parts = parseInline('보기: [작업 화면](http://h/term?pane=%4) · **중요** `cmd` 끝 ** 남음');
      expect(parts.map((p) => p.text).join(), '보기: 작업 화면 · 중요 cmd 끝 ** 남음');
      expect(parts.firstWhere((p) => p.url != null).url, 'http://h/term?pane=%4');
      expect(parts.firstWhere((p) => p.bold).text, '중요');
      expect(parts.firstWhere((p) => p.code).text, 'cmd');
    });

    test('학생 화면 링크 — 인코딩 안 된 %42 가 B 로 풀리지 않고, 기계 접두도 읽는다', () {
      expect(termLinkOf('http://127.0.0.1:1/u/slug/term?pane=%42', root)?.pane, '%42');
      expect(termLinkOf('http://127.0.0.1:1/u/slug/term?pane=%4', root)?.pane, '%4');
      expect(termLinkOf('http://127.0.0.1:1/u/slug/term/grid?pane=%2512', root)?.pane, '%12');
      final m = termLinkOf('http://127.0.0.1:1/u/slug/m/%EB%A7%A5%EB%B6%81/term?pane=%255', root);
      expect((m?.machine, m?.pane), ('맥북', '%5'));
      expect(termLinkOf('http://other/u/slug/term?pane=%4', root), isNull, reason: '남의 주소는 밖으로');
      expect(termLinkOf('http://127.0.0.1:1/u/slug/hub?pane=%4', root), isNull);
    });

    test('밖으로 여는 링크도 pane id 를 글자로 풀지 않는다', () {
      expect(Uri.parse('http://h/term?pane=%42').queryParameters['pane'], 'B', reason: '고치기 전의 함정');
      expect(externalUri('http://h/term?pane=%42')!.queryParameters['pane'], '%42');
      expect(externalUri('http://h/term?pane=%2542')!.queryParameters['pane'], '%42');
    });
  });

  group('대화 화면', () {
    testWidgets('첫 보고 밑에 학생 카드 — 이름·기기·상태, 원문 로그는 접힌 상세에', (tester) async {
      final s = FakeKasa()
        ..panesOf[''] = [pane('%42', '미도리', status: 'waiting', slug: 'midori')]
        ..add({'kind': 'message', 'id': 'a', 'text': '학생 목록 느린 거 봐줘'})
        ..add({'kind': 'reply', 'message': 'a', 'task': 'w1', 'text': startReply})
        ..add({'kind': 'status', 'message': 'a', 'state': 'answered'})
        ..cards = [seated('w1', {'surface': '%42', 'host': '미니', 'machine_id': 'mini-id'})];
      final d = await open(tester, s);
      expect(find.byType(StudentWorkCard), findsOneWidget);
      expect(find.text('미도리'), findsOneWidget);
      expect(find.text('맥미니 · 답 기다림'), findsOneWidget, reason: '기기는 이 주소의 기계, 상태는 지금 목록에서');
      expect(find.text('진행 중 · 학생 %42 이 일하는 중'), findsOneWidget, reason: '장부의 상태와 단계');
      expect(find.text('미도리에게 학생 목록 속도 조사를 맡겼어.'), findsOneWidget);
      expect(find.textContaining('POST'), findsNothing, reason: '실행 명령은 접혀 있다');
      expect(find.textContaining('board --all'), findsNothing);
      expect(find.textContaining('작업 화면 보기'), findsNothing, reason: '카드의 작업 열기와 같은 링크');
      expect(find.text('94s · 9턴 · gpt-6'), findsOneWidget);
      expect(find.textContaining('_94s'), findsNothing);
      await tester.tap(find.text('실행 명령·진단'));
      await settle(tester);
      expect(
        find.text('실행: POST http://127.0.0.1:8765/spawn-student?character=미도리 → cd /Users/nachoneko/x && claude'),
        findsOneWidget,
        reason: '옮긴 줄은 글자 그대로',
      );
      expect(find.text(startReply), findsNothing);
      await tester.tap(find.text('원문 보기'));
      await settle(tester);
      expect(find.text(startReply), findsOneWidget, reason: '원문은 상세에 그대로');
      await close(tester, d, s);
    });

    testWidgets('작업 열기 — 그 학생 화면을 앱 안에서 연다', (tester) async {
      final s = FakeKasa()
        ..panesOf[''] = [pane('%42', '미도리')]
        ..add({'kind': 'reply', 'task': 'w1', 'text': startReply})
        ..cards = [seated('w1', {'surface': '%42', 'host': '미니', 'machine_id': ''})];
      final d = await open(tester, s);
      await tester.tap(find.widgetWithText(OutlinedButton, '작업 열기'));
      await settle(tester);
      final term = tester.widget<TerminalScreen>(find.byType(TerminalScreen));
      expect((term.pane.id, term.pane.machine), ('%42', null));
      await close(tester, d, s);
    });

    testWidgets('다른 기계 학생 — 명부의 machine_id 로 그 기계에서 찾는다', (tester) async {
      final s = FakeKasa()
        ..roster = [const Machine(label: '맥북', route: '~mb', online: true, panes: [])]
        ..panesOf['~mb'] = [pane('%42', '유즈', machine: '~mb')]
        ..panesOf[''] = [pane('%42', '엉뚱한 학생')]
        ..add({'kind': 'reply', 'task': 'w1', 'text': '유즈에게 맡겼어'})
        ..cards = [seated('w1', {'surface': '%42', 'host': '맥북', 'machine_id': 'mb'})];
      final d = await open(tester, s);
      expect(find.text('유즈'), findsOneWidget);
      expect(find.text('엉뚱한 학생'), findsNothing, reason: 'pane 번호는 기계마다 따로다');
      expect(find.text('맥북 · 작업 중'), findsOneWidget);
      expect(s.panesAsked, contains('~mb'));
      await close(tester, d, s);
    });

    testWidgets('어느 기계인지 못 정하면 짐작하지 않고 열기를 막는다', (tester) async {
      final s = FakeKasa()
        ..panesOf[''] = [pane('%42', '엉뚱한 학생')]
        ..add({'kind': 'reply', 'task': 'w1', 'text': '맡겼어'})
        ..cards = [seated('w1', {'surface': '%42', 'host': '맥북', 'machine_id': 'gone'})];
      final d = await open(tester, s);
      expect(find.text('학생 %42'), findsOneWidget);
      expect(find.text('맥북 · 어느 기기인지 확인 안 됨'), findsOneWidget);
      expect(find.text('엉뚱한 학생'), findsNothing);
      final open0 = tester.widget<OutlinedButton>(find.widgetWithText(OutlinedButton, '작업 열기'));
      expect(open0.onPressed, isNull);
      await close(tester, d, s);
    });

    testWidgets('카드는 그 일의 첫 답에만 — 뒤 답은 작업 보기 칩', (tester) async {
      final s = FakeKasa()
        ..panesOf[''] = [pane('%42', '미도리')]
        ..add({'kind': 'reply', 'task': 'w1', 'text': '맡겼어'})
        ..add({'kind': 'reply', 'task': 'w1', 'text': '반쯤 됐어'})
        ..cards = [seated('w1', {'surface': '%42', 'host': '미니', 'machine_id': ''})];
      final d = await open(tester, s);
      expect(find.byType(StudentWorkCard), findsOneWidget);
      expect(find.textContaining('작업 보기 ·'), findsOneWidget);
      await close(tester, d, s);
    });

    testWidgets('본문 링크는 라벨만 보이고, 이 서버 학생 링크를 누르면 앱 안에서 연다', (tester) async {
      final s = FakeKasa()
        ..panesOf[''] = [pane('%42', '미도리')]
        ..add({'kind': 'reply', 'text': '여기 봐: [작업 화면 보기](http://127.0.0.1:1/u/slug/term?pane=%42)'});
      final d = await open(tester, s);
      expect(find.text('여기 봐: 작업 화면 보기'), findsOneWidget, reason: '주소 원문은 안 보인다');
      expect(find.textContaining('http'), findsNothing);
      await tester.tapOnText(find.textRange.ofSubstring('작업 화면 보기'));
      await settle(tester);
      final term = tester.widget<TerminalScreen>(find.byType(TerminalScreen));
      expect(term.pane.id, '%42', reason: '%42 가 B 로 풀리면 못 찾는다');
      await close(tester, d, s);
    });
  });
}
