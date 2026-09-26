import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/screens/work_mode_sheet.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/work_mode.dart';

/// 허브 중계(`nacho/app/…`)를 흉내 낸다 — 부른 길·몸통을 적고, 길마다 정한 답을 준다.
class _FakeNacho extends Server {
  _FakeNacho(this.replies) : super(Uri.parse('http://127.0.0.1:1/u/slug/'));

  final Map<String, List<(int, Map<String, Object?>)>> replies;
  final calls = <(String, Map<String, Object?>?)>[];
  bool dropNextPost = false;

  @override
  Future<(int, Map<String, Object?>)> nacho(
    String path, {
    Map<String, String>? query,
    Map<String, Object?>? body,
    Duration timeout = const Duration(seconds: 20),
  }) async {
    calls.add((path, body));
    if (body != null && dropNextPost) {
      dropNextPost = false;
      throw const ServerException('허브에 닿지 못했다');
    }
    final key = '${body == null ? 'GET' : 'POST'} $path';
    final queue = replies[key];
    if (queue == null || queue.isEmpty) return (404, <String, Object?>{});
    return queue.length == 1 ? queue.first : queue.removeAt(0);
  }

  List<Map<String, Object?>> get posts => [
    for (final c in calls)
      if (c.$2 != null) c.$2!,
  ];
}

(int, Map<String, Object?>) mode(String m, int rev, {int? at, String? by}) => (
  200,
  {
    'ok': true,
    'work_mode': {
      'schema': 'nacho-work-mode/1',
      'mode': m,
      'rev': rev,
      'changed_at_ms': at,
      'changed_by': by,
    },
  },
);

final caps = <String, Object?>{
  'ok': true,
  'approvals': {
    'enabled': true,
    'http_actions': ['kasaterm_restart'],
    'decide_in_app': false,
    'decide_via': 'owner_dm_button',
  },
  'work_mode': {
    'modes': [
      {
        'mode': 'organize',
        'label': '정리',
        'summary': '직접 일한다',
        'effects': ['정리만 한다'],
      },
      {'mode': 'coordinate', 'label': '조율', 'summary': '맡긴다', 'effects': []},
      {'mode': 'yolo', 'label': '무확인', 'summary': '묻지 않는다'},
    ],
    'default': 'coordinate',
    'is_permission': false,
  },
  'policy': {
    'tiers': [
      {
        'tier': 'delegated',
        'label': '맡긴 범위',
        'rule': '묻지 않고 한다',
        'actions': [
          {'id': 'read', 'label': '조회', 'how': 'none'},
          'local_commit',
        ],
      },
      {
        'tier': 'confirm_once',
        'label': '한 번 확인',
        'rule': '대상·범위·만료를 묶는다',
        'actions': [
          {'id': 'merge_pr', 'label': '머지', 'how': 'button'},
          {'id': 'kasaterm_restart', 'label': '앱 재시작', 'how': 'approval'},
          {'id': 'install', 'label': '설치', 'how': 'card'},
          {'id': 'deploy', 'label': '배포', 'how': 'card'},
        ],
      },
    ],
    'unlimited_mode': false,
    'notes': ['모드를 바꿔도 이 표는 그대로다'],
  },
};

void main() {
  test('모드는 나쵸의 두 이름뿐 — 모르는 이름을 가까운 모드로 짐작하지 않는다', () {
    expect(WorkMode.parse('organize'), WorkMode.organize);
    expect(WorkMode.parse('coordinate'), WorkMode.coordinate);
    for (final w in ['yolo', 'Organize', '', null, 1]) {
      expect(WorkMode.parse(w), isNull, reason: '$w');
    }
    expect(
      ModeState.fromJson({'mode': 'organize'}),
      isNull,
      reason: 'rev 없는 판은 받지 않는다',
    );
    expect(ModeState.fromJson({'mode': 'unknown', 'rev': 1}), isNull);
  });

  test('기능 안내 — 모르는 모드는 버리고 권한 표·무제한 여부는 나쵸 말 그대로', () {
    final c = Capabilities.fromJson(caps);
    expect(c.modes.map((m) => m.mode), [
      WorkMode.organize,
      WorkMode.coordinate,
    ]);
    expect(c.info(WorkMode.organize)!.effects, ['정리만 한다']);
    expect(c.unlimitedMode, isFalse);
    expect(c.approvals!.enabled, isTrue);
    expect(c.approvals!.decideInApp, isFalse);
    final actions = [for (final t in c.tiers) ...t.actions];
    expect(actions.map((a) => '${a.id}:${a.howWord}'), [
      'read:묻지 않음',
      'local_commit:확인 방식 미확인',
      'merge_pr:확인 단추',
      'kasaterm_restart:승인(범위·해시·만료·1회)',
      'install:갈림길 카드',
      'deploy:갈림길 카드',
    ]);
    expect(
      Capabilities.fromJson({}).unlimitedMode,
      isNull,
      reason: '말 안 한 것을 「없다」로 적지 않는다',
    );
  });

  test('옛 나쵸는 기본 404 뿐 — 허브 오류어와 가른다', () {
    expect(isOldNacho(404, {}), isTrue);
    expect(isOldNacho(404, {'error': 'not_found'}), isTrue);
    expect(isOldNacho(404, {'error': 'bad_path'}), isFalse);
    expect(isOldNacho(503, {'error': 'nacho_key_missing'}), isFalse);
  });

  test('읽기는 GET 만 — 열어 보기만으로 모드를 쓰지 않는다', () async {
    final nacho = _FakeNacho({
      'GET work-mode': [mode('coordinate', 0)],
      'GET capabilities': [(200, caps)],
    });
    final desk = WorkModeDesk(nacho);
    await desk.load();
    expect(desk.state!.mode, WorkMode.coordinate);
    expect(desk.state!.rev, 0);
    expect(desk.caps!.tiers, hasLength(2));
    expect(desk.writeBlock, isNull);
    expect(nacho.posts, isEmpty);
    await desk.choose(WorkMode.coordinate);
    expect(nacho.posts, isEmpty, reason: '같은 모드를 고르면 보내지 않는다');
  });

  test('고를 때 한 번 — 본 rev 그대로(정수)와 새 nonce', () async {
    final nacho = _FakeNacho({
      'GET work-mode': [mode('coordinate', 4)],
      'GET capabilities': [(200, caps)],
      'POST work-mode': [
        mode('organize', 5, at: 1790449200000, by: 'app:%EC%A3%BC%EC%9D%B8'),
      ],
    });
    final desk = WorkModeDesk(nacho);
    await desk.load();
    await desk.choose(WorkMode.organize);
    expect(nacho.posts, hasLength(1));
    final body = nacho.posts.single;
    expect(body['mode'], 'organize');
    expect(body['rev'], 4);
    expect(body['rev'], isA<int>());
    expect((body['nonce'] as String).length, inInclusiveRange(8, 64));
    expect(desk.state!.mode, WorkMode.organize);
    expect(desk.state!.rev, 5);
    expect(desk.notice, isNull);
  });

  test('그사이 바뀌었으면(409) 나쵸의 판으로 바꾸고 다시 보내지 않는다', () async {
    final nacho = _FakeNacho({
      'GET work-mode': [mode('coordinate', 1)],
      'GET capabilities': [(200, caps)],
      'POST work-mode': [
        (409, {...mode('organize', 2).$2, 'ok': false, 'error': 'stale_rev'}),
      ],
    });
    final desk = WorkModeDesk(nacho);
    await desk.load();
    await desk.choose(WorkMode.organize);
    expect(nacho.posts, hasLength(1));
    expect(desk.state!.rev, 2);
    expect(desk.notice, contains('다른 곳에서 모드가 바뀌었어요'));
  });

  test('결과를 모르면 다시 쓰지 않고 다시 읽는다', () async {
    final nacho = _FakeNacho({
      'GET work-mode': [mode('coordinate', 1), mode('organize', 2)],
      'GET capabilities': [(200, caps)],
    })..dropNextPost = true;
    final desk = WorkModeDesk(nacho);
    await desk.load();
    await desk.choose(WorkMode.organize);
    expect(nacho.posts, hasLength(1));
    expect(nacho.calls.last.$1, 'work-mode');
    expect(nacho.calls.last.$2, isNull, reason: '마지막은 다시 읽기');
    expect(desk.state!.mode, WorkMode.organize);
    expect(desk.writing, isFalse);
  });

  test('옛 나쵸·키 없는 허브는 모드를 지어내지 않고 단추를 끈다', () async {
    final old = _FakeNacho({});
    final a = WorkModeDesk(old);
    await a.load();
    expect(a.state, isNull);
    expect(a.oldNacho, isTrue);
    expect(a.writeBlock, contains('작업 모드를 몰라요'));
    expect(a.capsProblem, contains('기능 안내'));
    await a.choose(WorkMode.organize);
    expect(old.posts, isEmpty);

    final keyless = _FakeNacho({
      'GET work-mode': [
        (503, {'ok': false, 'error': 'nacho_key_missing'}),
      ],
      'GET capabilities': [
        (503, {'ok': false, 'error': 'nacho_key_missing'}),
      ],
    });
    final b = WorkModeDesk(keyless);
    await b.load();
    expect(b.oldNacho, isFalse);
    expect(b.writeBlock, contains('나쵸 키가 없다'));
    await b.choose(WorkMode.organize);
    expect(keyless.posts, isEmpty);
  });

  test('예시 판은 나쵸에 묻지도 쓰지도 않는다', () async {
    final desk = demoModes();
    await desk.load();
    await desk.choose(WorkMode.organize);
    expect(desk.state!.mode, WorkMode.coordinate);
    expect(desk.writeBlock, contains('예시'));
  });

  testWidgets('시트 — 모드를 누르면 한 번 쓰고, 권한 표는 줄을 다 보인다', (tester) async {
    final nacho = _FakeNacho({
      'GET work-mode': [mode('coordinate', 3)],
      'GET capabilities': [(200, caps)],
      'POST work-mode': [mode('organize', 4)],
    });
    final desk = WorkModeDesk(nacho);
    await desk.load();
    await _pumpSheet(tester, desk);
    expect(find.text('지금'), findsOneWidget);
    await tester.tap(
      find.descendant(
        of: find.byType(SegmentedButton<WorkMode>),
        matching: find.text('정리'),
      ),
    );
    await tester.pumpAndSettle();
    expect(nacho.posts, hasLength(1));
    expect(nacho.posts.single['mode'], 'organize');
    expect(desk.state!.mode, WorkMode.organize);
    for (final label in ['조회', 'local_commit', '머지', '앱 재시작', '설치', '배포']) {
      await tester.scrollUntilVisible(
        find.text(label),
        80,
        scrollable: find.byType(Scrollable).first,
      );
      expect(find.text(label), findsOneWidget, reason: label);
    }
    for (final fact in [
      '없어요 — 확인 없이',
      '지원 안 해요',
      '켜짐 · 받는 동작 앱 재시작',
      '모드를 바꿔도 이 표는 그대로다',
    ]) {
      await tester.scrollUntilVisible(
        find.textContaining(fact),
        80,
        scrollable: find.byType(Scrollable).first,
      );
      expect(find.textContaining(fact), findsOneWidget, reason: fact);
    }

    desk.dispose();
  });

  testWidgets('옛 나쵸면 모드 단추가 꺼지고 까닭을 적는다', (tester) async {
    final nacho = _FakeNacho({});
    final desk = WorkModeDesk(nacho);
    await desk.load();
    await _pumpSheet(tester, desk);
    final seg = tester.widget<SegmentedButton<WorkMode>>(
      find.byType(SegmentedButton<WorkMode>),
    );
    expect(seg.onSelectionChanged, isNull);
    expect(seg.selected, isEmpty, reason: '모르는 모드를 골라 둔 것처럼 그리지 않는다');
    expect(find.textContaining('작업 모드를 몰라요'), findsOneWidget);
    await tester.tap(
      find.descendant(
        of: find.byType(SegmentedButton<WorkMode>),
        matching: find.text('정리'),
      ),
    );
    await tester.pumpAndSettle();
    expect(nacho.posts, isEmpty);
    desk.dispose();
  });

  testWidgets('예시 시트 골든', (tester) async {
    final desk = demoModes();
    await _pumpSheet(tester, desk);
    await expectLater(
      find.byType(Scaffold),
      matchesGoldenFile('goldens/work_mode_sheet.png'),
    );
    desk.dispose();
  });

  final fixtures = Platform.environment['NACHO_DESK_FIXTURES'];
  test(
    '나쵸 고정 자료와 같은 모양으로 읽는다',
    () {
      Map<String, Object?> read(String name) =>
          (jsonDecode(File('$fixtures/$name').readAsStringSync()) as Map)
              .cast<String, Object?>();
      final wm = read('work_mode.implemented.json');
      final get = (wm['get'] as Map).cast<String, Object?>();
      final set = ModeState.fromJson((get['200'] as Map)['work_mode'])!;
      expect(set.rev, isA<int>());
      expect(set.changedBy, startsWith('app:'));
      final never = ModeState.fromJson(
        (get['200_never_set'] as Map)['work_mode'],
      )!;
      expect(never.mode, WorkMode.coordinate);
      expect(never.changedAtMs, isNull);
      final stale = ((wm['post'] as Map)['409_stale_rev'] as Map)
          .cast<String, Object?>();
      expect(stale['error'], 'stale_rev');
      expect(ModeState.fromJson(stale['work_mode']), isNotNull);

      final c = Capabilities.fromJson(
        (read('capabilities.implemented.json')['200'] as Map)
            .cast<String, Object?>(),
      );
      expect(c.modes.map((m) => m.mode), WorkMode.values);
      expect(c.unlimitedMode, isFalse);
      expect(c.approvals!.decideInApp, isFalse);
      final actions = [for (final t in c.tiers) ...t.actions];
      expect(actions, isNotEmpty);
      for (final a in actions) {
        expect(
          a.howWord,
          isNot('확인 방식 미확인'),
          reason: '${a.id} 의 how(${a.how})를 모른다',
        );
      }
      for (final id in [
        'install',
        'restart',
        'delete',
        'merge_pr',
        'deploy',
        'permission',
      ]) {
        expect(
          actions.firstWhere((a) => a.id == id).how,
          isNot('none'),
          reason: id,
        );
      }
    },
    skip: fixtures == null
        ? 'NACHO_DESK_FIXTURES 없음 — 나쵸 레포 fixtures 경로를 주면 돈다'
        : false,
  );
}

Future<void> _pumpSheet(WidgetTester tester, WorkModeDesk desk) async {
  tester.view.physicalSize = const Size(390 * 3, 844 * 3);
  tester.view.devicePixelRatio = 3;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    MaterialApp(
      theme: ThemeData(colorSchemeSeed: const Color(0xff4a90e2)),
      home: MediaQuery(
        data: const MediaQueryData(
          size: Size(390, 844),
          disableAnimations: true,
        ),
        child: Scaffold(body: WorkModeSheet(desk: desk)),
      ),
    ),
  );
  await tester.pumpAndSettle();
}
