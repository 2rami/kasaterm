import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';

import 'look.dart';

/// 말풍선 속 마크다운의 글꼴 — 대화 본문과 인라인 이미지 앞뒤의 글이 같은 서체를 쓰도록 한다.
///
/// [codeBg] 는 말풍선 바탕과 달라야 코드가 보여서 부르는 쪽이 고른다.
MarkdownStyleSheet chatMarkdownStyle(
  ThemeData theme, {
  required TextStyle base,
  required Color codeBg,
}) {
  final scheme = theme.colorScheme;
  final size = base.fontSize ?? 15;
  final mono = TextStyle(
    fontFamily: 'TermMono',
    fontFamilyFallback: Look.flowMonoFallback,
    fontSize: size - 2,
    color: base.color,
    backgroundColor: codeBg,
  );
  final cell = base.copyWith(fontSize: size - 2, height: 1.4);
  const cellPad = EdgeInsets.symmetric(
    horizontal: Look.tableCellPadX,
    vertical: Look.tableCellPadY,
  );
  return MarkdownStyleSheet.fromTheme(theme).copyWith(
    p: base,
    // 굵기는 400·600 두 가지 — 마크다운 굵게도 600.
    strong: base.copyWith(fontWeight: FontWeight.w600),
    listBullet: base,
    tableBody: cell,
    tableHead: cell.copyWith(fontWeight: FontWeight.w600),
    tableHeadAlign: TextAlign.left,
    tableColumnWidth: const _CappedIntrinsicWidth(Look.tableCellMax),
    tableScrollbarThumbVisibility: true,
    // 가로 스크롤 막대가 마지막 줄을 덮지 않을 자리.
    tablePadding: const EdgeInsets.only(bottom: Look.tableCellPadY),
    tableBorder: TableBorder(
      horizontalInside: BorderSide(color: scheme.outline, width: Look.border),
      borderRadius: Look.smallCorners,
    ),
    tableCellsPadding: cellPad,
    tableHeadCellsPadding: cellPad,
    tableHeadCellsDecoration: BoxDecoration(color: codeBg),
    tableVerticalAlignment: TableCellVerticalAlignment.top,
    h1: base.copyWith(fontSize: size + 3, fontWeight: FontWeight.w600),
    h2: base.copyWith(fontSize: size + 1, fontWeight: FontWeight.w600),
    h3: base.copyWith(fontSize: size, fontWeight: FontWeight.w600),
    code: mono,
    codeblockPadding: const EdgeInsets.all(10),
    codeblockDecoration: BoxDecoration(
      color: codeBg,
      borderRadius: Look.smallCorners,
    ),
    blockquoteDecoration: BoxDecoration(
      border: Border(left: BorderSide(color: scheme.outline, width: 3)),
    ),
    blockquotePadding: const EdgeInsets.fromLTRB(10, 2, 0, 2),
    a: base.copyWith(
      color: scheme.primary,
      decoration: TextDecoration.underline,
    ),
  );
}

/// 표 칸 폭 — 말풍선 폭을 칸 수로 나누면 폰에서 한 칸이 두세 글자로 꺾였다. 글 길이만큼 세우되 긴 칸은
/// [cap] 에서 꺾고, 그래도 넘치면 표만 옆으로 민다. 패키지는 [IntrinsicColumnWidth] 일 때만 가로 스크롤로 감싼다.
class _CappedIntrinsicWidth extends IntrinsicColumnWidth {
  const _CappedIntrinsicWidth(this.cap);

  final double cap;

  @override
  double minIntrinsicWidth(Iterable<RenderBox> cells, double containerWidth) =>
      math.min(super.minIntrinsicWidth(cells, containerWidth), cap);

  @override
  double maxIntrinsicWidth(Iterable<RenderBox> cells, double containerWidth) =>
      math.min(super.maxIntrinsicWidth(cells, containerWidth), cap);
}
