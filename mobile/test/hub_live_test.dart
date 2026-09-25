import 'dart:async';

import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/hub_model.dart';
import 'package:kasaterm_mobile/server.dart';

const _mini = '~mini-stable';

Pane pane(String id, {String status = 'idle', String? machine}) => Pane(
  id: id,
  name: '학생$id',
  title: '',
  status: status,
  window: 0,
  cwd: '/',
  machine: machine,
);

/// 기계마다 대답을 붙잡아 둘 수 있는 서버 — 느린 기계·알림 번호를 손으로 움직인다.
class LiveServer extends Server {
  LiveServer() : super(Uri.parse('http://127.0.0.1:9/'));

  final calls = <String>[];
  final gates = <String, Completer<void>>{};
  final rows = <String, List<Pane>>{
    '': [pane('%0')],
    _mini: [pane('%1', machine: _mini)],
  };
  bool machinesOnline = true;
  bool statusAware = true;
  bool changesMissing = false;
  bool changesBroken = false;
  final epochs = <String, int>{'': 1, _mini: 1};
  final waiters = <String, Completer<Changes>>{};
  final changeCalls = <String>[];

  Future<void> _gate(String key) async {
    final g = gates[key];
    if (g != null) await g.future;
  }

  @override
  Future<List<Pane>> panes({String? machine}) async {
    final key = machine ?? '';
    calls.add('panes:$key');
    await _gate('panes:$key');
    return rows[key]!;
  }

  @override
  Future<List<String>> sessions() async => ['교실'];

  @override
  Future<List<Machine>> machines() async {
    calls.add('machines');
    await _gate('machines');
    return [
      Machine(label: '미니', route: _mini, online: machinesOnline, panes: []),
    ];
  }

  @override
  Future<List<WindowLayout>> windows({String? machine}) async => const [];

  @override
  Future<List<Note>> notes({String? machine}) async {
    calls.add('notes:${machine ?? ''}');
    await _gate('notes:${machine ?? ''}');
    return const [];
  }

  @override
  Future<Me> me() async => const Me(name: 'miku', owner: true);

  @override
  Future<Changes> changes({
    String? machine,
    required int since,
    int wait = Changes.longPollSecs,
  }) async {
    final key = machine ?? '';
    changeCalls.add(key);
    if (changesMissing) {
      throw const ServerException('옛 판', status: 404);
    }
    if (changesBroken) throw const ServerException('끊김', status: 502);
    final now = epochs[key]!;
    if (since != now) return Changes(epoch: now, status: statusAware);
    final w = Completer<Changes>();
    waiters[key] = w;
    return w.future;
  }

  /// 원본이 번호를 올렸다 — 매달린 롱폴이 돌아온다.
  void bump(String key) {
    epochs[key] = epochs[key]! + 1;
    waiters
        .remove(key)
        ?.complete(Changes(epoch: epochs[key]!, status: statusAware));
  }

  int count(String call) => calls.where((c) => c == call).length;
}

HubSection? section(HubModel m, String? machine) =>
    m.sections.where((s) => s.machine == machine).firstOrNull;

void main() {
  setUp(HubModel.clearCache);

  test('느린 원격 기계를 기다리지 않고 주소 기계부터 그린다', () async {
    final server = LiveServer();
    server.gates['panes:$_mini'] = Completer();
    final model = HubModel(server);
    final done = model.refresh();
    await pumpEventQueue();
    expect(section(model, null)?.paneCount, 1);
    expect(section(model, '미니')?.online, isTrue);
    // 명부가 실어 온 행이 없으면 원격 절은 비어 있다가, 그 기계가 답하면 찬다.
    expect(section(model, '미니')?.paneCount, 0);
    server.gates['panes:$_mini']!.complete();
    await done;
    expect(section(model, '미니')?.paneCount, 1);
    model.dispose();
  });

  test('쪽지가 늦어도 학생 목록은 먼저 선다', () async {
    final server = LiveServer();
    server.gates['notes:'] = Completer();
    final model = HubModel(server);
    final done = model.refresh();
    await pumpEventQueue();
    expect(section(model, null)?.paneCount, 1);
    server.gates['notes:']!.complete();
    await done;
    model.dispose();
  });

  test('원격 요청은 주소 기계 목록과 한 물결로 나간다', () async {
    final server = LiveServer();
    final first = HubModel(server);
    await first.refresh();
    first.dispose();
    // 명부를 기억하는 다음 허브는 주소 기계 목록도 명부도 기다리지 않고 원격을 바로 묻는다.
    server.calls.clear();
    server.gates['panes:'] = Completer();
    server.gates['machines'] = Completer();
    final model = HubModel(server);
    final done = model.refresh();
    await pumpEventQueue();
    expect(server.calls, contains('panes:$_mini'));
    server.gates['panes:']!.complete();
    server.gates['machines']!.complete();
    await done;
    model.dispose();
  });

  test('다시 연 허브는 지난 목록을 곧바로 그리고 새 목록이 오면 확인 표시를 걷는다', () async {
    final server = LiveServer();
    final first = HubModel(server);
    await first.refresh();
    first.dispose();
    server.rows[''] = [pane('%0'), pane('%2')];
    server.gates['panes:'] = Completer();
    final model = HubModel(server);
    expect(section(model, null)?.paneCount, 1);
    expect(section(model, '미니')?.paneCount, 1);
    expect(model.showingCached, isTrue);
    final done = model.refresh();
    await pumpEventQueue();
    expect(model.showingCached, isTrue);
    server.gates['panes:']!.complete();
    await done;
    expect(model.showingCached, isFalse);
    expect(section(model, null)?.paneCount, 2);
    model.dispose();
  });

  test('주소 기계에 못 닿으면 지난 목록을 두고 오류만 알린다', () async {
    final server = LiveServer();
    final first = HubModel(server);
    await first.refresh();
    first.dispose();
    final broken = _FailingServer(server);
    final model = HubModel(broken);
    await model.refresh();
    expect(model.error, isNotNull);
    expect(model.showingCached, isFalse);
    expect(section(model, null)?.paneCount, 1);
    model.dispose();
  });

  test('읽는 중에 또 부르면 겹쳐 쏘지 않고 끝난 뒤 한 번만 더 읽는다', () async {
    final server = LiveServer();
    server.gates['panes:'] = Completer();
    final model = HubModel(server);
    final a = model.refresh();
    final b = model.refresh();
    final c = model.refresh();
    await pumpEventQueue();
    expect(server.count('panes:'), 1);
    server.gates['panes:']!.complete();
    await Future.wait([a, b, c]);
    expect(server.count('panes:'), 2);
    model.dispose();
  });

  testWidgets('주소 기계 목록이 끝내 안 오면 시간 제한 뒤 그 기계만 오류로 둔다', (tester) async {
    final server = LiveServer();
    server.gates['panes:'] = Completer();
    final model = HubModel(server);
    var finished = false;
    model.refresh().then((_) => finished = true);
    await tester.pump();
    // 원격은 그 사이 이미 섰다.
    expect(section(model, '미니')?.paneCount, 1);
    await tester.pump(HubModel.listTimeout + const Duration(seconds: 1));
    expect(finished, isTrue);
    expect(model.error, contains('늦다'));
    model.dispose();
  });

  testWidgets('변경 번호가 오르면 그 기계만 곧바로 다시 읽는다', (tester) async {
    final server = LiveServer();
    final model = HubModel(server)..start();
    await tester.pump();
    await tester.pump();
    expect(server.waiters.keys, containsAll(['', _mini]));
    server.calls.clear();
    server.rows[_mini] = [pane('%1', status: 'waiting', machine: _mini)];
    server.bump(_mini);
    await tester.pump();
    await tester.pump();
    expect(server.count('panes:$_mini'), 1);
    expect(server.count('panes:'), 0);
    expect(model.waiting, 1);
    // 롱폴은 새 번호로 다시 매달린다.
    expect(server.waiters.keys, contains(_mini));
    model.dispose();
  });

  testWidgets('상태 전이를 알려 주는 서버에는 폴링을 15초로 늦춘다', (tester) async {
    final server = LiveServer();
    final model = HubModel(server)..start();
    await tester.pump();
    await tester.pump();
    server.calls.clear();
    await tester.pump(const Duration(seconds: 10));
    expect(server.count('panes:'), 0);
    await tester.pump(const Duration(seconds: 5));
    expect(server.count('panes:'), 1);
    model.dispose();
  });

  testWidgets('상태 전이를 모르는 서버에는 5초 폴링을 지킨다', (tester) async {
    final server = LiveServer()..statusAware = false;
    final model = HubModel(server)..start();
    await tester.pump();
    await tester.pump();
    server.calls.clear();
    await tester.pump(HubModel.pollEvery);
    expect(server.count('panes:'), 1);
    expect(server.count('panes:$_mini'), 1);
    model.dispose();
  });

  testWidgets('알림 길이 없는 옛 판에는 다시 묻지 않고 폴링만 한다', (tester) async {
    final server = LiveServer()..changesMissing = true;
    final model = HubModel(server)..start();
    await tester.pump();
    await tester.pump();
    final asked = server.changeCalls.length;
    expect(asked, 2);
    server.calls.clear();
    await tester.pump(const Duration(seconds: 30));
    expect(server.changeCalls.length, asked);
    expect(server.count('panes:'), greaterThanOrEqualTo(5));
    model.dispose();
  });

  testWidgets('알림 길이 끊기면 물러서며 다시 붙고, 닫으면 타이머가 남지 않는다', (tester) async {
    final server = LiveServer()..changesBroken = true;
    final model = HubModel(server)..start();
    await tester.pump();
    await tester.pump();
    final first = server.changeCalls.where((k) => k == '').length;
    expect(first, 1);
    await tester.pump(const Duration(seconds: 2));
    await tester.pump();
    expect(server.changeCalls.where((k) => k == '').length, 2);
    // 다음은 4초 뒤 — 1초만 지나선 다시 안 묻는다.
    await tester.pump(const Duration(seconds: 1));
    expect(server.changeCalls.where((k) => k == '').length, 2);
    server.changesBroken = false;
    await tester.pump(const Duration(seconds: 3));
    await tester.pump();
    // 기준 번호 한 번 + 새로 매달린 롱폴 한 번.
    expect(server.changeCalls.where((k) => k == '').length, 4);
    expect(server.waiters.keys, contains(''));
    model.dispose();
  });

  testWidgets('앱이 쉬었다 돌아오면 바로 다시 읽고 알림 줄을 새로 건다', (tester) async {
    final server = LiveServer();
    final model = HubModel(server)..start();
    await tester.pump();
    await tester.pump();
    model.stop();
    server.calls.clear();
    server.changeCalls.clear();
    await tester.pump(const Duration(minutes: 1));
    expect(server.calls, isEmpty);
    model.start();
    await tester.pump();
    await tester.pump();
    expect(server.count('panes:'), 1);
    expect(server.changeCalls, containsAll(['', _mini]));
    model.dispose();
  });

  testWidgets('지난 기억에서 기다리던 학생은 새로 받아도 진동하지 않는다', (tester) async {
    final buzz = <String>[];
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      SystemChannels.platform,
      (call) async {
        if (call.method == 'HapticFeedback.vibrate') {
          buzz.add('${call.arguments}');
        }
        return null;
      },
    );
    final server = LiveServer();
    final first = HubModel(server);
    await tester.runAsync(first.refresh);
    first.dispose();
    server.rows[''] = [pane('%0', status: 'waiting')];
    final model = HubModel(server)..start();
    await tester.pump();
    await tester.pump();
    expect(model.waiting, 1);
    expect(buzz, isEmpty);
    server.rows[''] = [pane('%0')];
    server.bump('');
    await tester.pump();
    await tester.pump();
    server.rows[''] = [pane('%0', status: 'waiting')];
    server.bump('');
    await tester.pump();
    await tester.pump();
    expect(buzz, ['HapticFeedbackType.mediumImpact']);
    model.dispose();
  });
}

/// 주소 기계에 전혀 닿지 않는 서버 — 같은 주소라 기억은 공유한다.
class _FailingServer extends Server {
  _FailingServer(LiveServer like) : super(like.root);

  @override
  Future<List<Pane>> panes({String? machine}) async =>
      throw const ServerException('닿지 못했다');

  @override
  Future<List<String>> sessions() async => throw const ServerException('x');

  @override
  Future<List<Machine>> machines() async => throw const ServerException('x');

  @override
  Future<List<WindowLayout>> windows({String? machine}) async =>
      throw const ServerException('x');

  @override
  Future<List<Note>> notes({String? machine}) async =>
      throw const ServerException('x');
}
