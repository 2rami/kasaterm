import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/grid.dart';
import 'package:kasaterm_mobile/grid_canvas.dart';
import 'package:kasaterm_mobile/server.dart';

DesignTokens tokens(bool dark) => DesignTokens(
  dark: dark,
  fg: dark ? 0xffcccccc : 0xff222222,
  bg: dark ? 0xff111111 : 0xfffafafa,
  accent: 0xffc070d0,
  ansi: dark ? base16Dark : base16Light,
  surface: 0xff333333,
  surfaceHover: 0xff444444,
  border: 0xff555555,
  text: 0xffcccccc,
  textDim: 0xff888888,
  onAccent: 0xffffffff,
  danger: 0xffff0000,
  characterAccents: const {'seia': 0xffc070d0},
);

void main() {
  for (final dark in [true, false]) {
    testWidgets(
      'explicit ${dark ? 'dark' : 'light'} ignores source brightness',
      (tester) async {
        final source = tokens(!dark);
        late TerminalPalette palette;
        late TerminalPalette local;
        await tester.pumpWidget(
          MaterialApp(
            theme: ThemeData(
              brightness: dark ? Brightness.dark : Brightness.light,
            ),
            home: Builder(
              builder: (context) {
                local = TerminalPalette.of(context);
                palette = TerminalPalette.forViewer(
                  context,
                  mode: dark ? ThemeMode.dark : ThemeMode.light,
                  source: source,
                );
                return const SizedBox();
              },
            ),
          ),
        );
        expect(palette.dark, dark);
        expect(palette.bg, local.bg);
        expect(palette.fg, local.fg);
        expect(palette.ansi, local.ansi);
        final n = dark ? 0xfa : 0x11;
        final f = dark ? 0x22 : 0xcc;
        expect(palette.resolve(RgbColor(n, n, n), foreground: false), local.bg);
        expect(palette.resolve(RgbColor(f, f, f), foreground: true), local.fg);
        // Source-colored syntax, diff backgrounds and student accents survive.
        expect(
          palette.resolve(const RgbColor(192, 112, 208), foreground: true),
          const Color(0xffc070d0),
        );
        expect(
          palette.resolve(const RgbColor(32, 128, 48), foreground: false),
          const Color(0xff208030),
        );
        expect(
          palette.resolve(const RgbColor(128, 128, 128), foreground: false),
          const Color(0xff808080),
        );
        expect(source.characterAccents['seia'], 0xffc070d0);
      },
    );
  }

  testWidgets('explicit desktop-follow still uses source palette', (
    tester,
  ) async {
    final source = tokens(false);
    late TerminalPalette palette;
    await tester.pumpWidget(
      MaterialApp(
        theme: ThemeData.dark(),
        home: Builder(
          builder: (context) {
            palette = TerminalPalette.forViewer(
              context,
              mode: ThemeMode.system,
              source: source,
            );
            return const SizedBox();
          },
        ),
      ),
    );
    expect(palette, TerminalPalette.fromTokens(source));
  });
}
