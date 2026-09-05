import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/grid.dart';
import 'package:kasaterm_mobile/reflow.dart';

Run r(String t, {CellColor? bg, int flags = 0}) =>
    Run(t, const DefaultColor(), bg ?? const DefaultColor(), flags);

String text(List<Run> runs) => runs.map((x) => x.text).join();

void main() {
  test('짧은 줄은 뒤 빈칸을 잘라 한 줄', () {
    final out = reflowRow([r('hi'), r('      ')], 40);
    expect(out.chunks.map(text), ['hi']);
  });

  test('색 있는 빈칸은 안 자른다', () {
    final out = reflowRow([r('a'), r('  ', bg: const IndexColor(4))], 40);
    expect(out.chunks.map(text), ['a  ']);
  });

  test('긴 줄은 열 수로 접고 조각 시작 열을 남긴다', () {
    final out = reflowRow([r('a' * 95)], 40);
    expect(out.chunks.map((c) => text(c).length), [40, 40, 15]);
    expect(out.starts, [0, 40, 80]);
  });

  test('wide 글자는 조각 끝에서 잘리지 않고 다음 줄로', () {
    final out = reflowRow([r('${'a' * 39}가b')], 40);
    expect(out.chunks.map(text), ['a' * 39, '가b']);
  });

  test('테두리 줄은 접지 않고 폭에 맞춘다', () {
    final out = reflowRow([r('╭${'─' * 98}╮')], 40);
    expect(out.chunks.map(text), ['╭${'─' * 38}╮']);
  });

  test('상자 안 짧은 글은 오른쪽 세로선을 당겨 한 줄', () {
    final out = reflowRow([r('│ hi${' ' * 95}│')], 40);
    expect(out.chunks.map(text), ['│ hi${' ' * 35}│']);
  });

  test('상자 안 긴 글은 접는다', () {
    final out = reflowRow([r('│ ${'x' * 96} │')], 40);
    expect(out.chunks.length, 3);
  });

  test('라벨 낀 테두리는 선을 줄여 한 줄 — 데스크톱의 상자 윗변 그대로', () {
    final out = reflowRow([r('${'─' * 77} mobile ─')], 44);
    expect(out.chunks.map(text), ['${'─' * 35} mobile ─']);
  });

  test('선이 모자라면 테두리 취급을 않고 접는다', () {
    final out = reflowRow([r('── ${'x' * 60} ──')], 40);
    expect(out.chunks.length, greaterThan(1));
  });

  test('긴 글줄은 낱말 가운데가 아니라 빈칸에서 끊고 그 빈칸은 버린다', () {
    final out = reflowRow([
      r('Fable 5.1 1M | main | kasaterm | 17% | xhigh'),
    ], 40);
    expect(out.chunks.map(text), [
      'Fable 5.1 1M | main | kasaterm | 17% |',
      'xhigh',
    ]);
    expect(out.starts, [0, 39]);
  });

  test('빈칸이 너무 멀면 그냥 자른다', () {
    final out = reflowRow([r('ab ${'x' * 60}')], 40);
    expect(out.chunks.map(text), ['ab ${'x' * 37}', 'x' * 23]);
  });

  test('빈 행은 빈 줄 하나', () {
    expect(reflowRow(const [], 40).chunks, [<Run>[]]);
  });

  test('전체 접기: 커서가 접힌 조각으로 옮겨지고 아래 빈 행은 잘린다', () {
    final g = Grid()
      ..apply({
        'cols': 100,
        'rows': 4,
        'dirty': [
          [
            0,
            [
              ['a' * 95, null, null, 0],
            ],
          ],
          [
            1,
            [
              ['prompt', null, null, 0],
            ],
          ],
        ],
        'cursor': [0, 45],
      });
    final v = Reflow().apply(g, 40);
    expect(v.lines.map(text), ['a' * 40, 'a' * 40, 'a' * 15, 'prompt']);
    expect(v.cursorRow, 1);
    expect(v.cursorCol, 5);
  });

  combinedTests();

  test('커서가 놓인 빈 행은 남긴다', () {
    final g = Grid()
      ..apply({
        'cols': 10,
        'rows': 3,
        'dirty': [
          [
            0,
            [
              ['x', null, null, 0],
            ],
          ],
        ],
        'cursor': [1, 0],
      });
    final v = Reflow().apply(g, 10);
    expect(v.rows, 2);
    expect(v.cursorRow, 1);
  });
}

void combinedTests() {
  test('지난 줄 위에 살아 있는 화면이 이어지고 커서는 그만큼 내려간다', () {
    final g = Grid()
      ..apply({
        'cols': 10,
        'rows': 2,
        'dirty': [
          [
            0,
            [
              ['live', null, null, 0],
            ],
          ],
        ],
        'cursor': [0, 2],
      });
    final c = CombinedGrid([
      [const Run('old1', DefaultColor(), DefaultColor(), 0)],
      [const Run('old2', DefaultColor(), DefaultColor(), 0)],
    ], g);
    expect(c.rows, 4);
    expect(c.cursorRow, 2);
    final v = Reflow().apply(c, 10);
    expect(v.lines.map(text), ['old1', 'old2', 'live']);
    expect(v.cursorRow, 2);
  });
}
