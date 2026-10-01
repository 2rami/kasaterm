import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/contrast.dart';
import 'package:kasaterm_mobile/desktop_palette.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/theme_prefs.dart';

/// 데스크톱 `GET /design-tokens` 가 실제로 보내는 값(2026-10-01, 내장 Dark·카푸치노 라테).
DesignTokens desktop(String theme, Map<String, String> palette) => DesignTokens.fromJson({
  'theme': theme,
  'palette': palette,
  'ansi': List.filled(16, '#808080'),
})!;

final darkPreset = desktop('dark', {
  'bg': '#252c35',
  'fg': '#ffffff',
  'surface': '#1a1d23',
  'surface_hover': '#303843',
  'border': '#505c6e6e',
  'accent': '#5a8ce6',
  'on_accent': '#000000',
  'text': '#eceef3',
  'text_dim': '#a0a6b0',
  'danger': '#e0584e',
});

final latte = desktop('catppuccin-latte', {
  'bg': '#eff1f5',
  'fg': '#4c4f69',
  'surface': '#e6e9ef',
  'surface_hover': '#dce0e8',
  'border': '#bcc0ccb4',
  'accent': '#5a8ce6',
  'on_accent': '#000000',
  'text': '#4c4f69',
  'text_dim': '#6a6d82',
  'danger': '#d20f39',
});

double contrast(int a, int b) =>
    contrastOf(luminance(Color(a)), luminance(Color(b)));

double hue(int argb) => HSVColor.fromColor(Color(argb)).hue;

void main() {
  test('밝기는 테마 키가 아니라 바탕으로 잰다', () {
    expect(darkPreset.looksDark, isTrue);
    // 서버 응답의 dark 는 키가 'light' 인지만 봐서 라테를 어둡다고 한다.
    expect(latte.dark, isTrue);
    expect(latte.looksDark, isFalse);
  });

  test('같은 밝기면 데스크톱 색 그대로', () {
    final same = darkPreset.inBrightness(dark: true);
    expect(same.bg, darkPreset.bg);
    expect(same.accent, darkPreset.accent);
    expect(same.ansi, darkPreset.ansi);
    final l = latte.inBrightness(dark: false);
    expect(l.dark, isFalse);
    expect(l.bg, latte.bg);
    expect(l.text, latte.text);
  });

  for (final (name, source, toDark) in [
    ('Dark → 밝게', darkPreset, false),
    ('라테 → 어둡게', latte, true),
  ]) {
    test('$name: 같은 색조로 뒤집고 읽힌다', () {
      final t = source.inBrightness(dark: toDark);
      expect(t.dark, toDark);
      expect(t.looksDark, toDark);
      expect(contrast(t.text, t.bg), greaterThanOrEqualTo(7));
      expect(contrast(t.textDim, t.bg), greaterThanOrEqualTo(4.5));
      expect(contrast(t.accent, t.bg), greaterThanOrEqualTo(4.5));
      expect(contrast(t.danger, t.bg), greaterThanOrEqualTo(4.5));
      expect(contrast(t.onAccent, t.accent), greaterThanOrEqualTo(4.5));
      // 바탕의 푸른 기·강조색의 파랑이 그대로 따라온다.
      expect((hue(t.accent) - hue(source.accent)).abs(), lessThan(8));
      expect((hue(t.bg) - hue(source.bg)).abs(), lessThan(25));
      // 면·선·흐린 글자의 순서(바탕에서 글자 쪽으로 얼마나 갔나)가 유지된다.
      final from = contrast(source.textDim, source.bg) > contrast(source.border, source.bg);
      expect(contrast(t.textDim, t.bg) > contrast(t.border, t.bg), from);
      expect(t.characterAccents, source.characterAccents);
    });
  }

  testWidgets('폰에서 밝게를 골라도 데스크톱 팔레트를 입는다', (tester) async {
    addTearDown(() {
      phoneThemeMode.value = ThemeMode.system;
      designTokens.value = null;
    });
    late ThemeData seen;
    final probe = Builder(builder: (context) {
      seen = Theme.of(context);
      return const SizedBox();
    });
    await tester.pumpWidget(_ThemeHost(child: probe));
    designTokens.value = darkPreset;
    phoneThemeMode.value = ThemeMode.light;
    await tester.pumpAndSettle();
    final flipped = darkPreset.inBrightness(dark: false);
    expect(seen.brightness, Brightness.light);
    expect(seen.scaffoldBackgroundColor, Color(flipped.bg));
    expect(seen.colorScheme.primary, Color(flipped.accent));

    phoneThemeMode.value = ThemeMode.system;
    await tester.pumpAndSettle();
    expect(seen.brightness, Brightness.dark);
    expect(seen.scaffoldBackgroundColor, Color(darkPreset.bg));
    expect(seen.colorScheme.primary, Color(darkPreset.accent));
  });
}

/// `KasatermApp` 의 테마 고르기만 — RootScreen 은 저장소·네트워크를 부른다.
class _ThemeHost extends StatelessWidget {
  const _ThemeHost({required this.child});
  final Widget child;

  @override
  Widget build(BuildContext context) => ValueListenableBuilder<ThemeMode>(
    valueListenable: phoneThemeMode,
    builder: (context, mode, _) => ValueListenableBuilder<DesignTokens?>(
      valueListenable: designTokens,
      builder: (context, tokens, _) {
        final themes = appThemes(mode, tokens);
        return MaterialApp(
          themeMode: themes.mode,
          theme: themes.light,
          darkTheme: themes.dark,
          home: child,
        );
      },
    ),
  );
}
