import 'dart:math' as math;
import 'dart:ui';

import 'grid.dart';

/// 데스크톱 `theme.rs` 의 대비 바닥과 같은 공식 — 같은 셀 색이 두 화면에서 같은 값으로
/// 나와야 폰이 「다른 테마」로 보이지 않는다. 숫자를 바꾸면 그쪽도 같이 바꿔라.
final List<double> _srgb = List.generate(256, (i) {
  final c = i / 255.0;
  return c <= 0.04045
      ? c / 12.92
      : math.pow((c + 0.055) / 1.055, 2.4).toDouble();
});

/// WCAG 상대 휘도.
double luminance(Color c) {
  final v = c.toARGB32();
  return 0.2126 * _srgb[(v >> 16) & 0xff] +
      0.7152 * _srgb[(v >> 8) & 0xff] +
      0.0722 * _srgb[v & 0xff];
}

double contrastOf(double a, double b) {
  final hi = math.max(a, b), lo = math.min(a, b);
  return (hi + 0.05) / (lo + 0.05);
}

/// `fg` 를 `bg` 와 `min` 이상 벌어질 때까지 흰색 또는 검정 쪽으로 민다. 바닥에 못
/// 닿는 바탕(중간 회색)도 있어 여덟 번 이분한 자리에서 멈춘다.
Color enforceContrast(Color fg, Color bg, double min) {
  if (min <= 1.0) return fg;
  final lBg = luminance(bg);
  if (contrastOf(luminance(fg), lBg) >= min) return fg;
  final target = lBg > 0.18 ? 0.0 : 255.0;
  final v = fg.toARGB32();
  final r = (v >> 16) & 0xff, g = (v >> 8) & 0xff, b = v & 0xff;
  Color mix(double t) => Color.fromARGB(
    255,
    (r + (target - r) * t).round(),
    (g + (target - g) * t).round(),
    (b + (target - b) * t).round(),
  );
  var lo = 0.0, hi = 1.0;
  for (var i = 0; i < 8; i++) {
    final mid = (lo + hi) * 0.5;
    if (contrastOf(luminance(mix(mid)), lBg) >= min) {
      hi = mid;
    } else {
      lo = mid;
    }
  }
  return mix(hi);
}

/// `fg` 를 `bg` 쪽으로 `t` 만큼 섞는다 — 흐림(SGR 2) 글자.
Color mixToward(Color fg, Color bg, double t) {
  final a = fg.toARGB32(), b = bg.toARGB32();
  int ch(int shift) {
    final x = (a >> shift) & 0xff, y = (b >> shift) & 0xff;
    return (x * (1 - t) + y * t).round();
  }

  return Color.fromARGB(255, ch(16), ch(8), ch(0));
}

/// 셀이 테마가 정한 색이 아니라 스스로 고른 색인가 — 기본색과 ANSI 0~15 는 팔레트가
/// 이미 읽히게 골라 두었으니 그 위에 보정을 덧대지 않는다.
bool namesOwnColor(CellColor c) => switch (c) {
  DefaultColor() => false,
  IndexColor(:final index) => index >= 16,
  RgbColor() => true,
};
