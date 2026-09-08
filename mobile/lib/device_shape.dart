import 'dart:ui';

/// 화면 네 귀의 둥근 정도(논리 px). 화면 가장자리에 두르는 테(`_StudentFrame`)가 이
/// 값과 다르면 귀퉁이가 비거나 테가 화면 밖으로 잘린다(2026-09-08 지적 「꼭짓점 쪽이
/// 비어 있음」 — 46 으로 박아 뒀는데 iPhone 16 Pro 는 62). iOS 는 이 값을 안 알려 주므로
/// 논리 크기로 기종을 가른다. 모르는 크기는 요즘 기종의 흔한 값 55.
double screenCornerRadius(Size logical) {
  final short = logical.shortestSide.round();
  final long = logical.longestSide.round();
  return switch ((short, long)) {
    (402, 874) || (440, 956) => 62, // 16 Pro · 16 Pro Max
    (393, 852) || (430, 932) => 55, // 14 Pro·15·15 Pro·16 / Plus·Pro Max
    (390, 844) => 47, // 12·13·14
    (428, 926) => 53, // 12·13 Pro Max · 14 Plus
    (375, 812) => 44, // 12·13 mini (X·XS 는 39 — 지금 쓰는 기기가 아니다)
    (414, 896) => 41, // XR·11
    _ => 55,
  };
}
