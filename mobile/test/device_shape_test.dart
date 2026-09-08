import 'dart:ui';

import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/device_shape.dart';

void main() {
  test('기종별 귀 반지름 — 가로세로 순서와 무관, 모르는 크기는 55', () {
    expect(screenCornerRadius(const Size(402, 874)), 62);
    expect(screenCornerRadius(const Size(874, 402)), 62);
    expect(screenCornerRadius(const Size(393, 852)), 55);
    expect(screenCornerRadius(const Size(390, 844)), 47);
    expect(screenCornerRadius(const Size(1024, 1366)), 55);
  });
}
