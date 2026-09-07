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
      'kind': 'done_ok',
      'summary': '폰 상태줄 한 줄로 → 폴더부터 빼서 맞춤',
      'when': 1700000000,
      'read': false,
      'image': true,
    }, machine: '맥미니');
    expect(n.id, 7);
    expect(n.machine, '맥미니');
    expect(n.key, '맥미니|7');
    expect(n.when.millisecondsSinceEpoch, 1700000000 * 1000);
    expect(n.image, isTrue);
    expect(n.copyWith(read: true).read, isTrue);
    expect(n.asked, '', reason: '빠진 칸은 빈 문자열');
  });

  test('쪽지 종류 → 말·색은 상태 칩과 같은 체계', () {
    const scheme = ColorScheme.dark();
    expect(noteKindStyle('done_ok', scheme).$1, '끝냄');
    expect(noteKindStyle('permission', scheme).$2, const Color(0xffFA8C2A));
    expect(noteKindStyle('done_fail', scheme).$2, scheme.error);
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
