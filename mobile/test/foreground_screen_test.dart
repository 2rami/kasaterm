import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/background_grace.dart';
import 'package:kasaterm_mobile/screens/conversation_view.dart';
import 'package:kasaterm_mobile/screens/terminal.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/term_session.dart';

class _Server extends Server {
  _Server(this.pane) : super(Uri.parse('https://fixture.invalid/'));
  final Pane pane;
  final reads = <String?>[];
  @override
  Future<List<Pane>> panes({String? machine}) async {
    reads.add(machine);
    return [pane];
  }
  @override
  Future<({String raw, int offset, bool reset, String source})?> transcriptRaw(
    String pane, int offset, {String? machine, int? waitMs, String source = ''}
  ) async => null;
}

class _Session extends TermSession {
  _Session(super.server, super.pane);
  int connects = 0;
  final sent = <String>[];
  @override
  void connect() {
    connects++;
    state = TermState.connected;
    notifyListeners();
  }
  @override
  bool get canSend => !server.isClosed && state == TermState.connected;
  @override
  void sendText(String text) => sent.add(text);
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  const channel = MethodChannel('kasaterm/background');
  final messenger = TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
  final grace = BackgroundGrace.instance;

  for (final harness in ['claude', 'codex']) {
    for (final view in PaneView.values) {
      testWidgets('$harness $view keeps its draft, view and device on short and long return', (tester) async {
        grace..attach()..resetForTest();
        messenger.setMockMethodCallHandler(channel, (call) async => call.method == 'begin' ? true : null);
        addTearDown(() {
          grace.resetForTest();
          messenger.setMockMethodCallHandler(channel, null);
        });
        final pane = Pane(id: '%3', name: 'fixture', title: '', status: 'idle', window: 0,
          cwd: '/', machine: '~mini', harness: harness, session: 'fixture-session');
        final server = _Server(pane);
        final session = _Session(server, pane);
        await tester.pumpWidget(MaterialApp(home: TerminalScreen(
          server: server, pane: pane, session: session, initialView: view,
          initialChatDraft: 'chat draft', initialTermDraft: 'terminal draft',
        )));
        await tester.pump();
        final page = tester.widget<PageView>(find.byType(PageView)).controller!.page;
        final draft = view == PaneView.chat ? 'chat draft' : 'terminal draft';
        expect(find.text(draft), findsOneWidget);
        for (final away in [const Duration(seconds: 2), BackgroundGrace.cap]) {
          grace.didChangeAppLifecycleState(AppLifecycleState.hidden);
          await tester.pump(away);
          grace.didChangeAppLifecycleState(AppLifecycleState.resumed);
          await tester.pump();
          expect(find.text(draft), findsOneWidget);
          expect(tester.widget<PageView>(find.byType(PageView)).controller!.page, page);
          expect(session.pane.machine, '~mini');
          expect(session.pane.session, 'fixture-session');
        }
        expect(session.connects, 3);
        expect(session.sent, isEmpty, reason: 'restoring a draft is not sending it');
        expect(server.reads, isNotEmpty);
        expect(server.reads.every((m) => m == '~mini'), isTrue);
        await tester.pumpWidget(const SizedBox());
        await tester.pump(const Duration(seconds: 3));
        server.close();
      });
    }
  }
}
