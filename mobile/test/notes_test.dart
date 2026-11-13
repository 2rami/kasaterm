import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/screens/notes_sheet.dart';
import 'package:kasaterm_mobile/server.dart';

void main() {
  test('쪽지: 서버 JSON → Note, when 은 초 단위', () {
    final n = Note.fromJson({
      'id': 7,
      'pane': '%3',
      'character': '유우카',
      'kind': 'done',
      'summary': '폰 상태줄 한 줄로 맞췄어요',
      'when': 1700000000,
      'read': false,
    }, machine: '맥미니');
    expect(n.id, 7);
    expect(n.machine, '맥미니');
    expect(n.key, '맥미니|7');
    expect(n.when.millisecondsSinceEpoch, 1700000000 * 1000);
    expect(n.copyWith(read: true).read, isTrue);
    expect(Note.kinds, {'permission', 'question', 'blocked', 'done'}, reason: '알림 두 가지 — 고를 것과 끝냄');
  });

  test('쪽지 종류 → 말·색은 상태 칩과 같은 체계', () {
    const scheme = ColorScheme.dark();
    expect(noteKindStyle('done', scheme).$1, '끝냄');
    for (final kind in ['permission', 'question', 'blocked']) {
      expect(noteKindStyle(kind, scheme).$2, const Color(0xffFA8C2A), reason: '$kind 은 고를 것');
    }
    expect(noteKindStyle('weird', scheme).$1, 'weird');
  });

  test('시간은 사람 말로', () {
    final now = DateTime(2026, 9, 8, 3, 0);
    expect(timeAgo(now.subtract(const Duration(seconds: 20)), now), '방금');
    expect(timeAgo(now.subtract(const Duration(minutes: 5)), now), '5분 전');
    expect(timeAgo(now.subtract(const Duration(hours: 3)), now), '3시간 전');
    expect(timeAgo(now.subtract(const Duration(days: 2)), now), '2일 전');
  });
}
