import 'package:flutter/material.dart';

import 'contrast.dart';

/// 카사모바일 폰의 단추·배치 값. 정본은 docs/design.md 「카사모바일 폰 — 단추·배치」 — 값을 바꾸려면
/// 거기부터 고치고 여기를 맞춘다. 화면 파일 안에 새 치수를 만들지 않는다.
abstract final class Look {
  static const tap = 44.0;
  static const border = 1.0;

  /// 모서리 세 단계(쌍둥이 결) — 작은 칸 · 조작 · 판. 칩·알약은 완전 둥긂이라 값이 없다.
  static const radiusSm = 8.0;
  static const radius = 12.0;
  static const radiusLg = 18.0;

  static const buttonH = 44.0;
  static const buttonPadX = 18.0;
  static const iconSize = 20.0;

  static const chipH = 24.0;

  /// 아이콘 고르기 알약(`IconChoice`)의 안쪽 여백 — 높이·칸 폭은 [tap].
  static const choiceInset = 3.0;
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

  /// 판 안쪽 좌우 여백 — 방 제목 줄·설정 줄의 글자선.
  static const cardPad = 14.0;

  /// 판 사이 — 방 판 아래.
  static const cardGap = 12.0;

  /// 설정 줄 최소 높이와 그 왼쪽 아이콘 타일.
  static const settingRow = 56.0;
  static const tile = 32.0;

  /// 목록 줄 썸네일 — 파일 40, 폴더 48(KASA-share).
  static const thumb = 40.0;
  static const thumbLg = 48.0;

  /// 설정 맨 위 계정 판의 쌍둥이 그림, 강조색 고르기 동그라미.
  static const accountArt = 64.0;
  static const swatch = 34.0;

  /// 학생 줄 상태 띠 — 판 안쪽 왼쪽의 알약.
  static const stripe = 3.0;
  static const stripeH = 28.0;
  static const stripeX = 5.0;
  static const face = 32.0;

  /// 기기 머리글의 기기색 물 동그라미.
  static const machineBadge = 36.0;

  static const roomHeadH = 44.0;
  static const mapMaxPhone = 140.0;
  static const mapMaxPad = 200.0;

  static const fieldGap = 12.0;

  /// 대화 보기 입력줄 위, 붙여 두고 아직 안 보낸 사진.
  static const attachThumb = 48.0;
  static const inputMaxLines = 6;

  /// 학생 화면 터미널 ↔ 대화 밀기(design.md 「터미널 ↔ 대화 밀기」). 가로가 세로의 이 배수 이상일
  /// 때만 쪽을 넘긴다(약 34° 안) — 그보다 비스듬하면 세로 읽기 스크롤이 가져간다.
  static const swipeRatio = 1.5;

  /// iOS 뒤로 가기 가장자리 띠 — CupertinoPageRoute 와 같은 폭(왼쪽 안전 여백이 더 넓으면 그 폭).
  /// 여기서 시작한 밀기는 쪽 넘김이 받지 않는다.
  static const backEdge = 20.0;

  /// 단추·점으로 바꿀 때 쪽이 미끄러지는 시간. 동작 줄이기면 바로 바뀐다.
  static const viewFlip = Duration(milliseconds: 240);

  /// 자판이 떠 전환 줄을 접었을 때 지금 보기를 알리는 점 둘.
  static const viewDot = 6.0;
  static const viewDotGap = 6.0;

  /// 격자 밖 흐르는 글(인라인 코드·코드 칸·도구 결과·글자 선택)의 고정폭 대체 글꼴. TermHangul 은
  /// 한글 진행 폭이 반 칸이라 칸마다 놓는 격자에서만 맞고, 흐르는 글에선 한글이 서로 포개진다 —
  /// 한글은 UI 고딕으로 보낸다(데스크톱 인라인 코드와 같은 규칙).
  static const flowMonoFallback = ['Pretendard', 'TermSymbol'];

  /// 경고 띠 바탕 — `danger` 12%. 원색 채움 금지.
  static const dangerTint = 0.12;

  /// 눌림 — 바탕에 `text` 8%.
  static const pressTint = 0.08;

  /// 판 그림자(밝은 테마) — 검정 알파·흐림·아래. 어두운 테마는 그림자 대신 `border` [cardEdge] 테.
  static const cardShadow = 0.06;
  static const cardBlur = 18.0;
  static const cardDrop = 6.0;
  static const cardEdge = 0.6;

  // 대화와 모달은 A 이전 모습이다(design.md 「예외 — 대화와 모달」).
  static const bubbleRadius = 16.0;

  /// 말한 쪽 아래 모서리 — 누구 말인지 꼬리로 가리킨다.
  static const bubbleTail = 4.0;
  static const bubblePadX = 14.0;
  static const bubblePadY = 10.0;

  /// 말풍선 맞은편에 남기는 폭 — 내 말과 상대 말이 한 줄에 겹쳐 보이지 않게.
  static const bubbleFar = 48.0;
  static const dialogRadius = 28.0;
  static const sheetRadius = 28.0;
  static const modalButtonRadius = 12.0;

  /// 켤 때 계정 확인 화면의 쌍둥이(`twins_loading.dart`) — 그림 한 장의 정사각 크기, 동생이 뛰는 높이
  /// (언니는 절반), 발밑 그림자 두께, 한 박. 두 사람이 반 박씩 엇갈려 번갈아 뛴다.
  static const twins = 136.0;
  static const twinsHop = 12.0;
  static const twinsShadow = 6.0;
  static const twinsBeat = Duration(milliseconds: 1100);

  /// 빈·실패 화면과 목록 받는 중의 쌍둥이 그림.
  static const twinsSmall = 96.0;

  /// 앱바 쌍둥이 표 — 얼굴 동그라미, 둘이 겹치는 폭, 새로 고칠 때 뛰는 높이.
  static const markFace = 30.0;
  static const markOverlap = 10.0;
  static const markHop = 4.0;

  /// 당겨서 새로 고침 — 문턱, 다 당겼을 때 얼굴 크기.
  static const pullTrigger = 80.0;
  static const pullFace = 40.0;

  /// 바탕 머리 빛이 닿는 위쪽 높이(`TwinBackdrop`).
  static const glowH = 240.0;

  /// 확인 중 띠 — 두께, 빛이 한 번 흐르는 시간.
  static const twinBarH = 2.0;
  static const twinBarSweep = Duration(milliseconds: 1400);

  static BorderRadius get corners => BorderRadius.circular(radius);
  static BorderRadius get cardCorners => BorderRadius.circular(radiusLg);
  static BorderRadius get smallCorners => BorderRadius.circular(radiusSm);

  /// 아이패드 판정은 device_shape 와 같다 — 짧은 변 600 이상이거나 긴 변 1000 이상.
  static bool isPad(BuildContext context) {
    final s = MediaQuery.sizeOf(context);
    return s.shortestSide >= 600 || s.longestSide >= 1000;
  }

  /// 동작 줄이기 — 깡총·흐름·떠오르기를 멈춘다. iOS 「동작 줄이기」는 엔진이 reduceMotion 으로만
  /// 알린다 — MediaQuery 에는 안 실린다.
  static bool still(BuildContext context) =>
      MediaQuery.disableAnimationsOf(context) ||
      View.of(context).platformDispatcher.accessibilityFeatures.reduceMotion;
}

/// 쌍둥이 두 색을 지금 테마 위에 얹은 값 — 테마마다 다시 계산한다(design.md 「쌍둥이 결」).
/// 장식·포인트 전용이다. 상태 뜻과 기기색 자리에는 쓰지 않는다.
@immutable
class TwinTone extends ThemeExtension<TwinTone> {
  const TwinTone({
    required this.skyWash,
    required this.amberWash,
    required this.skyInk,
    required this.amberInk,
    required this.skyGlow,
    required this.amberGlow,
    required this.card,
    required this.cardShadow,
    required this.cardEdge,
  });

  /// 그림에서 뽑은 원래 두 색 — 언니 비옷·우산 하늘 칸, 동생 비옷·우산 호박 칸.
  static const sky = Color(0xff8ccbf5);
  static const amber = Color(0xfff5c95a);

  /// 판 위에 섞은 파스텔 — 아이콘 타일·당김 원 바탕.
  final Color skyWash, amberWash;

  /// 물 위 그림 — 대비 3:1 까지 민 두 색.
  final Color skyInk, amberInk;

  /// 바탕 위쪽 둥근 빛의 가운데 색(가장자리는 투명).
  final Color skyGlow, amberGlow;

  /// 판(방·설정 묶음) 채움·그림자·테. 그림자가 없는 테마는 투명, 테가 없는 테마도 투명이다.
  final Color card, cardShadow, cardEdge;

  /// 판은 바탕에서 떠 보여야 한다 — 데스크톱 팔레트의 `surface` 는 대개 바탕보다 어두워(Dark #1a1d23 < #252c35,
  /// 라테도) 그대로 깔면 꺼져 보였다. 어두운 테마는 데스크톱 `panel_bg`(바탕·`surface_hover` 반반), 밝은 테마는
  /// 바탕을 흰빛으로 60% 민다.
  factory TwinTone.on({
    required Brightness brightness,
    required Color surfaceHigh,
    required Color background,
    required Color outline,
  }) {
    final dark = brightness == Brightness.dark;
    final card = dark
        ? Color.lerp(background, Color.alphaBlend(surfaceHigh, background), 0.5)!
        : Color.lerp(background, Colors.white, 0.6)!;
    Color wash(Color c) =>
        Color.alphaBlend(c.withValues(alpha: dark ? 0.30 : 0.18), card);
    final skyWash = wash(sky), amberWash = wash(amber);
    final glow = dark ? 0.14 : 0.10;
    return TwinTone(
      skyWash: skyWash,
      amberWash: amberWash,
      skyInk: enforceContrast(sky, skyWash, 3),
      amberInk: enforceContrast(amber, amberWash, 3),
      skyGlow: sky.withValues(alpha: glow),
      amberGlow: amber.withValues(alpha: glow),
      card: card,
      cardShadow: dark
          ? Colors.transparent
          : Colors.black.withValues(alpha: Look.cardShadow),
      cardEdge: dark
          ? outline.withValues(alpha: Look.cardEdge)
          : Colors.transparent,
    );
  }

  static TwinTone of(BuildContext context) {
    final t = Theme.of(context);
    return t.extension<TwinTone>() ??
        TwinTone.on(
          brightness: t.brightness,
          surfaceHigh: t.colorScheme.surfaceContainerHigh,
          background: t.scaffoldBackgroundColor,
          outline: t.colorScheme.outline,
        );
  }

  /// 묶음 차례로 하늘·호박을 번갈아 — 짝수 하늘.
  (Color wash, Color ink) pick(int i) =>
      i.isEven ? (skyWash, skyInk) : (amberWash, amberInk);

  /// 방·설정 묶음의 둥근 판.
  BoxDecoration cardBox() => BoxDecoration(
    color: card,
    borderRadius: Look.cardCorners,
    border: cardEdge.a == 0 ? null : Border.all(color: cardEdge),
    boxShadow: cardShadow.a == 0
        ? null
        : [
            BoxShadow(
              color: cardShadow,
              blurRadius: Look.cardBlur,
              offset: const Offset(0, Look.cardDrop),
            ),
          ],
  );

  @override
  TwinTone copyWith() => this;

  @override
  TwinTone lerp(TwinTone? other, double t) {
    if (other == null) return this;
    Color m(Color a, Color b) => Color.lerp(a, b, t)!;
    return TwinTone(
      skyWash: m(skyWash, other.skyWash),
      amberWash: m(amberWash, other.amberWash),
      skyInk: m(skyInk, other.skyInk),
      amberInk: m(amberInk, other.amberInk),
      skyGlow: m(skyGlow, other.skyGlow),
      amberGlow: m(amberGlow, other.amberGlow),
      card: m(card, other.card),
      cardShadow: m(cardShadow, other.cardShadow),
      cardEdge: m(cardEdge, other.cardEdge),
    );
  }
}
