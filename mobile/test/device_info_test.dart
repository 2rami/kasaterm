import 'dart:ui';

import 'package:flutter/foundation.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/device_info.dart';

void main() {
  test('논리 크기·배율·플랫폼이 mobile/device 꼴로 나간다', () {
    final d = describeDevice(
      logical: const Size(393, 852),
      devicePixelRatio: 3,
      platform: TargetPlatform.iOS,
      web: false,
    );
    expect(d.toJson(), {
      'width': 393,
      'height': 852,
      'dpr': 3.0,
      'platform': 'ios',
      'model': 'iPhone 15',
    });
  });

  test('웹은 platform=web 이고 기종은 안 짚는다', () {
    final d = describeDevice(
      logical: const Size(393.4, 851.6),
      devicePixelRatio: 2,
      platform: TargetPlatform.iOS,
      web: true,
    );
    expect(d.platform, 'web');
    expect(d.model, isNull);
    expect(d.width, 393);
    expect(d.height, 852);
    expect(d.toJson().containsKey('model'), isFalse);
  });

  test('안드로이드·모르는 크기는 기종이 비고 platform 만', () {
    expect(platformName(TargetPlatform.android), 'android');
    expect(platformName(TargetPlatform.macOS), 'macos');
    expect(guessModel(const Size(1, 1), TargetPlatform.iOS), isNull);
    expect(guessModel(const Size(393, 852), TargetPlatform.android), isNull);
  });
}
