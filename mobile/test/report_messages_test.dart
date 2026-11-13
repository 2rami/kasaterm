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
  testWidgets('reports remain incoming and each changed status has its own heading', (tester) async {
    tester.view.physicalSize = const Size(390, 844);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    final server = Server(Uri.parse('https://report-fixture.invalid/'),
      client: MockClient((_) async => http.Response('{}', 404)));
    const pane = Pane(id:'%1', name:'호시노', title:'보고 확인', status:'idle', window:0, cwd:'');
    final session = TermSession(server, pane);
    const marks = ['완료', '실패', '막힘', '재시작 필요', '승인 필요'];
    final raw = [
      '보고를 확인해 줘.',
      for (final mark in marks) '[$mark] 시즈코(%13) — $mark 상태의 작업 보고입니다.',
    ].map((text) => jsonEncode({'type':'user','message':{'content':text}})).join('\n');
    final conversation = Conversation(archived:true)..apply(raw,reset:true,next:100);
    await tester.pumpWidget(MaterialApp(
      debugShowCheckedModeBanner:false,
      theme:buildTheme(Brightness.light),
      home:Scaffold(appBar:AppBar(title:const Text('보고 확인')),body:ConversationView(
        server:server,pane:pane,session:session,accent:Colors.blue,onTerminal:(){},
        readOnly:true,conversation:conversation,
      )),
    ));
    await tester.pump();
    final bubbles = tester.widgetList<SpeechBubble>(find.byType(SpeechBubble)).toList();
    expect(bubbles.where((b) => b.mine),hasLength(1));
    expect(bubbles.where((b) => !b.mine),hasLength(5));
    for (final mark in marks) {
      expect(find.text('시즈코 · $mark 보고',findRichText:true),findsOneWidget);
      final body = tester.getRect(find.text('$mark 상태의 작업 보고입니다.',findRichText:true));
      expect(body.left,lessThan(195));
    }
    expect(tester.takeException(),isNull);
    await expectLater(find.byType(MaterialApp),matchesGoldenFile('goldens/report_senders_390.png'));
    await tester.pumpWidget(const SizedBox());
    session.dispose();server.close();
  });
}
