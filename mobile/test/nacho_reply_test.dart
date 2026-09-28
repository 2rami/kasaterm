import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/nacho.dart';
import 'package:kasaterm_mobile/nacho_reply.dart';
import 'package:kasaterm_mobile/screens/nacho_home.dart';
import 'package:kasaterm_mobile/screens/nacho_reply_view.dart';
import 'package:kasaterm_mobile/screens/nacho_task.dart';
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


/// 화면에 그려진 글 전부 — 기호가 먹혔거나 글자가 빠졌으면 여기서 드러난다.
String shown(WidgetTester tester, [Finder? within]) => tester
    .widgetList<RichText>(
      within == null ? find.byType(RichText) : find.descendant(of: within, matching: find.byType(RichText)),
    )
    .map((r) => r.text.toPlainText())
    .join('\n');

TextStyle? styleOf(WidgetTester tester, String piece) {
  TextStyle? found;
  for (final r in tester.widgetList<RichText>(find.byType(RichText))) {
    r.text.visitChildren((span) {
      if (span is TextSpan && span.text == piece) found = span.style;
      return found == null;
    });
    if (found != null) break;
  }
  return found;
}

Future<void> reply(WidgetTester tester, String text, {ValueChanged<String>? onLink, double width = 360}) =>
    tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: ListView(
            children: [
              Align(
                alignment: Alignment.topLeft,
                child: SizedBox(
                  width: width,
                  child: ReplyText(text: text, onLink: onLink ?? (_) {}, style: const TextStyle(fontSize: 15)),
                ),
              ),
            ],
          ),
        ),
      ),
    );

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

    test('줄머리 >_< 는 얼굴로 두고 — 인용과 코드 울타리 안은 그대로', () {
      expect(guardMarkdown('>_< 미안'), r'\>_< 미안');
      expect(guardMarkdown('  >>_<< 헉'), r'  \>>_<< 헉');
      expect(guardMarkdown('> 인용\n>> 겹 인용\n>'), '> 인용\n>> 겹 인용\n>');
      expect(guardMarkdown('```\n>_<\n```\n>_<'), '```\n>_<\n```\n' r'\>_<');
      expect(guardMarkdown('~~~~\n>_<\n~~~\n>_<'), '~~~~\n>_<\n~~~\n>_<', reason: '짧은 울타리로는 안 닫힌다');
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


  group('마크다운', () {
    testWidgets('목록·제목·인용·코드 울타리·표 — 기호 대신 모양으로', (tester) async {
      await reply(
        tester,
        '## 오늘 한 일\n'
        '- 첫째\n'
        '- 둘째\n'
        '\n'
        '1. 하나\n'
        '2. 둘\n'
        '\n'
        '> 인용 한 줄\n'
        '\n'
        '```\n'
        'code **x** >_<\n'
        '```\n'
        '\n'
        '| 이름 | 상태 |\n'
        '|---|---|\n'
        '| 미도리 | 끝 |',
      );
      expect(find.text('•'), findsNWidgets(2));
      expect(find.text('1.'), findsOneWidget);
      expect(find.text('2.'), findsOneWidget);
      expect(find.text('첫째'), findsOneWidget);
      expect(find.text('하나'), findsOneWidget);
      final head = tester.widget<Text>(find.text('오늘 한 일'));
      expect((head.textSpan!.style!.fontSize, head.textSpan!.style!.fontWeight), (17, FontWeight.w700));
      expect(find.text('인용 한 줄'), findsOneWidget);
      expect(find.text('code **x** >_<'), findsOneWidget, reason: '코드 안은 글자 그대로');
      expect(find.byType(Table), findsOneWidget);
      expect(find.text('미도리'), findsOneWidget);
      final all = shown(tester);
      for (final mark in ['##', '- ', '> ', '```', '|']) {
        expect(all.contains(mark), isFalse, reason: '「$mark」가 글자로 남았다');
      }
    });

    testWidgets('인라인 — 링크는 라벨만, 굵게·코드는 기호 없이, 짝 없는 기호는 글자로', (tester) async {
      final got = <String>[];
      await reply(tester, '보기: [작업 화면](http://h/term?pane=%4) · **중요** `cmd` 끝 ** 남음', onLink: got.add);
      expect(shown(tester), '보기: 작업 화면 · 중요 cmd 끝 ** 남음');
      expect(styleOf(tester, '중요')?.fontWeight, FontWeight.w700);
      expect(styleOf(tester, 'cmd')?.fontFamily, 'TermMono');
      await tester.tapOnText(find.textRange.ofSubstring('작업 화면'));
      expect(got, hasLength(1));
      expect(termLinkOf(got.single, Uri.parse('http://h/'))?.pane, '%4');
    });

    testWidgets('한글이 바로 붙은 굵게도 닫힌다 — CommonMark 는 여기서 별표를 남긴다', (tester) async {
      await reply(tester, '**(선택)**은 나중에, **「작업 열기」**를 눌러');
      expect(shown(tester), '(선택)은 나중에, 「작업 열기」를 눌러');
      expect(styleOf(tester, '(선택)')?.fontWeight, FontWeight.w700);
    });

    testWidgets('카오모지·물결·밑줄 — 빠지거나 기울어지는 글자가 없다', (tester) async {
      for (final face in [
        r'¯\_(ツ)_/¯',
        '좋아 (*^▽^*) 그럼 (*^▽^*)',
        '(^_^) 그리고 (^_^)',
        '3~5개 그리고 7~9개, 좋아~~ 해볼게~~',
        '>_< 미안',
        '(｡•̀ᴗ-)✧ ฅ^•ﻌ•^ฅ (ㅠ_ㅠ) -_-',
        'snake_case 와 __init__ 과 *별*',
        '<pane-id> 는 태그가 아니다',
      ]) {
        await reply(tester, face);
        expect(shown(tester), face, reason: face);
      }
    });

    testWidgets('긴 줄 — 좁은 폭에서 넘치지 않고, 코드는 옆으로 민다', (tester) async {
      final word = List.filled(40, '가나다라마바사아자차').join();
      await reply(tester, '$word\n\nhttp://example.com/${'a' * 300}\n\n```\n${'x' * 400}\n```', width: 240);
      expect(tester.takeException(), isNull);
      expect(shown(tester), contains(word));
      expect(
        find.byWidgetPredicate((w) => w is SingleChildScrollView && w.scrollDirection == Axis.horizontal),
        findsOneWidget,
        reason: '코드 줄은 가로 스크롤',
      );
      for (final text in [find.text(word), find.textContaining('example.com')]) {
        expect(tester.renderObject<RenderParagraph>(find.descendant(of: text, matching: find.byType(RichText))).size.width,
            lessThanOrEqualTo(240));
      }
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

    testWidgets('답의 목록·굵게는 모양으로, 본문은 길게 눌러 고를 수 있다', (tester) async {
      final s = FakeKasa()..add({'kind': 'reply', 'text': '끝낸 것:\n- **목록 속도** 고침\n- 알림 정리'});
      final d = await open(tester, s);
      expect(find.text('•'), findsNWidgets(2));
      expect(find.text('목록 속도 고침'), findsOneWidget);
      expect(styleOf(tester, '목록 속도')?.fontWeight, FontWeight.w700);
      expect(find.textContaining('**'), findsNothing);
      expect(find.ancestor(of: find.text('알림 정리'), matching: find.byType(SelectionArea)), findsOneWidget);
      await close(tester, d, s);
    });

    testWidgets('알림 줄 — 링크·굵게가 글자 그대로 새지 않고, 링크는 앱 안에서 연다', (tester) async {
      final s = FakeKasa()
        ..panesOf[''] = [pane('%42', '미도리')]
        ..add({
          'kind': 'notice',
          'notice': 'watch',
          'text': '#알림 요약: [작업 화면](http://127.0.0.1:1/u/slug/term?pane=%42) 에 **새 글** 둘',
        });
      final d = await open(tester, s);
      expect(find.text('#알림 요약: 작업 화면 에 새 글 둘'), findsOneWidget);
      expect(find.textContaining('http'), findsNothing);
      expect(styleOf(tester, '새 글')?.fontWeight, FontWeight.w700);
      expect(
        find.ancestor(of: find.text('#알림 요약: 작업 화면 에 새 글 둘'), matching: find.byType(SelectionArea)),
        findsNothing,
        reason: '줄 전체 누름(작업 상세)을 선택 영역이 가져가지 않게',
      );
      await tester.tapOnText(find.textRange.ofSubstring('작업 화면'));
      await settle(tester);
      expect(tester.widget<TerminalScreen>(find.byType(TerminalScreen)).pane.id, '%42');
      await close(tester, d, s);
    });

    testWidgets('작업이 달린 알림 줄은 링크 밖을 누르면 작업 상세로', (tester) async {
      final s = FakeKasa()
        ..add({'kind': 'notice', 'notice': 'watch', 'task': 'w1', 'text': '**학생**이 끝났대'})
        ..cards = [card('w1', '학생 목록 속도', 'done')];
      final d = await open(tester, s);
      expect(find.text('학생이 끝났대'), findsOneWidget);
      await tester.tap(find.text('학생이 끝났대'));
      await settle(tester);
      expect(find.byType(NachoTaskScreen), findsOneWidget);
      await close(tester, d, s);
    });

    testWidgets('작업 상세의 알림도 — 링크는 라벨, 굵게·목록은 모양, 내 말은 적은 그대로', (tester) async {
      final s = FakeKasa()
        ..details['w1'] = {
          ...card('w1', '학생 목록 속도', 'active'),
          'request': '학생 목록 속도',
          'verify': null, 'report': null, 'approval': null, 'history': [], 'hops': [],
          'preview': {'url': null, 'shot': false}, 'can_direct': true, 'remaining': [],
          'events': [
            {'seq': 1, 'kind': 'message', 'id': 'a', 'at_ms': 1, 'text': '**그대로** 보여줘'},
            {'seq': 2, 'kind': 'notice', 'notice': 'watch', 'at_ms': 1,
              'text': '[배포 스레드](https://example.com/x) 에 **새 글**\n- 하나\n- 둘'},
          ],
        };
      final d = NachoDesk(s);
      await tester.pumpWidget(MaterialApp(home: NachoTaskScreen(desk: d, taskId: 'w1', onOpenStudents: () {})));
      await settle(tester);
      expect(find.text('배포 스레드 에 새 글'), findsOneWidget);
      expect(styleOf(tester, '새 글')?.fontWeight, FontWeight.w700);
      expect(find.text('•'), findsNWidgets(2));
      expect(find.textContaining('example.com'), findsNothing);
      expect(find.textContaining('**그대로** 보여줘'), findsOneWidget, reason: '사람이 친 말은 해석하지 않는다');
      d.stop();
      s.hold?.complete();
      await tester.pumpWidget(const SizedBox.shrink());
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
