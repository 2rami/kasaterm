import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import 'claude_style.dart';
import 'contrast.dart';
import 'fill_viewer.dart';
import 'grid.dart';
import 'reflow.dart';
import 'server.dart';
import 'sprite_cache.dart';

/// 테마의 기본 fg/bg 와 256 팔레트 판을 한데 묶는다.
class TerminalPalette {
  const TerminalPalette({
    required this.dark,
    required this.fg,
    required this.bg,
    required this.cursor,
    required this.ansi,
    this.minContrast = DesignTokens.defaultMinContrast,
  });

  /// 데스크톱이 지금 쓰는 색 그대로 — 같은 학생 화면이 폰에서도 같은 얼굴로 보인다.
  TerminalPalette.fromTokens(DesignTokens t)
    : dark = t.dark,
      fg = Color(t.fg),
      bg = Color(t.bg),
      cursor = Color(t.accent),
      ansi = t.ansi,
      minContrast = t.minContrast;

  final bool dark;
  final Color fg;
  final Color bg;
  final Color cursor;
  final List<int> ansi;
  final double minContrast;

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
      other.minContrast == minContrast &&
      listEquals(other.ansi, ansi);

  @override
  int get hashCode =>
      Object.hash(dark, fg, bg, cursor, minContrast, Object.hashAll(ansi));
}

const _fontFamily = 'TermMono';
// 한글은 JetBrains 에 없어 D2Coding 으로 — 데스크톱 렌더러의 폴백 순서와 같다.
// ⏺·⎿ 는 STIX Two Math(데스크톱과 같은 출처), 그래도 없는 점자·기호는 iOS 의 Menlo·
// Apple Symbols 로 — 이모지 글꼴보다 먼저 잡혀야 동그라미가 이모지풍으로 안 커진다.
const _fontFallback = ['TermHangul', 'TermSymbol', 'Menlo', 'Apple Symbols'];
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
        fontFamilyFallback: _fontFallback,
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
  _RowLayout(this.pieces, this.generation);
  final List<_Piece> pieces;
  final int generation;

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

/// 행 레이아웃을 **행 객체에** 매달아 둔다(Expando) — 서버가 바뀐 행만 새 List 로
/// 보내므로 객체가 같으면 내용도 같다. 행 번호로 들면 지난 줄이 한 줄 늘 때마다 전부
/// 밀려 수천 줄을 다시 잰다. 팔레트·글꼴이 바뀌면 세대를 올려 통째로 버린다.
class _RowCache {
  final _rows = Expando<_RowLayout>();
  TerminalPalette? _palette;
  _CellMetrics? _metrics;
  int _generation = 0;

  _RowLayout layout(List<Run> runs, TerminalPalette palette, _CellMetrics m) {
    if (_palette != palette || _metrics != m) {
      _generation++;
      _palette = palette;
      _metrics = m;
    }
    final cached = _rows[runs];
    if (cached != null && cached.generation == _generation) return cached;
    final built = _build(runs, palette, m, _generation);
    _rows[runs] = built;
    return built;
  }

  static _RowLayout _build(
    List<Run> runs,
    TerminalPalette palette,
    _CellMetrics m,
    int generation,
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
      // 데스크톱(cells.rs)과 같은 순서 — 흐림(SGR 2)은 바탕 쪽으로 55% 섞고 끝(물러나라는
      // 뜻이라 대비 바닥으로 되살리지 않는다). 그 밖에 셀이 스스로 고른 색(256색·트루컬러)만
      // 바탕과의 대비를 바닥까지 끌어올린다 — 기본색·ANSI 16색은 테마가 이미 읽히게 골랐다.
      if (run.flags & flagDim != 0) {
        fg = mixToward(fg, bgColor, 0.55);
      } else if (namesOwnColor(inverse ? run.bg : run.fg)) {
        fg = enforceContrast(fg, bgColor, palette.minContrast);
      }
      final style = TextStyle(
        fontFamily: _fontFamily,
        fontFamilyFallback: _fontFallback,
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
    return _RowLayout(pieces, generation);
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
    this.slug,
    this.sprites,
    this.walkFrame = 0,
    this.idleFrame = 0,
  });

  final GridLines grid;
  final int version;
  final TerminalPalette palette;
  final _CellMetrics metrics;
  final _RowCache cache;
  final String? slug;
  final SpriteCache? sprites;
  final int walkFrame;
  final int idleFrame;

  @override
  void paint(Canvas canvas, Size size) {
    canvas.drawRect(Offset.zero & size, Paint()..color = palette.bg);
    // 스크롤 안에서는 보이는 줄만 — 지난 줄 수천 개를 프레임마다 다 그리면 폰이 버벅인다.
    final clip = canvas.getLocalClipBounds();
    final first = clip.top.isFinite
        ? math.max(0, (clip.top / metrics.height).floor())
        : 0;
    final last = clip.bottom.isFinite
        ? math.min(grid.lines.length, (clip.bottom / metrics.height).ceil() + 1)
        : grid.lines.length;
    for (var row = first; row < last; row++) {
      final runs = grid.lines[row];
      if (runs.isEmpty) continue;
      cache
          .layout(runs, palette, metrics)
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
    _paintSprites(canvas);
  }

  /// 데스크톱 `paint_student_overlays` 와 같이 셀 **위**에 얹는다 — 픽셀 도트라 보간 없이.
  void _paintSprites(Canvas canvas) {
    final slug = this.slug;
    final sprites = this.sprites;
    if (slug == null || sprites == null || grid.slots.isEmpty) return;
    final paint = Paint()..filterQuality = FilterQuality.none;
    for (final s in grid.slots) {
      final i = s.motion == 'walk' ? walkFrame : idleFrame;
      final img = sprites.frame(slug, s.motion, i);
      if (img == null) continue;
      canvas.drawImageRect(
        img,
        Rect.fromLTWH(0, 0, img.width.toDouble(), img.height.toDouble()),
        Rect.fromLTWH(
          s.col * metrics.width,
          s.row * metrics.height,
          s.cols * metrics.width,
          s.rows * metrics.height,
        ),
        paint,
      );
    }
  }

  @override
  bool shouldRepaint(_GridPainter old) =>
      old.version != version ||
      old.palette != palette ||
      old.metrics != metrics ||
      old.grid != grid ||
      old.walkFrame != walkFrame ||
      old.idleFrame != idleFrame;
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
    this.history = const [],
    this.student,
    this.fontSize = 13,
  });

  /// 살아 있는 화면. 지난 줄은 `history` 로 따로 받아 꾸밈(스피너·입력상자 판독)은
  /// 살아 있는 화면에만 건다 — 데스크톱도 화면에 보이는 격자만 판독한다.
  final GridLines grid;
  final List<List<Run>> history;
  final int version;
  final TerminalPalette palette;

  /// 학생 꾸밈(도트·학생색). 셸 화면처럼 학생이 없으면 null.
  final StudentStyle? student;
  final double fontSize;

  @override
  State<WrappedCanvas> createState() => _WrappedCanvasState();
}

class _WrappedCanvasState extends State<WrappedCanvas> {
  final _cache = _RowCache();
  final _reflow = Reflow();
  final _t0 = DateTime.now();
  Timer? _anim;

  @override
  void initState() {
    super.initState();
    spriteCache.addListener(_repaint);
  }

  @override
  void dispose() {
    _anim?.cancel();
    spriteCache.removeListener(_repaint);
    super.dispose();
  }

  void _repaint() {
    if (mounted) setState(() {});
  }

  double get _seconds => DateTime.now().difference(_t0).inMilliseconds / 1000.0;

  /// 스피너 glow 와 도트 걸음은 시간이 움직여야 보인다 — 꾸밈이 살아 있는 동안만 돈다.
  void _setAnimating(bool on) {
    if (on && _anim == null) {
      _anim = Timer.periodic(
        const Duration(milliseconds: 70),
        (_) => _repaint(),
      );
    } else if (!on && _anim != null) {
      _anim!.cancel();
      _anim = null;
    }
  }

  late _CellMetrics _base = _CellMetrics(widget.fontSize);
  _CellMetrics? _scaled;

  /// 좁은 pane(42열)은 기본 글꼴이면 폰 폭의 2/3 만 쓰고 글자도 작다 — pane 열 수가 폰
  /// 열 수보다 적으면 글꼴을 키워 그 열 수가 폭을 채우게 한다(상한 22pt). 넓은 pane 은
  /// 기본 글꼴로 접는다.
  static const _maxFont = 22.0;

  @override
  void didUpdateWidget(WrappedCanvas old) {
    super.didUpdateWidget(old);
    if (old.fontSize != widget.fontSize) {
      _base = _CellMetrics(widget.fontSize);
      _scaled = null;
    }
  }

  _CellMetrics _metricsFor(double maxWidth) {
    final paneCols = widget.grid.cols;
    final phoneCols = (maxWidth / _base.width).floor();
    if (paneCols <= 0 || paneCols >= phoneCols) return _base;
    final ratio = _base.width / _base.fontSize;
    final size = (maxWidth / (paneCols * ratio)).clamp(
      widget.fontSize,
      _maxFont,
    );
    final cached = _scaled;
    if (cached != null && (cached.fontSize - size).abs() < 0.01) return cached;
    return _scaled = _CellMetrics(size);
  }

  @override
  Widget build(BuildContext context) => LayoutBuilder(
    builder: (context, constraints) {
      final metrics = _metricsFor(constraints.maxWidth);
      final cols = math.max(20, (constraints.maxWidth / metrics.width).floor());
      final st = widget.student;
      final slug = st?.slug;
      final t = _seconds;
      final live = st == null
          ? widget.grid
          : restyleClaude(
              widget.grid,
              StudentStyle(
                slug: slug,
                accent: st.accent,
                bg: st.bg,
                hasWalk: slug != null && spriteCache.available(slug, 'walk'),
                hasIdle: slug != null && spriteCache.available(slug, 'idle'),
              ),
              t,
            );
      final animated = live is StyledGrid && live.animated;
      WidgetsBinding.instance.addPostFrameCallback(
        (_) => _setAnimating(animated),
      );
      final view = _reflow.apply(CombinedGrid(widget.history, live), cols);
      // 스크롤 상자는 내용만큼만 줄어들려 한다 — 느슨한 높이(Row 안 등)에서는 두 줄짜리
      // 상자가 세로 가운데에 떠서 바닥 정렬이 깨진다. 주어진 자리를 통째로 차지시킨다.
      return SizedBox.expand(
        child: ColoredBox(
          color: widget.palette.bg,
          // reverse 라 짧은 내용은 바닥에 앉고, 길면 스크롤 0 이 곧 맨 아래다 — 새 줄이
          // 와도 보던 바닥이 그대로 바닥이다.
          child: SingleChildScrollView(
            reverse: true,
            child: SizedBox(
              width: constraints.maxWidth,
              height: math.max(view.rows, 1) * metrics.height,
              child: CustomPaint(
                painter: _GridPainter(
                  grid: view,
                  version: widget.version,
                  palette: widget.palette,
                  metrics: metrics,
                  cache: _cache,
                  slug: slug,
                  sprites: spriteCache,
                  walkFrame: (t * 1000 ~/ spriteWalkFrameMs) % spriteWalkFrames,
                  idleFrame: (t * 1000 ~/ spriteIdleFrameMs) % spriteIdleFrames,
                ),
              ),
            ),
          ),
        ),
      );
    },
  );
}
