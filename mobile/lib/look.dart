import 'package:flutter/material.dart';

/// 카사모바일 폰의 단추·배치 값. 정본은 docs/design.md 「카사모바일 폰 — 단추·배치」 — 값을 바꾸려면
/// 거기부터 고치고 여기를 맞춘다. 화면 파일 안에 새 치수를 만들지 않는다.
abstract final class Look {
  static const tap = 44.0;
  static const radius = 6.0;
  static const border = 1.0;

  static const buttonH = 44.0;
  static const buttonPadX = 16.0;
  static const iconSize = 20.0;

  static const chipH = 24.0;
  static const chipPadX = 8.0;

  /// 상태 칩이 줄 폭에서 가져갈 수 있는 몫 — 이름 3 : 상태 2.
  static const nameFlex = 3;
  static const statusFlex = 2;

  static const title = 18.0;
  static const group = 16.0;
  static const body = 15.0;
  static const sub = 13.0;
  static const chip = 12.0;

  static const pagePad = 16.0;
  static const groupGap = 24.0;
  static const groupTitleGap = 8.0;
  static const appBarH = 56.0;

  static const row1 = 48.0;
  static const row2 = 60.0;
  static const rowGap = 2.0;

  static const stripe = 2.0;
  static const face = 32.0;

  static const roomHeadH = 40.0;
  static const mapMaxPhone = 140.0;
  static const mapMaxPad = 200.0;

  static const fieldGap = 12.0;
  static const inputMaxLines = 6;

  /// 경고 띠 바탕 — `danger` 12%. 원색 채움 금지.
  static const dangerTint = 0.12;

  /// 눌림 — 바탕에 `text` 8%.
  static const pressTint = 0.08;

  static BorderRadius get corners => BorderRadius.circular(radius);

  /// 아이패드 판정은 device_shape 와 같다 — 짧은 변 600 이상이거나 긴 변 1000 이상.
  static bool isPad(BuildContext context) {
    final s = MediaQuery.sizeOf(context);
    return s.shortestSide >= 600 || s.longestSide >= 1000;
  }
}
