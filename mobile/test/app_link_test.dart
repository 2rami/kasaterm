import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/app_link.dart';

void main() {
  group('AppLink.parse', () {
    test('kasaterm://open 링크에서 root·machine·pane 을 읽는다', () {
      final l = AppLink.parse(
        Uri.parse(
          'kasaterm://open?root=https%3A%2F%2Fh%2Fu%2Fabc%2F&machine=%EB%A7%A5%EB%AF%B8%EB%8B%88&pane=%253',
        ),
      );
      expect(l, isNotNull);
      expect(l!.root, 'https://h/u/abc/');
      expect(l.machine, '맥미니');
      expect(l.pane, '%3');
    });

    test('스킴이 벗겨져 와도 쿼리로 알아보고, 빈 값은 null', () {
      final l = AppLink.parse(Uri.parse('/?pane=%253&machine='));
      expect(l, isNotNull);
      expect(l!.pane, '%3');
      expect(l.machine, isNull);
      expect(l.root, isNull);
    });

    test('보통 경로는 링크가 아니다', () {
      expect(AppLink.parse(Uri.parse('/')), isNull);
      expect(AppLink.parse(Uri.parse('https://h/u/abc/')), isNull);
      expect(AppLink.parse(null), isNull);
    });
  });
}
