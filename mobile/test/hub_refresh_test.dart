import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/hub_model.dart';
import 'package:kasaterm_mobile/server.dart';

Pane pane(String id, {String? machine}) => Pane(
  id: id,
  name: '학생',
  title: '',
  status: 'idle',
  window: 0,
  cwd: '/',
  machine: machine,
);

class FakeServer extends Server {
  FakeServer() : super(Uri.parse('http://127.0.0.1/'));

  bool failRemotePanes = false;
  final List<String?> paneRoutes = [];
  final List<String?> windowRoutes = [];

  @override
  Future<List<Pane>> panes({String? machine}) async {
    if (machine == null) return [pane('%0')];
    paneRoutes.add(machine);
    if (failRemotePanes) throw ServerException('temporary');
    return [pane('%1', machine: machine)];
  }

  @override
  Future<List<String>> sessions() async => ['로컬'];

  @override
  Future<List<Machine>> machines() async => const [
    Machine(label: '미니', route: '~mini-stable', online: true, panes: []),
  ];

  @override
  Future<List<WindowLayout>> windows({String? machine}) async {
    if (machine != null) windowRoutes.add(machine);
    return const [];
  }

  @override
  Future<List<Note>> notes({String? machine}) async => const [];
}

void main() {
  test('uplink online 기계는 같은 machine route에서 pane을 따로 읽는다', () async {
    final server = FakeServer();
    final model = HubModel(server);
    await model.refresh();
    final remote = model.sections.singleWhere(
      (section) => section.machine == '미니',
    );
    expect(remote.online, isTrue);
    expect(remote.route, '~mini-stable');
    expect(remote.paneCount, 1);
    expect(remote.rooms.single.panes.single.id, '%1');
    expect(remote.rooms.single.panes.single.machine, '~mini-stable');
    expect(server.paneRoutes, ['~mini-stable']);
    expect(server.windowRoutes, ['~mini-stable']);
  });

  test('원격 pane 재조회 실패는 online과 직전 유효 pane을 보존한다', () async {
    final server = FakeServer();
    final model = HubModel(server);
    await model.refresh();
    server.failRemotePanes = true;
    await model.refresh();
    final remote = model.sections.singleWhere(
      (section) => section.machine == '미니',
    );
    expect(remote.online, isTrue);
    expect(remote.paneCount, 1);
    expect(remote.rooms.single.panes.single.id, '%1');
  });
}
