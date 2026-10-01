import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/contrast.dart';
import 'package:kasaterm_mobile/look.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/screens/controls.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/twins_loading.dart';

/// 데스크톱 `GET /design-tokens` 의 팔레트 — 내장 Dark, 카푸치노 라테, 앰버 CRT(바탕이 이미 호박빛).
DesignTokens desktop(String theme, Map<String, String> palette) => DesignTokens.fromJson({
  'theme': theme,
  'palette': palette,
  'ansi': List.filled(16, '#808080'),
})!;

final palettes = {
  'Dark': desktop('dark', {
    'bg': '#252c35', 'fg': '#ffffff', 'surface': '#1a1d23', 'surface_hover': '#303843',
    'border': '#505c6e6e', 'accent': '#5a8ce6', 'on_accent': '#000000', 'text': '#eceef3',
    'text_dim': '#a0a6b0', 'danger': '#e0584e',
  }),
  '라테': desktop('catppuccin-latte', {
    'bg': '#eff1f5', 'fg': '#4c4f69', 'surface': '#e6e9ef', 'surface_hover': '#dce0e8',
    'border': '#bcc0ccb4', 'accent': '#5a8ce6', 'on_accent': '#000000', 'text': '#4c4f69',
    'text_dim': '#6a6d82', 'danger': '#d20f39',
  }),
  '앰버 CRT': desktop('amber-crt', {
    'bg': '#1a130c', 'fg': '#ffb000', 'surface': '#251b11', 'surface_hover': '#362717',
    'border': '#5e452296', 'accent': '#ff8c42', 'on_accent': '#000000', 'text': '#ffb000',
    'text_dim': '#dba24a', 'danger': '#ff5a33',
  }),
};

double contrast(Color a, Color b) => contrastOf(luminance(a), luminance(b));

void main() {
  for (final MapEntry(key: name, value: tokens) in palettes.entries) {
    test('$name — 쌍둥이 그림은 물 위에서 3:1, 판은 바탕보다 떠 있다', () {
      // 앱이 입는 길 그대로 — 「데스크톱 따라감」은 바탕 밝기로 밝음·어두움을 고른다.
      final themes = appThemes(ThemeMode.system, tokens);
      final theme = themes.mode == ThemeMode.dark ? themes.dark : themes.light;
      final tone = theme.extension<TwinTone>()!;
      expect(contrast(tone.skyInk, tone.skyWash), greaterThanOrEqualTo(3));
      expect(contrast(tone.amberInk, tone.amberWash), greaterThanOrEqualTo(3));
      final bg = theme.scaffoldBackgroundColor;
      // 판이 바탕보다 밝아야 「떠 있다」 — 데스크톱 surface 를 그대로 깔면 Dark·라테 둘 다 바탕보다 어두웠다.
      expect(luminance(tone.card), greaterThan(luminance(bg)));
      // 판 위 본문 글자는 그대로 읽힌다.
      expect(contrast(theme.colorScheme.onSurface, tone.card), greaterThanOrEqualTo(4.5));
    });
  }

  Widget host(Widget child, {bool still = false}) => MaterialApp(
    theme: buildTheme(Brightness.light),
    home: Builder(
      builder: (context) => MediaQuery(
        data: MediaQuery.of(context).copyWith(disableAnimations: still),
        child: Scaffold(body: Center(child: child)),
      ),
    ),
  );

  testWidgets('앱바 쌍둥이는 새로 고칠 때만 뛰고, 동작 줄이기면 서 있다', (tester) async {
    await tester.pumpWidget(host(const TwinsMark()));
    expect(tester.hasRunningAnimations, isFalse);
    await tester.pumpWidget(host(const TwinsMark(hopping: true)));
    expect(tester.hasRunningAnimations, isTrue);
    await tester.pumpWidget(host(const TwinsMark(hopping: true), still: true));
    await tester.pumpAndSettle();
    expect(tester.hasRunningAnimations, isFalse);
  });

  testWidgets('아이콘 고르기 — 글자 없이 고르고, 이름은 읽기 도구에 남는다', (tester) async {
    ThemeMode? picked;
    await tester.pumpWidget(
      host(
        IconChoice<ThemeMode>(
          options: const [
            (ThemeMode.system, Icons.desktop_windows_outlined, '데스크톱 따라감'),
            (ThemeMode.light, Icons.light_mode_outlined, '밝게'),
            (ThemeMode.dark, Icons.dark_mode_outlined, '어둡게'),
          ],
          selected: ThemeMode.system,
          onSelect: (m) => picked = m,
        ),
      ),
    );
    expect(find.text('밝게'), findsNothing);
    expect(find.bySemanticsLabel('밝게'), findsOneWidget);
    for (final icon in [Icons.desktop_windows_outlined, Icons.light_mode_outlined, Icons.dark_mode_outlined]) {
      expect(tester.getSize(find.ancestor(of: find.byIcon(icon), matching: find.byType(InkWell))).width,
          greaterThanOrEqualTo(Look.tap));
    }
    await tester.tap(find.byIcon(Icons.dark_mode_outlined));
    expect(picked, ThemeMode.dark);
  });
}
