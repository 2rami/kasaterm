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

  static const chipPadX = 8.0;

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
  static const subLine = 18.0;

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

  // 대화와 모달은 A 이전 모습이다(design.md 「예외 — 대화와 모달」) — 플랫 규칙 밖의 값.
  static const bubbleRadius = 16.0;

  /// 말한 쪽 아래 모서리 — 누구 말인지 꼬리로 가리킨다.
  static const bubbleTail = 4.0;
  static const bubblePadX = 14.0;
  static const bubblePadY = 10.0;

  /// 말풍선 맞은편에 남기는 폭 — 내 말과 상대 말이 한 줄에 겹쳐 보이지 않게.
  static const bubbleFar = 48.0;
  static const dialogRadius = 28.0;
  static const sheetRadius = 28.0;
  static const modalButtonRadius = 8.0;

  /// 켤 때 계정 확인 화면의 쌍둥이(`twins_loading.dart`) — 그림 한 장의 정사각 크기, 동생이 뛰는 높이
  /// (언니는 절반), 발밑 그림자 두께, 한 박. 두 사람이 반 박씩 엇갈려 번갈아 뛴다.
  static const twins = 136.0;
  static const twinsHop = 12.0;
  static const twinsShadow = 6.0;
  static const twinsBeat = Duration(milliseconds: 1100);

  static BorderRadius get corners => BorderRadius.circular(radius);

  /// 아이패드 판정은 device_shape 와 같다 — 짧은 변 600 이상이거나 긴 변 1000 이상.
  static bool isPad(BuildContext context) {
    final s = MediaQuery.sizeOf(context);
    return s.shortestSide >= 600 || s.longestSide >= 1000;
  }
}
