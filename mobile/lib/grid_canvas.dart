import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import 'fill_viewer.dart';
import 'grid.dart';
import 'reflow.dart';
import 'server.dart';

/// 테마의 기본 fg/bg 와 256 팔레트 판을 한데 묶는다.
class TerminalPalette {
  const TerminalPalette({
    required this.dark,
    required this.fg,
    required this.bg,
    required this.cursor,
    required this.ansi,
  });

  /// 데스크톱이 지금 쓰는 색 그대로 — 같은 학생 화면이 폰에서도 같은 얼굴로 보인다.
  TerminalPalette.fromTokens(DesignTokens t)
    : dark = t.dark,
      fg = Color(t.fg),
      bg = Color(t.bg),
      cursor = Color(t.accent),
      ansi = t.ansi;

  final bool dark;
  final Color fg;
  final Color bg;
  final Color cursor;
  final List<int> ansi;

  /// 서버 색을 아직 못 받았을 때의 앱 기본색.
  static TerminalPalette of(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final dark = scheme.brightness == Brightness.dark;
    return TerminalPalette(
      dark: dark,
      fg: dark ? const Color(0xffc0caf5) : const Color(0xff15294a),
      bg: dark ? const Color(0xff12161c) : Colors.white,
      cursor: scheme.primary,
      ansi: dark ? base16Dark : base16Light,
    );
  }

  Color resolve(CellColor c, {required bool foreground}) => switch (c) {
    DefaultColor() => foreground ? fg : bg,
    IndexColor(:final index) => Color(palette256(index, base16: ansi)),
    RgbColor(:final r, :final g, :final b) => Color.fromARGB(255, r, g, b),
  };

  @override
  bool operator ==(Object other) =>
      other is TerminalPalette &&
      other.dark == dark &&
      other.fg == fg &&
      other.bg == bg &&
      other.cursor == cursor &&
      listEquals(other.ansi, ansi);

  @override
  int get hashCode => Object.hash(dark, fg, bg, cursor, Object.hashAll(ansi));
}

const _fontFamily = 'TermMono';
const _lineHeight = 1.2;

class _CellMetrics {
  _CellMetrics(this.fontSize)
    : width = _measure(fontSize).width,
      height = _measure(fontSize).height;

  final double fontSize;
  final double width;
  final double height;

  static TextPainter _measure(double fontSize) => TextPainter(
    text: TextSpan(
      text: 'M',
      style: TextStyle(
        fontFamily: _fontFamily,
        fontSize: fontSize,
        height: _lineHeight,
      ),
    ),
    textDirection: TextDirection.ltr,
  )..layout();
}

class _Piece {
  _Piece({
    required this.x,
    required this.width,
    required this.painter,
    required this.underline,
    required this.color,
    this.bg,
  });

  final double x;
  final double width;
  final TextPainter? painter;
  final bool underline;
  final Color color;
  final Color? bg;
}

class _RowLayout {
  _RowLayout(this.runs, this.pieces);
  final List<Run> runs;
  final List<_Piece> pieces;

  void paint(Canvas canvas, double y, _CellMetrics m) {
    for (final p in pieces) {
      final bg = p.bg;
      if (bg != null) {
        canvas.drawRect(
          Rect.fromLTWH(p.x, y, p.width, m.height),
          Paint()..color = bg,
        );
      }
    }
    for (final p in pieces) {
      final tp = p.painter;
      if (tp == null) continue;
      // wide 글자는 두 칸 상자 가운데에 — 폴백 글꼴은 정확히 두 배 폭이 아니다.
      final dx = tp.width < p.width ? (p.width - tp.width) / 2 : 0.0;
      tp.paint(canvas, Offset(p.x + dx, y));
      if (p.underline) {
        final uy = y + m.height - 1.5;
        canvas.drawRect(
          Rect.fromLTWH(p.x, uy, p.width, 1),
          Paint()..color = p.color,
        );
      }
    }
  }
}

/// 행마다 레이아웃을 들고 있다가 그 행의 런 목록 객체가 바뀐 때만 다시 만든다 —
/// 서버가 바뀐 행만 새 List 로 보내므로 `identical` 이 곧 「바뀌었다」다.
class _RowCache {
  final List<_RowLayout?> _rows = [];
  TerminalPalette? _palette;
  _CellMetrics? _metrics;

  _RowLayout layout(
    int row,
    List<Run> runs,
    TerminalPalette palette,
    _CellMetrics m,
  ) {
    if (_palette != palette || _metrics != m) {
      _rows.clear();
      _palette = palette;
      _metrics = m;
    }
    while (_rows.length <= row) {
      _rows.add(null);
    }
    final cached = _rows[row];
    if (cached != null && identical(cached.runs, runs)) return cached;
    final built = _build(runs, palette, m);
    _rows[row] = built;
    return built;
  }

  static _RowLayout _build(
    List<Run> runs,
    TerminalPalette palette,
    _CellMetrics m,
  ) {
    final pieces = <_Piece>[];
    var col = 0;
    for (final run in runs) {
      final inverse = run.flags & flagInverse != 0;
      var fg = palette.resolve(inverse ? run.bg : run.fg, foreground: !inverse);
      final bgColor = palette.resolve(
        inverse ? run.fg : run.bg,
        foreground: inverse,
      );
      final bgIsDefault = !inverse && run.bg is DefaultColor;
      if (run.flags & flagDim != 0) fg = fg.withValues(alpha: 0.6);
      final style = TextStyle(
        fontFamily: _fontFamily,
        fontSize: m.fontSize,
        height: _lineHeight,
        color: fg,
        fontWeight: run.flags & flagBold != 0
            ? FontWeight.w700
            : FontWeight.w400,
        fontStyle: run.flags & flagItalic != 0
            ? FontStyle.italic
            : FontStyle.normal,
      );
      final underline = run.flags & flagUnderline != 0;
      final startCol = col;
      final buffer = StringBuffer();
      var bufferCol = col;
      void flush() {
        if (buffer.isEmpty) return;
        pieces.add(
          _Piece(
            x: bufferCol * m.width,
            width: (col - bufferCol) * m.width,
            painter: _painter(buffer.toString(), style),
            underline: underline,
            color: fg,
          ),
        );
        buffer.clear();
      }

      for (final rune in run.text.runes) {
        final w = cellWidth(rune);
        if (w == 2) {
          flush();
          pieces.add(
            _Piece(
              x: col * m.width,
              width: 2 * m.width,
              painter: _painter(String.fromCharCode(rune), style),
              underline: underline,
              color: fg,
            ),
          );
          col += 2;
          bufferCol = col;
          continue;
        }
        if (buffer.isEmpty) bufferCol = col;
        buffer.writeCharCode(rune);
        col += w;
      }
      flush();
      if (!bgIsDefault && col > startCol) {
        pieces.insert(
          0,
          _Piece(
            x: startCol * m.width,
            width: (col - startCol) * m.width,
            painter: null,
            underline: false,
            color: fg,
            bg: bgColor,
          ),
        );
      }
    }
    return _RowLayout(runs, pieces);
  }

  static TextPainter _painter(String text, TextStyle style) => TextPainter(
    text: TextSpan(text: text, style: style),
    textDirection: TextDirection.ltr,
  )..layout();
}

class _GridPainter extends CustomPainter {
  _GridPainter({
    required this.grid,
    required this.version,
    required this.palette,
    required this.metrics,
    required this.cache,
  });

  final GridLines grid;
  final int version;
  final TerminalPalette palette;
  final _CellMetrics metrics;
  final _RowCache cache;

  @override
  void paint(Canvas canvas, Size size) {
    canvas.drawRect(Offset.zero & size, Paint()..color = palette.bg);
    for (var row = 0; row < grid.lines.length; row++) {
      final runs = grid.lines[row];
      if (runs.isEmpty) continue;
      cache
          .layout(row, runs, palette, metrics)
          .paint(canvas, row * metrics.height, metrics);
    }
    if (grid.cursorVisible && grid.rows > 0) {
      canvas.drawRect(
        Rect.fromLTWH(
          grid.cursorCol * metrics.width,
          grid.cursorRow * metrics.height,
          metrics.width,
          metrics.height,
        ),
        Paint()..color = palette.cursor.withValues(alpha: 0.55),
      );
    }
  }

  @override
  bool shouldRepaint(_GridPainter old) =>
      old.version != version ||
      old.palette != palette ||
      old.metrics.fontSize != metrics.fontSize ||
      old.grid != grid;
}

/// 격자를 그린다. 채우기·핀치는 FillViewer 가 맡는다(그림 모드와 같은 규칙).
class GridCanvas extends StatefulWidget {
  const GridCanvas({
    super.key,
    required this.grid,
    required this.version,
    required this.palette,
    this.fontSize = 13,
  });

  final Grid grid;
  final int version;
  final TerminalPalette palette;
  final double fontSize;

  @override
  State<GridCanvas> createState() => _GridCanvasState();
}

class _GridCanvasState extends State<GridCanvas> {
  final _cache = _RowCache();
  late _CellMetrics _metrics = _CellMetrics(widget.fontSize);

  @override
  void didUpdateWidget(GridCanvas old) {
    super.didUpdateWidget(old);
    if (old.fontSize != widget.fontSize) {
      _metrics = _CellMetrics(widget.fontSize);
    }
  }

  @override
  Widget build(BuildContext context) {
    final grid = widget.grid;
    final cols = math.max(grid.cols, 1);
    final rows = math.max(grid.rows, 1);
    return FillViewer(
      content: Size(cols * _metrics.width, rows * _metrics.height),
      background: widget.palette.bg,
      child: CustomPaint(
        painter: _GridPainter(
          grid: grid,
          version: widget.version,
          palette: widget.palette,
          metrics: _metrics,
          cache: _cache,
        ),
      ),
    );
  }
}

/// 폰 폭으로 접은 화면. 데스크톱 pane 은 제 크기 그대로 두고(크기 신호를 안 보낸다)
/// 받은 행을 이 폭의 열 수로 접는다. 세로로만 넘기고, 바닥(입력창)에 붙어 새 줄을 따라간다.
class WrappedCanvas extends StatefulWidget {
  const WrappedCanvas({
    super.key,
    required this.grid,
    required this.version,
    required this.palette,
    this.fontSize = 13,
  });

  final Grid grid;
  final int version;
  final TerminalPalette palette;
  final double fontSize;

  @override
  State<WrappedCanvas> createState() => _WrappedCanvasState();
}

class _WrappedCanvasState extends State<WrappedCanvas> {
  final _cache = _RowCache();
  final _reflow = Reflow();
  late _CellMetrics _metrics = _CellMetrics(widget.fontSize);

  @override
  void didUpdateWidget(WrappedCanvas old) {
    super.didUpdateWidget(old);
    if (old.fontSize != widget.fontSize) {
      _metrics = _CellMetrics(widget.fontSize);
    }
  }

  @override
  Widget build(BuildContext context) => LayoutBuilder(
    builder: (context, constraints) {
      final cols = math.max(
        20,
        (constraints.maxWidth / _metrics.width).floor(),
      );
      final view = _reflow.apply(widget.grid, cols);
      return ColoredBox(
        color: widget.palette.bg,
        // reverse 라 짧은 내용은 바닥에 앉고, 길면 스크롤 0 이 곧 맨 아래다 — 새 줄이
        // 와도 보던 바닥이 그대로 바닥이다.
        child: SingleChildScrollView(
          reverse: true,
          child: SizedBox(
            width: constraints.maxWidth,
            height: math.max(view.rows, 1) * _metrics.height,
            child: CustomPaint(
              painter: _GridPainter(
                grid: view,
                version: widget.version,
                palette: widget.palette,
                metrics: _metrics,
                cache: _cache,
              ),
            ),
          ),
        ),
      );
    },
  );
}
