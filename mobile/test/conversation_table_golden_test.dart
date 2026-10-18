import 'dart:convert';

import 'inline_result_fonts.dart';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/screens/conversation_view.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/term_session.dart';

const _answer = '''세 가지를 비교했어요.

| 방식 | 장점 | 단점 | 상태 |
|---|---|---|---|
| 칸 하단바 | 경로·브랜치를 늘 보여 준다 | 이 기기 화면에서만 보인다 | 걷음 |
| 파일 칸 TUI | 거울·폰에서도 같은 화면 | 칸 하나를 더 쓴다 | 적용 |
| GUI 파일트리 | 마우스로 끌어 옮기기 | 다른 기기에선 안 보인다 | 유지 |

짧은 표도 봐요.

| 키 | 하는 일 |
|---|---|
| `j` `k` | 위아래 |
| Enter | 폴더면 셸 이동 |''';

String _raw() {
  Map<String, Object?> ev(String type, Object content, String ts) => {
    'type': type,
    'sessionId': 's1',
    'timestamp': ts,
    'message': {'role': type, 'content': content},
  };
  return [
    ev('user', '표로 정리해 줘', '2026-10-19T03:10:00Z'),
    ev('assistant', [
      {'type': 'text', 'text': _answer},
    ], '2026-10-19T03:10:30Z'),
  ].map(jsonEncode).join('\n');
}

void main() {
  setUpAll(() async {
    await loadInlineResultFonts();
    // 표 칸 속 인라인 코드가 네모로 찍히지 않게.
    await (FontLoader('TermMono')
          ..addFont(rootBundle.load('assets/fonts/JetBrainsMonoNerdFontMono-Regular.ttf')))
        .load();
  });
  for (final dark in [false, true]) {
    testWidgets('대화 보기 표 (${dark ? '어두움' : '밝음'})', (tester) async {
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
        name: '시로코',
        title: '',
        status: 'idle',
        window: 0,
        cwd: '/m',
      );
      final session = TermSession(server, pane)..state = TermState.connected;
      await tester.pumpWidget(
        MaterialApp(
          theme: buildTheme(dark ? Brightness.dark : Brightness.light),
          home: Scaffold(
            appBar: AppBar(title: const Text('시로코')),
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
      expect(find.textContaining('세 가지를 비교', findRichText: true), findsOneWidget);
      await expectLater(
        find.byType(Scaffold),
        matchesGoldenFile('goldens/conversation_table_${dark ? 'dark' : 'light'}_390.png'),
      );
      // 짧은 표는 글 길이만큼만 서고, 넓은 표는 끝 칸이 화면 밖이라 표만 옆으로 밀어 본다.
      final last = find.textContaining('유지', findRichText: true);
      expect(tester.getRect(last).left, greaterThan(390));
      expect(
        tester.getRect(find.textContaining('폴더면 셸 이동', findRichText: true)).right,
        lessThan(390 * 0.8),
      );
      await tester.drag(find.textContaining('경로·브랜치', findRichText: true), const Offset(-300, 0));
      await tester.pumpAndSettle();
      expect(tester.getRect(last).right, lessThan(390));
      await tester.pumpWidget(const SizedBox());
      session.dispose();
    });
  }
}
