import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/nacho.dart';
import 'package:kasaterm_mobile/nacho_student.dart';
import 'package:kasaterm_mobile/screens/workboard_view.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/workboard.dart';

NachoTaskCard card(
  String id,
  String state, {
  String group = 'active',
  bool paused = false,
  Map<String, Object?>? student,
  String project = 'kasaterm',
}) => NachoTaskCard({
  'id': id,
  'goal': '일 $id',
  'state': state,
  'state_label': state,
  'group': group,
  'project': project,
  'paused': paused,
  'updated_ms': 1000,
  'student': ?student,
});

Pane pane(
  String id,
  String status, {
  String? kind,
  String cwd = '/w/kasaterm',
  String name = '케이',
  String? mirrorOf,
}) => Pane(
  id: id,
  name: name,
  title: '',
  status: status,
  window: 0,
  cwd: cwd,
  kind: kind,
  mirrorOf: mirrorOf,
);

void main() {
  test('장부 상태 아홉 → 네 줄기', () {
    (WorkLane, YoursKind?, bool) lane(
      String s, {
      String group = 'active',
      bool paused = false,
    }) => laneOfTask(card('x', s, group: group, paused: paused));
    expect(lane('queued').$1, WorkLane.running);
    expect(lane('working').$1, WorkLane.running);
    expect(lane('approval_needed', group: 'attention'), (
      WorkLane.yours,
      YoursKind.approval,
      false,
    ));
    expect(lane('working', group: 'attention'), (
      WorkLane.yours,
      YoursKind.blocked,
      false,
    ));
    expect(
      lane('working', group: 'attention', paused: true).$2,
      YoursKind.paused,
    );
    for (final s in [
      'verifying',
      'restart_pending',
      'post_restart_verifying',
    ]) {
      expect(lane(s).$1, WorkLane.verifying, reason: s);
    }
    expect(lane('done', group: 'closed'), (WorkLane.done, null, false));
    expect(lane('failed', group: 'closed').$3, isTrue);
    expect(lane('cancelled', group: 'closed').$3, isTrue);
  });

  test('판의 학생 — 승인·질문만 거노 차례, 쉬는 학생·거울·셸은 줄에 안 선다', () {
    final board = buildBoard(
      tasks: const [],
      nowMs: 100000,
      devices: [
        WorkDevice(
          label: '맥북',
          online: true,
          here: true,
          students: [
            pane('%1', 'waiting', kind: 'permission'),
            pane('%2', 'waiting', kind: 'question', cwd: '/w/nacho-neko'),
            pane('%3', 'waiting', kind: 'idle'),
            pane('%4', 'working'),
            pane('%5', 'working', mirrorOf: '맥미니'),
            pane('%6', 'working', name: ''),
            pane('%7', 'idle'),
          ],
        ),
      ],
    );
    final byId = {for (final i in board.items) i.pane!.id: i};
    expect(byId.keys, unorderedEquals(['%1', '%2', '%4']));
    expect(byId['%1']!.yours, YoursKind.approval);
    expect(byId['%2']!.yours, YoursKind.question);
    expect(byId['%2']!.project, 'nacho-neko');
    expect(byId['%4']!.lane, WorkLane.running);
    expect(board.items.every((i) => i.source == WorkSource.live), isTrue);
    expect(board.devices.single.waiting, 2);
  });

  test('장부 작업이 잡은 학생은 판에서 또 세우지 않는다', () {
    final worker = pane('%4', 'working');
    final board = buildBoard(
      tasks: [
        card('t1', 'working', student: {'surface': '%4', 'host': '맥북'}),
      ],
      devices: [
        WorkDevice(label: '맥북', online: true, here: true, students: [worker]),
      ],
      seatOf: (_) => StudentSeat(surface: '%4', machine: '맥북', pane: worker),
    );
    expect(board.items, hasLength(1));
    expect(board.items.single.source, WorkSource.ledger);
    expect(board.items.single.pane, same(worker));
    expect(board.items.single.machine, '맥북');
  });

  test('창 제목의 도는 표시와 명령 원문은 걷는다', () {
    final p = Pane(
      id: '%1',
      name: '케이',
      title: '◑ 모바일앱 이중 뷰',
      status: 'working',
      window: 0,
      cwd: '/w/kasaterm',
      doing: 'Bash 빌드 돌리기 — cargo build --release -p kasapet',
    );
    final board = buildBoard(
      tasks: const [],
      devices: [
        WorkDevice(label: '맥북', online: true, students: [p]),
      ],
    );
    expect(board.items.single.title, '모바일앱 이중 뷰');
    expect(board.items.single.detail, 'Bash 빌드 돌리기');
  });

  test('프로젝트는 거노 차례가 많은 것부터', () {
    final board = buildBoard(
      tasks: [
        card('a', 'working', project: 'b-proj'),
        card('b', 'working', project: 'b-proj'),
        card('c', 'approval_needed', group: 'attention', project: 'a-proj'),
      ],
      devices: const [],
    );
    expect([for (final p in board.projects) p.name], ['a-proj', 'b-proj']);
    expect(board.lane(WorkLane.running, project: 'b-proj'), hasLength(2));
  });

  test('신선도', () {
    expect(freshLabel(1000, nowMs: 3000), '지금');
    expect(freshLabel(1000, nowMs: 31000), '30초 전');
    expect(freshSecsLabel(3600 * 5), '5시간 전');
    expect(freshSecsLabel(null), '언제인지 모름');
  });

  // 동작 확인과 골든 비교를 가른다 — 골든은 Flutter 판마다 글자 번짐이 달라 어긋나는데,
  // 한 검사 안에 두면 그 실패가 뒤의 확인(허용 단추가 꺼져 있나)까지 막는다.
  testWidgets('예시 판 — 띠·거노 차례·기기 단추, 승인 시트의 허용은 꺼짐', (tester) async {
    final board = await _pumpDemoBoard(tester);
    expect(find.text('예시 데이터 — 실제 작업이 아니에요'), findsOneWidget);
    expect(find.text('거노 차례 2'), findsOneWidget);
    expect(find.text('기기 2/3'), findsOneWidget);
    expect(find.text('검토'), findsOneWidget);

    await tester.tap(find.text('검토'));
    await tester.pumpAndSettle();
    final allow = tester.widget<OutlinedButton>(
      find.ancestor(
        of: find.text('한 번 허용 — 아직 못 켜요'),
        matching: find.byWidgetPredicate((w) => w is OutlinedButton),
      ),
    );
    expect(allow.onPressed, isNull, reason: '서버가 범위·만료·1회용을 검증하기 전엔 켜지 않는다');
    expect(find.textContaining('Face ID 는 이 폰의 잠금 확인일 뿐'), findsOneWidget);
    await board.close(tester);
  });

  testWidgets('예시 판 골든 — 판과 승인 시트', (tester) async {
    final board = await _pumpDemoBoard(tester);
    await expectLater(
      find.byType(Scaffold),
      matchesGoldenFile('goldens/workboard_demo.png'),
    );
    await tester.tap(find.text('검토'));
    await tester.pumpAndSettle();
    await expectLater(
      find.byType(Scaffold),
      matchesGoldenFile('goldens/workboard_approval.png'),
    );
    await board.close(tester);
  });
}

/// 390×844 폰에 예시 데이터 판을 띄운다. 동작 줄이기를 켜 움직이는 그림 대신 정지 얼굴로.
Future<({Future<void> Function(WidgetTester) close})> _pumpDemoBoard(
  WidgetTester tester,
) async {
  tester.view.physicalSize = const Size(390 * 3, 844 * 3);
  tester.view.devicePixelRatio = 3;
  addTearDown(tester.view.reset);
  final server = Server(Uri.parse('http://127.0.0.1:1/'));
  final desk = NachoDesk(server);
  final students = StudentLookup(server);
  await tester.pumpWidget(
    MaterialApp(
      theme: ThemeData(colorSchemeSeed: const Color(0xff4a90e2)),
      home: MediaQuery(
        data: const MediaQueryData(
          size: Size(390, 844),
          disableAnimations: true,
        ),
        child: Scaffold(
          body: WorkBoardView(
            desk: desk,
            server: server,
            students: students,
            demo: true,
            onOpenTask: (_) {},
            onOpenPane: (_) {},
            onGoChat: () {},
          ),
        ),
      ),
    ),
  );
  await tester.pump();
  return (
    close: (WidgetTester t) async {
      await t.pumpWidget(const SizedBox());
      desk.dispose();
      students.dispose();
    },
  );
}
