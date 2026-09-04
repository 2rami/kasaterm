import 'package:flutter/material.dart';
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
          ['true', [255, 128, 0], [0, 0, 128], 0],
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
    final loader = FontLoader('TermMono')
      ..addFont(rootBundle.load('assets/fonts/D2CodingLigatureNerdFontMono-Regular.ttf'))
      ..addFont(rootBundle.load('assets/fonts/D2CodingLigatureNerdFontMono-Bold.ttf'));
    await loader.load();
  });

  testWidgets('다크 — 폭이 넉넉하면 원 크기로', (tester) async {
    final g = sample();
    await tester.pumpWidget(host(g, _dark, 320));
    await tester.pump();
    await expectLater(find.byType(GridCanvas), matchesGoldenFile('goldens/grid_dark.png'));
    expect(g.rowText(0), 'abc 가나다 x');
  });

  testWidgets('라이트 — 폭이 모자라면 줄여 담는다', (tester) async {
    final g = sample();
    await tester.pumpWidget(host(g, _light, 120));
    await tester.pump();
    await expectLater(find.byType(GridCanvas), matchesGoldenFile('goldens/grid_light_fit.png'));
  });
}
