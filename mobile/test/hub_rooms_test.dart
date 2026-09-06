import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/hub_model.dart';
import 'package:kasaterm_mobile/hub_prefs.dart';
import 'package:kasaterm_mobile/server.dart';

Pane pane(String id, int window, {String status = 'idle', String name = 'a'}) =>
    Pane(
      id: id,
      name: name,
      title: '',
      status: status,
      window: window,
      cwd: '/',
    );

void main() {
  filterTests();
  test('방마다 배치가 붙고, 배치 없는 방은 목록만', () {
    final layouts = [
      WindowLayout.fromJson({
        'idx': 1,
        'active': false,
        'aspect': 1.75,
        'panes': [
          {'surface_id': '%3', 'x': 0, 'y': 0, 'w': 50, 'h': 100},
          {'surface_id': '%4', 'x': 50, 'y': 0, 'w': 50, 'h': 100},
          {'x': 0, 'y': 0, 'w': 1, 'h': 1},
        ],
      }),
    ];
    final rooms = HubModel.rooms(
      [pane('%1', 0), pane('%3', 1, status: 'waiting'), pane('%4', 1)],
      const ['첫 방'],
      layouts,
    );
    expect(rooms.map((r) => r.title), ['첫 방', '방 2']);
    expect(rooms[0].rects, isEmpty);
    expect(rooms[1].rects.map((r) => r.surface), ['%3', '%4']);
    expect(rooms[1].rects[1].x, 50);
    expect(rooms[1].aspect, 1.75);
    expect(rooms[0].aspect, isNull);
    expect(rooms[1].paneOf('%3')?.status, 'waiting');
    expect(rooms[1].paneOf('%9'), isNull);
  });

  test('서버가 배치를 안 주면 옛날과 같다', () {
    final rooms = HubModel.rooms([pane('%1', 0)], const []);
    expect(rooms.single.rects, isEmpty);
  });
}

void filterTests() {
  HubSection sec(String? machine) =>
      HubSection(machine: machine, online: true, rooms: const []);
  final all = [sec(null), sec('맥미니'), sec('랙')];

  test('기기 고르기 — 전체·주소 기계·다른 기계', () {
    expect(HubModel.filterSections(all, null), all);
    expect(HubModel.filterSections(all, '').single.machine, isNull);
    expect(HubModel.filterSections(all, '맥미니').single.machine, '맥미니');
  });

  test('고른 기기가 사라졌으면 전부를 보인다', () {
    expect(HubModel.filterSections(all, '없는기계'), all);
  });

  test('보기 설정 복사', () {
    const v = HubView(machine: '맥미니', shape: HubShape.map);
    expect(v.copyWith(clearMachine: true).machine, isNull);
    expect(v.copyWith(shape: HubShape.list).machine, '맥미니');
    expect(v.copyWith(machine: '').machine, '');
  });
}
