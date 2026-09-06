import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/hub_model.dart';
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
  test('방마다 배치가 붙고, 배치 없는 방은 목록만', () {
    final layouts = [
      WindowLayout.fromJson({
        'idx': 1,
        'active': false,
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
    expect(rooms[1].paneOf('%3')?.status, 'waiting');
    expect(rooms[1].paneOf('%9'), isNull);
  });

  test('서버가 배치를 안 주면 옛날과 같다', () {
    final rooms = HubModel.rooms([pane('%1', 0)], const []);
    expect(rooms.single.rects, isEmpty);
  });
}
