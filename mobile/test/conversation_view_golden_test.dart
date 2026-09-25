import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/grid.dart';
import 'package:kasaterm_mobile/screens/conversation_view.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/term_session.dart';

String _raw() {
  Map<String, Object?> ev(String type, Object content, String ts) => {
    'type': type,
    'sessionId': 's1',
    'timestamp': ts,
    'message': {'role': type, 'content': content},
  };
  return [
    ev('user', '폰에서도 대화로 보게 해 줘', '2026-09-25T03:10:00Z'),
    ev('assistant', [
      {'type': 'thinking', 'thinking': '웹의 두 갈래를 폰 한 화면으로'},
      {'type': 'text', 'text': '네, **터미널**과 **대화** 두 얼굴로 나눌게요.'},
      for (final (i, f) in ['hub.dart', 'terminal.dart', 'server.dart'].indexed)
        {
          'type': 'tool_use',
          'id': 'r$i',
          'name': 'Read',
          'input': {'file_path': '/m/lib/$f'},
        },
      {
        'type': 'tool_use',
        'id': 'b1',
        'name': 'Bash',
        'input': {'command': 'flutter test', 'description': '테스트'},
      },
    ], '2026-09-25T03:10:04Z'),
    ev('user', [
      for (final id in ['r0', 'r1', 'r2', 'b1'])
        {'type': 'tool_result', 'tool_use_id': id, 'content': 'ok'},
    ], '2026-09-25T03:10:30Z'),
    ev('assistant', [
      {'type': 'text', 'text': '다 됐어요.\n\n- 앱바 밑 `터미널 | 대화`\n- 말풍선은 꾹 눌러 복사'},
    ], '2026-09-25T03:11:00Z'),
  ].map(jsonEncode).join('\n');
}

void main() {
  testWidgets('대화 보기', (tester) async {
    tester.view.physicalSize = const Size(390 * 3, 844 * 3);
    tester.view.devicePixelRatio = 3;
    addTearDown(tester.view.reset);
    final server = Server(
      Uri.parse('http://127.0.0.1:1/'),
      client: MockClient((req) async {
        if (!req.url.path.endsWith('transcript-raw')) {
          return http.Response('', 404);
        }
        return http.Response(
          jsonEncode({'ok': true, 'raw': _raw(), 'offset': 10, 'reset': true}),
          200,
          headers: {'content-type': 'application/json'},
        );
      }),
    );
    const pane = Pane(
      id: '%1',
      name: '아리스',
      title: '',
      status: 'working',
      window: 0,
      cwd: '/m',
      doing: 'Bash flutter test',
    );
    final session = TermSession(server, pane)
      ..state = TermState.connected
      ..grid.lines = [
        for (final l in [
          ' Bash command',
          '   flutter test',
          ' Do you want to proceed?',
          ' ❯ 1. Yes',
          '   2. Yes, and don\'t ask again for flutter test commands',
          '   3. No, and tell Claude what to do differently (esc)',
        ])
          [Run(l, const DefaultColor(), const DefaultColor(), 0)],
      ];
    await tester.pumpWidget(
      MaterialApp(
        theme: ThemeData(colorSchemeSeed: const Color(0xff4a90e2)),
        home: Scaffold(
          appBar: AppBar(
            title: const Text('아리스'),
            bottom: const PaneViewSwitch(),
          ),
          body: ConversationView(
            server: server,
            pane: pane,
            session: session,
            accent: const Color(0xff7c9cff),
            onTerminal: () {},
          ),
        ),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));
    expect(find.textContaining('다 됐어요', findRichText: true), findsOneWidget);
    // 도구 넷은 한 묶음으로 접히고 끝의 둘만 보인다.
    expect(find.textContaining('도구 4개'), findsOneWidget);
    expect(find.text('Bash flutter test'), findsOneWidget);
    // 화면의 승인 메뉴가 입력줄 위 단추로 선다.
    expect(find.text('Do you want to proceed?'), findsOneWidget);
    expect(find.text('1. Yes'), findsOneWidget);
    await expectLater(
      find.byType(Scaffold),
      matchesGoldenFile('goldens/conversation_view.png'),
    );
    await tester.pumpWidget(const SizedBox());
    session.dispose();
  });
}
