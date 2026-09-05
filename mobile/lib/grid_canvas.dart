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
    this.composing,
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

  /// 조합 중인 한글 — 커서 자리에 겹쳐 그린다(아직 pane 에 안 보낸 글).
  final String? composing;

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
      final ime = composing ?? '';
      var cursorCol = grid.cursorCol;
      if (ime.isNotEmpty) {
        cursorCol += _paintComposing(canvas, ime, grid.cursorRow, cursorCol);
      }
      canvas.drawRect(
        Rect.fromLTWH(
          cursorCol * metrics.width,
          grid.cursorRow * metrics.height,
          metrics.width,
          metrics.height,
        ),
        Paint()..color = palette.cursor.withValues(alpha: 0.55),
      );
    }
    _paintSprites(canvas);
  }

  /// 조합 중인 글을 커서 자리에 밑줄 친 채 얹는다 — 데스크톱의 IME 조합 표시와 같은
  /// 자리. 차지한 칸 수를 돌려준다(커서는 그 뒤로 간다).
  int _paintComposing(Canvas canvas, String text, int row, int col) {
    var cells = 0;
    for (final r in text.runes) {
      cells += cellWidth(r);
    }
    final x = col * metrics.width;
    final y = row * metrics.height;
    canvas.drawRect(
      Rect.fromLTWH(x, y, cells * metrics.width, metrics.height),
      Paint()..color = palette.fg.withValues(alpha: 0.18),
    );
    final tp = TextPainter(
      text: TextSpan(
        text: text,
        style: TextStyle(
          fontFamily: 'TermMono',
          fontFamilyFallback: _fontFallback,
          fontSize: metrics.fontSize,
          color: palette.fg,
          decoration: TextDecoration.underline,
          decorationColor: palette.fg,
        ),
      ),
      textDirection: TextDirection.ltr,
    )..layout();
    tp.paint(canvas, Offset(x, y + (metrics.height - tp.height) / 2));
    return cells;
  }

  /// 데스크톱 `paint_student_overlays` 와 같이 셀 **위**에 얹는다 — 픽셀 도트라 보간 없이.
  void _paintSprites(Canvas canvas) {
    final slug = this.slug;
    final sprites = this.sprites;
    if (sprites == null || grid.slots.isEmpty) return;
    final paint = Paint()..filterQuality = FilterQuality.none;
    for (final s in grid.slots) {
      final box = Rect.fromLTWH(
        s.col * metrics.width,
        s.row * metrics.height,
        s.cols * metrics.width,
        s.rows * metrics.height,
      );
      if (s.motion.startsWith('icon:')) {
        final img = sprites.icon(s.motion.substring(5));
        if (img == null) continue;
        // 데스크톱 paint_status_model_icons — 칸 안 가운데, 높이의 72%·폭의 78% 중 작은 쪽.
        final size = math.min(box.height * 0.72, box.width * 0.78);
        canvas.drawImageRect(
          img,
          Rect.fromLTWH(0, 0, img.width.toDouble(), img.height.toDouble()),
          Rect.fromLTWH(
            box.left + (box.width - size) / 2,
            box.top + (box.height - size) / 2,
            size,
            size,
          ),
          Paint()
            ..filterQuality = FilterQuality.medium
            ..colorFilter = const ColorFilter.mode(
              statusModelColor,
              BlendMode.srcIn,
            ),
        );
        continue;
      }
      if (slug == null) continue;
      final i = s.motion == 'walk' ? walkFrame : idleFrame;
      final img = sprites.frame(slug, s.motion, i);
      if (img == null) continue;
      // 그림 비율은 지킨다 — 자리 상자 안에 맞춰 바닥에 세우고 가로는 가운데.
      final iw = img.width.toDouble(), ih = img.height.toDouble();
      final k = math.min(box.width / iw, box.height / ih);
      final dw = iw * k, dh = ih * k;
      canvas.drawImageRect(
        img,
        Rect.fromLTWH(0, 0, iw, ih),
        Rect.fromLTWH(box.left + (box.width - dw) / 2, box.bottom - dh, dw, dh),
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
      old.idleFrame != idleFrame ||
      old.composing != composing;
}

/// 격자를 그린다. 채우기·핀치는 FillViewer 가 맡는다(그림 모드와 같은 규칙).
class GridCanvas extends StatefulWidget {
  const GridCanvas({
    super.key,
    required this.grid,
    required this.version,
    required this.palette,
    this.fontSize = 13,
    this.composing,
  });

  final Grid grid;
  final int version;
  final TerminalPalette palette;
  final double fontSize;

  /// 조합 중인 한글 — 커서 자리에 겹쳐 보인다.
  final String? composing;

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
          composing: widget.composing,
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
    this.historyVersion = 0,
    this.student,
    this.bottomTick = 0,
    this.initialScroll,
    this.fontSize = 13,
    this.composing,
  });

  /// 조합 중인 한글 — 커서 자리에 겹쳐 보인다(바로 치기).
  final String? composing;

  /// 살아 있는 화면. 지난 줄은 `history` 로 따로 받아 꾸밈(스피너·입력상자 판독)은
  /// 살아 있는 화면에만 건다 — 데스크톱도 화면에 보이는 격자만 판독한다.
  final GridLines grid;
  final List<List<Run>> history;

  /// 지난 줄이 바뀔 때마다 오른다 — 지난 줄 꾸밈은 이 값이 바뀔 때만 다시 한다(수천 줄).
  final int historyVersion;
  final int version;
  final TerminalPalette palette;

  /// 학생 꾸밈(도트·학생색). 셸 화면처럼 학생이 없으면 null.
  final StudentStyle? student;

  /// 값이 바뀌면 맨 아래로 내려간다 — 답장·키를 보낸 순간(터미널이 입력에 바닥으로
  /// 내려가는 것과 같다).
  final int bottomTick;

  /// 검증용 — 내용이 차면 한 번 이만큼(px) 위로 넘긴 상태로 시작한다.
  final double? initialScroll;
  final double fontSize;

  @override
  State<WrappedCanvas> createState() => _WrappedCanvasState();
}

class _WrappedCanvasState extends State<WrappedCanvas> {
  final _cache = _RowCache();
  final _reflow = Reflow();
  final _t0 = DateTime.now();
  Timer? _anim;
  final _scroll = ScrollController();

  /// 바닥에서 떠 있는가 — 떠 있으면 입력상자를 바닥에 붙잡아 둔다.
  bool _away = false;

  /// 아직 안 쓴 초기 넘김 — 넘길 만큼 내용이 차는 첫 프레임에 한 번 쓴다.
  late double? _pendingScroll = widget.initialScroll;

  /// 꾸민 지난 줄 — (historyVersion, 학생) 이 같으면 재사용.
  List<List<Run>>? _histLines;
  List<SpriteSlot> _histSlots = const [];
  int _histVersion = -1;
  String? _histKey;

  @override
  void initState() {
    super.initState();
    spriteCache.addListener(_repaint);
    _scroll.addListener(_onScroll);
  }

  @override
  void dispose() {
    _anim?.cancel();
    spriteCache.removeListener(_repaint);
    _scroll.dispose();
    super.dispose();
  }

  void _onScroll() {
    final away = _scroll.hasClients && _scroll.offset > 4;
    if (away != _away) setState(() => _away = away);
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
    if (old.bottomTick != widget.bottomTick && _scroll.hasClients) {
      _scroll.jumpTo(0);
    }
  }

  /// 좁은 pane 에 맞춰 키운 글꼴인가 — 그때는 열 수를 pane 열 수와 **정확히** 맞춘다.
  bool _fitsPane(double maxWidth) {
    final paneCols = widget.grid.cols;
    return paneCols > 0 && paneCols < (maxWidth / _base.width).floor();
  }

  _CellMetrics _metricsFor(double maxWidth) {
    final paneCols = widget.grid.cols;
    if (!_fitsPane(maxWidth)) return _base;
    final ratio = _base.width / _base.fontSize;
    var size = (maxWidth / (paneCols * ratio)).clamp(widget.fontSize, _maxFont);
    final cached = _scaled;
    if (cached != null && (cached.fontSize - size).abs() < 0.01) return cached;
    var m = _CellMetrics(size);
    // 잰 칸 폭은 계산값보다 살짝 넓을 수 있다 — 그러면 pane 열 수가 폭을 넘쳐 한 열이
    // 다음 줄로 밀린다(상태줄이 두 줄, 테두리 밑에 「─」 하나). 넘치지 않을 때까지 줄인다.
    for (
      var i = 0;
      i < 6 && paneCols * m.width > maxWidth && size > widget.fontSize;
      i++
    ) {
      size = math.max(widget.fontSize, size - 0.15);
      m = _CellMetrics(size);
    }
    return _scaled = m;
  }

  @override
  Widget build(BuildContext context) => LayoutBuilder(
    builder: (context, constraints) {
      final metrics = _metricsFor(constraints.maxWidth);
      // pane 에 맞춘 글꼴이면 열 수도 pane 그대로 — 44칸 줄이 43열에 접히지 않는다.
      final cols = _fitsPane(constraints.maxWidth)
          ? widget.grid.cols
          : math.max(20, (constraints.maxWidth / metrics.width).floor());
      final st = widget.student;
      final slug = st?.slug;
      final t = _seconds;
      final live = st == null
          ? widget.grid
          : restyleClaude(
              widget.grid,
              StudentStyle(
                slug: slug,
                name: st.name,
                accent: st.accent,
                bg: st.bg,
                hasWalk: slug != null && spriteCache.available(slug, 'walk'),
                hasIdle: slug != null && spriteCache.available(slug, 'idle'),
              ),
              t,
            );
      final animated = live is StyledGrid && live.animated;
      WidgetsBinding.instance.addPostFrameCallback((_) {
        _setAnimating(animated);
        final want = _pendingScroll;
        if (want != null && _scroll.hasClients) {
          final max = _scroll.position.maxScrollExtent;
          if (max >= want) {
            _pendingScroll = null;
            _scroll.jumpTo(want);
          }
        }
      });
      var history = widget.history;
      var historySlots = const <SpriteSlot>[];
      if (st != null && history.isNotEmpty) {
        final key = '${st.slug}/${st.name}/${st.accent.toARGB32()}';
        if (_histLines == null ||
            _histVersion != widget.historyVersion ||
            _histKey != key) {
          final (lines, slots) = restyleHistory(history, st);
          _histLines = lines;
          _histSlots = slots;
          _histVersion = widget.historyVersion;
          _histKey = key;
        }
        history = _histLines!;
        historySlots = _histSlots;
      }
      final view = _reflow.apply(
        CombinedGrid(history, live, historySlots: historySlots),
        cols,
      );
      final walkFrame = (t * 1000 ~/ spriteWalkFrameMs) % spriteWalkFrames;
      final idleFrame = (t * 1000 ~/ spriteIdleFrameMs) % spriteIdleFrames;
      _GridPainter painter(GridLines g) => _GridPainter(
        grid: g,
        version: widget.version,
        palette: widget.palette,
        metrics: metrics,
        cache: _cache,
        slug: slug,
        sprites: spriteCache,
        walkFrame: walkFrame,
        idleFrame: idleFrame,
        composing: widget.composing,
      );
      // 스크롤을 올린 동안은 입력상자부터 화면 끝까지를 바닥에 붙잡아 둔다 — 데스크톱과
      // 같이 지나간 대화를 읽는 중에도 타이핑하는 자리가 제자리에 있다.
      ReflowedGrid? tail;
      if (_away) {
        final top = pinnedInputTop(live.lines);
        if (top != null) {
          tail = _reflow.apply(_TailGrid(live, top), cols);
        }
      }
      // 스크롤 상자는 내용만큼만 줄어들려 한다 — 느슨한 높이(Row 안 등)에서는 두 줄짜리
      // 상자가 세로 가운데에 떠서 바닥 정렬이 깨진다. 주어진 자리를 통째로 차지시킨다.
      return SizedBox.expand(
        child: ColoredBox(
          color: widget.palette.bg,
          child: Stack(
            children: [
              // reverse 라 짧은 내용은 바닥에 앉고, 길면 스크롤 0 이 곧 맨 아래다 — 새 줄이
              // 와도 보던 바닥이 그대로 바닥이다.
              Positioned.fill(
                child: SingleChildScrollView(
                  controller: _scroll,
                  reverse: true,
                  child: SizedBox(
                    width: constraints.maxWidth,
                    height: math.max(view.rows, 1) * metrics.height,
                    child: CustomPaint(painter: painter(view)),
                  ),
                ),
              ),
              if (tail != null)
                Positioned(
                  left: 0,
                  right: 0,
                  bottom: 0,
                  height: math.max(tail.rows, 1) * metrics.height,
                  child: CustomPaint(painter: painter(tail)),
                ),
            ],
          ),
        ),
      );
    },
  );
}

/// 살아 있는 화면의 꼬리(입력상자 위 테두리부터 끝까지) — 붙잡아 둘 부분만 든 격자.
class _TailGrid implements GridLines {
  _TailGrid(this.live, this.top) : lines = live.lines.sublist(top);

  final GridLines live;
  final int top;
  @override
  final List<List<Run>> lines;
  @override
  List<SpriteSlot> get slots => const [];
  @override
  int get cols => live.cols;
  @override
  int get rows => lines.length;
  @override
  int get cursorRow => live.cursorRow - top;
  @override
  int get cursorCol => live.cursorCol;
  @override
  bool get cursorVisible => live.cursorVisible && live.cursorRow >= top;
}
