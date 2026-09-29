import 'dart:math' as math;

import 'package:flutter/rendering.dart';
import 'package:flutter/widgets.dart';

// 넓은 화면(아이패드·가로 폰·스플릿 뷰) 배치. 기준은 기기 종류가 아니라 창 폭이다 —
// 아이패드도 스플릿 뷰에선 폰만큼 좁아지고, 큰 폰은 가로로 눕히면 넓어진다.
// 값의 정본은 `docs/design.md` 「태블릿(아이패드)」.

/// 방 상자 한 열의 최소 폭 — 가장 좁은 폰(320) 한 장에 여백을 조금 더한 것.
const roomColumnMinWidth = 340.0;

/// 방 상자 열 사이. 상자의 아래 여백(10)과 같게 둬 가로·세로 틈이 같아 보이게.
const roomColumnGap = 10.0;

/// 이 폭부터 나쵸 첫 화면의 「대화」와 「작업」을 탭 대신 나란히 편다.
const sideBySideMinWidth = 900.0;

/// 나란히 펼 때 오른쪽 「작업」 열의 폭 — 폰에서 다듬은 작업판을 그 폭 그대로 쓴다.
const workColumnWidth = 400.0;

/// 대화처럼 읽는 열의 상한(PC 설정 화면 `CONTENT_MAX_W` 와 같은 값).
const readColumnMaxWidth = 800.0;

/// [width] 에 [minWidth] 이상인 열이 몇 개 들어가는지. 폰 세로는 1열이다.
int columnsFor(
  double width, {
  double minWidth = roomColumnMinWidth,
  double gap = roomColumnGap,
}) => math.max(1, ((width + gap) / (minWidth + gap)).floor());

bool sideBySide(double width) => width >= sideBySideMinWidth;

/// 높이가 제각각인 상자를 차례대로 가장 낮은 열에 쌓는다(같으면 왼쪽). 돌려주는 것은
/// 상자마다 (열, 위쪽 y). 차례를 지키므로 위에서 아래로 읽으면 원래 순서에 가깝다.
List<(int, double)> masonryPlace(List<double> heights, int columns) {
  final tops = List<double>.filled(math.max(1, columns), 0);
  return [
    for (final h in heights)
      () {
        var col = 0;
        for (var c = 1; c < tops.length; c++) {
          if (tops[c] < tops[col]) col = c;
        }
        final y = tops[col];
        tops[col] += h;
        return (col, y);
      }(),
  ];
}

/// 벽돌 쌓기 — 받은 폭에 [minColumnWidth] 이상인 열을 되는 만큼 세우고, 자식을 차례대로
/// 가장 낮은 열에 놓는다. 1열이면 그냥 세로 줄이다(폰 모양이 그대로 남는다).
class Masonry extends MultiChildRenderObjectWidget {
  const Masonry({
    super.key,
    this.minColumnWidth = roomColumnMinWidth,
    this.gap = roomColumnGap,
    super.children,
  });

  final double minColumnWidth;
  final double gap;

  @override
  RenderMasonry createRenderObject(BuildContext context) =>
      RenderMasonry(minColumnWidth: minColumnWidth, gap: gap);

  @override
  void updateRenderObject(BuildContext context, RenderMasonry renderObject) {
    renderObject
      ..minColumnWidth = minColumnWidth
      ..gap = gap;
  }
}

class MasonryParentData extends ContainerBoxParentData<RenderBox> {}

class RenderMasonry extends RenderBox
    with
        ContainerRenderObjectMixin<RenderBox, MasonryParentData>,
        RenderBoxContainerDefaultsMixin<RenderBox, MasonryParentData> {
  RenderMasonry({required double minColumnWidth, required double gap})
    : _minColumnWidth = minColumnWidth,
      _gap = gap;

  double _minColumnWidth;
  set minColumnWidth(double v) {
    if (v == _minColumnWidth) return;
    _minColumnWidth = v;
    markNeedsLayout();
  }

  double _gap;
  set gap(double v) {
    if (v == _gap) return;
    _gap = v;
    markNeedsLayout();
  }

  @override
  void setupParentData(RenderBox child) {
    if (child.parentData is! MasonryParentData) {
      child.parentData = MasonryParentData();
    }
  }

  @override
  void performLayout() {
    final width = constraints.maxWidth;
    final cols = columnsFor(width, minWidth: _minColumnWidth, gap: _gap);
    final colWidth = (width - _gap * (cols - 1)) / cols;
    final heights = <double>[];
    for (var c = firstChild; c != null; c = childAfter(c)) {
      c.layout(BoxConstraints.tightFor(width: colWidth), parentUsesSize: true);
      heights.add(c.size.height);
    }
    final spots = masonryPlace(heights, cols);
    var bottom = 0.0;
    var i = 0;
    for (var c = firstChild; c != null; c = childAfter(c)) {
      final (col, y) = spots[i];
      (c.parentData! as MasonryParentData).offset = Offset(
        col * (colWidth + _gap),
        y,
      );
      bottom = math.max(bottom, y + heights[i]);
      i++;
    }
    size = constraints.constrain(Size(width, bottom));
  }

  @override
  void paint(PaintingContext context, Offset offset) =>
      defaultPaint(context, offset);

  @override
  bool hitTestChildren(BoxHitTestResult result, {required Offset position}) =>
      defaultHitTestChildren(result, position: position);
}
