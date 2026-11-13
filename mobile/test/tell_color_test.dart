import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/conversation.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/screens/controls.dart';
import 'package:kasaterm_mobile/screens/conversation_view.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/term_session.dart';

import 'inline_result_fonts.dart';

void main() {
  setUpAll(loadInlineResultFonts);
  for (final brightness in Brightness.values) {
    testWidgets('다른 학생이 넣은 말풍선은 그 학생 색 (${brightness.name})', (tester) async {
      tester.view.physicalSize = const Size(390, 844);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.reset);
      final server = Server(Uri.parse('https://tell-fixture.invalid/'),
        client: MockClient((_) async => http.Response('{}', 404)));
      const pane = Pane(id: '%1', name: '유즈', title: '쪽지', status: 'idle', window: 0, cwd: '');
      final session = TermSession(server, pane);
      final raw = [
        '⟦호시노⟧ 맥북 재시작은 유즈가 맡아 줄래요?',
        '⟦카사텀⟧ 색이 없는 보낸 쪽',
        '[완료] 시즈코(%13) — 끝냈어요.',
      ].map((text) => jsonEncode({'type': 'user', 'message': {'content': text}})).join('\n');
      final conversation = Conversation(archived: true)..apply(raw, reset: true, next: 100);
      await tester.pumpWidget(MaterialApp(
        debugShowCheckedModeBanner: false,
        theme: buildTheme(brightness),
        home: Scaffold(body: ConversationView(
          server: server, pane: pane, session: session, accent: Colors.blue, onTerminal: () {},
          senderAccents: const {'호시노': 0xfff2a0c0, '시즈코': 0xff4c6ef5},
          readOnly: true, conversation: conversation,
        )),
      ));
      await tester.pump();
      final context = tester.element(find.byType(ConversationView));
      final fills = tester.widgetList<SpeechBubble>(find.byType(SpeechBubble)).map((b) => b.fill).toList();
      // 대화 목록은 아래(최근)부터 쌓인다.
      expect(fills.reversed.toList(), [
        senderFill(context, const Color(0xfff2a0c0)),
        null,
        senderFill(context, const Color(0xff4c6ef5)),
      ]);
      final plain = SpeechBubble.otherFill(context);
      final tinted = senderFill(context, const Color(0xfff2a0c0));
      expect(tinted, isNot(plain));
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox());
      session.dispose();
      server.close();
    });
  }
}
