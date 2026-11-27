import 'package:flutter/material.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/hub_actions.dart';
import 'package:kasaterm_mobile/hub_model.dart';
import 'package:kasaterm_mobile/hub_prefs.dart';
import 'package:kasaterm_mobile/main.dart' show buildTheme;
import 'package:kasaterm_mobile/screens/hub.dart';
import 'package:kasaterm_mobile/screens/hub_drag.dart';
import 'package:kasaterm_mobile/server.dart';

import 'fixtures/hub_interactions.dart';

Future<void> flush(WidgetTester tester) async {
  for (var i = 0; i < 6; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
}

Finder tile(Pane pane) =>
    find.byKey(ValueKey('hub-drag-${pane.viewKey}')).first;

Future<void> dragTo(WidgetTester tester, Pane pane, Finder target) async {
  final gesture = await tester.startGesture(tester.getCenter(tile(pane)));
  await tester.pump(const Duration(milliseconds: 600));
  await gesture.moveTo(tester.getCenter(target));
  await tester.pump(const Duration(milliseconds: 100));
  await gesture.up();
  await flush(tester);
}

Future<void> pumpHub(
  WidgetTester tester,
  HubFixture fixture, {
  double width = 390,
  double scale = 1,
}) async {
  tester.view.physicalSize = Size(width, 844);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    MaterialApp(
      theme: buildTheme(Brightness.light),
      builder: (context, child) => MediaQuery(
        data: MediaQuery.of(context).copyWith(
          disableAnimations: true,
          textScaler: TextScaler.linear(scale),
        ),
        child: child!,
      ),
      home: HubScreen(
        server: fixture.server,
        prefs: const HubPrefs(scope: 'fixture'),
        onChangeAddress: () async {},
      ),
    ),
  );
  await flush(tester);
  addTearDown(() async {
    await tester.pumpWidget(const SizedBox());
    fixture.server.close();
  });
}

void main() {
  setUp(() {
    HubModel.clearCache();
    FlutterSecureStorage.setMockInitialValues({});
  });

  test(
    'hidden state persists by source identity, independent of name and recycled pane ID',
    () async {
      final fixture = HubFixture();
      final pane = fixture.pane(0);
      final renamed = Pane.fromJson({
        ...fixture.rows.first,
        'name': '새 이름',
        'session': '새 제목',
      }, machine: '~fixture');
      final recycled = Pane.fromJson({
        ...fixture.rows.first,
        'surface_key': 'new-key',
      }, machine: '~fixture');
      final prefs = HubPrefs(scope: 'fixture');
      await prefs.save(HubView(hidden: {pane.viewKey, 'e:legacy'}));
      final loaded = await prefs.load();
      expect(loaded.hidden, {renamed.viewKey});
      expect(loaded.hidden.contains(recycled.viewKey), isFalse);
      expect((await const HubPrefs(scope: 'other').load()).hidden, isEmpty);
    },
  );

  test(
    'hiding an active tab reveals remaining tabs without changing desktop geometry',
    () {
      final a = Pane.fromJson({'id': '%1', 'surface_key': 'a'});
      final b = Pane.fromJson({'id': '%2', 'surface_key': 'b'});
      final room = HubRoom(
        title: '방',
        panes: [a, b],
        rects: const [
          PaneRect(
            surface: '%1',
            x: 0,
            y: 0,
            w: 100,
            h: 100,
            tabs: ['%1', '%2'],
            tabActive: 0,
          ),
        ],
      );
      final visible = room.visible({a.viewKey});
      expect(visible.panes.single.id, '%2');
      expect(visible.rects.single.surface, '%2');
      expect(visible.rects.single.tabs, ['%2']);
      expect(visible.rects.single.w, 100);
      expect(room.panes, hasLength(2));
    },
  );

  test('device and room order survive storage and always retain new entries', () async {
    final fixture = HubFixture();
    final prefs = HubPrefs(scope: 'fixture');
    final model = HubModel(fixture.server, prefs: prefs);
    addTearDown(() {
      model.dispose();
      fixture.server.close();
    });
    final rooms = HubModel.rooms(
      [Pane.fromJson({...fixture.rows[0], 'window': 0}), Pane.fromJson({...fixture.rows[1], 'window': 1})],
      ['같은 방', '같은 방'],
    );
    expect(rooms[0].layoutKey, isNot(rooms[1].layoutKey));
    final root = HubSection(machine: null, online: true, rooms: rooms);
    const remote = HubSection(machine: '검증 맥', route: 'id:remote', online: true, rooms: []);
    const added = HubSection(machine: '새 기기', route: 'id:new', online: true, rooms: []);
    model.sections = [root, remote, added];
    await model.orderDevices([remote.layoutKey, '사라진 기기', root.layoutKey]);
    await model.orderRooms(root.layoutKey, [rooms[1].layoutKey, rooms[0].layoutKey]);
    await model.toggleRoomFold(root, rooms[0]);
    await model.toggleFold(remote);
    model.view = await prefs.load();

    expect(model.visible, [remote, root, added]);
    expect(model.orderedRooms(root), rooms.reversed);
    expect(model.view.isRoomFolded(root.layoutKey, rooms[0].layoutKey), isTrue);
    expect(model.view.isRoomFolded(root.layoutKey, rooms[1].layoutKey), isFalse);
    expect(model.view.isRoomFolded(remote.layoutKey, rooms[0].layoutKey), isFalse);
    expect(model.view.isFolded(remote.machine), isTrue);
  });

  testWidgets('room folds independently and can be reordered while folded', (tester) async {
    final fixture = HubFixture();
    fixture.rows[1]['window'] = 1;
    await const HubPrefs(scope: 'fixture').save(const HubView(shape: HubShape.list));
    await pumpHub(tester, fixture);

    await tester.tap(find.byTooltip('방 접기').first);
    await flush(tester);
    expect(find.byKey(ValueKey('hub-drag-${fixture.pane(0).viewKey}')), findsNothing);
    expect(tile(fixture.pane(1)), findsOneWidget);

    await tester.tap(find.byTooltip('보기'));
    await flush(tester);
    await tester.tap(find.text('목록 순서'));
    await flush(tester);
    final handle = tester.getCenter(find.byTooltip('검증 방 순서 이동'));
    final gesture = await tester.startGesture(handle);
    await gesture.moveBy(const Offset(0, 26));
    await tester.pump();
    await gesture.moveBy(const Offset(0, 86));
    await tester.pump(const Duration(milliseconds: 200));
    await gesture.up();
    await flush(tester);
    final reorderList = find.byType(ReorderableListView);
    expect(
      tester.getTopLeft(find.descendant(of: reorderList, matching: find.text('방 2'))).dy,
      lessThan(tester.getTopLeft(find.descendant(of: reorderList, matching: find.text('검증 방'))).dy),
    );
    await tester.tapAt(const Offset(20, 100));
    await flush(tester);

    expect(tester.getTopLeft(find.text('방 2')).dy, lessThan(tester.getTopLeft(find.text('검증 방')).dy));
    expect(find.byKey(ValueKey('hub-drag-${fixture.pane(0).viewKey}')), findsNothing);
    await tester.tap(find.byTooltip('방 펼치기'));
    await flush(tester);
    expect(tile(fixture.pane(0)), findsOneWidget);
    expect(fixture.commands, isEmpty);
  });

  testWidgets('live hub app bar keeps only notes, view and settings', (tester) async {
    await pumpHub(tester, HubFixture());
    expect(find.byTooltip('캐릭터 쪽지'), findsOneWidget);
    expect(find.byTooltip('설정'), findsOneWidget);
    expect(find.byTooltip('저장한 대화'), findsNothing);
    expect(find.byTooltip('클립보드'), findsNothing);
    expect(find.byIcon(Icons.offline_pin_outlined), findsNothing);
    expect(find.byIcon(Icons.content_paste_outlined), findsNothing);
  });

  testWidgets('long press, hide drop and restore never close the PC pane', (
    tester,
  ) async {
    final fixture = HubFixture();
    final pane = fixture.pane(0);
    await pumpHub(tester, fixture);
    await dragTo(tester, pane, find.byKey(const ValueKey('hub-drop-hide')));
    expect(find.byKey(ValueKey('hub-drag-${pane.viewKey}')), findsNothing);
    expect(fixture.commands, isEmpty);
    await tester.tap(find.byKey(const Key('hub-hidden')));
    await flush(tester);
    await tester.tap(find.byKey(ValueKey('hub-restore-${pane.viewKey}')));
    await flush(tester);
    await tester.tapAt(const Offset(10, 100));
    await flush(tester);
    expect(tile(pane), findsOneWidget);
    expect(fixture.commands, isEmpty);
  });

  testWidgets(
    'Pinterest selection accepts multiple panes and hides them together',
    (tester) async {
      final fixture = HubFixture();
      final a = fixture.pane(0), b = fixture.pane(1);
      await pumpHub(tester, fixture);
      await dragTo(tester, a, find.byKey(const ValueKey('hub-drop-select')));
      await tester.tap(tile(b));
      await flush(tester);
      expect(find.text('2개 선택'), findsOneWidget);
      await tester.tap(find.byKey(const ValueKey('hub-drop-hide')));
      await flush(tester);
      expect(find.byKey(ValueKey('hub-drag-${a.viewKey}')), findsNothing);
      expect(find.byKey(ValueKey('hub-drag-${b.viewKey}')), findsNothing);
      expect(fixture.commands, isEmpty);
    },
  );

  testWidgets('dragging to another pane moves beside the exact destination', (
    tester,
  ) async {
    final fixture = HubFixture();
    final a = fixture.pane(0), b = fixture.pane(1);
    await pumpHub(tester, fixture);
    final gesture = await tester.startGesture(tester.getCenter(tile(a)));
    await tester.pump(const Duration(milliseconds: 600));
    final destination = tester.getRect(
      find.byKey(ValueKey('hub-destination-${b.viewKey}')).first,
    );
    await gesture.moveTo(destination.centerRight - const Offset(4, 0));
    await tester.pump(const Duration(milliseconds: 100));
    await gesture.up();
    await flush(tester);
    final command = fixture.commands.single;
    expect(command['method'], 'surface.move');
    expect(command['path'], '/relay/account/m/~fixture/cmd');
    expect(command['params'], {
      'surface_id': '%1',
      'target': '%2',
      'direction': 'right',
    });
  });

  testWidgets(
    'trash asks before closing agents and supports receipt-based undo',
    (tester) async {
      final fixture = HubFixture();
      final pane = fixture.pane(0);
      await pumpHub(tester, fixture);
      await dragTo(tester, pane, find.byKey(const ValueKey('hub-drop-trash')));
      expect(fixture.commands, isEmpty);
      await tester.tap(find.widgetWithText(FilledButton, '닫기'));
      await flush(tester);
      expect(fixture.commands.single['method'], 'surface.close_checked');
      expect(fixture.commands.single['params'], {
        'kind': 'tabs',
        'addresses': [
          {
            'machine_id': 'fixture',
            'surface_id': '%1',
            'surface_key': 'alpha-key',
            'instance_id': 'fixture-instance',
            'session_id': 'alpha-key-session',
          },
        ],
      });
      await tester.tap(find.text('되살리기'));
      await flush(tester);
      expect(fixture.commands.last['method'], 'surface.reopen_checked');
      expect(tile(pane), findsOneWidget);
    },
  );

  testWidgets('recycled identity rejects trash without sending a close', (
    tester,
  ) async {
    final fixture = HubFixture();
    final pane = fixture.pane(0);
    await pumpHub(tester, fixture);
    fixture.replacedKey = 'replacement';
    await dragTo(tester, pane, find.byKey(const ValueKey('hub-drop-trash')));
    expect(fixture.commands, isEmpty);
    expect(find.text('칸이 바뀌었어요. 새 목록에서 다시 골라 주세요.'), findsOneWidget);
  });

  test(
    'idle-only close requests confirmation instead of silently closing a running shell',
    () async {
      final fixture = HubFixture()..requireConfirmation = true;
      final actions = HubActions(fixture.server);
      final targets = await actions.prepare([fixture.pane(0)]);
      final result = await actions.close(targets, idleOnly: true);
      expect(result.needsConfirmation, isTrue);
      expect(fixture.rows, hasLength(2));
      expect(fixture.commands.single['method'], 'surface.close_idle_checked');
      fixture.server.close();
    },
  );

  test(
    'partial close retains undo receipts for panes that already closed',
    () async {
      final fixture = HubFixture()..partialClose = true;
      final actions = HubActions(fixture.server);
      final targets = await actions.prepare([fixture.pane(0), fixture.pane(1)]);
      try {
        await actions.close(targets, idleOnly: false);
        fail('partial close must report an error');
      } on HubCloseException catch (error) {
        expect(error.restores, hasLength(1));
        expect(fixture.rows, hasLength(1));
        await actions.restore(error.restores);
        expect(fixture.rows, hasLength(2));
      } finally {
        fixture.server.close();
      }
    },
  );

  testWidgets(
    'plan-mode waiting parent exposes live subagents and removes finished work',
    (tester) async {
      final fixture = HubFixture();
      final pane = fixture.pane(0);
      await pumpHub(tester, fixture);
      expect(
        find.byKey(ValueKey('subagent-badge-${pane.viewKey}')),
        findsOneWidget,
      );
      expect(find.byKey(ValueKey('subagents-${pane.viewKey}')), findsOneWidget);
      await tester.tap(find.byKey(ValueKey('subagents-${pane.viewKey}')));
      await flush(tester);
      expect(find.text('실행 중인 서브에이전트 2개'), findsOneWidget);
      expect(find.text('구조 검토'), findsOneWidget);
      fixture.rows.first['subagents'] = [];
      await tester.pump(const Duration(seconds: 6));
      await flush(tester);
      expect(find.text('실행 중인 하위 작업이 없어요.'), findsOneWidget);
      expect(
        find.byKey(ValueKey('subagent-badge-${pane.viewKey}')),
        findsNothing,
      );
    },
  );

  testWidgets('drag dock fits a narrow phone with large text', (tester) async {
    final fixture = HubFixture();
    final pane = fixture.pane(0);
    await pumpHub(tester, fixture, width: 320, scale: 2);
    await dragTo(tester, pane, find.byKey(const ValueKey('hub-drop-select')));
    expect(find.byType(HubActionDock), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('room hide and show-all stay local to the phone', (tester) async {
    final fixture = HubFixture();
    final alpha = fixture.pane(0);
    await pumpHub(tester, fixture);
    await tester.tap(find.byTooltip('방 메뉴').first);
    await flush(tester);
    await tester.tap(find.text('이 폰에서 방 숨기기'));
    await flush(tester);
    expect(find.byKey(ValueKey('hub-drag-${alpha.viewKey}')), findsNothing);
    expect(fixture.rows, hasLength(2));
    expect(fixture.commands, isEmpty);
    await tester.tap(find.byKey(const Key('hub-hidden')));
    await flush(tester);
    await tester.tap(find.text('전부 다시 보이기'));
    await flush(tester);
    await tester.tapAt(const Offset(10, 100));
    await flush(tester);
    expect(tile(alpha), findsOneWidget);
  });

  testWidgets('cancelling a trash confirmation leaves every pane running', (
    tester,
  ) async {
    final fixture = HubFixture();
    final alpha = fixture.pane(0);
    await pumpHub(tester, fixture);
    await dragTo(tester, alpha, find.byKey(const ValueKey('hub-drop-trash')));
    await tester.tap(find.widgetWithText(TextButton, '취소'));
    await flush(tester);
    expect(fixture.commands, isEmpty);
    expect(fixture.rows, hasLength(2));
    expect(tile(alpha), findsOneWidget);
  });

  testWidgets('an undo snackbar cannot intercept the next drag action', (
    tester,
  ) async {
    final fixture = HubFixture();
    final alpha = fixture.pane(0);
    await pumpHub(tester, fixture);
    ScaffoldMessenger.of(tester.element(find.byType(HubScreen))).showSnackBar(
      const SnackBar(content: Text('이전 작업 안내'), duration: Duration(minutes: 1)),
    );
    await flush(tester);
    await dragTo(tester, alpha, find.byKey(const ValueKey('hub-drop-trash')));
    expect(find.widgetWithText(FilledButton, '닫기'), findsOneWidget);
    expect(fixture.commands, isEmpty);
  });
}
