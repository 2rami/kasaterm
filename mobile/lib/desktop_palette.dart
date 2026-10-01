import 'dart:math' as math;
import 'dart:ui';

import 'contrast.dart';
import 'grid.dart';
import 'server.dart';

/// 바탕과 글자가 서는 밝기(OKLab L). 데스크톱 내장 Light(.979/.242)·Dark(.290/.949)와
/// 카푸치노 모카(.243)·나쵸(.22) 사이에 둔다 — 어느 팔레트에서 뒤집어도 그 밝기의 내장 테마 곁에 선다.
const _lightBg = 0.975, _lightText = 0.25;
const _darkBg = 0.25, _darkText = 0.93;

/// 강조·위험색이 바탕 위 글자로도 읽히는 바닥 — 데스크톱 `accent_text_bg` 와 같은 4.5.
const _inkContrast = 4.5;

extension DesktopPalette on DesignTokens {
  /// 배경 밝기로 본 명암. 서버 응답의 `dark` 는 테마 키가 `light` 인지만 봐서 카푸치노 라테 같은
  /// 밝은 프리셋도 어둡다고 한다 — 데스크톱 `is_light` 와 같은 BT.601 휘도로 다시 잰다.
  bool get looksDark {
    final r = (bg >> 16) & 0xff, g = (bg >> 8) & 0xff, b = bg & 0xff;
    return 0.299 * r + 0.587 * g + 0.114 * b <= 128;
  }

  /// 같은 팔레트를 [dark] 밝기로. 데스크톱이 이미 그 밝기면 그대로 두고, 반대면 바탕·면·선·글자를
  /// 밝기 축에서만 거울처럼 뒤집는다 — 색조·채도는 그대로라 같은 테마로 읽힌다.
  /// 강조·위험색은 원래 색을 지키고 새 바탕 위에서 읽힐 만큼만 민다. 학생 색은 이름표라 손대지 않는다.
  DesignTokens inBrightness({required bool dark}) {
    final from = looksDark;
    if (from == dark) {
      return this.dark == dark ? this : _copy(dark: dark, onAccent: onAccent);
    }
    final srcBg = _Oklab.of(bg).l, srcText = _Oklab.of(text).l;
    final toBg = dark ? _darkBg : _lightBg, toText = dark ? _darkText : _lightText;
    final span = srcText - srcBg;
    int mirror(int argb) {
      final c = _Oklab.of(argb);
      final t = span.abs() < 1e-3 ? 0.0 : (c.l - srcBg) / span;
      return c.withL(toBg + t * (toText - toBg)).argb;
    }

    final newBg = mirror(bg);
    int ink(int argb) =>
        enforceContrast(Color(argb), Color(newBg), _inkContrast).toARGB32();
    final newAccent = ink(accent);
    return DesignTokens(
      dark: dark,
      bg: newBg,
      fg: mirror(fg),
      accent: newAccent,
      ansi: dark ? base16Dark : base16Light,
      surface: mirror(surface),
      surfaceHover: mirror(surfaceHover),
      border: mirror(border),
      text: mirror(text),
      textDim: mirror(textDim),
      onAccent: _onColor(newAccent),
      danger: ink(danger),
      characterAccents: characterAccents,
      minContrast: minContrast,
    );
  }

  DesignTokens _copy({required bool dark, required int onAccent}) => DesignTokens(
    dark: dark,
    bg: bg,
    fg: fg,
    accent: accent,
    ansi: ansi,
    surface: surface,
    surfaceHover: surfaceHover,
    border: border,
    text: text,
    textDim: textDim,
    onAccent: onAccent,
    danger: danger,
    characterAccents: characterAccents,
    minContrast: minContrast,
  );
}

int _onColor(int fill) {
  final l = luminance(Color(fill));
  return contrastOf(l, 0) >= contrastOf(l, 1) ? 0xff000000 : 0xffffffff;
}

/// 밝기만 옮기기 위한 OKLab — 사람 눈의 밝기 축이 색조와 갈라져 있어 L 만 바꿔도 색이 안 돈다.
class _Oklab {
  const _Oklab(this.l, this.a, this.b);
  final double l, a, b;

  static double _lin(int c) {
    final v = c / 255.0;
    return v <= 0.04045 ? v / 12.92 : math.pow((v + 0.055) / 1.055, 2.4).toDouble();
  }

  static double _gamma(double v) =>
      v <= 0.0031308 ? 12.92 * v : 1.055 * math.pow(v, 1 / 2.4) - 0.055;

  static double _cbrt(double v) => v < 0 ? -math.pow(-v, 1 / 3).toDouble() : math.pow(v, 1 / 3).toDouble();

  factory _Oklab.of(int argb) {
    final r = _lin((argb >> 16) & 0xff), g = _lin((argb >> 8) & 0xff), b = _lin(argb & 0xff);
    final l = _cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
    final m = _cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
    final s = _cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
    return _Oklab(
      0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
      1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
      0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    );
  }

  _Oklab withL(double l) => _Oklab(l.clamp(0.0, 1.0), a, b);

  /// sRGB 밖으로 나가면 색조를 지킨 채 채도만 줄인다 — 채널별로 자르면 색이 돈다.
  int get argb {
    var k = 1.0;
    var rgb = _rgb(l, a, b);
    for (var i = 0; i < 24 && !rgb.every((v) => v >= -1e-4 && v <= 1 + 1e-4); i++) {
      k *= 0.85;
      rgb = _rgb(l, a * k, b * k);
    }
    int ch(double v) => (_gamma(v.clamp(0.0, 1.0)) * 255).round().clamp(0, 255);
    return 0xff000000 | ch(rgb[0]) << 16 | ch(rgb[1]) << 8 | ch(rgb[2]);
  }

  static List<double> _rgb(double l, double a, double b) {
    final l_ = math.pow(l + 0.3963377774 * a + 0.2158037573 * b, 3).toDouble();
    final m_ = math.pow(l - 0.1055613458 * a - 0.0638541728 * b, 3).toDouble();
    final s_ = math.pow(l - 0.0894841775 * a - 1.2914855480 * b, 3).toDouble();
    return [
      4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_,
      -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_,
      -0.0041960863 * l_ - 0.7034186147 * m_ + 1.7076147010 * s_,
    ];
  }
}
