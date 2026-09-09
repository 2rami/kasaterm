import 'dart:ui' show Size;

import 'package:flutter/foundation.dart'
    show TargetPlatform, defaultTargetPlatform, kIsWeb;

import 'server.dart';

/// 이 폰의 모양을 `POST mobile/device` 에 실을 꼴로. 크기는 MediaQuery 의 논리 크기
/// (CSS px 와 같은 단위)라 학생 쪽 KasaChrome 이 그대로 흉내 낸다.
DeviceInfo describeDevice({
  required Size logical,
  required double devicePixelRatio,
  TargetPlatform? platform,
  bool? web,
}) {
  final isWeb = web ?? kIsWeb;
  final p = platform ?? defaultTargetPlatform;
  return DeviceInfo(
    width: logical.width.round(),
    height: logical.height.round(),
    dpr: devicePixelRatio,
    platform: platformName(p, web: isWeb),
    model: isWeb ? null : guessModel(logical, p),
  );
}

/// ios · android · web. 나머지 데스크톱은 이름 그대로(macos·windows·linux) —
/// `dart:io` 없이 `defaultTargetPlatform` 으로만 가른다(크롬 개발 루프가 깨진다).
String platformName(TargetPlatform p, {bool web = false}) {
  if (web) return 'web';
  return switch (p) {
    TargetPlatform.iOS => 'ios',
    TargetPlatform.android => 'android',
    TargetPlatform.macOS => 'macos',
    TargetPlatform.windows => 'windows',
    TargetPlatform.linux => 'linux',
    TargetPlatform.fuchsia => 'fuchsia',
  };
}

/// 기종 이름은 iOS 가 안 알려 준다(플러그인 없이). 논리 크기로 흔한 기종만 짚고,
/// 모르는 크기는 null — 목록에 틀린 이름을 싣느니 비워 둔다.
String? guessModel(Size logical, TargetPlatform p) {
  if (p != TargetPlatform.iOS) return null;
  final short = logical.shortestSide.round();
  final long = logical.longestSide.round();
  return switch ((short, long)) {
    (402, 874) => 'iPhone 16 Pro',
    (440, 956) => 'iPhone 16 Pro Max',
    (393, 852) => 'iPhone 15',
    (430, 932) => 'iPhone 15 Plus',
    (390, 844) => 'iPhone 14',
    (428, 926) => 'iPhone 14 Plus',
    (375, 812) => 'iPhone 13 mini',
    (414, 896) => 'iPhone 11',
    (820, 1180) => 'iPad Air',
    (834, 1194) => 'iPad Pro 11',
    (1024, 1366) => 'iPad Pro 12.9',
    _ => null,
  };
}
