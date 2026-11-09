import 'dart:async';
import 'dart:convert';
import 'dart:math';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/connection.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/screens/shared_rooms.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/shared_rooms_fork.dart';
import 'package:kasaterm_mobile/shared_rooms_model.dart';
import 'package:kasaterm_mobile/shared_rooms_repository.dart';

import 'archive_fixtures.dart';

final _at = DateTime.utc(2026, 11, 10, 3);

SharedSession sessionFor(
  String raw, {
  String id = 's1',
  String generation = 'g1',
  bool checkpoint = false,
  bool project = false,
  String publisher = '',
}) => SharedSession(
  id: id,
  title: '공유한 대화',
  harness: 'claude',
  revision: 1,
  generation: generation,
  bytes: utf8.encode(raw).length,
  updatedAt: _at,
  caughtUp: true,
  publisherMachineId: publisher,
  checkpoint: checkpoint
      ? SharedCheckpoint(id: 'cp1', harness: 'claude', boundaryLabel: '세 번째 답', capturedAt: _at, hasProject: project)
      : null,
);

SharedRoom roomWith({
  String id = 'r1',
  String role = 'member',
  bool allowFork = false,
  List<SharedSession> sessions = const [],
  List<SharedInviteSummary> invites = const [],
}) => SharedRoom(
  id: id,
  title: '같이 보는 방',
  ownerAccount: 'owner-key',
  ownerName: '선배',
  myRole: role,
  allowFork: allowFork,
  revision: 3,
  updatedAt: _at,
  members: [
    const SharedMember(account: 'owner-key', displayName: '선배', role: 'owner'),
    const SharedMember(account: 'alice', displayName: '앨리스', role: 'member'),
  ],
  sessions: sessions,
  invites: invites,
);

AccountException _err(int status, String code) => AccountException(sharedError(code, status), status: status, code: code);

class FakeSharedApi implements SharedRoomsApi {
  final stored = <String, SharedRoom>{};
  final raw = <String, String>{};
  final calls = <String>[];
  int chunk = 1 << 20;
  Object? roomError;
  Object? forkError;
  Completer<void>? listGate;
  bool closed = false;

  @override
  Future<List<SharedRoom>> rooms() async {
    calls.add('rooms');
    if (listGate != null) await listGate!.future;
    return stored.values.toList();
  }

  @override
  Future<SharedRoom> room(String id) async {
    calls.add('room:$id');
    if (roomError case final e?) throw e;
    return stored[id] ?? (throw _err(404, 'shared_missing'));
  }

  @override
  Future<SharedRoom> create(String title, {required bool allowFork, required String requestId}) async {
    calls.add('create:$title:$allowFork:$requestId');
    return stored['new'] = roomWith(id: 'new', role: 'owner', allowFork: allowFork);
  }

  @override
  Future<SharedRoom> update(String id, int expectedRevision, {String? title, bool? allowFork}) async {
    calls.add('update:$id:$expectedRevision:$title:$allowFork');
    return stored[id]!;
  }

  @override
  Future<void> delete(String id) async => calls.add('delete:$id');

  @override
  Future<SharedInvite> invite(String roomId) async {
    calls.add('invite:$roomId');
    return SharedInvite(id: 'i1', roomId: roomId, token: 'INVITE-secret-token', expiresAt: _at, maxUses: 1);
  }

  @override
  Future<void> revokeInvite(String roomId, String inviteId) async => calls.add('revoke:$roomId:$inviteId');

  @override
  Future<SharedRoom?> accept(String token) async {
    calls.add('accept:$token');
    if (token == 'used-token') throw _err(410, 'invite_unavailable');
    return stored['r1'];
  }

  @override
  Future<void> removeMember(String roomId, String account) async => calls.add('remove:$roomId:$account');

  @override
  Future<SharedContent> content(String roomId, String sessionId, {required String generation, required int offset, int? end}) async {
    calls.add('content:$generation:$offset:$end');
    final session = stored[roomId]!.session(sessionId)!;
    if (session.generation != generation) throw _err(409, 'shared_conflict');
    final bytes = utf8.encode(raw[sessionId]!);
    final stop = end ?? bytes.length;
    var next = min(offset + chunk, stop);
    while (next < stop && next > offset && (bytes[next] & 0xc0) == 0x80) {
      next--;
    }
    return SharedContent(
      generation: generation,
      revision: session.revision,
      offset: offset,
      next: next,
      end: stop,
      raw: utf8.decode(bytes.sublist(offset, next)),
    );
  }

  @override
  Future<SharedForkInfo> fork(String roomId, String sessionId, {required String checkpointId, required String requestId}) async {
    calls.add('fork:$checkpointId:$requestId');
    if (forkError case final e?) {
      forkError = null;
      throw e;
    }
    return SharedForkInfo(id: 'f1', roomId: roomId, sessionId: sessionId, checkpointId: checkpointId);
  }

  @override
  void close() => closed = true;
}

class FakeHost implements ForkHost {
  FakeHost({this.machine = 'mine', this.outdated = false});
  final String machine;
  final bool outdated;
  final calls = <String>[];
  final ops = <ForkOperation>[];

  @override
  Future<ForkDestinationStatus> status(String? route) async {
    if (outdated) throw const ForkHostOutdated();
    return ForkDestinationStatus(available: true, authenticated: true, machineId: route == null ? machine : 'other', defaultCwd: '/work');
  }

  @override
  Future<String> start(String? route, {required String forkId, required String requestId, required bool includeProject, String? cwd}) async {
    calls.add('start:$route:$forkId:$requestId:$includeProject:$cwd');
    return 'op1';
  }

  @override
  Future<ForkOperation> operation(String? route, String operationId) async =>
      ops.length > 1 ? ops.removeAt(0) : ops.single;

  @override
  Future<List<PublishSource>> sources(String? route) async => const [];

  @override
  Future<String> publish(String? route, {required String roomId, required List<Map<String, Object?>> addresses, required bool includeProject}) async =>
      'op2';
}

/// 기준 기기만 있는 데스크톱 연결. 다른 경로로 새는 요청은 [leaks] 에 남는다.
Server desktopServer(List<String> leaks) => Server.account(
  archiveSession(),
  connectDirect: false,
  client: MockClient((req) async {
    final path = req.url.path;
    if (path.endsWith('/mobile/me')) return http.Response.bytes(utf8.encode(jsonEncode({'name': 'alice', 'machine': '내 맥'})), 200);
    if (path.endsWith('/machines')) return http.Response(jsonEncode({'machines': []}), 200);
    if (path.endsWith('/account-machines')) return http.Response(jsonEncode({'machines': []}), 200);
    leaks.add('${req.method} $path');
    return http.Response('{}', 404);
  }),
);

String chat(int n) => [for (var i = 0; i < n; i++) message(i.isEven ? 'user' : 'assistant', '한글 메시지 $i — 공유 읽기')].join();

void main() {
  group('model', () {
    test('parses rooms and rejects malformed values', () {
      final json = {
        'rooms': [
          {
            'id': 'r1',
            'title': '방',
            'owner_account': 'k',
            'owner_name': '',
            'my_role': 'member',
            'allow_fork': true,
            'revision': 2,
            'updated_at_ms': 1,
            'members': [
              {'account': 'k', 'display_name': '주인', 'role': 'owner'},
            ],
            'sessions': [
              {
                'id': 's',
                'source_key': '',
                'title': '',
                'harness': 'codex',
                'publisher_machine_id': '',
                'revision': 1,
                'generation': 'g',
                'bytes': 10,
                'cursor': 0,
                'updated_at_ms': 1,
                'caught_up': false,
                'checkpoint': {
                  'id': 'c',
                  'harness': 'codex',
                  'parent_session_id': 'p',
                  'boundary_id': 'b',
                  'boundary_label': '',
                  'captured_at_ms': 1,
                  'has_project': true,
                },
              },
            ],
          },
        ],
      };
      final room = sharedRoomsFrom(json).single;
      expect(room.owner, isFalse);
      expect(room.canFork(room.sessions.single), isTrue);
      expect(room.sessions.single.displayTitle, 'Codex 대화');
      ((json['rooms'] as List).single as Map)['title'] = 'bad\u0007';
      expect(() => sharedRoomsFrom(json), throwsA(isA<SharedRoomsException>()));
    });

    test('content offsets are UTF-8 bytes', () {
      final ok = {'generation': 'g', 'revision': 1, 'offset': 3, 'next': 9, 'end': 9, 'raw': '한글'};
      expect(SharedContent.fromJson(ok).next, 9);
      expect(() => SharedContent.fromJson({...ok, 'next': 5}), throwsA(isA<SharedRoomsException>()));
      expect(() => SharedContent.fromJson({...ok, 'end': 8}), throwsA(isA<SharedRoomsException>()));
    });

    test('invite codes accept pasted text and links only', () {
      expect(parseInviteToken('  abcDEF12_-~.  \n'), 'abcDEF12_-~.');
      expect(parseInviteToken('https://kasa.example/join?token=abcdefgh123'), 'abcdefgh123');
      expect(parseInviteToken('short'), isNull);
      expect(parseInviteToken('has/slash/abc'), isNull);
    });

    test('gateway errors read as user sentences', () {
      expect(sharedError('invite_unavailable', 410), contains('초대 코드'));
      expect(sharedError('fork_disabled', 403), contains('포크'));
      expect(sharedError(null, 404), contains('업데이트'));
      expect(sharedAccessLost('shared_missing', 404), isTrue);
      expect(sharedAccessLost('desktop_only', 403), isFalse);
    });

    test('fork operation requires an address when ready', () {
      expect(() => ForkOperation.fromJson({'status': 'ready'}), throwsA(isA<SharedRoomsException>()));
      final op = ForkOperation.fromJson({
        'status': 'ready',
        'address': {'machine_id': 'm', 'surface_id': '%3', 'surface_key': 'k'},
      });
      expect(op.address!.surfaceId, '%3');
    });

    test('publish sources keep the full address and its machine', () {
      final source = {
        'address': {'machine_id': 'm', 'surface_key': 'k', 'surface_id': '%1', 'session_id': 's', 'instance_id': 'i'},
        'title': '작업',
        'harness': 'claude',
        'ready': true,
      };
      final list = publishSourcesFrom({'machine_id': 'm', 'sources': [source]});
      expect(list.single.address['instance_id'], 'i');
      expect(() => publishSourcesFrom({'machine_id': 'other', 'sources': [source]}), throwsA(isA<SharedRoomsException>()));
      final partial = {...source, 'address': {'machine_id': 'm', 'surface_id': '%1'}};
      expect(() => publishSourcesFrom({'machine_id': 'm', 'sources': [partial]}), throwsA(isA<SharedRoomsException>()));
    });
  });

  test('relay api speaks the documented routes', () async {
    final seen = <String>[];
    final api = RelayAccountApi(
      Uri.parse('https://archive.invalid'),
      session: archiveSession(),
      client: MockClient((req) async {
        seen.add('${req.method} ${req.url.path}${req.url.hasQuery ? '?${req.url.query}' : ''} ${req.body}');
        expect(req.headers['authorization'], 'Bearer fixture-device-token');
        if (req.url.path.endsWith('/accept')) return http.Response(jsonEncode({'ok': false, 'error': 'invite_unavailable'}), 410);
        if (req.url.path.endsWith('/content')) {
          return http.Response(
            jsonEncode({'ok': true, 'content': {'generation': 'g', 'revision': 1, 'offset': 0, 'next': 3, 'end': 3, 'raw': 'ab\n'}}),
            200,
          );
        }
        if (req.url.path.endsWith('/fork')) {
          return http.Response(jsonEncode({'ok': true, 'fork': {'id': 'f', 'room_id': 'r 1', 'session_id': 's', 'checkpoint_id': 'c', 'created_at_ms': 1}}), 200);
        }
        return http.Response(jsonEncode({'ok': true, 'rooms': []}), 200);
      }),
    );
    await api.sharedRooms();
    await expectLater(api.acceptSharedInvite('tok12345'), throwsA(isA<AccountException>().having((e) => e.code, 'code', 'invite_unavailable')));
    final content = await api.sharedContent('r 1', 's', generation: 'g', offset: 0, end: 3);
    expect(content.raw, 'ab\n');
    await api.createSharedFork('r 1', 's', checkpointId: 'c', requestId: 'q');
    await api.deleteSharedRoom('r 1');
    expect(seen, [
      'GET /relay/shared-rooms ',
      'POST /relay/shared-room-invites/accept {"token":"tok12345"}',
      'GET /relay/shared-rooms/r%201/sessions/s/content?generation=g&offset=0&end=3 ',
      'POST /relay/shared-rooms/r%201/sessions/s/fork {"checkpoint_id":"c","request_id":"q"}',
      'DELETE /relay/shared-rooms/r%201 ',
    ]);
    api.close();
  });

  group('repository', () {
    test('late responses after an account switch are dropped', () async {
      final api = FakeSharedApi()..stored['r1'] = roomWith();
      api.listGate = Completer<void>();
      final repo = SharedRoomsRepository(archiveSession(), api: api);
      final loading = repo.load();
      repo.close();
      api.listGate!.complete();
      await loading;
      expect(repo.locked, isTrue);
      expect(repo.rooms, isEmpty);
      expect(api.closed, isTrue);
      await expectLater(repo.issueInvite('r1'), throwsA(isA<SharedRoomsException>()));
      expect(api.calls.where((c) => c.startsWith('invite')), isEmpty);
    });

    test('losing access forgets the room and its readers', () async {
      final raw = chat(2);
      final api = FakeSharedApi()
        ..stored['r1'] = roomWith(sessions: [sessionFor(raw)])
        ..raw['s1'] = raw;
      final repo = SharedRoomsRepository(archiveSession(), api: api);
      final reader = repo.reader('r1', 's1');
      await reader.poll();
      expect(reader.conversation.items, isNotEmpty);
      api.roomError = _err(404, 'shared_missing');
      await reader.poll();
      expect(reader.gone, isNotNull);
      expect(reader.conversation.items, isEmpty);
      expect(repo.room('r1'), isNull);
      expect(repo.gone['r1'], isNotNull);
    });

    test('401 signs out once and locks the repository', () async {
      final api = FakeSharedApi()..roomError = _err(401, 'unauthorized');
      var expired = 0;
      final repo = SharedRoomsRepository(archiveSession(), api: api)..onUnauthorized = () => expired++;
      await expectLater(repo.refreshRoom('r1'), completion(isNull));
      expect(expired, 1);
      expect(repo.locked, isTrue);
    });

    test('leave uses the canonical account key; create passes the form request id', () async {
      final api = FakeSharedApi()..stored['r1'] = roomWith();
      final repo = SharedRoomsRepository(archiveSession(), api: api);
      await repo.load();
      await repo.leave('r1');
      expect(api.calls, contains('remove:r1:alice'));
      expect(repo.room('r1'), isNull);
      await repo.create(' 새 방 ', allowFork: true, requestId: 'form-1');
      expect(api.calls, contains('create:새 방:true:form-1'));
      await expectLater(repo.accept('used-token'), throwsA(isA<SharedRoomsException>().having((e) => e.message, 'message', contains('초대 코드'))));
    });

    test('fork retries reuse one request id', () async {
      final room = roomWith(allowFork: true, sessions: [sessionFor('', checkpoint: true)]);
      final api = FakeSharedApi()
        ..stored['r1'] = room
        ..forkError = const AccountException('끊김');
      final repo = SharedRoomsRepository(archiveSession(), api: api);
      await expectLater(repo.fixFork(room, room.sessions.single), throwsA(isA<SharedRoomsException>()));
      await repo.fixFork(room, room.sessions.single);
      final ids = api.calls.where((c) => c.startsWith('fork:')).toSet();
      expect(api.calls.where((c) => c.startsWith('fork:')), hasLength(2));
      expect(ids, hasLength(1));
      final closed = roomWith(sessions: [sessionFor('', checkpoint: true)]);
      await expectLater(repo.fixFork(closed, closed.sessions.single), throwsA(isA<SharedRoomsException>()));
    });
  });

  group('reader', () {
    test('joins lines split across 256KiB-style chunks by byte offset', () async {
      final raw = chat(4);
      final api = FakeSharedApi()
        ..stored['r1'] = roomWith(sessions: [sessionFor(raw)])
        ..raw['s1'] = raw
        ..chunk = 37;
      final repo = SharedRoomsRepository(archiveSession(), api: api);
      final reader = repo.reader('r1', 's1');
      for (var i = 0; i < 20 && (reader.loading || reader.offset < utf8.encode(raw).length); i++) {
        await reader.poll();
      }
      expect(reader.offset, utf8.encode(raw).length);
      expect(reader.conversation.items.whereType<Object>().length, 4);
      final offsets = api.calls.where((c) => c.startsWith('content')).map((c) => int.parse(c.split(':')[2])).toList();
      expect(offsets.first, 0);
      expect(offsets, orderedEquals([...offsets]..sort()));
    });

    test('holds new lines while reading back and resets on a new generation', () async {
      final first = chat(2);
      final api = FakeSharedApi()
        ..stored['r1'] = roomWith(sessions: [sessionFor(first)])
        ..raw['s1'] = first;
      final repo = SharedRoomsRepository(archiveSession(), api: api);
      final reader = repo.reader('r1', 's1');
      await reader.poll();
      expect(reader.conversation.items, hasLength(2));

      reader.hold = true;
      final more = first + message('assistant', '읽는 동안 온 답');
      api
        ..stored['r1'] = roomWith(sessions: [sessionFor(more)])
        ..raw['s1'] = more;
      await reader.poll();
      expect(reader.fresh, isTrue);
      expect(reader.conversation.items, hasLength(2));
      reader.hold = false;
      expect(reader.fresh, isFalse);
      expect(reader.conversation.items, hasLength(3));

      final fresh = message('user', '새 세대');
      api
        ..stored['r1'] = roomWith(sessions: [sessionFor(fresh, generation: 'g2')])
        ..raw['s1'] = fresh;
      await reader.poll();
      expect(reader.conversation.items, hasLength(1));
      expect(api.calls.last, startsWith('content:g2:0:'));
    });
  });

  group('fork flow', () {
    test('without a desktop it keeps reading and asks for a PC', () async {
      final repo = SharedRoomsRepository(archiveSession(), api: FakeSharedApi());
      final flow = SharedForkFlow(repo, roomId: 'r1', sessionId: 's1', host: (_) => FakeHost());
      await flow.search();
      expect(flow.phase, ForkPhase.choosing);
      expect(flow.message, contains('PC 연결'));
      flow.dispose();
    });

    test('creates on my desktop, verifies the address and never targets the publisher', () async {
      final leaks = <String>[];
      final room = roomWith(allowFork: true, sessions: [sessionFor('', checkpoint: true, project: true)]);
      final api = FakeSharedApi()..stored['r1'] = room;
      final repo = SharedRoomsRepository(archiveSession(), api: api);
      await repo.load();
      repo.desktop = desktopServer(leaks);
      final host = FakeHost()
        ..ops.addAll(const [
          ForkOperation(stage: ForkStage.preparing),
          ForkOperation(stage: ForkStage.ready, address: ForkAddress(machineId: 'mine', surfaceId: '%9', surfaceKey: 'k')),
        ]);
      final flow = SharedForkFlow(repo, roomId: 'r1', sessionId: 's1', host: (_) => host, pollEvery: Duration.zero);
      await flow.search();
      expect(flow.destinations.single.label, '내 맥');
      await flow.start(flow.destinations.single, cwd: '/work/app', includeProject: false);
      expect(flow.phase, ForkPhase.done, reason: flow.message);
      expect(flow.result!.surfaceId, '%9');
      expect(host.calls.single, matches(RegExp(r'^start:null:f1:[0-9a-f]{32}:false:/work/app$')));
      expect(leaks, isEmpty);

      final wrong = FakeHost()..ops.add(const ForkOperation(stage: ForkStage.ready, address: ForkAddress(machineId: 'else', surfaceId: '%1', surfaceKey: '')));
      final again = SharedForkFlow(repo, roomId: 'r1', sessionId: 's1', host: (_) => wrong, pollEvery: Duration.zero);
      await again.search();
      await again.start(again.destinations.single);
      expect(again.phase, ForkPhase.failed);
      expect(again.result, isNull);
      flow.dispose();
      again.dispose();

      final published = roomWith(allowFork: true, sessions: [sessionFor('', checkpoint: true, publisher: 'mine')]);
      api.stored['r1'] = published;
      await repo.refreshRoom('r1');
      final own = SharedForkFlow(repo, roomId: 'r1', sessionId: 's1', host: (_) => FakeHost());
      await own.search();
      expect(own.destinations, isEmpty);
      own.dispose();
    });

    test('an old desktop asks for an update instead of typing into a terminal', () async {
      final room = roomWith(allowFork: true, sessions: [sessionFor('', checkpoint: true)]);
      final repo = SharedRoomsRepository(archiveSession(), api: FakeSharedApi()..stored['r1'] = room);
      await repo.load();
      repo.desktop = desktopServer([]);
      final flow = SharedForkFlow(repo, roomId: 'r1', sessionId: 's1', host: (_) => FakeHost(outdated: true));
      await flow.search();
      expect(flow.destinations, isEmpty);
      expect(flow.message, contains('업데이트'));
      flow.dispose();
    });
  });

  group('screens', () {
    Future<void> phone(WidgetTester tester, Size size) async {
      tester.view.physicalSize = size * 3;
      tester.view.devicePixelRatio = 3;
      addTearDown(tester.view.reset);
    }

    testWidgets('shared session is read-only and never reaches the source desktop', (tester) async {
      await phone(tester, const Size(320, 568));
      final raw = chat(3);
      final leaks = <String>[];
      final api = FakeSharedApi()
        ..stored['r1'] = roomWith(allowFork: true, sessions: [sessionFor(raw, checkpoint: true)])
        ..raw['s1'] = raw;
      final repo = SharedRoomsRepository(archiveSession(), api: api);
      final desktop = desktopServer(leaks);
      repo.desktop = desktop;
      await repo.load();
      await tester.pumpWidget(
        MaterialApp(
          theme: buildTheme(Brightness.light),
          home: SharedSessionScreen(repository: repo, roomId: 'r1', sessionId: 's1'),
        ),
      );
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 100));
      expect(find.textContaining('공유 읽기', findRichText: true), findsWidgets);
      expect(find.textContaining('읽기 전용'), findsOneWidget);
      expect(find.byType(TextField), findsNothing);
      expect(find.text('허락'), findsNothing);
      expect(find.text('내 계정으로 포크'), findsOneWidget);
      expect(leaks, isEmpty);
      expect(tester.takeException(), isNull);

      repo.close();
      await tester.pump();
      expect(find.text('현재 계정의 연결이 종료됐어요.'), findsOneWidget);
      expect(find.textContaining('공유 읽기', findRichText: true), findsNothing);
      await tester.pumpWidget(const SizedBox());
    });

    testWidgets('owner room fits a small phone and the invite code disappears on account switch', (tester) async {
      await phone(tester, const Size(320, 568));
      final api = FakeSharedApi()
        ..stored['r1'] = roomWith(
          role: 'owner',
          allowFork: true,
          invites: [SharedInviteSummary(id: 'i0', expiresAt: _at, maxUses: 1, uses: 0)],
        );
      final repo = SharedRoomsRepository(archiveSession(), api: api);
      await repo.load();
      await tester.pumpWidget(
        MaterialApp(theme: buildTheme(Brightness.light), home: SharedRoomScreen(repository: repo, roomId: 'r1')),
      );
      await tester.pump();
      expect(find.text('초대 코드 만들기'), findsOneWidget);
      expect(find.text('PC 대화 게시하기'), findsOneWidget);
      expect(tester.takeException(), isNull);

      await tester.tap(find.text('초대 코드 만들기'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));
      expect(find.text('INVITE-secret-token'), findsOneWidget);
      repo.close();
      await tester.pump();
      expect(find.text('INVITE-secret-token'), findsNothing);
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox());
    });

    testWidgets('member list fits a small phone and offers leave, not owner actions', (tester) async {
      await phone(tester, const Size(320, 568));
      final repo = SharedRoomsRepository(archiveSession(), api: FakeSharedApi()..stored['r1'] = roomWith());
      await repo.load();
      await tester.pumpWidget(MaterialApp(theme: buildTheme(Brightness.light), home: SharedRoomsScreen(repository: repo)));
      await tester.pump();
      expect(find.text('참여한 방'), findsOneWidget);
      expect(find.text('초대 코드로 참여'), findsOneWidget);
      await tester.tap(find.text('같이 보는 방'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      expect(find.text('이 방에서 나가기'), findsOneWidget);
      expect(find.text('초대 코드 만들기'), findsNothing);
      expect(find.text('공유 끝내기'), findsNothing);
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox());
    });

    testWidgets('waiting for a desktop still opens shared rooms', (tester) async {
      await phone(tester, const Size(320, 568));
      final connection = ConnectionController()
        ..account = archiveSession()
        ..phase = ConnectionPhase.waiting;
      final repo = SharedRoomsRepository(archiveSession(), api: FakeSharedApi()..stored['r1'] = roomWith());
      await tester.pumpWidget(
        MaterialApp(theme: buildTheme(Brightness.light), home: AccountWaitingScreen(connection: connection, shared: repo)),
      );
      await tester.pump();
      await tester.tap(find.text('공유방'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      await tester.pump();
      expect(find.text('같이 보는 방'), findsOneWidget);
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox());
    });
  });
}
