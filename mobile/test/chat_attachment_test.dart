import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/chat_markdown.dart';
import 'package:kasaterm_mobile/main.dart' show buildTheme;
import 'package:kasaterm_mobile/screens/conversation_view.dart';
import 'package:kasaterm_mobile/screens/terminal.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/term_session.dart';

import 'mobile_terminal_qa_test.dart' show loadFonts;

const target = Pane(
  id: '%7',
  name: '세이아',
  title: '',
  status: 'idle',
  window: 0,
  cwd: '/',
  harness: 'claude',
);

class ChatServer extends Server {
  ChatServer() : super(Uri.parse('https://example.com/'));
  final sent = <String>[];
  int pasted = 0;

  @override
  Future<void> pasteImage(String pane, List<int> png, {String? machine}) async {
    pasted++;
  }

  @override
  Future<void> send(String pane, String text, {String? machine}) async {
    sent.add(text);
  }

  @override
  Future<({String raw, int offset, bool reset})?> transcriptRaw(
    String pane,
    int offset, {
    String? machine,
    int? waitMs,
  }) async => null;

  @override
  Future<List<Pane>> panes({String? machine}) async => const [target];
}

class ChatSession extends TermSession {
  ChatSession(super.server, super.pane) {
    state = TermState.connected;
  }
  final entered = <String>[];
  @override
  void connect() {}
  @override
  void sendText(String text) => entered.add(text);
}

/// 1×1 PNG — 미리보기 칸이 실제로 그림을 그릴 수 있어야 한다.
final _png = Uint8List.fromList([
  0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, //
  0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
  0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00,
  0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
  0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00,
  0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
]);

Future<(ChatServer, ChatSession)> _openChat(WidgetTester tester) async {
  paneView.value = PaneView.chat;
  addTearDown(() => paneView.value = PaneView.terminal);
  final server = ChatServer();
  final session = ChatSession(server, target);
  await tester.pumpWidget(
    MaterialApp(
      home: TerminalScreen(
        server: server,
        pane: target,
        session: session,
        pickImage: () async => _png,
      ),
    ),
  );
  await tester.pump();
  return (server, session);
}

Future<void> _close(WidgetTester tester) async {
  await tester.pumpWidget(const SizedBox());
  await tester.pump(const Duration(seconds: 3));
}

void main() {
  testWidgets('대화 보기: 사진 뒤 글은 입력상자를 비우지 않고 붙여 보낸다', (tester) async {
    final semantics = tester.ensureSemantics();
    final (server, session) = await _openChat(tester);
    await tester.tap(find.byTooltip('사진 첨부'));
    await tester.pump();
    await tester.pump();
    expect(server.pasted, 1);
    expect(find.text('보내면 함께 가요'), findsOneWidget);
    expect(find.byType(SnackBar), findsNothing, reason: '띠가 입력줄을 덮는다');
    expect(find.bySemanticsLabel(RegExp('보낼 사진 1장')), findsOneWidget);

    await tester.enterText(find.byType(TextField), '이 화면 봐 줘');
    await tester.tap(find.byTooltip('보내기'));
    await tester.pump();
    await tester.pump(TermSession.enterGap);
    await tester.pump();
    // 서버 `send` 는 Ctrl+U 로 상자를 비워 `[Image #1]` 을 지운다 — 타면 안 된다.
    expect(server.sent, isEmpty);
    expect(session.entered, ['\x1b[200~ 이 화면 봐 줘\x1b[201~', '\r']);
    expect(find.text('보내면 함께 가요'), findsNothing);

    // 사진이 나간 뒤의 글은 원래 길(서버 send)로.
    await tester.enterText(find.byType(TextField), '고마워');
    await tester.tap(find.byTooltip('보내기'));
    await tester.pump();
    expect(server.sent, ['고마워']);
    expect(session.entered, hasLength(2));
    await _close(tester);
    semantics.dispose();
  });

  testWidgets('대화 보기: 사진만 보내면 Enter 하나', (tester) async {
    final (server, session) = await _openChat(tester);
    await tester.tap(find.byTooltip('사진 첨부'));
    await tester.pump();
    await tester.pump();
    await tester.tap(find.byTooltip('보내기'));
    await tester.pump();
    expect(session.entered, ['\r']);
    expect(server.sent, isEmpty);
    await _close(tester);
  });

  testWidgets('끊겼으면 붙여 보내지 않고 글을 남긴다', (tester) async {
    final (server, session) = await _openChat(tester);
    await tester.tap(find.byTooltip('사진 첨부'));
    await tester.pump();
    await tester.pump();
    await tester.enterText(find.byType(TextField), '남아 있어야 해');
    session.state = TermState.reconnecting;
    await tester.tap(find.byTooltip('보내기'));
    await tester.pump();
    expect(session.entered, isEmpty);
    expect(server.sent, isEmpty);
    expect(find.text('남아 있어야 해'), findsOneWidget);
    await _close(tester);
  });

  group('흐르는 고정폭 글의 한글', () {
    setUpAll(loadFonts);

    test('인라인 코드의 한글이 제 폭을 가진다', () {
      final code = chatMarkdownStyle(
        buildTheme(Brightness.light),
        base: const TextStyle(fontFamily: 'Pretendard', fontSize: 15),
        codeBg: Colors.black12,
      ).code!;
      double width(String s) {
        final p = TextPainter(
          text: TextSpan(text: s, style: code),
          textDirection: TextDirection.ltr,
        )..layout();
        return p.width;
      }

      final size = code.fontSize!;
      // TermHangul(D2Coding Nerd Mono)은 한글 진행 폭이 반 칸 — 글자 다섯이 2.5 em 에 포개졌다.
      expect(width('구글깃허브'), greaterThan(5 * size * 0.85));
      expect(width('ab'), closeTo(2 * size * 0.6, 0.5), reason: '영문은 그대로 고정폭');
    });
  });
}
