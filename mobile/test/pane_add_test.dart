import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/hub_model.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/term_session.dart';
import 'package:kasaterm_mobile/screens/terminal.dart';

const slug = 'abcdefghij0123456789abcde';
const viewed = Pane(
  id: '%3',
  name: '세이아',
  title: '',
  status: 'idle',
  window: 1,
  cwd: '/w',
  machine: '~mini-stable',
);

Pane shell(String id, {int window = 0, String? machine}) => Pane(
  id: id,
  name: '',
  title: '',
  status: '',
  window: window,
  cwd: '/w/new',
  machine: machine,
);

/// split·tab 요청을 적어 두고, 목록엔 새 pane 이 한 박자 뒤에 실린다.
class AddServer extends Server {
  AddServer() : super(Uri.parse('https://example.com/u/$slug/'));
  final calls = <String>[];
  int listed = 0;
  bool listNew = true;

  @override
  Future<String?> splitPane(String from, {String? machine}) async {
    calls.add('split $machine/$from');
    return '%9';
  }

  @override
  Future<String?> newTab(String outer, {String? machine}) async {
    calls.add('tab $machine/$outer');
    return '%9';
  }

  @override
  Future<List<Pane>> panes({String? machine}) async {
    listed++;
    return [
      viewed,
      if (listNew) shell('%9', window: 1, machine: machine),
    ];
  }
}

class QuietSession extends TermSession {
  QuietSession(super.server, super.pane) {
    state = TermState.connected;
  }
  @override
  void connect() {}
}

Future<void> pumpScreen(
  WidgetTester tester,
  AddServer server, {
  Future<void> Function()? onPaneCreated,
}) async {
  await tester.pumpWidget(
    MaterialApp(
      home: TerminalScreen(
        server: server,
        pane: viewed,
        session: QuietSession(server, viewed),
        onPaneCreated: onPaneCreated,
      ),
    ),
  );
}

void main() {
  testWidgets('상단바 「pane 추가」→「옆에 쪼개기」는 보는 pane 옆을 쪼개고 새 pane 으로 옮겨 간다', (
    tester,
  ) async {
    final server = AddServer();
    var refreshed = 0;
    await pumpScreen(tester, server, onPaneCreated: () async => refreshed++);
    await tester.tap(find.byTooltip('pane 추가'));
    await tester.pumpAndSettle();
    expect(find.text('옆에 쪼개기'), findsOneWidget);
    expect(find.text('탭으로'), findsOneWidget);
    await tester.tap(find.text('옆에 쪼개기'));
    await tester.pumpAndSettle();
    expect(server.calls, ['split ~mini-stable/%3']);
    expect(refreshed, 1, reason: '허브 목록을 폴링 전에 다시 받는다');
    // 새 pane 화면으로 바뀌었다 — 이름 없는 pane 은 「셸」.
    expect(find.text('셸'), findsWidgets);
    expect(find.text('세이아'), findsNothing);
    final screen = tester.widget<TerminalScreen>(find.byType(TerminalScreen));
    expect(screen.pane.id, '%9');
    expect(screen.pane.machine, '~mini-stable');
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('「탭으로」는 surface.new_tab — 목록에 아직 없어도 id 로 들어간다', (
    tester,
  ) async {
    final server = AddServer()..listNew = false;
    await pumpScreen(tester, server);
    await tester.tap(find.byTooltip('pane 추가'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('탭으로'));
    // 되묻기 3번 × 400ms 를 넘긴다.
    await tester.pumpAndSettle(const Duration(seconds: 2));
    expect(server.calls, ['tab ~mini-stable/%3']);
    expect(server.listed, greaterThanOrEqualTo(3));
    final screen = tester.widget<TerminalScreen>(find.byType(TerminalScreen));
    expect(screen.pane.id, '%9');
    expect(screen.pane.window, viewed.window, reason: '같은 방');
    expect(screen.pane.isShell, isTrue);
    await tester.pumpWidget(const SizedBox());
  });

  test('HubModel.locateNew 는 목록을 다시 받아 id 로, 또는 새 방 번호로 찾는다', () async {
    final server = LocateServer();
    final model = HubModel(server);
    await model.refresh();
    expect(model.windowsOf(null), {0});
    // id 로 — 첫 조회엔 없고 둘째에 실린다.
    server.appear = 2;
    final byId = await model.locateNew(
      null,
      id: '%9',
      wait: const Duration(milliseconds: 1),
    );
    expect(byId?.id, '%9');
    // 새 방 — id 를 모르니 전에 없던 방 번호의 pane.
    server.appear = 0;
    server.extra = [shell('%12', window: 3)];
    final inRoom = await model.locateNew(
      null,
      before: {0, 1},
      wait: const Duration(milliseconds: 1),
    );
    expect(inRoom?.id, '%12');
    expect(inRoom?.window, 3);
  });
}

class LocateServer extends Server {
  LocateServer() : super(Uri.parse('http://127.0.0.1/'));
  int appear = 0;
  int polls = 0;
  List<Pane> extra = const [];

  @override
  Future<List<Pane>> panes({String? machine}) async {
    polls++;
    return [
      shell('%1'),
      if (appear > 0 && polls >= appear) shell('%9', window: 1),
      ...extra,
    ];
  }

  @override
  Future<List<String>> sessions() async => const [];
  @override
  Future<List<Machine>> machines() async => const [];
  @override
  Future<List<WindowLayout>> windows({String? machine}) async => const [];
  @override
  Future<List<Note>> notes({String? machine}) async => const [];
}
