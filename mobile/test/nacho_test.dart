import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/nacho.dart';
import 'package:kasaterm_mobile/screens/nacho_home.dart';
import 'package:kasaterm_mobile/screens/nacho_task.dart';
import 'package:kasaterm_mobile/server.dart';

/// 나쵸 창구의 원장·작업을 흉내 내는 서버. 폰이 무엇을 물었는지 적어 둔다.
class FakeNacho extends Server {
  FakeNacho() : super(Uri.parse('http://127.0.0.1:1/u/slug/'));

  final List<Map<String, Object?>> ledger = [];
  final List<(String, Map<String, String>?, Map<String, Object?>?)> asked = [];
  final Map<String, Map<String, Object?>> details = {};
  List<Map<String, Object?>> cards = [];
  int dropNextPosts = 0;
  (int, Map<String, Object?>)? postReply;
  Completer<void>? hold;
  List<Map<String, Object?>> pets = [];
  String? linked;

  int get lastSeq => ledger.isEmpty ? 0 : ledger.last['seq'] as int;

  void add(Map<String, Object?> e) => ledger.add({'seq': lastSeq + 1, 'at_ms': 1, ...e});

  @override
  Future<(int, Map<String, Object?>)> nacho(
    String path, {
    Map<String, String>? query,
    Map<String, Object?>? body,
    Duration timeout = const Duration(seconds: 20),
  }) async {
    asked.add((path, query, body));
    if (path == 'events') {
      if (query?['wait'] != null) {
        await (hold ??= Completer<void>()).future;
        hold = null;
      }
      final after = int.tryParse(query?['after'] ?? '') ?? 0;
      final tail = int.tryParse(query?['tail'] ?? '');
      final evs = tail != null
          ? ledger.skip(ledger.length > tail ? ledger.length - tail : 0).toList()
          : [for (final e in ledger) if ((e['seq'] as int) > after) e];
      return (200, {'ok': true, 'events': evs, 'last_seq': lastSeq});
    }
    if (path == 'messages') {
      if (dropNextPosts > 0) {
        dropNextPosts--;
        throw const ServerException('닿지 못했다');
      }
      final r = postReply;
      if (r != null) return r;
      final id = body!['id'] as String;
      if (!ledger.any((e) => e['kind'] == 'message' && e['id'] == id)) {
        add({'kind': 'message', 'id': id, 'text': body['text'], 'task': body['task']});
      }
      return (200, {'ok': true, 'receipt': {'id': id, 'state': 'accepted'}});
    }
    if (path == 'pets') {
      return (200, {'ok': true, 'linked': linked, 'pets': [
        for (final p in pets) {...p, 'linked': p['conv'] == linked},
      ]});
    }
    if (path == 'pets/link') {
      final conv = body!['conv'] as String;
      if (conv.isNotEmpty && !pets.any((p) => p['conv'] == conv)) return (404, {'ok': false, 'error': 'no_pet'});
      linked = conv.isEmpty ? null : conv;
      add({'kind': 'notice', 'notice': 'link', 'target': linked, 'text': '연결 바뀜'});
      return (200, {'ok': true, 'linked': linked});
    }
    if (path == 'tasks') {
      return (200, {'ok': true, 'tasks': cards, 'groups': {
        for (final g in ['attention', 'active', 'closed'])
          g: cards.where((c) => c['group'] == g).length,
      }});
    }
    if (path.startsWith('tasks/')) {
      final d = details[path.substring(6)];
      return d == null ? (404, {'ok': false, 'error': 'no_task'}) : (200, {'ok': true, 'task': d});
    }
    return (404, <String, Object?>{'error': 'no_route'});
  }
}

Map<String, Object?> card(String id, String goal, String group, {String project = 'kasaterm', String state = 'working'}) => {
  'id': id, 'goal': goal, 'group': group, 'project': project, 'state': state,
  'state_label': {'working': '진행 중', 'approval_needed': '승인 대기', 'failed': '실패', 'done': '끝남'}[state] ?? state,
  'place': '카사모바일', 'step': '학생 보고 확인 중', 'attention': group == 'attention' ? '승인 대기: 크론을 켜면 DM 이 나간다' : '',
  'paused': false, 'rev': '100', 'updated_ms': DateTime.now().millisecondsSinceEpoch - 180000,
};

Future<void> settle(WidgetTester tester) async {
  for (var i = 0; i < 5; i++) {
    await tester.pump(const Duration(milliseconds: 50));
  }
}

void main() {
  group('원장 이어 받기', () {
    test('처음엔 최근 줄, 그 뒤로는 마지막 순번 뒤만 — 같은 줄은 두 번 안 넣는다', () async {
      final s = FakeNacho()
        ..add({'kind': 'message', 'id': 'a', 'text': '안녕'})
        ..add({'kind': 'reply', 'message': 'a', 'text': '안녕이양'});
      final d = NachoDesk(s);
      await d.start();
      expect(d.events.map((e) => e.seq), [1, 2]);
      expect(s.asked.first.$2, {'tail': '200'});
      s.add({'kind': 'notice', 'notice': 'watch', 'text': '학생이 끝났대'});
      s.hold!.complete();
      await Future<void>.delayed(const Duration(milliseconds: 20));
      final polls = s.asked.where((a) => a.$1 == 'events' && a.$2?['wait'] != null).toList();
      expect(polls.first.$2?['after'], '2', reason: '마지막으로 본 순번 뒤부터');
      expect(d.events.map((e) => e.seq), [1, 2, 3]);
      d.stop();
      s.hold?.complete();
    });

    test('접수 상태는 원장의 마지막 status 를 따른다', () async {
      final s = FakeNacho()
        ..add({'kind': 'message', 'id': 'a', 'text': '해줘'})
        ..add({'kind': 'status', 'message': 'a', 'state': 'queued', 'note': '앞 턴이 끝나면 이어서 돌아요'})
        ..add({'kind': 'progress', 'message': 'a', 'text': 'bash: ls'})
        ..add({'kind': 'status', 'message': 'a', 'state': 'running'});
      final d = NachoDesk(s);
      await d.start();
      expect(d.stateOf('a'), 'running');
      expect(d.progressOf('a'), 'bash: ls');
      expect(receiptLabel('queued'), '앞 턴이 끝나면 이어서');
      d.stop();
      s.hold?.complete();
    });
  });

  group('보내기', () {
    test('끊기면 같은 id 로 다시 보낸다 — 한 번만 접수된다', () async {
      final s = FakeNacho()..dropNextPosts = 2;
      final d = NachoDesk(s);
      final id = await d.send('빨간 배경부터 봐');
      final posts = s.asked.where((a) => a.$1 == 'messages').toList();
      expect(posts.length, 3);
      expect(posts.map((p) => p.$3!['id']).toSet(), {id}, reason: '재시도도 처음 id 그대로');
      expect(s.ledger.where((e) => e['kind'] == 'message').length, 1);
    });

    test('서버가 거절하면 까닭이 그대로 올라오고, 판이 바뀐 일은 새 카드가 딸려 온다', () async {
      final s = FakeNacho()
        ..postReply = (409, {'ok': false, 'error': 'stale_rev', 'task': card('w1', '화면 고쳐', 'active')});
      final d = NachoDesk(s);
      await expectLater(
        d.send('이거', task: 'w1', rev: '99'),
        throwsA(isA<NachoError>()
            .having((e) => e.code, 'code', 'stale_rev')
            .having((e) => e.task?.id, 'task', 'w1')
            .having((e) => e.message, 'message', contains('작업이 바뀌었다'))),
      );
      expect(d.outgoing, isEmpty, reason: '거절된 말은 보내는 중으로 남기지 않는다');
      s.postReply = (503, {'ok': false, 'error': 'app_key_missing'});
      await expectLater(d.send('x'), throwsA(isA<NachoError>().having((e) => e.message, 'm', contains('키 설정'))));
    });
  });

  group('펫과 이어 보기', () {
    testWidgets('답 아래에 펫 전달 상태 — 표시됨·대기·만료·폰에만', (tester) async {
      final s = FakeNacho()
        ..add({'kind': 'message', 'id': 'a', 'text': '하나'})
        ..add({'kind': 'reply', 'message': 'a', 'text': '답 하나'})
        ..add({'kind': 'deliver', 'reply': 2, 'state': 'queued', 'target': 'kasapet:미니'})
        ..add({'kind': 'deliver', 'reply': 2, 'state': 'delivered', 'target': 'kasapet:미니'})
        ..add({'kind': 'message', 'id': 'b', 'text': '둘'})
        ..add({'kind': 'reply', 'message': 'b', 'text': '답 둘'})
        ..add({'kind': 'deliver', 'reply': 6, 'state': 'queued', 'target': 'kasapet:맥북'})
        ..add({'kind': 'message', 'id': 'c', 'text': '셋'})
        ..add({'kind': 'reply', 'message': 'c', 'text': '답 셋'})
        ..add({'kind': 'deliver', 'reply': 9, 'state': 'no_target'})
        ..add({'kind': 'message', 'id': 'd', 'text': '넷'})
        ..add({'kind': 'reply', 'message': 'd', 'text': '답 넷'})
        ..add({'kind': 'deliver', 'reply': 12, 'state': 'expired', 'target': 'kasapet:맥북'});
      final d = NachoDesk(s);
      await tester.pumpWidget(MaterialApp(home: NachoHome(server: s, onChangeAddress: () async {}, desk: d)));
      await settle(tester);
      expect(find.text('펫(미니)에도 표시됨'), findsOneWidget, reason: '받아 간 영수증이 있을 때만');
      expect(find.text('펫(맥북) 우편함 대기 — 펫이 받아 가면 표시'), findsOneWidget);
      expect(find.text('연결된 펫 없음 — 폰에만'), findsOneWidget);
      expect(find.text('펫(맥북)이 받아 가기 전에 만료'), findsOneWidget);
      d.stop();
      s.hold?.complete();
    });

    testWidgets('펫 고르기 — 꺼진 펫은 꺼져 있다고, 한 대만 잇는다', (tester) async {
      final s = FakeNacho()
        ..pets = [
          {'conv': 'kasapet:미니', 'place': '미니', 'alive': true, 'seen_ago_s': 5, 'can_receive': true},
          {'conv': 'kasapet:맥북', 'place': '맥북', 'alive': false, 'seen_ago_s': 7200, 'can_receive': true},
          {'conv': 'kasapet:옛판', 'place': '옛판', 'alive': true, 'seen_ago_s': 3, 'can_receive': false},
        ];
      final d = NachoDesk(s);
      await tester.pumpWidget(MaterialApp(home: NachoHome(server: s, onChangeAddress: () async {}, desk: d)));
      await settle(tester);
      await tester.tap(find.byTooltip('펫 연결 — 지금은 폰에만'));
      await settle(tester);
      expect(find.text('켜져 있음'), findsOneWidget);
      expect(find.text('꺼져 있음 · 2시간 전까지'), findsOneWidget, reason: '떠 있다고 짐작하지 않는다');
      expect(find.text('켜져 있음 · 말풍선 받기 확인 안 됨(우편함을 끌어가는 펫 판 필요)'), findsOneWidget,
          reason: '묻기만 하는 옛 판은 받을 수 있다고 하지 않는다');
      await tester.tap(find.widgetWithText(FilledButton, '잇기').first);
      await settle(tester);
      expect(s.linked, 'kasapet:미니');
      expect(s.asked.where((a) => a.$1 == 'pets/link').single.$3, {'conv': 'kasapet:미니'});
      expect(find.text('연결됨 · 켜져 있음'), findsOneWidget);
      d.stop();
      s.hold?.complete();
    });

    testWidgets('펫이 하나도 없으면 없다고 말한다', (tester) async {
      final s = FakeNacho();
      final d = NachoDesk(s);
      await tester.pumpWidget(MaterialApp(home: NachoHome(server: s, onChangeAddress: () async {}, desk: d)));
      await settle(tester);
      await tester.tap(find.byTooltip('펫 연결 — 지금은 폰에만'));
      await settle(tester);
      expect(find.text('이을 수 있는 펫이 없어요'), findsOneWidget);
      d.stop();
      s.hold?.complete();
    });
  });

  group('화면', () {
    testWidgets('대화: 내 말·접수 상태·나쵸 답·기존 창구 확인 필요·작업 보기', (tester) async {
      final s = FakeNacho()
        ..add({'kind': 'message', 'id': 'a', 'text': '카사텀 폰 화면 고쳐'})
        ..add({'kind': 'status', 'message': 'a', 'state': 'answered'})
        ..add({'kind': 'reply', 'message': 'a', 'task': 'w1', 'text': '코유키에게 맡겼어'})
        ..add({'kind': 'message', 'id': 'b', 'text': '그 PR 머지해'})
        ..add({'kind': 'status', 'message': 'b', 'state': 'queued'})
        ..add({'kind': 'notice', 'notice': 'confirm_needed', 'message': 'b', 'text': '기존 창구 확인 필요 — PR 머지'})
        ..add({'kind': 'message', 'id': 'pet-1', 'text': '다들 뭐 해', 'surface': 'pet', 'place': '미니'})
        ..add({'kind': 'reply', 'message': 'pet-1', 'text': '유우카가 펫 고치는 중', 'surface': 'pet', 'place': '미니'})
        ..add({'kind': 'status', 'message': 'pet-1', 'state': 'answered'})
        ..cards = [card('w1', '카사텀 폰 화면 고쳐', 'active')]
        ..details['w1'] = {...card('w1', '카사텀 폰 화면 고쳐', 'active'), 'request': '카사텀 폰 화면 고쳐',
          'verify': null, 'report': null, 'approval': null, 'history': [], 'hops': [],
          'preview': {'url': null, 'shot': false}, 'can_direct': true, 'events': [], 'remaining': []};
      final d = NachoDesk(s);
      await tester.pumpWidget(MaterialApp(home: NachoHome(server: s, onChangeAddress: () async {}, desk: d)));
      await settle(tester);
      expect(find.text('카사텀 폰 화면 고쳐'), findsOneWidget);
      expect(find.text('답함'), findsOneWidget, reason: '앱에서 한 말은 출처 없이 접수 상태만');
      expect(find.text('코유키에게 맡겼어'), findsOneWidget);
      expect(find.text('앞 턴이 끝나면 이어서'), findsOneWidget);
      expect(find.textContaining('기존 창구 확인 필요'), findsOneWidget);
      expect(find.text('펫(미니)에서 · 답함'), findsOneWidget, reason: '어느 화면에서 한 말인지 보인다');
      expect(find.text('펫(미니)에서'), findsOneWidget);
      await tester.tap(find.textContaining('작업 보기'));
      await settle(tester);
      expect(find.text('검증 기록 없음'), findsOneWidget, reason: '적힌 검증이 없으면 없다고 쓴다');
      expect(find.text('미리보기'), findsNothing, reason: '없는 사진을 만들어 보이지 않는다');
      expect(find.widgetWithText(TextField, '이 작업에 방향 주기'), findsOneWidget);
      d.stop();
      s.hold?.complete();
    });

    testWidgets('작업: 판단 필요·진행·끝남으로 나뉘고 프로젝트로 거른다', (tester) async {
      final s = FakeNacho()
        ..cards = [
          card('w1', '크론 켜기', 'attention', project: 'mission-control', state: 'approval_needed'),
          card('w2', '폰 화면 고쳐', 'active'),
          card('w3', '푸시 재발송', 'closed', state: 'failed'),
        ];
      final d = NachoDesk(s);
      await tester.pumpWidget(MaterialApp(home: NachoHome(server: s, onChangeAddress: () async {}, desk: d)));
      await settle(tester);
      await tester.tap(find.text('작업'));
      await settle(tester);
      await tester.pump(const Duration(milliseconds: 400));
      expect(find.text('판단 필요 1'), findsOneWidget);
      expect(find.text('진행 중 1'), findsOneWidget);
      expect(find.text('끝남·실패 1'), findsOneWidget);
      await tester.tap(find.widgetWithText(FilterChip, 'mission-control'));
      await settle(tester);
      expect(find.text('크론 켜기'), findsOneWidget);
      expect(find.text('폰 화면 고쳐'), findsNothing);
      d.stop();
      s.hold?.complete();
    });

    testWidgets('상세: 승인은 기존 창구 확인 필요, 판이 바뀌면 알리고 다시 읽는다', (tester) async {
      final s = FakeNacho()
        ..details['w1'] = {...card('w1', '크론 켜기', 'attention', state: 'approval_needed'), 'request': '크론 켜기',
          'verify': {'ok': true, 'note': 'opt-in 0 확인', 'at_ms': 1}, 'report': null,
          'approval': {'needed': true, 'what': '켜면 DM 이 나간다', 'note': '기존 창구 확인 필요 — 이 승인은 아직 앱에서 할 수 없어요.'},
          'history': [{'at_ms': 1, 'state': 'approval_needed', 'why': '켜기 전 승인'}], 'hops': [],
          'preview': {'url': null, 'shot': false}, 'can_direct': true, 'events': [], 'remaining': []}
        ..postReply = (409, {'ok': false, 'error': 'stale_rev'});
      final d = NachoDesk(s);
      await tester.pumpWidget(MaterialApp(home: NachoTaskScreen(desk: d, taskId: 'w1', onOpenStudents: () {})));
      await settle(tester);
      expect(find.text('승인 대기'), findsWidgets);
      expect(find.textContaining('기존 창구 확인 필요'), findsOneWidget);
      expect(find.text('검증 통과 · opt-in 0 확인'), findsOneWidget);
      final reads = s.asked.where((a) => a.$1 == 'tasks/w1').length;
      await tester.enterText(find.byType(TextField), '테스트 채널로만');
      await tester.tap(find.byTooltip('보내기'));
      await settle(tester);
      expect(find.textContaining('작업이 바뀌었다'), findsOneWidget);
      expect(s.asked.where((a) => a.$1 == 'tasks/w1').length, greaterThan(reads));
      expect(s.asked.lastWhere((a) => a.$1 == 'messages').$3!['rev'], '100', reason: '본 판(rev)을 싣는다');
    });
  });
}
