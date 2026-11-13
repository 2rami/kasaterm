import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/hub_model.dart';
import 'package:kasaterm_mobile/main.dart' show buildTheme;
import 'package:kasaterm_mobile/screens/conversation_view.dart';
import 'package:kasaterm_mobile/screens/notes_sheet.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/term_session.dart';

import 'inline_result_fonts.dart';

Note _note(int id, {String pane = '%3', String kind = 'done', bool read = false, String? machine, String summary = '한 줄'}) => Note(
  id: id,
  pane: pane,
  character: '유우카',
  kind: kind,
  summary: summary,
  // 「N분 전」이 시험 시각에 따라 안 바뀌게 지금에 묶는다.
  when: DateTime.now().subtract(Duration(minutes: 10 - id)),
  read: read,
  machine: machine,
);

Pane _pane(String id, {String status = 'idle', String? kind, String harness = 'claude', String name = '유우카', String? machine}) => Pane(
  id: id,
  name: name,
  title: '',
  status: status,
  window: 0,
  cwd: '/',
  harness: harness,
  kind: kind,
  machine: machine,
);

/// 보낸 요청을 적고, `chat-send` 를 [failChat] 이면 거절하는 서버. mod-live 는 [live] 를 준다.
class _Rig {
  _Rig({this.failChat = false, this.live});
  final bool failChat;
  final Map<String, Object?>? live;
  final requests = <(String, Map<String, Object?>)>[];

  late final server = Server(
    Uri.parse('http://127.0.0.1:1/'),
    client: MockClient((req) async {
      final path = req.url.path;
      final body = req.body.isEmpty ? <String, Object?>{} : (jsonDecode(req.body) as Map).cast<String, Object?>();
      requests.add((path, body));
      Response json(Object? j) => http.Response(jsonEncode(j), 200, headers: {'content-type': 'application/json'});
      if (path.endsWith('term/chat-send')) {
        return failChat ? json({'ok': false, 'error': '입력칸에 쓰던 글이 있어요'}) : json({'ok': true});
      }
      if (path.endsWith('term/mod-live') && live != null) {
        // 바뀔 때까지 쥐는 다음 물음은 답하지 않는다 — 바로 답하면 매달리기가 쉬지 않고 돈다.
        if (req.url.queryParameters.containsKey('seq')) return Completer<http.Response>().future;
        return json(live);
      }
      if (path.endsWith('term/mod-decide')) return json({'ok': true});
      if (path.endsWith('term/notes/read')) return json({'ok': true});
      if (path.endsWith('/cmd')) return json({'ok': true});
      return http.Response('', 404);
    }),
  );
}

typedef Response = http.Response;

HubModel _model(Server server, List<Note> notes, List<Pane> panes, {bool online = true}) => HubModel(server)
  ..sections = [
    HubSection(machine: null, online: online, rooms: [HubRoom(title: '1', panes: panes)]),
  ]
  ..notes = notes;

Future<void> _show(WidgetTester tester, HubModel model, Server server, {Size size = const Size(390, 640)}) async {
  tester.view.physicalSize = size;
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  // 실제처럼 아래 시트로 — 자판 자리는 시트가 비켜야 한다(화면 Scaffold 는 시트를 밀어 올리지 않는다).
  await tester.pumpWidget(
    MaterialApp(
      theme: buildTheme(Brightness.light),
      home: Builder(
        builder: (context) => Scaffold(
          body: Center(
            child: TextButton(
              onPressed: () => showModalBottomSheet<void>(
                context: context,
                isScrollControlled: true,
                showDragHandle: true,
                builder: (_) => NotesSheet(model: model, server: server, onOpen: (_) {}, openSession: TermSession.new),
              ),
              child: const Text('종'),
            ),
          ),
        ),
      ),
    ),
  );
  await tester.tap(find.text('종'));
  for (var i = 0; i < 20; i++) {
    await tester.pump(const Duration(milliseconds: 30));
  }
}

void main() {
  setUpAll(loadInlineResultFonts);
  group('종 목록 줄', () {
    test('안 읽은 것과 이번에 답한 것만 선다', () {
      final lines = HubModel.noteLines(
        [_note(3), _note(2, read: true), _note(1, read: true, pane: '%9')],
        {'|1': NoteReply(text: '그거 말고', at: DateTime(2026), after: 3)},
      );
      expect([for (final l in lines) l.note.id], [3, 1]);
      expect(lines.last.sent?.text, '그거 말고');
      expect(lines.first.sent, isNull);
    });

    test('학생의 다음 쪽지가 오면 답한 쪽지는 빠지고 새 쪽지가 내 답을 이어 받는다', () {
      final reply = NoteReply(text: '커밋까지 해 줘', at: DateTime(2026), after: 4);
      final lines = HubModel.noteLines(
        [_note(6), _note(5, pane: '%9'), _note(4, read: true), _note(2, read: true)],
        {'|2': reply},
      );
      expect([for (final l in lines) l.note.id], [6, 5], reason: '번호가 보낼 때 본 것보다 큰 같은 칸 쪽지가 다음 답');
      expect(lines.first.before, same(reply));
      expect(lines.last.before, isNull, reason: '다른 칸 쪽지는 이어 받지 않는다');
    });

    test('다음 답이 둘 오면 첫 것만 이어 받고, 못 보낸 답은 기다리지 않는다', () {
      final sent = NoteReply(text: '응', at: DateTime(2026), after: 1);
      final failed = NoteReply(text: '아니', at: DateTime(2026), after: 1, error: '닿지 못했다');
      final lines = HubModel.noteLines(
        [_note(3), _note(2), _note(1, read: true), _note(1, machine: '~mini', read: false)],
        {'|1': sent, '~mini|1': failed},
      );
      expect([for (final l in lines) (l.note.key, l.before?.text)], [('|3', null), ('|2', '응'), ('~mini|1', null)]);
      expect(lines.last.sent?.error, '닿지 못했다');
    });

    test('답 칸을 막는 까닭', () {
      final student = _pane('%3');
      expect(noteReplyBlock(student, online: true), isNull);
      expect(noteReplyBlock(student, online: false), '기기가 끊겨 답할 수 없어요');
      expect(noteReplyBlock(null, online: true), '칸이 닫혀 답할 수 없어요');
      expect(noteReplyBlock(_pane('%3', harness: '', name: ''), online: true), '학생이 나가 셸만 남았어요');
    });
  });

  testWidgets('오른쪽으로 밀면 답 칸이 열리고, 보내면 그 쪽지에 「보냈어요」와 글이 남는다', (tester) async {
    final rig = _Rig();
    final model = _model(rig.server, [_note(3)], [_pane('%3')]);
    await _show(tester, model, rig.server);
    expect(find.byType(ChatComposer), findsNothing);

    await tester.drag(find.text('한 줄'), const Offset(160, 0));
    await tester.pumpAndSettle();
    expect(find.byType(ChatComposer), findsOneWidget);
    expect(find.text('한 줄'), findsOneWidget, reason: '쪽지는 제자리로 돌아온다');

    await tester.enterText(find.byType(TextField), '커밋까지 해 줘');
    await tester.tap(find.byTooltip('보내기'));
    await tester.pumpAndSettle();

    final sent = rig.requests.where((r) => r.$1.endsWith('term/chat-send')).single.$2;
    expect(sent['surface'], '%3');
    expect(sent['text'], '커밋까지 해 줘');
    expect(sent['from'], 'phone', reason: '데스크톱이 글 끝에 「⟦폰⟧」 꼬리표를 붙인다');
    expect(find.byType(ChatComposer), findsNothing, reason: '보냈으면 답 칸을 닫는다');
    expect(find.textContaining('보냈어요 · '), findsOneWidget);
    expect(find.text('커밋까지 해 줘'), findsOneWidget);
    expect(model.notes.single.read, isTrue, reason: '답한 쪽지는 읽음');
    expect(model.unread, 0);
  });

  testWidgets('못 보내면 까닭을 말하고 쓴 글과 답 칸을 둔다', (tester) async {
    final rig = _Rig(failChat: true);
    final model = _model(rig.server, [_note(3)], [_pane('%3')]);
    await _show(tester, model, rig.server);

    await tester.tap(find.byTooltip('답'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField), '다시 해 줘');
    await tester.tap(find.byTooltip('보내기'));
    await tester.pumpAndSettle();

    expect(find.text('못 보냈어요 · 답장이 안 갔다 · 입력칸에 쓰던 글이 있어요'), findsOneWidget);
    expect(find.byType(ChatComposer), findsOneWidget);
    expect(tester.widget<TextField>(find.byType(TextField)).controller!.text, '다시 해 줘');
    expect(model.notes.single.read, isFalse);
  });

  testWidgets('「데스크톱에서 보기」는 그 기기 앱의 초점을 그 칸으로 옮기고, 닿지 않는 칸엔 없다', (tester) async {
    final rig = _Rig();
    final model = _model(rig.server, [_note(3), _note(2, pane: '%9', kind: 'blocked')], [_pane('%3')]);
    await _show(tester, model, rig.server);

    expect(find.byTooltip('데스크톱에서 보기'), findsOneWidget, reason: '닫힌 칸 쪽지엔 없다');
    await tester.tap(find.byTooltip('데스크톱에서 보기'));
    await tester.pump(const Duration(milliseconds: 200));
    final focus = rig.requests.where((r) => r.$1.endsWith('/cmd')).single.$2;
    expect(focus['method'], 'surface.focus');
    expect((focus['params'] as Map)['surface_id'], '%3');
    expect(model.notes.firstWhere((n) => n.id == 3).read, isFalse, reason: '쪽지는 그대로 둔다');
  });

  testWidgets('닫힌 칸 쪽지는 답 칸 대신 까닭', (tester) async {
    final rig = _Rig();
    final model = _model(rig.server, [_note(3, kind: 'blocked')], const []);
    await _show(tester, model, rig.server);

    await tester.tap(find.byTooltip('답'));
    await tester.pumpAndSettle();
    expect(find.text('칸이 닫혀 답할 수 없어요'), findsOneWidget);
    expect(find.byType(TextField), findsNothing);
  });

  testWidgets('자판이 올라와도 답 칸과 그 쪽지는 자판 위에 선다', (tester) async {
    final rig = _Rig();
    final notes = [for (var i = 8; i >= 1; i--) _note(i, pane: '%$i', summary: '쪽지 $i')];
    final model = _model(rig.server, notes, [for (var i = 1; i <= 8; i++) _pane('%$i')]);
    await _show(tester, model, rig.server);

    await tester.scrollUntilVisible(find.text('쪽지 1'), 200);
    await tester.drag(find.text('쪽지 1'), const Offset(160, 0));
    await tester.pumpAndSettle();
    tester.view.viewInsets = const FakeViewPadding(bottom: 300);
    addTearDown(tester.view.resetViewInsets);
    for (var i = 0; i < 10; i++) {
      await tester.pump(const Duration(milliseconds: 30));
    }
    await tester.pumpAndSettle();

    const keyboardTop = 640.0 - 300;
    final box = tester.getRect(find.byType(TextField));
    final note = tester.getRect(find.text('쪽지 1'));
    expect(box.bottom, lessThanOrEqualTo(keyboardTop));
    expect(note.top, greaterThanOrEqualTo(0));
    expect(note.bottom, lessThan(box.top));
  });

  testWidgets('승인을 기다리는 쪽지는 쪽지 안에서 허락·거절하고 거절은 답 칸 글이 까닭', (tester) async {
    final rig = _Rig(
      live: {
        'live': true,
        'seq': 1,
        'session': 's1',
        'permissions': [
          {'id': 'p1', 'tool': 'Bash', 'input': {'command': 'git push'}},
        ],
      },
    );
    final notes = [_note(5, kind: 'permission', summary: '선생님, git push 해도 될까요?'), _note(4, kind: 'permission', summary: '앞 승인')];
    final model = _model(rig.server, notes, [_pane('%3', status: 'waiting', kind: 'permission')]);
    await _show(tester, model, rig.server);
    await tester.pump(const Duration(milliseconds: 200));

    expect(find.byType(ConversationView), findsOneWidget, reason: '같은 칸의 고를 것은 최근 쪽지에만');
    expect(find.text('git push'), findsOneWidget);

    await tester.tap(find.byTooltip('답').first);
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField), '푸시는 내가 할게');
    await tester.tap(find.text('거절'));
    await tester.pump(const Duration(milliseconds: 200));

    final decided = rig.requests.where((r) => r.$1.endsWith('term/mod-decide')).single.$2;
    expect(decided['id'], 'p1');
    expect(decided['decision'], 'deny');
    expect(decided['message'], '푸시는 내가 할게');
    // 쥔 채 둔 mod-live 물음의 시간 상한까지 흘려 보낸다.
    await tester.pumpWidget(const SizedBox());
    await tester.pump(const Duration(seconds: 30));
  });

  testWidgets('쪽지 답 칸 생김새', (tester) async {
    final rig = _Rig();
    final model = _model(
      rig.server,
      [
        _note(6, summary: '폰 상태줄 한 줄로 맞추고 커밋'),
        _note(5, pane: '%7', read: true, summary: '미니맵 탭 줄 굽기 끝'),
        _note(4, read: true, summary: '상태줄 두 줄 → 한 줄 시안'),
        _note(2, pane: '%9', kind: 'blocked', summary: '선생님, 막혔어요. 세션이 꺼졌어요'),
      ],
      [_pane('%3'), _pane('%7')],
    );
    // 쪽지 4 에서 보낸 답을 학생의 다음 쪽지 6 이 이어 받고, 쪽지 5 는 보낸 답을 단 채 기다린다.
    model.replies['|4'] = NoteReply(text: '커밋까지 해 줘', at: DateTime(2026, 11, 13, 14, 32), after: 4);
    model.replies['|5'] = NoteReply(text: '테스트만 돌리고 기다려 줘', at: DateTime(2026, 11, 13, 14, 40), after: 6);
    await _show(tester, model, rig.server, size: const Size(390, 760));
    await tester.tap(find.byTooltip('답').first);
    await tester.pumpAndSettle();
    await expectLater(find.byType(NotesSheet), matchesGoldenFile('goldens/notes_sheet_reply.png'));
  });
}
