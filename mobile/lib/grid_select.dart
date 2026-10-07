import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';

import 'grid.dart';
import 'reflow.dart';

/// 글자 사이 한 자리. [col] 은 칸 경계지만 두 칸 글자(한글)의 가운데는 되지 않는다.
@immutable
class GridSpot implements Comparable<GridSpot> {
  const GridSpot(this.row, this.col);
  final int row;
  final int col;

  @override
  int compareTo(GridSpot o) => row != o.row ? row - o.row : col - o.col;

  @override
  bool operator ==(Object other) =>
      other is GridSpot && other.row == row && other.col == col;

  @override
  int get hashCode => Object.hash(row, col);

  @override
  String toString() => 'GridSpot($row, $col)';
}

/// 격자 글자를 칸 단위로 읽는다 — 손가락 자리를 글자 경계로, 고른 범위를 글로.
class GridText {
  GridText(this.lines, {this.folds = const []});

  final List<List<Run>> lines;

  /// 폰 폭으로 접혀 이어진 줄([ReflowedGrid.folds]) — 복사할 때 줄바꿈 대신 이어 붙인다.
  final List<Fold?> folds;

  // 행 객체가 같으면 글도 같다(렌더러 캐시와 같은 규칙).
  static final _rows = Expando<_Row>();
  _Row _row(int r) => _rows[lines[r]] ??= _Row(lines[r]);

  int get rows => lines.length;
  int width(int r) => _row(r).width;

  Fold? _fold(int r) => r < folds.length ? folds[r] : null;

  /// [x] 칸(소수) 자리에서 가장 가까운 글자 경계 — 글자 가운데를 넘으면 그 뒤다.
  int boundary(int r, double x) {
    final row = _row(r);
    for (var i = 0; i < row.starts.length; i++) {
      final s = row.starts[i], w = row.widths[i];
      if (x < s + w / 2) return s;
      if (x < s + w) return s + w;
    }
    return row.width;
  }

  /// [x] 칸 자리의 낱말. 빈칸이면 왼쪽(없으면 오른쪽) 낱말, 글 뒤 빈 곳이면 그 줄 마지막 낱말.
  /// 글이 없는 줄이면 null.
  (int, int)? word(int r, double x) {
    final row = _row(r);
    final n = row.starts.length;
    if (n == 0) return null;
    var i = 0;
    while (i < n - 1 && x >= row.starts[i] + row.widths[i]) {
      i++;
    }
    if (_kind(row.chars[i]) == _space) {
      var j = i;
      while (j >= 0 && _kind(row.chars[j]) == _space) {
        j--;
      }
      if (j < 0) {
        j = i;
        while (j < n && _kind(row.chars[j]) == _space) {
          j++;
        }
        if (j == n) return null;
      }
      i = j;
    }
    final kind = _kind(row.chars[i]);
    var lo = i, hi = i;
    if (kind == _letter) {
      while (lo > 0 && _kind(row.chars[lo - 1]) == kind) {
        lo--;
      }
      while (hi < n - 1 && _kind(row.chars[hi + 1]) == kind) {
        hi++;
      }
    }
    return (row.starts[lo], row.starts[hi] + row.widths[hi]);
  }

  /// [r] 줄이 든 한 줄 전체(접혀 이어진 조각까지).
  (GridSpot, GridSpot) line(int r) {
    var lo = r, hi = r;
    while (lo > 0 && _fold(lo) != null) {
      lo--;
    }
    while (hi < rows - 1 && _fold(hi + 1) != null) {
      hi++;
    }
    return (GridSpot(lo, 0), GridSpot(hi, width(hi)));
  }

  /// [a]..[b] 의 글. 접혀 이어진 줄은 줄바꿈 없이 잇고(끊으며 버린 빈칸은 되살린다), 줄 끝 빈칸은 걷는다.
  String text(GridSpot a, GridSpot b) {
    if (b.compareTo(a) < 0) (a, b) = (b, a);
    final out = <String>[];
    final line = StringBuffer();
    for (var r = a.row; r <= b.row && r < rows; r++) {
      final fold = _fold(r);
      if (r > a.row) {
        if (fold == null) {
          out.add(line.toString().trimRight());
          line.clear();
        } else if (fold.gap) {
          line.write(' ');
        }
      }
      final from = math.max(r == a.row ? a.col : 0, fold?.indent ?? 0);
      final to = r == b.row ? b.col : width(r);
      line.write(_row(r).slice(from, to));
    }
    final last = line.toString();
    out.add(b.col >= width(b.row) ? last.trimRight() : last);
    return out.join('\n');
  }

  static const _space = 0, _letter = 1, _mark = 2;

  /// 낱말로 묶는 글자 — 글자·숫자와, 경로·주소가 한 낱말로 잡히게 `_-./~:@%+=?&#`.
  static int _kind(String ch) {
    final r = ch.runes.first;
    if (r == 0x20 || r == 0x09) return _space;
    if ((r >= 0x30 && r <= 0x39) ||
        (r >= 0x41 && r <= 0x5a) ||
        (r >= 0x61 && r <= 0x7a) ||
        '_-./~:@%+=?&#'.codeUnits.contains(r) ||
        (r >= 0xc0 && r <= 0x24f) ||
        (r >= 0x370 && r <= 0x52f) ||
        (r >= 0x1100 && r <= 0x11ff) ||
        (r >= 0x3040 && r <= 0x30ff) ||
        (r >= 0x3130 && r <= 0x318f) ||
        (r >= 0x3400 && r <= 0x9fff) ||
        (r >= 0xac00 && r <= 0xd7a3)) {
      return _letter;
    }
    return _mark;
  }
}

/// 한 줄의 글자 — 글자마다 시작 칸과 폭. 폭 0(결합 문자)은 앞 글자에 붙인다.
class _Row {
  _Row(List<Run> runs) {
    var col = 0;
    for (final run in runs) {
      for (final rune in run.text.runes) {
        final w = cellWidth(rune);
        if (w == 0) {
          if (chars.isNotEmpty) chars.last += String.fromCharCode(rune);
          continue;
        }
        starts.add(col);
        widths.add(w);
        chars.add(String.fromCharCode(rune));
        col += w;
      }
    }
    width = col;
  }

  final starts = <int>[];
  final widths = <int>[];
  final chars = <String>[];
  late final int width;

  String slice(int from, int to) {
    final b = StringBuffer();
    for (var i = 0; i < starts.length; i++) {
      if (starts[i] >= from && starts[i] < to) b.write(chars[i]);
    }
    return b.toString();
  }
}

/// 격자 글자를 둘레의 [SelectionArea] 에 올린다 — 마크다운 글처럼 길게 눌러 낱말을 잡고, 끌거나
/// 손잡이로 넓히고, 놓으면 복사 메뉴. 가장자리로 끌면 둘레 스크롤이 따라 굴린다.
class GridSelectable extends SingleChildRenderObjectWidget {
  const GridSelectable({
    super.key,
    required this.text,
    required this.cell,
    required this.color,
    super.child,
  });

  final GridText text;

  /// 칸 하나의 크기 — 자식이 그린 격자와 같아야 고른 자리가 글자에 맞는다.
  final Size cell;
  final Color color;

  @override
  RenderGridSelectable createRenderObject(BuildContext context) =>
      RenderGridSelectable(
        text: text,
        cell: cell,
        color: color,
        registrar: SelectionContainer.maybeOf(context),
      );

  @override
  void updateRenderObject(
    BuildContext context,
    RenderGridSelectable renderObject,
  ) {
    renderObject
      ..text = text
      ..cell = cell
      ..color = color
      ..registrar = SelectionContainer.maybeOf(context);
  }
}

class RenderGridSelectable extends RenderProxyBox
    with Selectable, SelectionRegistrant {
  RenderGridSelectable({
    required GridText text,
    required Size cell,
    required Color color,
    SelectionRegistrar? registrar,
  }) : _text = text,
       _cell = cell,
       _color = color {
    this.registrar = registrar;
  }

  GridText _text;
  Size _cell;
  Color _color;
  GridSpot? _start;
  GridSpot? _end;

  /// 길게 눌러 처음 잡은 낱말 — 끌어서 넓히는 동안 이 낱말은 늘 고른 안에 남는다.
  (GridSpot, GridSpot)? _origin;
  LayerLink? _startHandle;
  LayerLink? _endHandle;

  /// 둘레 스크롤의 선택 묶음은 자식이 먼저 버려진 뒤에도 손잡이를 거두러 한 번 더 부른다.
  bool _disposed = false;

  late final _geometry = ValueNotifier<SelectionGeometry>(_measure());

  GridText get text => _text;
  Size get cell => _cell;
  set text(GridText v) {
    if (identical(v, _text)) return;
    _text = v;
    _clamp();
    _changed();
  }

  set cell(Size v) {
    if (v == _cell) return;
    _cell = v;
    _changed();
  }

  set color(Color v) {
    if (v == _color) return;
    _color = v;
    markNeedsPaint();
  }

  /// 내용이 줄거나 바뀌어도 고른 자리가 글자 경계에 남게.
  void _clamp() {
    if (_text.rows == 0) {
      _start = _end = _origin = null;
      return;
    }
    GridSpot? fit(GridSpot? s) {
      if (s == null) return null;
      final r = math.min(s.row, _text.rows - 1);
      return GridSpot(r, _text.boundary(r, s.col.toDouble()));
    }

    _start = fit(_start);
    _end = fit(_end);
  }

  void _changed() {
    _geometry.value = _measure();
    markNeedsPaint();
  }

  @override
  SelectionGeometry get value => _geometry.value;

  @override
  void addListener(VoidCallback listener) => _geometry.addListener(listener);

  @override
  void removeListener(VoidCallback listener) =>
      _geometry.removeListener(listener);

  Offset _point(GridSpot s) =>
      Offset(s.col * _cell.width, (s.row + 1) * _cell.height);

  Rect _rowRect(int r, GridSpot lo, GridSpot hi) {
    final x0 = r == lo.row ? lo.col : 0;
    // 줄을 넘어가는 고름은 줄 끝까지 — 빈 줄도 한 칸은 칠해 줄바꿈이 든 것이 보이게.
    final x1 = r == hi.row ? hi.col : math.max(_text.width(r), x0 + 1);
    return Rect.fromLTRB(
      x0 * _cell.width,
      r * _cell.height,
      x1 * _cell.width,
      (r + 1) * _cell.height,
    );
  }

  SelectionGeometry _measure() {
    final a = _start, b = _end;
    if (a == null || b == null) {
      return SelectionGeometry(
        status: SelectionStatus.none,
        hasContent: _text.rows > 0,
      );
    }
    final forward = a.compareTo(b) <= 0;
    final collapsed = a == b;
    final (lo, hi) = forward ? (a, b) : (b, a);
    TextSelectionHandleType handle(bool first) => collapsed
        ? TextSelectionHandleType.collapsed
        : first
        ? TextSelectionHandleType.left
        : TextSelectionHandleType.right;
    return SelectionGeometry(
      status: collapsed
          ? SelectionStatus.collapsed
          : SelectionStatus.uncollapsed,
      hasContent: true,
      startSelectionPoint: SelectionPoint(
        localPosition: _point(a),
        lineHeight: _cell.height,
        handleType: handle(forward),
      ),
      endSelectionPoint: SelectionPoint(
        localPosition: _point(b),
        lineHeight: _cell.height,
        handleType: handle(!forward),
      ),
      selectionRects: collapsed
          ? const []
          : [for (var r = lo.row; r <= hi.row; r++) _rowRect(r, lo, hi)],
    );
  }

  Offset _local(Offset global) {
    final m = Matrix4.tryInvert(getTransformTo(null));
    return m == null ? global : MatrixUtils.transformPoint(m, global);
  }

  /// 손가락이 상자 위·아래로 나가면 앞·뒤 선택 가능한 것의 몫이다.
  SelectionResult _result(Offset local) => local.dy < 0
      ? SelectionResult.previous
      : local.dy >= size.height
      ? SelectionResult.next
      : SelectionResult.end;

  /// 손가락 자리의 글자 경계 — 상자 밖이면 처음·끝에 붙인다.
  GridSpot _spot(Offset local) {
    final rows = _text.rows;
    if (local.dy < 0) return const GridSpot(0, 0);
    final r = (local.dy / _cell.height).floor();
    if (r >= rows) return GridSpot(rows - 1, _text.width(rows - 1));
    return GridSpot(r, _text.boundary(r, math.max(0, local.dx / _cell.width)));
  }

  /// 손가락 자리의 낱말(없으면 그 자리 하나).
  (GridSpot, GridSpot) _wordAt(Offset local) {
    final s = _spot(local);
    final inside = local.dy >= 0 && s.row == (local.dy / _cell.height).floor();
    final w = inside
        ? _text.word(s.row, math.max(0, local.dx / _cell.width))
        : null;
    return w == null ? (s, s) : (GridSpot(s.row, w.$1), GridSpot(s.row, w.$2));
  }

  (GridSpot, GridSpot) _rangeAt(Offset local, TextGranularity g) => switch (g) {
    TextGranularity.word => _wordAt(local),
    TextGranularity.line ||
    TextGranularity.paragraph => _text.line(_spot(local).row),
    TextGranularity.document => (
      const GridSpot(0, 0),
      GridSpot(_text.rows - 1, _text.width(_text.rows - 1)),
    ),
    TextGranularity.character => (_spot(local), _spot(local)),
  };

  SelectionResult _edge(
    Offset global, {
    required bool isEnd,
    required TextGranularity granularity,
  }) {
    final local = _local(global);
    if (_text.rows == 0) return _result(local);
    if (granularity == TextGranularity.character) {
      var s = _spot(local);
      final origin = _origin, end = _end;
      // 둘레 스크롤은 굴린 뒤 시작 끝을 길게 누른 낱말의 앞 자리로 다시 보낸다. 앞으로(위로) 넓힌
      // 고름의 시작 끝은 그 낱말의 뒤라 그대로 두면 처음 잡은 낱말이 빠진다.
      if (!isEnd &&
          origin != null &&
          s == origin.$1 &&
          end != null &&
          end.compareTo(origin.$1) < 0) {
        s = origin.$2;
      }
      if (isEnd) {
        _end = s;
      } else {
        _start = s;
      }
      return _result(local);
    }
    // 낱말·줄 단위로 넓힐 때는 처음 잡은 범위를 붙든 채, 손가락이 그 앞이면 뒤로 넓힌다.
    final a = _start, b = _end;
    final origin =
        _origin ??
        (a != null && b != null
            ? (a.compareTo(b) <= 0 ? (a, b) : (b, a))
            : _rangeAt(local, granularity));
    final (lo, hi) = _rangeAt(local, granularity);
    final GridSpot fixed, moving;
    if (lo.compareTo(origin.$1) < 0) {
      fixed = origin.$2;
      moving = lo;
    } else {
      fixed = origin.$1;
      moving = hi.compareTo(origin.$2) > 0 ? hi : origin.$2;
    }
    if (isEnd) {
      _start = fixed;
      _end = moving;
    } else {
      _start = moving;
      _end = fixed;
    }
    return _result(local);
  }

  SelectionResult _select(Offset global, TextGranularity g) {
    final local = _local(global);
    final result = _result(local);
    if (result != SelectionResult.end || _text.rows == 0) return result;
    final range = _rangeAt(local, g);
    _origin = range;
    _start = range.$1;
    _end = range.$2;
    return result;
  }

  @override
  SelectionResult dispatchSelectionEvent(SelectionEvent event) {
    final before = (_start, _end);
    final SelectionResult result;
    switch (event.type) {
      case SelectionEventType.startEdgeUpdate:
      case SelectionEventType.endEdgeUpdate:
        final e = event as SelectionEdgeUpdateEvent;
        result = _edge(
          e.globalPosition,
          isEnd: e.type == SelectionEventType.endEdgeUpdate,
          granularity: e.granularity,
        );
      case SelectionEventType.clear:
        _start = _end = _origin = null;
        result = SelectionResult.none;
      case SelectionEventType.selectAll:
        if (_text.rows > 0) {
          _origin = null;
          _start = const GridSpot(0, 0);
          _end = GridSpot(_text.rows - 1, _text.width(_text.rows - 1));
        }
        result = SelectionResult.none;
      case SelectionEventType.selectWord:
        result = _select(
          (event as SelectWordSelectionEvent).globalPosition,
          TextGranularity.word,
        );
      case SelectionEventType.selectParagraph:
        final e = event as SelectParagraphSelectionEvent;
        result = _select(e.globalPosition, TextGranularity.paragraph);
      // 자판으로 넓히기는 없다 — 격자 선택은 초점을 잡지 않아 그 키가 오지 않는다.
      case SelectionEventType.granularlyExtendSelection:
      case SelectionEventType.directionallyExtendSelection:
        result = SelectionResult.end;
    }
    if (before != (_start, _end)) _changed();
    return result;
  }

  @override
  SelectedContent? getSelectedContent() {
    final a = _start, b = _end;
    if (a == null || b == null) return null;
    return SelectedContent(plainText: _text.text(a, b));
  }

  /// 칸 자리를 한 줄로 편 번호 — 줄마다 넉넉한 폭을 잡아 두 자리의 앞뒤만 맞으면 된다.
  static const _stride = 1 << 12;

  @override
  SelectedContentRange? getSelection() {
    final a = _start, b = _end;
    if (a == null || b == null) return null;
    return SelectedContentRange(
      startOffset: a.row * _stride + a.col,
      endOffset: b.row * _stride + b.col,
    );
  }

  @override
  int get contentLength => _text.rows * _stride;

  @override
  List<Rect> get boundingBoxes => [Offset.zero & size];

  @override
  void pushHandleLayers(LayerLink? startHandle, LayerLink? endHandle) {
    if (startHandle == _startHandle && endHandle == _endHandle) return;
    _startHandle = startHandle;
    _endHandle = endHandle;
    if (!_disposed) markNeedsPaint();
  }

  @override
  void paint(PaintingContext context, Offset offset) {
    super.paint(context, offset);
    final a = _start, b = _end;
    if (a == null || b == null) return;
    if (a != b) {
      final (lo, hi) = a.compareTo(b) <= 0 ? (a, b) : (b, a);
      // 보이는 줄만 칠한다 — 전체 선택이면 수천 줄이다.
      final canvas = context.canvas;
      final clip = canvas.getLocalClipBounds().shift(-offset);
      final first = clip.top.isFinite
          ? math.max(lo.row, (clip.top / _cell.height).floor())
          : lo.row;
      final last = clip.bottom.isFinite
          ? math.min(hi.row, (clip.bottom / _cell.height).ceil())
          : hi.row;
      final paint = Paint()..color = _color;
      for (var r = first; r <= last; r++) {
        canvas.drawRect(_rowRect(r, lo, hi).shift(offset), paint);
      }
    }
    void leader(LayerLink? link, GridSpot s) {
      if (link == null) return;
      context.pushLayer(
        LeaderLayer(link: link, offset: offset + _point(s)),
        (_, _) {},
        Offset.zero,
      );
    }

    leader(_startHandle, a);
    leader(_endHandle, b);
  }

  @override
  void dispose() {
    _disposed = true;
    super.dispose();
    _geometry.dispose();
  }
}

/// 격자 선택의 메뉴 — 복사·전체 선택 둘만, 우리말로. 복사하면 대화 보기처럼 알린다.
Widget gridSelectionMenu(BuildContext context, SelectableRegionState region) {
  final items = <ContextMenuButtonItem>[
    for (final item in region.contextMenuButtonItems)
      if (item.type == ContextMenuButtonType.copy)
        item.copyWith(
          label: '복사',
          onPressed: () {
            item.onPressed?.call();
            HapticFeedback.lightImpact();
            ScaffoldMessenger.maybeOf(context)
              ?..hideCurrentSnackBar()
              ..showSnackBar(const SnackBar(content: Text('복사했어요')));
          },
        )
      else if (item.type == ContextMenuButtonType.selectAll)
        item.copyWith(label: '전체 선택'),
  ];
  return AdaptiveTextSelectionToolbar.buttonItems(
    anchors: region.contextMenuAnchors,
    buttonItems: items,
  );
}
