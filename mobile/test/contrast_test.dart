import 'dart:ui';

import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/contrast.dart';
import 'package:kasaterm_mobile/grid.dart';

const bg = Color(0xff252c35);

void main() {
  test('바닥이 1 이하면 손대지 않는다', () {
    const fg = Color(0xff505c6e);
    expect(enforceContrast(fg, bg, 1.0), fg);
  });

  test('이미 충분히 벌어진 색은 그대로', () {
    const fg = Color(0xffffffff);
    expect(enforceContrast(fg, bg, 3.5), fg);
  });

  test('어두운 바탕의 흐릿한 회색은 흰색 쪽으로 밀려 바닥에 닿는다', () {
    const fg = Color(0xff505c6e);
    final out = enforceContrast(fg, bg, 3.5);
    expect(
      contrastOf(luminance(out), luminance(bg)),
      greaterThanOrEqualTo(3.5),
    );
    expect(luminance(out), greaterThan(luminance(fg)));
    // 데스크톱 theme.rs 와 같은 여덟 번 이분 — 값까지 같아야 한다.
    expect(out, const Color(0xff76808e));
  });

  test('밝은 바탕에서는 검정 쪽으로 민다', () {
    const light = Color(0xffffffff);
    const fg = Color(0xffc0c0c0);
    final out = enforceContrast(fg, light, 3.5);
    expect(luminance(out), lessThan(luminance(fg)));
  });

  test('흐림은 바탕 쪽으로 55% 섞는다', () {
    expect(
      mixToward(const Color(0xffffffff), const Color(0xff000000), 0.55),
      const Color(0xff737373),
    );
  });

  test('기본색·ANSI 16색은 보정 대상이 아니다', () {
    expect(namesOwnColor(const DefaultColor()), isFalse);
    expect(namesOwnColor(const IndexColor(7)), isFalse);
    expect(namesOwnColor(const IndexColor(16)), isTrue);
    expect(namesOwnColor(const RgbColor(1, 2, 3)), isTrue);
  });
}
