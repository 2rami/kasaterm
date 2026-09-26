import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/nacho.dart';
import 'package:kasaterm_mobile/nacho_student.dart';
import 'package:kasaterm_mobile/organize.dart';
import 'package:kasaterm_mobile/screens/organize_sheet.dart';
import 'package:kasaterm_mobile/screens/workboard_view.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/work_mode.dart';
import 'package:kasaterm_mobile/workboard.dart';

const now = 1790450000000;

WorkItem ledgerItem({String id = 'w1', Pane? pane}) => WorkItem(
  key: 'task:$id',
  source: WorkSource.ledger,
  lane: WorkLane.running,
  title: '일 $id',
  project: 'kasaterm',
  taskId: id,
  pane: pane,
  machine: '맥미니',
);

Pane pane({
  String status = 'working',
  String? kind,
  String? doing = 'Edit organize.dart',
  String? branch = 'main',
}) => Pane(
  id: '%4',
  name: '케이',
  title: '',
  status: status,
  window: 0,
  cwd: '/w/kasaterm',
  kind: kind,
  doing: doing,
  branch: branch,
);

WorkItem liveItem(Pane p) => WorkItem(
  key: 'pane:|%4',
  source: WorkSource.live,
  lane: WorkLane.running,
  title: '폰 정리 화면',
  project: 'kasaterm',
  pane: p,
  machine: '맥북',
);

NachoTaskDetail detail(Map<String, Object?> extra) => NachoTaskDetail({
  'id': 'w1',
  'goal': '폰 정리 화면',
  'request': '폰 정리 화면 만들기',
  'state': 'working',
  'state_label': '진행 중',
  'updated_ms': now - 60000,
  ...extra,
});

Map<String, String> slots(OrganizeView v) => {
  for (final s in v.slots) s.label: s.value,
};

SlotTone tone(OrganizeView v, String label) =>
    v.slots.firstWhere((s) => s.label == label).tone;

void main() {
  test('다섯 칸은 늘 같은 순서 — PC 정리 칸과 같은 이름', () {
    for (final v in [
      organize(item: ledgerItem(), task: detail({}), nowMs: now),
      organize(item: liveItem(pane()), nowMs: now),
      organize(item: liveItem(pane()), ledgerProblem: '키 없음', nowMs: now),
    ]) {
      expect(v.slots.map((s) => s.label), ['현재 작업', '변경', '다음 일', '막힘', '검증']);
    }
  });

  test('학생 보고가 있으면 바꾼 파일·다음 일·학생 검사를 그대로 — 나쵸 검증 전엔 통과라고 안 한다', () {
    final v = organize(
      item: ledgerItem(pane: pane()),
      task: detail({
        'step': '학생 보고를 받았다',
        'report': {
          'status': 'done',
          'changed': ['a.dart', 'b.dart', 'c.dart', 'd.dart', 'e.dart'],
          'tests': 'flutter test 21 통과',
          'next': '실제 글꼴 캡처',
          'character': '케이',
          'at_ms': now - 120000,
        },
      }),
      nowMs: now,
    );
    final s = slots(v);
    expect(s['현재 작업'], '폰 정리 화면 만들기\n학생 보고를 받았다');
    expect(
      s['변경'],
      '브랜치 main\n보고한 파일 5개\na.dart\nb.dart\nc.dart\nd.dart\n외 1개',
    );
    expect(s['다음 일'], '실제 글꼴 캡처');
    expect(s['막힘'], '없음');
    expect(s['검증'], '학생이 적은 검사 · flutter test 21 통과');
    expect(tone(v, '검증'), SlotTone.plain, reason: '장부 검증이 아니면 통과 색을 쓰지 않는다');
    expect(v.sources, contains('학생 보고 · 케이 · done · 2분 전'));
  });

  test('막힘 — 승인·막힘·재시작 보고·사람 답 기다림을 가른다', () {
    final approval = organize(
      item: ledgerItem(),
      task: detail({
        'state': 'approval_needed',
        'approval': {'needed': true, 'what': 'dev 배포'},
      }),
      nowMs: now,
    );
    expect(slots(approval)['막힘'], '승인이 있어야 이어져요 · dev 배포');
    expect(tone(approval, '막힘'), SlotTone.alert);

    final blocked = organize(
      item: ledgerItem(),
      task: detail({'blocked': '시뮬레이터가 꺼졌다'}),
      nowMs: now,
    );
    expect(slots(blocked)['막힘'], '막혔어요 · 시뮬레이터가 꺼졌다');

    final restart = organize(
      item: ledgerItem(),
      task: detail({
        'report': {'status': 'needs_restart', 'next': '앱을 껐다 켜야 반영'},
      }),
      nowMs: now,
    );
    expect(slots(restart)['막힘'], '재시작이 있어야 이어져요 · 앱을 껐다 켜야 반영');

    final waiting = organize(
      item: liveItem(pane(status: 'waiting', kind: 'permission')),
      nowMs: now,
    );
    expect(slots(waiting)['막힘'], startsWith('사람 답을 기다려요 · '));
  });

  test('검증 — 장부에 적힌 검증만 통과/실패, 끝남만 있으면 검증 기록 없음', () {
    final ok = organize(
      item: ledgerItem(),
      task: detail({
        'state': 'done',
        'verify': {'ok': true, 'note': '설치 확인'},
      }),
      nowMs: now,
    );
    expect(slots(ok)['검증'], '나쵸 검증 통과 · 설치 확인');
    expect(tone(ok, '검증'), SlotTone.ok);
    final bad = organize(
      item: ledgerItem(),
      task: detail({
        'verify': {'ok': false, 'note': '검사 2개 실패'},
      }),
      nowMs: now,
    );
    expect(slots(bad)['검증'], '나쵸 검증 실패 · 검사 2개 실패');
    expect(tone(bad, '검증'), SlotTone.alert);
    final done = organize(
      item: ledgerItem(),
      task: detail({'state': 'done'}),
      nowMs: now,
    );
    expect(slots(done)['검증'], '끝남 · 검증 기록 없음');
    final remaining = organize(
      item: ledgerItem(),
      task: detail({
        'remaining': ['검증', '보고'],
      }),
      nowMs: now,
    );
    expect(
      slots(remaining)['다음 일'],
      '· 검증\n· 보고',
      reason: '학생이 안 적었으면 나쵸의 남은 단계',
    );
  });

  test('장부에 없는 학생은 판에서 아는 것만 — 나머지는 없다고 적는다', () {
    final v = organize(item: liveItem(pane()), nowMs: now);
    final s = slots(v);
    expect(s['현재 작업'], '폰 정리 화면\nEdit organize.dart');
    expect(s['변경'], startsWith('브랜치 main\n나쵸 장부에 이 학생의 작업이 없어요'));
    expect(s['다음 일'], startsWith('나쵸 장부에 이 학생의 작업이 없어요'));
    expect(s['검증'], startsWith('나쵸 장부에 이 학생의 작업이 없어요'));
    expect(s['막힘'], '없음');
  });

  test('키 없는 허브 — 장부를 못 읽는 까닭을 칸에 적고 다른 길로 채우지 않는다', () {
    const why = '이 허브에 나쵸 키가 없다 — 허브 설정이 필요하다';
    final v = organize(item: liveItem(pane()), ledgerProblem: why, nowMs: now);
    final s = slots(v);
    for (final label in ['다음 일', '검증']) {
      expect(s[label], '나쵸 장부를 못 읽어요 — $why', reason: label);
    }
    expect(s['변경'], contains('나쵸 장부를 못 읽어요'));
    expect(s['막힘'], '판에선 막힘이 안 보여요 · 나쵸 장부는 못 읽어요');
    expect(v.sources, contains('나쵸 장부 없음 · $why'));
    expect(s['현재 작업'], '폰 정리 화면\nEdit organize.dart', reason: '판에서 아는 것은 보인다');
  });

  test('예시 판의 정리는 예시라고 밝힌다', () {
    final board = demoBoard(nowMs: now);
    final item = board.items.firstWhere((i) => i.key == 'demo:3');
    final v = organize(
      item: item,
      task: demoTaskDetail(item, nowMs: now),
      nowMs: now,
    );
    expect(v.demo, isTrue);
    expect(v.sources.first, '예시 데이터 — 실제 작업이 아니에요');
    expect(slots(v)['변경'], contains('보고한 파일 5개'));
  });

  testWidgets('정리 모드 — 작업판 줄을 누르면 정리 시트가 먼저 뜬다', (tester) async {
    final done = await _pumpBoard(tester, WorkMode.organize);
    await tester.tap(find.text('폰 작업판 구현'));
    await tester.pumpAndSettle();
    expect(find.text('정리'), findsOneWidget);
    expect(find.textContaining('보고한 파일 5개'), findsOneWidget);
    expect(find.text('실제 글꼴 캡처로 화면 확인'), findsOneWidget);
    await done(tester);
  });

  testWidgets('조율 모드 — 누르면 원래 길, 길게 누르면 정리', (tester) async {
    final done = await _pumpBoard(tester, WorkMode.coordinate);
    await tester.tap(find.text('폰 작업판 구현'));
    await tester.pumpAndSettle();
    expect(find.text('정리'), findsNothing);
    await tester.longPress(find.text('폰 작업판 구현'));
    await tester.pumpAndSettle();
    expect(find.text('정리'), findsOneWidget);
    await done(tester);
  });

  testWidgets('장부를 못 가져오면 까닭을 칸에 적는다', (tester) async {
    _phone(tester);
    final load = Completer<NachoTaskDetail?>();
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: OrganizeSheet(item: ledgerItem(), load: load.future),
        ),
      ),
    );
    expect(find.byType(CircularProgressIndicator), findsOneWidget);
    load.completeError(const NachoError('nacho_key_missing', '이 허브에 나쵸 키가 없다'));
    await tester.pumpAndSettle();
    expect(find.text('나쵸 장부를 못 읽어요 — 이 허브에 나쵸 키가 없다'), findsWidgets);
  });

  testWidgets('예시 정리 시트 골든', (tester) async {
    _phone(tester);
    final item = demoBoard().items.firstWhere((i) => i.key == 'demo:3');
    await tester.pumpWidget(
      MaterialApp(
        theme: ThemeData(colorSchemeSeed: const Color(0xff4a90e2)),
        home: MediaQuery(
          data: const MediaQueryData(
            size: Size(390, 844),
            disableAnimations: true,
          ),
          child: Scaffold(
            body: OrganizeSheet(
              item: item,
              load: Future.value(demoTaskDetail(item)),
              onOpenTask: () {},
            ),
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();
    await expectLater(
      find.byType(Scaffold),
      matchesGoldenFile('goldens/organize_sheet.png'),
    );
  });

  final fixtures = Platform.environment['NACHO_DESK_FIXTURES'];
  test(
    '나쵸 작업 상세 고정 자료로 정리한다',
    () {
      final raw =
          jsonDecode(
                File(
                  '$fixtures/task.detail.desk.implemented.json',
                ).readAsStringSync(),
              )
              as Map;
      final task = NachoTaskDetail(
        (raw['task'] as Map).cast<String, Object?>(),
      );
      final v = organize(
        item: ledgerItem(id: task.id),
        task: task,
        nowMs: now,
      );
      final s = slots(v);
      expect(s['막힘'], startsWith('승인이 있어야 이어져요'));
      expect(s['검증'], '검증 기록 없음', reason: 'verify null 은 통과가 아니다');
      expect(s['변경'], contains('학생 보고 전이라 바뀐 파일을 몰라요'));
    },
    skip: fixtures == null
        ? 'NACHO_DESK_FIXTURES 없음 — 나쵸 레포 fixtures 경로를 주면 돈다'
        : false,
  );
}

void _phone(WidgetTester tester) {
  tester.view.physicalSize = const Size(390 * 3, 844 * 3);
  tester.view.devicePixelRatio = 3;
  addTearDown(tester.view.reset);
}

Future<Future<void> Function(WidgetTester)> _pumpBoard(
  WidgetTester tester,
  WorkMode mode,
) async {
  _phone(tester);
  final server = Server(Uri.parse('http://127.0.0.1:1/'));
  final desk = NachoDesk(server);
  final students = StudentLookup(server);
  final modes = WorkModeDesk.demo(state: ModeState(mode: mode, rev: 1));
  await tester.pumpWidget(
    MaterialApp(
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
            modes: modes,
            onOpenTask: (_) {},
            onOpenPane: (_) {},
            onGoChat: () {},
          ),
        ),
      ),
    ),
  );
  await tester.pumpAndSettle();
  return (WidgetTester t) async {
    await t.pumpWidget(const SizedBox());
    desk.dispose();
    students.dispose();
    modes.dispose();
  };
}
