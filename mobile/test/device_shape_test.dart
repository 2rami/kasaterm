import 'dart:ui';

import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/device_shape.dart';

void main() {
  test('기종별 귀 반지름 — 가로세로 순서와 무관, 모르는 폰 크기는 55, 아이패드는 18', () {
    expect(screenCornerRadius(const Size(402, 874)), 62);
    expect(screenCornerRadius(const Size(874, 402)), 62);
    expect(screenCornerRadius(const Size(393, 852)), 55);
    expect(screenCornerRadius(const Size(390, 844)), 47);
    expect(screenCornerRadius(const Size(1024, 1366)), 18);
    expect(screenCornerRadius(const Size(1210, 834)), 18);
    expect(screenCornerRadius(const Size(744, 1133)), 21.5);
    expect(screenCornerRadius(const Size(507, 1210)), 18, reason: '스플릿 뷰 반쪽');
  });
}
