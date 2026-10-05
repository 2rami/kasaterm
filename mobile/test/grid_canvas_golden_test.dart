import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/grid.dart';
import 'package:kasaterm_mobile/grid_canvas.dart';

/// 한 장에 한글(두 칸)·트루컬러·굵게/반전/밑줄/흐림·박스드로잉·커서를 다 담는다.
Grid sample() => Grid()
  ..apply({
    'cols': 24,
    'rows': 5,
    'dirty': [
      [
        0,
        [
          ['abc ', null, null, 0],
          ['가나다', null, null, 0],
          [' x', null, null, 0],
        ],
      ],
      [
        1,
        [
          ['bold ', null, null, flagBold],
          ['inverse', null, null, flagInverse],
          [' dim', null, null, flagDim],
        ],
      ],
      [
        2,
        [
          ['red ', 1, null, 0],
          [
            'true',
            [255, 128, 0],
            [0, 0, 128],
            0,
          ],
          [' 208', 208, null, 0],
        ],
      ],
      [
        3,
        [
          ['┌──┐ ', null, null, 0],
          ['under', null, null, flagUnderline],
          [' 漢字', 4, null, flagItalic],
        ],
      ],
    ],
    'cursor': [3, 2],
    'cursorVisible': true,
    'appCursor': false,
    'bracketedPaste': false,
  });

const _dark = TerminalPalette(
  dark: true,
  fg: Color(0xffc0caf5),
  bg: Color(0xff12161c),
  cursor: Color(0xff7ab8ff),
  ansi: base16Dark,
);

const _light = TerminalPalette(
  dark: false,
  fg: Color(0xff15294a),
  bg: Colors.white,
  cursor: Color(0xff4a90e2),
  ansi: base16Light,
);

Widget host(Grid g, TerminalPalette palette, double width) => MaterialApp(
  home: Scaffold(
    body: Center(
      child: SizedBox(
        width: width,
        height: 120,
        child: GridCanvas(grid: g, version: g.version, palette: palette),
      ),
    ),
  ),
);

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  setUpAll(() async {
    // flutter test 는 글꼴을 Ahem 사각형으로 바꾼다 — 번들 ttf 를 직접 올려야
    // 실제 글리프(한글·박스드로잉)가 찍힌다.
    final mono = FontLoader('TermMono')
      ..addFont(
        rootBundle.load('assets/fonts/JetBrainsMonoNerdFontMono-Regular.ttf'),
      )
      ..addFont(
        rootBundle.load('assets/fonts/JetBrainsMonoNerdFontMono-Bold.ttf'),
      );
    final hangul = FontLoader('TermHangul')
      ..addFont(
        rootBundle.load(
          'assets/fonts/D2CodingLigatureNerdFontMono-Regular.ttf',
        ),
      )
      ..addFont(
        rootBundle.load('assets/fonts/D2CodingLigatureNerdFontMono-Bold.ttf'),
      );
    await mono.load();
    await hangul.load();
  });

  testWidgets('다크 — 폭이 남으면 폭을 채운다', (tester) async {
    final g = sample();
    await tester.pumpWidget(host(g, _dark, 320));
    await tester.pump();
    await expectLater(
      find.byType(GridCanvas),
      matchesGoldenFile('goldens/grid_dark.png'),
    );
    expect(g.rowText(0), 'abc 가나다 x');
  });

  testWidgets('라이트 — 폭이 모자라면 높이를 채우고 옆으로 민다', (tester) async {
    final g = sample();
    await tester.pumpWidget(host(g, _light, 120));
    await tester.pump();
    await expectLater(
      find.byType(GridCanvas),
      matchesGoldenFile('goldens/grid_light_fit.png'),
    );
  });

  testWidgets('한글은 두 칸을 채운다 — 반 폭 글꼴로 벌어지거나 옆 칸으로 밀리지 않는다', (tester) async {
    // 「를」은 가로획이 글자 폭을 다 덮어 잉크 열이 끊기지 않는다. 아랫줄 「─」 은 칸을
    // 끝까지 채우는 자 — 여섯 칸의 실제 폭이다. FillViewer 는 덮어 채우므로(cover) 8열×4줄을
    // 폭에 맞춰 두 줄만 보이게 잘라 오른쪽 두 칸은 넘친 잉크를 받을 자리로 남긴다.
    final g = Grid()
      ..apply({
        'cols': 8,
        'rows': 4,
        'dirty': [
          [
            0,
            [
              ['를를를', null, null, 0],
            ],
          ],
          [
            1,
            [
              ['──────', null, null, 0],
            ],
          ],
        ],
        'cursor': [3, 7],
        'cursorVisible': false,
      });
    final key = GlobalKey();
    await tester.pumpWidget(
      MaterialApp(
        home: Center(
          child: RepaintBoundary(
            key: key,
            child: SizedBox(
              width: 200,
              height: 150,
              child: GridCanvas(grid: g, version: g.version, palette: _dark),
            ),
          ),
        ),
      ),
    );
    await tester.pump();
    final boundary =
        key.currentContext!.findRenderObject()! as RenderRepaintBoundary;
    final (image, bytes) = (await tester.runAsync(() async {
      final image = await boundary.toImage();
      return (image, await image.toByteData());
    }))!;
    final w = image.width, h = image.height;
    bool inked(int x, int y) {
      final i = (y * w + x) * 4;
      return (bytes!.getUint8(i) - 0x12).abs() +
              (bytes.getUint8(i + 1) - 0x16).abs() +
              (bytes.getUint8(i + 2) - 0x1c).abs() >
          120;
    }

    // 채우기 배율은 FillViewer 가 정한다 — 줄 경계는 잉크 띠로 찾는다.
    final bands = <(int, int)>[];
    int? top;
    for (var y = 0; y <= h; y++) {
      final on =
          y < h && [for (var x = 0; x < w; x++) x].any((x) => inked(x, y));
      if (on && top == null) top = y;
      if (!on && top != null) {
        bands.add((top, y));
        top = null;
      }
    }
    // 「를」 은 ㄹ·ㅡ·ㄹ 세 띠, 맨 아래 띠가 「─」.
    expect(bands.length, greaterThanOrEqualTo(2), reason: '$bands');

    List<int> columns((int, int) band) => [
      for (var x = 0; x < w; x++)
        if ([for (var y = band.$1; y < band.$2; y++) y].any((y) => inked(x, y)))
          x,
    ];
    final hangul = columns((bands.first.$1, bands.last.$1));
    final rule = columns(bands.last);
    expect(hangul, isNotEmpty);
    expect(rule, isNotEmpty);
    final cell = (rule.last - rule.first + 1) / 6;
    // 1em 글자를 0.6em 칸 두 개(1.2em)에 그대로 두면 잉크가 상자의 72% — 데스크톱은 86%.
    expect(hangul.length / (rule.last - rule.first + 1), greaterThan(0.8));
    expect(hangul.first - rule.first, lessThan(cell * 0.3));
    expect(hangul.last, lessThanOrEqualTo(rule.last + 1));
    image.dispose();
  });
}
