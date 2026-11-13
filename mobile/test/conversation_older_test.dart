
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/archive_repository.dart';
import 'package:kasaterm_mobile/conversation_cache.dart';
import 'package:kasaterm_mobile/screens/conversation_view.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/term_session.dart';

import 'archive_fixtures.dart';

/// 첫 답은 꼬리(512KB 넘는 파일의 끝)만, 저장본 창구는 처음부터 준다 — 원본 기기와 같은 약속.
class LongSource extends Server {
  LongSource() : super(Uri.parse('https://source.invalid/'), client: MockClient((_) async => http.Response('{}', 404)));

  final head = message('user', 'very first question');
  final tail = message('assistant', 'latest answer');
  int get end => 600 * 1024;
  bool archiveFails = false;

  @override
  Future<({String raw, int offset, bool reset, String source})?> transcriptRaw(
    String pane,
    int offset, {
    String? machine,
    int? waitMs,
    String source = '',
  }) async =>
      (raw: offset == 0 ? tail : '', offset: end, reset: offset == 0, source: 'one.jsonl');

  @override
  Future<Map<String, dynamic>> conversationArchive(Pane pane, int offset, String held) async {
    if (archiveFails) throw const ServerException('기기 앱이 옛 판이에요');
    // 두 조각으로 나눠 준다 — 화면은 지금 자리까지 이어 받아야 한다.
    final first = offset == 0;
    return {
      'ok': true, 'archive_version': 1, 'source': 'one.jsonl', 'reset': first,
      'offset': first ? end ~/ 2 : end, 'raw': first ? head : tail,
      'address': {'machine_id': 'machine-a', 'surface_key': 'key', 'surface_id': '%1'},
    };
  }
}

const pane = Pane(id: '%1', name: 'Assistant', title: 'conversation', status: 'idle', window: 0,
    cwd: '/fixture', surfaceKey: 'key', harness: 'claude');

Future<(LongSource, TermSession, ConversationCache)> _open(WidgetTester tester) async {
  final server = LongSource();
  final cache = ConversationCache(server, repository: ArchiveRepository(server, store: MemoryArchiveStore()));
  ConversationCache.bind(server, cache);
  final session = TermSession(server, pane);
  await tester.pumpWidget(MaterialApp(home: Scaffold(body: ConversationView(
    server: server, pane: pane, session: session, accent: Colors.blue, onTerminal: () {},
  ))));
  for (var i = 0; i < 10; i++) { await tester.pump(const Duration(milliseconds: 10)); }
  return (server, session, cache);
}

Future<void> _close(WidgetTester tester, LongSource server, TermSession session, ConversationCache cache) async {
  await tester.pumpWidget(const SizedBox());
  session.dispose();
  cache.dispose();
  server.close();
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  testWidgets('a tail longer than the first window offers the earlier conversation and loads it in order', (tester) async {
    final (server, session, cache) = await _open(tester);
    expect(find.textContaining('latest answer', findRichText: true), findsOneWidget);
    expect(find.textContaining('very first question', findRichText: true), findsNothing);

    await tester.tap(find.text('앞 대화 보기'));
    for (var i = 0; i < 10; i++) { await tester.pump(const Duration(milliseconds: 10)); }

    expect(find.textContaining('very first question', findRichText: true), findsOneWidget);
    expect(find.textContaining('latest answer', findRichText: true), findsOneWidget);
    expect(find.text('앞 대화 보기'), findsNothing);
    final first = tester.getTopLeft(find.textContaining('very first question', findRichText: true)).dy;
    final last = tester.getTopLeft(find.textContaining('latest answer', findRichText: true)).dy;
    expect(first, lessThan(last));
    await _close(tester, server, session, cache);
  });

  testWidgets('a failed earlier load shows the reason and can retry', (tester) async {
    final (server, session, cache) = await _open(tester);
    server.archiveFails = true;
    await tester.tap(find.text('앞 대화 보기'));
    for (var i = 0; i < 5; i++) { await tester.pump(const Duration(milliseconds: 10)); }
    expect(find.text('기기 앱이 옛 판이에요'), findsOneWidget);

    server.archiveFails = false;
    await tester.tap(find.text('다시 받기'));
    for (var i = 0; i < 10; i++) { await tester.pump(const Duration(milliseconds: 10)); }
    expect(find.textContaining('very first question', findRichText: true), findsOneWidget);
    await _close(tester, server, session, cache);
  });
}
