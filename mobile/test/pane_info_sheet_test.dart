import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/pane_info.dart';
import 'package:kasaterm_mobile/screens/pane_info_sheet.dart';
import 'package:kasaterm_mobile/screens/terminal.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/term_session.dart';

import 'mobile_terminal_qa_test.dart' show loadFonts;

const _pane = Pane(id: '%7', name: '세이아', title: '', status: 'idle', window: 0, cwd: '/', harness: 'claude', surfaceKey: 'sk');

String _said(String role, String text, String at) => '${jsonEncode({
      'type': role,
      'sessionId': 's1',
      'timestamp': at,
      'message': {
        'role': role,
        'content': role == 'user' ? text : [{'type': 'text', 'text': text}],
      },
    })}\n';

/// 질문 셋과 화면 몇 장짜리 답 — 첫 질문은 맨 아래에서 한참 위에 있다.
final _transcript = [
  for (final (i, q) in ['첫째 질문 — 빌드 고쳐 줘', '둘째 질문 — 시험도 돌려 줘', '셋째 질문 — 커밋해 줘'].indexed) ...[
    _said('user', q, '2026-11-13T0$i:00:00Z'),
    _said('assistant', List.generate(40, (n) => '${i + 1}번 답의 $n째 줄').join('\n\n'), '2026-11-13T0$i:00:05Z'),
  ],
].join();

class _InfoServer extends Server {
  _InfoServer() : super(Uri.parse('https://example.com/'));
  final asked = <Map<String, String?>>[];

  @override
  Future<Uint8List> imageBytes(Uri uri) async => throw const ServerException('그림 없음');

  @override
  Future<({String raw, int offset, bool reset, String source})?> transcriptRaw(
    String pane,
    int offset, {
    String? machine,
    int? waitMs,
    String source = '',
  }) async {
    final end = utf8.encode(_transcript).length;
    return offset == 0
        ? (raw: _transcript, offset: end, reset: true, source: 'info.jsonl')
        : (raw: '', offset: end, reset: false, source: 'info.jsonl');
  }

  @override
  Future<List<Pane>> panes({String? machine}) async => const [_pane];

  @override
  Future<PaneInfoAnswer> paneInfo(Pane pane, {String? since, int waitMs = 8000}) async {
    asked.add({'pane': pane.id, 'surface_key': pane.surfaceKey, 'since': since});
    if (since != null) await Future<void>.delayed(const Duration(seconds: 30));
    return const PaneInfoAnswer(
      seq: '1.a',
      card: FocusCard(
        pane: '%7',
        harness: 'claude',
        cwd: '/Users/someone/work/kasaterm',
        model: 'claude-opus-5-5',
        effort: 'high',
        facts: FocusFacts(state: 'working', tools: [FocusTool(tool: 'Bash', label: 'cargo test', ageMs: 4000)]),
        ports: [FocusPort(port: 5173, name: 'node', site: 'Vite App')],
      ),
    );
  }
}

class _Session extends TermSession {
  _Session(super.server, super.pane) {
    state = TermState.connected;
  }
  @override
  void connect() {}
  @override
  bool get canSend => false;
  @override
  void sendText(String text) {}
}

Future<_InfoServer> _open(WidgetTester tester) async {
  tester.view.physicalSize = const Size(390, 844);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  final server = _InfoServer();
  await tester.pumpWidget(MaterialApp(
    theme: ThemeData(platform: TargetPlatform.iOS),
    home: TerminalScreen(server: server, pane: _pane, session: _Session(server, _pane)),
  ));
  await tester.pumpAndSettle();
  return server;
}

Future<void> _openSheet(WidgetTester tester) async {
  await tester.tap(find.byTooltip('칸 정보'));
  await tester.pumpAndSettle();
}

Finder _inSheet(Finder f) => find.descendant(of: find.byType(PaneInfoSheet), matching: f);

Finder _bubble(String text) => find.descendant(
      of: find.byType(ListView).first,
      matching: find.textContaining(text, findRichText: true),
    );

void main() {
  setUpAll(loadFonts);
  setUp(() => FlutterSecureStorage.setMockInitialValues({}));

  testWidgets('칸 정보 시트 — 지금 보는 칸 카드와 내 프롬프트(새 것이 위), 터미널로 보기', (tester) async {
    final server = await _open(tester);
    await _openSheet(tester);
    expect(server.asked.first, {'pane': '%7', 'surface_key': 'sk', 'since': null});
    expect(_inSheet(find.text('Claude')), findsOneWidget);
    expect(_inSheet(find.text('일하는 중')), findsOneWidget);
    expect(_inSheet(find.text('~/work/kasaterm')), findsOneWidget);
    expect(_inSheet(find.text('claude-opus-5-5 · high')), findsOneWidget);
    expect(_inSheet(find.textContaining('Bash · cargo test · ')), findsOneWidget);
    expect(_inSheet(find.text('5173 · node · Vite App')), findsOneWidget);
    expect(_inSheet(find.text('내 프롬프트 · 3개')), findsOneWidget);
    final rows = tester.widgetList<Text>(_inSheet(find.textContaining('질문 — '))).map((t) => t.data).toList();
    expect(rows, ['셋째 질문 — 커밋해 줘', '둘째 질문 — 시험도 돌려 줘', '첫째 질문 — 빌드 고쳐 줘']);
    // 체크칸 누름 영역 44.
    expect(tester.getSize(find.bySemanticsLabel('끝낸 것으로 체크').first).height, greaterThanOrEqualTo(44));
    await tester.pumpWidget(const SizedBox());
    await tester.pump(const Duration(seconds: 40));
  });

  testWidgets('줄을 누르면 그 말풍선이 화면 맨 위에 서고, 다시 열면 그 줄이 지금 질문', (tester) async {
    await _open(tester);
    final list = tester.getRect(find.byType(ListView).first);
    expect(_bubble('첫째 질문'), findsNothing, reason: '첫 질문은 아직 그려지지 않을 만큼 위에 있다');
    await _openSheet(tester);
    await tester.tap(_inSheet(find.text('첫째 질문 — 빌드 고쳐 줘')));
    await tester.pumpAndSettle();
    expect(find.byType(PaneInfoSheet), findsNothing);
    final top = tester.getRect(_bubble('첫째 질문'));
    expect(top.top - list.top, inInclusiveRange(0, 40), reason: '말풍선 위 끝이 목록 맨 위에 붙는다');

    await _openSheet(tester);
    final current = tester.widget<Text>(_inSheet(find.text('첫째 질문 — 빌드 고쳐 줘')));
    expect(current.style?.fontWeight, FontWeight.w600);
    final other = tester.widget<Text>(_inSheet(find.text('셋째 질문 — 커밋해 줘')));
    expect(other.style?.fontWeight, isNull);

    // 체크는 남고 줄은 흐리게 그어진다.
    await tester.tap(_inSheet(find.bySemanticsLabel('끝낸 것으로 체크')).last);
    await tester.pumpAndSettle();
    expect(_inSheet(find.byIcon(Icons.check_box)), findsOneWidget);
    expect(tester.widget<Text>(_inSheet(find.text('첫째 질문 — 빌드 고쳐 줘'))).style?.decoration, TextDecoration.lineThrough);
    await tester.pumpWidget(const SizedBox());
    await tester.pump(const Duration(seconds: 40));
  });
}
