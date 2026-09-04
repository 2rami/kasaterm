import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import 'grid.dart';
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

  final Grid grid;
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

/// 격자를 그리고, 폭이 넘치면 줄여 담고, 핀치로 키운다. 미러 pane 은 크기를 못
/// 바꾸므로(데스크톱이 같이 좁아진다) 글꼴을 줄이는 대신 변환으로 맞춘다.
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
  final _controller = TransformationController();
  final _cache = _RowCache();
  late _CellMetrics _metrics = _CellMetrics(widget.fontSize);
  double _fit = 1;

  @override
  void didUpdateWidget(GridCanvas old) {
    super.didUpdateWidget(old);
    if (old.fontSize != widget.fontSize) {
      _metrics = _CellMetrics(widget.fontSize);
    }
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  /// 사용자가 손대지 않은 배율(= 직전 fit)일 때만 새 fit 을 적용한다 — 키운
  /// 상태를 프레임마다 되돌리면 핀치가 무의미해진다.
  void _applyFit(double fit) {
    final current = _controller.value.getMaxScaleOnAxis();
    final untouched = (current - _fit).abs() < 1e-3;
    _fit = fit;
    if (!untouched) return;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      _controller.value = Matrix4.diagonal3Values(fit, fit, 1);
    });
  }

  @override
  Widget build(BuildContext context) {
    final grid = widget.grid;
    return LayoutBuilder(
      builder: (context, constraints) {
        final cols = math.max(grid.cols, 1);
        final rows = math.max(grid.rows, 1);
        final contentW = cols * _metrics.width;
        final contentH = rows * _metrics.height;
        final fitW = constraints.maxWidth / contentW;
        final fitH = constraints.maxHeight / contentH;
        // 화면을 채운다: 폭에만 맞추면 넓은 pane(196열)이 위쪽에 손톱만 하게 붙고 아래가
        // 빈다. 높이를 채우고 옆으로 밀어 읽게 하되, 작은 pane 이 거대해지지 않게
        // 1.3배에서 멈춘다. 핀치로는 전체가 한눈에 들어오는 배율까지 줄일 수 있다.
        final fit = math.max(fitW, math.min(fitH, 1.3));
        if ((fit - _fit).abs() > 1e-6) _applyFit(fit);
        return ColoredBox(
          color: widget.palette.bg,
          child: ClipRect(
            child: InteractiveViewer(
              transformationController: _controller,
              constrained: false,
              minScale: math.min(math.min(fitW, fitH), fit),
              maxScale: 6,
              boundaryMargin: EdgeInsets.symmetric(
                horizontal: constraints.maxWidth,
                vertical: constraints.maxHeight,
              ),
              child: SizedBox(
                width: contentW,
                height: contentH,
                child: CustomPaint(
                  painter: _GridPainter(
                    grid: grid,
                    version: widget.version,
                    palette: widget.palette,
                    metrics: _metrics,
                    cache: _cache,
                  ),
                ),
              ),
            ),
          ),
        );
      },
    );
  }
}
