import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/status_style.dart';

void main() {
  const limit = "You've hit your weekly limit · resets Nov 18, 9pm (Asia/Seoul)";
  Pane pane(Map<String, Object?> extra) => Pane.fromJson({
    'id': '%1',
    'name': '아루',
    'harness': 'claude',
    'status': 'idle',
    ...extra,
  });

  test('a stalled turn shows the desktop error text instead of resting', () {
    final scheme = ColorScheme.fromSeed(seedColor: Colors.blue);
    final stalled = pane({'error': limit});
    expect(stalled.isStalled, isTrue);
    expect(stalled.statusWord, '멈춤');
    final style = StatusStyle.of(stalled, scheme);
    expect(style.label, '멈춤 · $limit');
    expect(style.color, scheme.error);
    expect(Pane.fromJson(stalled.toJson()).error, limit);
  });

  test('working, waiting and blank errors are not stalls', () {
    expect(pane({'error': '  '}).error, isNull);
    expect(pane({'error': limit, 'status': 'working'}).isStalled, isFalse);
    expect(
      pane({'error': limit, 'status': 'waiting', 'kind': 'permission'}).isStalled,
      isFalse,
    );
    expect(pane({}).statusWord, '쉬는 중');
  });
}
