import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/grid.dart';

Map<String, Object?> frame({
  int cols = 10,
  int rows = 3,
  List<Object?> dirty = const [],
  List<int> cursor = const [0, 0],
  bool appCursor = false,
}) =>
    {
      't': 'grid',
      'cols': cols,
      'rows': rows,
      'dirty': dirty,
      'cursor': cursor,
      'cursorVisible': true,
      'appCursor': appCursor,
      'bracketedPaste': false,
    };

void main() {
  group('Grid.apply', () {
    test('런을 행에 넣고 커서·모드를 읽는다', () {
      final g = Grid();
      g.apply(frame(dirty: [
        [
          0,
          [
            ['ab', null, null, 0],
            [
              '가나',
              1,
              [10, 20, 30],
              flagBold,
            ],
          ],
        ],
        [
          2,
          [
            ['x', 200, null, flagUnderline],
          ],
        ],
      ], cursor: [2, 1], appCursor: true));
      expect(g.cols, 10);
      expect(g.rows, 3);
      expect(g.rowText(0), 'ab가나');
      expect(g.rowText(1), '');
      expect(g.rowText(2), 'x');
      expect(g.cursorRow, 2);
      expect(g.cursorCol, 1);
      expect(g.appCursor, isTrue);
      expect(g.version, 1);
      final run = g.lines[0][1];
      expect(run.fg, isA<IndexColor>().having((c) => c.index, 'index', 1));
      expect(run.bg, isA<RgbColor>().having((c) => c.g, 'g', 20));
      expect(run.flags & flagBold, flagBold);
      // wide 글자는 스페이서 칸이 빠져 오므로 셀 수는 글자 수보다 크다.
      expect(g.lines[0].fold(0, (n, r) => n + r.cells), 2 + 4);
    });

    test('바뀐 행만 새 객체로 갈아 끼우고 나머지는 그대로 둔다', () {
      final g = Grid();
      g.apply(frame(dirty: [
        [
          0,
          [
            ['first', null, null, 0],
          ],
        ],
        [
          1,
          [
            ['keep', null, null, 0],
          ],
        ],
      ]));
      final keep = g.lines[1];
      g.apply(frame(dirty: [
        [
          0,
          [
            ['second', null, null, 0],
          ],
        ],
      ]));
      expect(g.rowText(0), 'second');
      expect(identical(g.lines[1], keep), isTrue);
      expect(g.version, 2);
    });

    test('크기가 바뀌면 행을 비우고 다시 받는다', () {
      final g = Grid();
      g.apply(frame(dirty: [
        [
          0,
          [
            ['old', null, null, 0],
          ],
        ],
      ]));
      g.apply(frame(cols: 5, rows: 2));
      expect(g.lines.length, 2);
      expect(g.rowText(0), '');
      expect(g.cols, 5);
    });

    test('범위 밖 행과 깨진 런은 버린다', () {
      final g = Grid();
      g.apply(frame(rows: 2, dirty: [
        [
          5,
          [
            ['nope', null, null, 0],
          ],
        ],
        [
          0,
          [
            ['ok', null, null, 0],
            ['short'],
          ],
        ],
      ]));
      expect(g.rowText(0), 'ok');
      expect(g.lines.length, 2);
    });
  });

  group('cellWidth', () {
    test('한글·CJK·이모지는 두 칸, 라틴·박스드로잉은 한 칸, 결합문자는 0', () {
      expect(cellWidth('가'.runes.first), 2);
      expect(cellWidth('漢'.runes.first), 2);
      expect(cellWidth(0x1f600), 2);
      expect(cellWidth('a'.runes.first), 1);
      expect(cellWidth('─'.runes.first), 1);
      expect(cellWidth('│'.runes.first), 1);
      expect(cellWidth(0x0301), 0);
      expect(cellWidth(0x200d), 0);
      expect(cellWidth(0xfe0f), 0);
      expect(cellWidth(0xe0b0), 1);
    });
  });

  group('palette256', () {
    test('큐브와 회색 계산이 xterm 과 같다', () {
      expect(palette256(16, dark: true), 0xff000000);
      expect(palette256(231, dark: true), 0xffffffff);
      expect(palette256(232, dark: true), 0xff080808);
      expect(palette256(255, dark: true), 0xffeeeeee);
      expect(palette256(196, dark: true), 0xffff0000);
    });

    test('0–15 는 테마별 표를 쓴다', () {
      expect(palette256(1, dark: true), base16Dark[1]);
      expect(palette256(1, dark: false), base16Light[1]);
      expect(base16Dark[1] != base16Light[1], isTrue);
    });
  });
}
