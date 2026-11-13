import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/grid.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/screens/terminal.dart';
import 'package:kasaterm_mobile/term_session.dart';

import 'codex_mobile_test.dart' show fixtureServer, viewport;
import 'fixtures/claude_prompt_menus.dart';
import 'fixtures/codex_mobile.dart';
import 'mobile_terminal_qa_test.dart' show loadFonts;

/// 실화면 줄을 그대로 격자에 싣는 가짜 칸 — 보낸 키를 모은다. [lit] 줄의 그 글은 배경색으로 칠한다(질문의 지금 탭).
class MenuScreenSession extends TermSession {
  MenuScreenSession(List<String> rows, {String? litRow, String? lit})
      : super(fixtureServer(CodexScene.approval), codexPane(CodexScene.approval)) {
    state = TermState.connected;
    setRows(rows, litRow: litRow, lit: lit);
  }
  final sent = <String>[];
  @override
  void connect() {}
  @override
  bool get canSend => state == TermState.connected;
  @override
  void sendText(String text) {
    if (canSend) sent.add(text);
  }

  void setRows(List<String> rows, {String? litRow, String? lit}) {
    var cols = 0;
    for (final r in rows) {
      final w = r.runes.fold<int>(0, (n, c) => n + cellWidth(c));
      if (w > cols) cols = w;
    }
    List<Object?> run(String text, {bool on = false}) => [text, null, on ? [177, 185, 249] : null, 0];
    grid.apply({
      'cols': cols,
      'rows': rows.length,
      'dirty': [
        for (final (i, row) in rows.indexed)
          if (litRow != null && lit != null && row.contains(litRow) && row.contains(lit))
            [
              i,
              [
                run(row.substring(0, row.indexOf(lit))),
                run(lit, on: true),
                run(row.substring(row.indexOf(lit) + lit.length)),
              ],
            ]
          else
            [i, [run(row)]],
      ],
      'cursor': [rows.length - 1, 0],
      'cursorVisible': false,
    });
    notifyListeners();
  }
}

Future<MenuScreenSession> open(WidgetTester tester, List<String> rows, {String? litRow, String? lit}) async {
  viewport(tester, 390);
  final session = MenuScreenSession(rows, litRow: litRow, lit: lit);
  await tester.pumpWidget(MaterialApp(
    debugShowCheckedModeBanner: false,
    theme: buildTheme(Brightness.light),
    home: TickerMode(
      enabled: false,
      child: TerminalScreen(server: session.server, pane: session.pane, session: session),
    ),
  ));
  await tester.pump(const Duration(milliseconds: 200));
  expect(tester.takeException(), isNull);
  return session;
}

Future<void> close(WidgetTester tester, MenuScreenSession session) async {
  await tester.pumpWidget(const SizedBox());
  session.server.close();
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(loadFonts);

  testWidgets('「Type something.」은 글 칸 — 빈 채로는 안 보내고, 쓴 글은 옮기고 치고 Enter', (tester) async {
    final session = await open(tester, claudeQuestionSingle);
    final field = find.widgetWithText(TextField, '4. Type something.');
    expect(field, findsOneWidget);
    expect(find.widgetWithText(OutlinedButton, '4. Type something.'), findsNothing);
    await tester.ensureVisible(field);
    await tester.tap(find.byTooltip('이 글로 답하기'));
    await tester.pump();
    expect(session.sent, isEmpty, reason: '빈 Enter 는 질문 전체 거절이 된다');
    await tester.enterText(field, '보라');
    await tester.testTextInput.receiveAction(TextInputAction.send);
    await tester.pump();
    expect(session.sent, ['\x1b[B', '\x1b[B', '\x1b[B']);
    await tester.pump(TermSession.enterGap);
    expect(session.sent.skip(3), ['보라']);
    await tester.pump(TermSession.enterGap);
    expect(session.sent.skip(4), ['\r']);
    await close(tester, session);
  });

  testWidgets('여러 개 고르기 — 상자 켜기는 카드가 남고, 화면이 바뀌기 전 두 번째 누름은 막는다', (tester) async {
    final session = await open(tester, claudeQuestionMulti);
    expect(find.byIcon(Icons.check_box_outline_blank), findsNWidgets(4));
    final second = find.widgetWithText(OutlinedButton, '2. 자동 저장');
    await tester.ensureVisible(second);
    await tester.tap(second);
    await tester.pump(TermSession.enterGap);
    expect(session.sent, ['\x1b[B', '\r']);
    expect(second, findsOneWidget, reason: '상자를 켠 뒤에도 창은 남는다');
    await tester.tap(second);
    await tester.pump(TermSession.enterGap);
    expect(session.sent, ['\x1b[B', '\r'], reason: '옛 커서 자리로 셈한 키를 또 보내면 엉뚱한 칸이 켜진다');
    // 번호 없는 Submit 도 단추다.
    session.setRows(claudeQuestionMultiOnSubmit);
    await tester.pump();
    final submit = find.widgetWithText(OutlinedButton, 'Submit');
    expect(submit, findsOneWidget);
    expect(find.byIcon(Icons.check_box), findsOneWidget);
    await close(tester, session);
  });

  testWidgets('답을 고른 뒤 다음 질문이 서면 접기를 기다리지 않고 곧바로 보인다', (tester) async {
    final session = await open(tester, claudeQuestionTabsFirst, litRow: '✔ Submit  →', lit: ' ☐ 언어 ');
    final first = find.widgetWithText(OutlinedButton, '1. 러스트');
    await tester.ensureVisible(first);
    await tester.tap(first);
    await tester.pump();
    expect(first, findsNothing, reason: '고른 창은 화면이 바뀔 때까지 접는다');
    session.setRows(claudeQuestionTabsSecond, litRow: '✔ Submit  →', lit: ' ☐ 테스트 ');
    await tester.pump();
    expect(find.widgetWithText(OutlinedButton, '1. 단위'), findsOneWidget);
    await close(tester, session);
  });

  testWidgets('화면이 멈춰 있어도 접기가 끝나면 같은 창을 다시 그린다', (tester) async {
    final session = await open(tester, claudeQuestionTabsFirst, litRow: '✔ Submit  →', lit: ' ☐ 언어 ');
    final first = find.widgetWithText(OutlinedButton, '1. 러스트');
    await tester.ensureVisible(first);
    await tester.tap(first);
    await tester.pump();
    expect(first, findsNothing);
    await tester.pump(const Duration(milliseconds: 1300));
    expect(first, findsOneWidget, reason: '답을 기다리는 TUI 는 화면을 다시 보내지 않는다');
    await close(tester, session);
  });

  testWidgets('질문 여러 개 — 탭을 누르면 Tab 으로 옮긴다', (tester) async {
    final session = await open(tester, claudeQuestionTabsFirst, litRow: '✔ Submit  →', lit: ' ☐ 언어 ');
    final chip = find.text('테스트');
    expect(chip, findsOneWidget);
    expect(tester.getSize(find.ancestor(of: chip, matching: find.byType(InkWell)).first).height,
        greaterThanOrEqualTo(44));
    await tester.tap(chip);
    await tester.pump();
    expect(session.sent, ['\t']);
    await close(tester, session);
  });

  testWidgets('글을 친 칸에 커서가 있으면 한 칸 위로 뺀 뒤 Tab', (tester) async {
    final session = await open(tester, claudeQuestionMultiOtherTyped, litRow: '✔ Submit  →', lit: ' ☒ 과일 ');
    await tester.tap(find.text('Submit').first);
    await tester.pump();
    expect(session.sent, ['\x1b[A', '\t']);
    await close(tester, session);
  });

  testWidgets('화면보다 긴 질문 — 숨은 선택지 쪽으로 커서만 옮긴다', (tester) async {
    final session = await open(tester, claudeQuestionScrolledTop);
    final more = find.text('아래 선택지 더 보기');
    await tester.ensureVisible(more);
    await tester.tap(more);
    await tester.pump(TermSession.enterGap * 2);
    expect(session.sent, ['\x1b[B', '\x1b[B', '\x1b[B', '\x1b[B']);
    await close(tester, session);
  });

  testWidgets('계획 승인 — 고칠 말은 글 칸, 계획은 접었다 편다', (tester) async {
    final session = await open(tester, claudePlanApproval);
    expect(find.widgetWithText(TextField, '3. Tell Claude what to change'), findsOneWidget);
    final more = find.text('2줄 더 보기');
    expect(more, findsOneWidget);
    await tester.ensureVisible(more);
    await tester.tap(more);
    await tester.pump();
    expect(find.text('3. 파일이 생성되었고 내용이 hi인지 확인한다.'), findsOneWidget);
    expect(session.sent, isEmpty);
    await close(tester, session);
  });
}
