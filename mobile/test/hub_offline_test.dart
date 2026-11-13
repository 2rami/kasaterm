import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/approvals.dart';
import 'package:kasaterm_mobile/archive_model.dart';
import 'package:kasaterm_mobile/archive_repository.dart';
import 'package:kasaterm_mobile/connection_store.dart';
import 'package:kasaterm_mobile/conversation_cache.dart';
import 'package:kasaterm_mobile/hub_model.dart';
import 'package:kasaterm_mobile/hub_snapshot.dart';
import 'package:kasaterm_mobile/hub_snapshot_disk.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/screens/hub.dart';
import 'package:kasaterm_mobile/screens/offline_hub.dart';
import 'package:kasaterm_mobile/screens/saved_conversations.dart';
import 'package:kasaterm_mobile/server.dart';

import 'archive_fixtures.dart';

const _mini = '~mini';
const _hub = '~machine-a';

Pane _pane(
  String id, {
  String status = 'idle',
  String? kind,
  String? machine = _hub,
  int window = 0,
  String name = '미쿠',
  String? surfaceKey,
  int? busySecs,
  List<String> subagents = const [],
}) => Pane(
  id: id,
  name: name,
  title: '◐ 작업 $id',
  status: status,
  kind: kind,
  window: window,
  cwd: '/workspace/project',
  slug: 'miku',
  machine: machine,
  harness: 'claude',
  session: '세션 $id',
  surfaceKey: surfaceKey ?? 'key-$id',
  busySecs: busySecs,
  subagents: subagents,
);

const _layout = WindowLayout(
  idx: 0,
  active: true,
  aspect: 1.6,
  name: '교실',
  cwd: '~/project',
  rects: [
    PaneRect(surface: '%0', x: 0, y: 0, w: 50, h: 100),
    PaneRect(surface: '%1', x: 50, y: 0, w: 50, h: 100, tabs: ['%1', '%2'], tabActive: 1),
  ],
);

/// 파일 대신 문자열을 붙드는 디스크 — 같은 것을 새 저장소에 넘기면 앱을 다시 켠 것과 같다.
class _MemoryDisk implements HubSnapshotDisk {
  final files = <String, String>{};
  @override
  Future<String?> read(String key) async => files[key];
  @override
  Future<void> write(String key, String text) async => files[key] = text;
  @override
  Future<void> delete(String key) async => files.remove(key);
}

/// 주소 기기·원격 기기를 손으로 끊었다 붙였다 하는 서버.
class _Desktop extends Server {
  _Desktop() : super(Uri.parse('http://127.0.0.1:9/'));

  bool rootUp = true;
  bool rosterUp = true;
  bool miniOnline = true;
  List<Pane> rootRows = [
    _pane('%0', status: 'working', busySecs: 300, subagents: ['찾기']),
    _pane('%1', status: 'waiting', kind: 'permission'),
    _pane('%2'),
  ];
  List<Pane> mini = [_pane('%7', machine: _mini, status: 'working', name: '린')];

  @override
  Future<List<Pane>> panes({String? machine}) async {
    if (machine == null) {
      if (!rootUp) throw const ServerException('닿지 못했다');
      return rootRows;
    }
    if (!miniOnline) throw const ServerException('끊김');
    return mini;
  }

  @override
  Future<List<String>> sessions() async {
    if (!rootUp) throw const ServerException('닿지 못했다');
    return ['교실'];
  }

  @override
  Future<List<WindowLayout>> windows({String? machine}) async {
    if (machine == null && !rootUp) throw const ServerException('닿지 못했다');
    return machine == null
        ? const [_layout]
        : const [WindowLayout(idx: 0, active: true, name: '미니 방', cwd: '~/mini', rects: [PaneRect(surface: '%7', x: 0, y: 0, w: 100, h: 100)])];
  }

  @override
  Future<List<Machine>> machines() async {
    if (!rosterUp) throw const ServerException('명부 없음');
    return [Machine(label: '미니', route: _mini, online: miniOnline, panes: const [])];
  }

  @override
  Future<List<Note>> notes({String? machine}) async => const [];

  @override
  Future<Me> me() async => const Me(name: 'miku', owner: true, machine: '맥북');

  @override
  Future<Changes> changes({String? machine, required int since, int wait = Changes.longPollSecs}) =>
      Completer<Changes>().future;
}

HubSection? _section(HubModel m, String? machine) =>
    m.sections.where((s) => s.machine == machine).firstOrNull;

Iterable<Pane> _students(HubSection s) => s.rooms.expand((r) => r.panes);

void main() {
  setUp(HubModel.clearCache);

  test('스냅숏은 방 이름·배치·학생·마지막으로 본 때를 그대로 되읽는다', () {
    final seen = DateTime(2026, 11, 13, 15, 32);
    final snap = HubSnapshot(
      rootPanes: [_pane('%0'), _pane('%1', window: 1)],
      rootLabels: ['교실', ''],
      rootLayouts: const [_layout],
      machines: [Machine(label: '미니', route: _mini, online: false, panes: [_pane('%7', machine: _mini)], agoSecs: 30)],
      remotePanes: {_mini: [_pane('%7', machine: _mini)]},
      remoteLayouts: const {_mini: [_layout]},
      rootName: '맥북',
      seenAt: {'': seen, _mini: seen},
    );
    final back = HubSnapshot.fromJson(jsonDecode(jsonEncode(snap.toJson())))!;
    expect(back.rootName, '맥북');
    expect(back.rootLabels, ['교실', '']);
    expect(back.seenAt[''], seen);
    expect(back.rootPanes!.map((p) => p.viewKey), snap.rootPanes!.map((p) => p.viewKey));
    final p = back.rootPanes!.first;
    expect((p.slug, p.session, p.harness, p.machine, p.title), ('miku', '세션 %0', 'claude', _hub, '◐ 작업 %0'));
    final l = back.rootLayouts.single;
    expect((l.title, l.aspect, l.rects.length), ('교실 · ~/project', 1.6, 2));
    expect(l.rects[1].tabs, ['%1', '%2']);
    expect(l.rects[1].tabActive, 1);
    expect(back.machines.single.route, _mini);
    expect(back.machines.single.online, isFalse);
    expect(back.remotePanes[_mini]!.single.machine, _mini);
    expect(HubSnapshot.fromJson({'version': 99}), isNull);
  });

  test('끊긴 학생은 지금 일을 빼고 얼굴·이름·자리만 남긴다', () {
    final live = _pane('%0', status: 'working', busySecs: 300, subagents: ['찾기']);
    final frozen = live.frozen;
    expect(frozen.isBusy, isFalse);
    expect(frozen.needsYou, isFalse);
    expect(frozen.hasRunningSubagents, isFalse);
    expect(frozen.busySecs, isNull);
    expect(frozen.statusWord, '끊김');
    expect((frozen.viewKey, frozen.slug, frozen.window, frozen.rowTitle), (live.viewKey, 'miku', 0, '세션 %0'));
    expect(_pane('%1', status: 'waiting', kind: 'permission').frozen.needsYou, isFalse);
  });

  test('주소 기기에 거듭 못 닿으면 지난 방을 끊김으로 얼리고, 다시 닿으면 저절로 실시간이다', () async {
    final desktop = _Desktop();
    final model = HubModel(desktop, snapshots: HubSnapshots.memory());
    await model.refresh();
    var root = _section(model, null)!;
    expect(root.online, isTrue);
    expect(_students(root).where((p) => p.isBusy), isNotEmpty);
    expect(model.waiting, 1);
    final seen = root.seenAt!;

    desktop.rootUp = false;
    await model.refresh();
    expect(_section(model, null)!.online, isTrue, reason: '한 번은 업링크가 다시 붙는 틈이다');
    await model.refresh();
    root = _section(model, null)!;
    expect(root.online, isFalse);
    expect(root.seenAt, seen);
    expect(root.rooms.single.title, '교실 · ~/project');
    expect(root.rooms.single.rects, hasLength(2));
    expect(_students(root).map((p) => p.id), containsAll(['%0', '%1', '%2']));
    expect(_students(root).where((p) => p.isBusy || p.needsYou || p.hasRunningSubagents), isEmpty);
    expect(model.waiting, 0);

    desktop.rootUp = true;
    await model.refresh();
    root = _section(model, null)!;
    expect(root.online, isTrue);
    expect(_students(root).where((p) => p.isBusy), isNotEmpty);
    expect(root.seenAt!.isBefore(seen), isFalse);
    model.dispose();
  });

  test('명부가 끊김이라 한 기기도 지난 방·이름·배치를 남기고, 명부를 거듭 못 받으면 원격도 끊김이다', () async {
    final desktop = _Desktop();
    final model = HubModel(desktop, snapshots: HubSnapshots.memory());
    await model.refresh();
    expect(_section(model, '미니')!.online, isTrue);
    desktop.miniOnline = false;
    await model.refresh();
    var mini = _section(model, '미니')!;
    expect(mini.online, isFalse);
    expect(mini.seenAt, isNotNull);
    expect(mini.rooms.single.title, '미니 방 · ~/mini');
    expect(mini.rooms.single.rects.single.surface, '%7');
    expect(_students(mini).single.isBusy, isFalse);

    desktop.miniOnline = true;
    await model.refresh();
    expect(_section(model, '미니')!.online, isTrue);
    desktop.rosterUp = false;
    await model.refresh();
    expect(_section(model, '미니')!.online, isTrue, reason: '명부 한 번 실패로 뒤집지 않는다');
    await model.refresh();
    mini = _section(model, '미니')!;
    expect(mini.online, isFalse);
    expect(_students(mini).single.isBusy, isFalse);
    model.dispose();
  });

  test('앱을 다시 켜면 디스크의 지난 허브를 연결 없이 그대로 그리고, 로그아웃하면 지운다', () async {
    final disk = _MemoryDisk();
    final before = HubSnapshots(disk: disk);
    final desktop = _Desktop();
    final live = HubModel(desktop, snapshots: before)..start();
    await pumpEventQueue();
    await before.flush();
    live.dispose();
    final scope = desktop.resumeScope;
    expect(disk.files.keys.single, HubSnapshots.fileKey(scope));
    expect(disk.files.values.single, isNot(contains(scope)), reason: '파일 안에 계정·주소를 남기지 않는다');

    final after = HubSnapshots(disk: disk);
    expect(after.peek(scope), isNull);
    expect(await after.load(scope), isNotNull);
    final offline = HubModel(_Desktop()..rootUp = false, snapshots: after, offline: true);
    offline.start();
    await pumpEventQueue();
    expect(offline.showingCached, isFalse);
    expect(offline.rootName, '맥북');
    final root = _section(offline, null)!;
    expect(root.online, isFalse);
    expect(root.rooms.single.title, '교실 · ~/project');
    expect(root.rooms.single.rects, hasLength(2));
    expect(_students(root).map((p) => p.slug).toSet(), {'miku'});
    expect(_section(offline, '미니')!.rooms.single.title, '미니 방 · ~/mini');
    expect(offline.sections.every((s) => !s.online && s.seenAt != null), isTrue);
    offline.dispose();

    await after.forget(scope);
    expect(disk.files, isEmpty);
    expect(after.peek(scope), isNull);
    expect(await HubSnapshots(disk: disk).load(scope), isNull);
  });

  test('계정마다 따로 둔다', () async {
    final disk = _MemoryDisk();
    final store = HubSnapshots(disk: disk);
    store.put('https://a|alice', HubSnapshot(rootPanes: [_pane('%0')]));
    store.put('https://a|bob', HubSnapshot(rootPanes: [_pane('%9')]));
    await store.flush();
    final reopened = HubSnapshots(disk: disk);
    expect((await reopened.load('https://a|alice'))!.rootPanes!.single.id, '%0');
    expect((await reopened.load('https://a|bob'))!.rootPanes!.single.id, '%9');
    await reopened.forget('https://a|alice');
    expect(await HubSnapshots(disk: disk).load('https://a|alice'), isNull);
    expect(await HubSnapshots(disk: disk).load('https://a|bob'), isNotNull);
  });

  test('끊긴 머리의 시각은 하루 안이면 시:분, 그 전이면 며칠 전', () {
    final now = DateTime(2026, 11, 13, 18);
    expect(lastSeenLabel(DateTime(2026, 11, 13, 15, 32), now: now), '15:32');
    expect(lastSeenLabel(DateTime(2026, 11, 11, 9, 5), now: now), '2일 전 09:05');
    expect(lastSeenLabel(null, now: now), '본 적 없음');
  });

  group('끊긴 허브 화면', () {
    late Server server;
    late ArchiveRepository repo;
    late SavedConversation saved;

    Future<void> pumpHub(WidgetTester tester, {Size size = const Size(390, 844)}) async {
      tester.view.physicalSize = size;
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.reset);
      server = Server.account(
        archiveSession(),
        connectDirect: false,
        client: MockClient((_) async => throw StateError('끊긴 허브는 데스크톱에 묻지 않는다')),
      );
      final raw = message('assistant', '끊긴 기기에서도 이 답변을 읽어요.');
      saved = SavedConversation(entry: entryFor(raw, key: 'key-%0'), raw: raw, savedAt: DateTime.utc(2026));
      final store = MemoryArchiveStore();
      await store.save(server.resumeScope, saved, current: () => true);
      repo = ArchiveRepository(server, store: store, api: FakeArchiveApi(saved.entry, raw)..failCatalog = true);
      ConversationCache.bind(server, ConversationCache(server, repository: repo));
      final seen = DateTime.now().subtract(const Duration(minutes: 5));
      final snapshots = HubSnapshots.memory()
        ..put(
          server.resumeScope,
          HubSnapshot(
            rootPanes: [
              _pane('%0', status: 'working', busySecs: 600),
              _pane('%1', status: 'waiting', kind: 'permission'),
              Pane(id: '%2', name: '', title: '', status: 'idle', window: 0, cwd: '/tmp', machine: _hub),
            ],
            rootLayouts: const [_layout],
            rootName: '맥북',
            seenAt: {'': seen},
          ),
        );
      await tester.pumpWidget(
        MaterialApp(
          theme: buildTheme(Brightness.light),
          home: HubScreen(
            server: server,
            offline: true,
            snapshots: snapshots,
            onChangeAddress: () async {},
          ),
        ),
      );
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 500));
    }

    Future<void> done(WidgetTester tester) async {
      await tester.pumpWidget(const SizedBox());
      await tester.pump(const Duration(seconds: 1));
      repo.dispose();
      server.close();
    }

    testWidgets('지난 방을 흐리게 그리고 기기 머리에 끊김과 마지막 시각을 단다', (tester) async {
      await pumpHub(tester);
      expect(find.text('맥북'), findsOneWidget);
      expect(find.textContaining('끊김 · ', findRichText: true), findsOneWidget);
      expect(find.text('교실'), findsOneWidget);
      expect(find.text('~/project'), findsOneWidget);
      expect(find.byKey(const ValueKey('stale-')), findsOneWidget);
      expect(tester.widget<Opacity>(find.byKey(const ValueKey('stale-'))).opacity, lessThan(1));
      expect(find.byTooltip('새 방'), findsNothing);
      expect(find.byTooltip('PC 기능'), findsNothing);
      expect(find.byTooltip('클립보드'), findsNothing);
      expect(find.byTooltip('저장한 대화'), findsNothing);
      expect(find.byTooltip('계정·연결'), findsOneWidget);
      expect(find.text('10분'), findsNothing, reason: '지난 도는 시간을 지금 일처럼 보이지 않는다');
      expect(tester.takeException(), isNull);
      await done(tester);
    });

    testWidgets('끊긴 학생을 누르면 저장한 대화를 읽기 전용으로 연다', (tester) async {
      await pumpHub(tester);
      await tester.tap(find.text('세션 %0').last);
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 500));
      expect(find.byType(SavedConversationScreen), findsOneWidget);
      expect(find.textContaining('끊긴 기기에서도', findRichText: true), findsOneWidget);
      expect(find.textContaining('읽기 전용'), findsOneWidget);
      expect(find.textContaining('기기가 끊겨 보내기'), findsOneWidget);
      expect(find.byType(TextField), findsNothing);
      expect(tester.takeException(), isNull);
      await done(tester);
    });

    testWidgets('보낼 수 없는 조작은 막고 까닭을 짧게 말한다', (tester) async {
      await pumpHub(tester);
      await tester.tap(find.byTooltip('방 메뉴'));
      await tester.pump();
      expect(find.text('기기가 끊겨 방을 고칠 수 없어요. 다시 붙으면 돼요.'), findsOneWidget);
      await tester.tap(find.text('세션 %1').last);
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 500));
      expect(find.byType(SavedConversationScreen), findsNothing);
      expect(find.textContaining('저장한 대화가 아직 없어요'), findsOneWidget);
      expect(tester.takeException(), isNull);
      await done(tester);
    });

    for (final width in [320.0, 430.0]) {
      testWidgets('$width 폭에서도 끊긴 머리와 방이 넘치지 않는다', (tester) async {
        await pumpHub(tester, size: Size(width, 700));
        expect(find.textContaining('끊김 · ', findRichText: true), findsOneWidget);
        expect(tester.takeException(), isNull);
        await done(tester);
      });
    }
  });

  group('켤 때', () {
    tearDown(() => HubSnapshots.shared = HubSnapshots());

    testWidgets('본 허브가 있는 계정은 연결 확인 중에도 대기 화면 대신 지난 허브를 띄우고, 로그아웃하면 지운다', (tester) async {
      tester.view.physicalSize = const Size(390, 844);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.reset);
      final account = archiveSession();
      FlutterSecureStorage.setMockInitialValues({ConnectionStore.key: jsonEncode(account.toJson())});
      final scope = '${account.origin}|${account.account}';
      final snapshots = HubSnapshots.memory()
        ..put(scope, HubSnapshot(rootPanes: [_pane('%0')], rootLayouts: const [_layout], rootName: '맥북', seenAt: {'': DateTime.now()}));
      HubSnapshots.shared = snapshots;
      await tester.pumpWidget(MaterialApp(navigatorKey: navigatorKey, theme: buildTheme(Brightness.light), home: const RootScreen()));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));
      expect(find.text('기기 연결'), findsNothing);
      expect(find.byType(HubScreen), findsOneWidget);
      expect(find.byKey(const Key('hub-offline-band')), findsOneWidget);
      expect(find.text('교실'), findsOneWidget);
      // 관문 확인이 끝나기 전이면 「확인 중」, 못 닿았으면 「끊김」 — 어느 쪽이든 지난 시각을 단다.
      expect(find.textContaining(RegExp('(확인 중|끊김) · '), findRichText: true), findsOneWidget);

      await tester.tap(find.byTooltip('계정·연결'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      expect(find.text('${account.label} 계정'), findsOneWidget);
      expect(find.text('저장한 대화'), findsWidgets);
      await tester.ensureVisible(find.text('로그아웃'));
      await tester.tap(find.text('로그아웃'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      expect(snapshots.peek(scope), isNull);
      expect(find.byType(HubScreen), findsNothing);
      expect(tester.takeException(), isNull);
      ApprovalCenter.instance.bind(null);
      await tester.pumpWidget(const SizedBox());
      await tester.pump(const Duration(seconds: 4));
    });

    testWidgets('본 허브가 하나도 없는 처음 로그인만 대기 화면을 쓴다', (tester) async {
      final account = archiveSession('first');
      FlutterSecureStorage.setMockInitialValues({ConnectionStore.key: jsonEncode(account.toJson())});
      HubSnapshots.shared = HubSnapshots.memory();
      await tester.pumpWidget(MaterialApp(navigatorKey: navigatorKey, theme: buildTheme(Brightness.light), home: const RootScreen()));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));
      expect(find.text('기기 연결'), findsOneWidget);
      expect(find.byType(HubScreen), findsNothing);
      expect(tester.takeException(), isNull);
      ApprovalCenter.instance.bind(null);
      await tester.pumpWidget(const SizedBox());
      await tester.pump(const Duration(seconds: 4));
    });
  });
}
