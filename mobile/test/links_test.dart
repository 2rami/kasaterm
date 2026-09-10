import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/grid.dart';
import 'package:kasaterm_mobile/links.dart';

List<Run> row(String text) => [Run(text, const DefaultColor(), const DefaultColor(), 0)];

void main() {
  test('https 주소를 찾고 끝의 구두점은 뗀다', () {
    final hits = detectLinks([row('봐라 https://example.com/a?b=1.')], 80);
    expect(hits.map((h) => h.url), ['https://example.com/a?b=1']);
    // 「봐라 」는 두 칸 글자 둘 — 주소는 5번째 칸부터.
    expect(hits.single.segments, [(0, 5, 30)]);
  });

  test('www. 는 https 를 붙이고, 닫는 괄호는 짝이 있을 때만 남긴다', () {
    final hits = detectLinks([
      row('www.a.com/x) 그리고 (https://b.com/y)'),
    ], 80);
    expect(hits.map((h) => h.url), ['https://www.a.com/x', 'https://b.com/y']);
  });

  test('폭을 꽉 채운 줄 끝에서 다음 줄로 접힌 주소는 하나로 잇는다', () {
    const cols = 20;
    final lines = [row('go https://example.c'), row('om/long/path 끝')];
    final hits = detectLinks(lines, cols);
    expect(hits.single.url, 'https://example.com/long/path');
    expect(hits.single.segments, [(0, 3, 20), (1, 0, 12)]);
    expect(linkAt(lines, cols, 1, 5)?.url, 'https://example.com/long/path');
    expect(linkAt(lines, cols, 1, 13), isNull);
  });

  test('짧은 줄 끝의 주소는 다음 줄과 안 잇는다', () {
    final lines = [row('https://a.com'), row('b.com')];
    expect(detectLinks(lines, 80).single.url, 'https://a.com');
  });

  test('두 칸 글자 뒤의 열 번호가 칸과 맞는다', () {
    final hits = detectLinks([row('한글 https://k.com')], 80);
    expect(hits.single.segments, [(0, 5, 18)]);
  });
}
