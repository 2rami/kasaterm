import 'dart:math' as math;
import 'dart:ui';

import 'grid.dart';

/// 데스크톱(`screenread.rs`·`render.rs`)이 claude 화면에 입히는 학생 꾸밈을 폰에서도
/// 같은 규칙으로 — 작업 중인 스피너 자리에 걷는 도트, 스피너 문구는 학생색으로 빛나고,
/// 쉴 땐 입력상자 위에 서 있고, 입력상자 테두리·❯·사용자 프롬프트 띠는 학생색이다.
/// 판독 규칙의 숫자(col<8, 30행, 대시 10개…)는 그쪽 실측을 그대로 옮긴 것이라 바꾸면
/// 두 화면이 갈린다.

/// 화면 위에 얹을 스프라이트 자리 — 원본 격자 셀 단위. 행·열이 실수인 것은 서 있는
/// 도트가 반 칸 단위로 놓이기 때문이다.
class SpriteSlot {
  const SpriteSlot(this.motion, this.row, this.col, this.rows, this.cols);

  /// `walk`(작업 중) 또는 `idle`(대기).
  final String motion;
  final double row;
  final double col;
  final double rows;
  final double cols;

  SpriteSlot shifted(double dRow, double dCol) =>
      SpriteSlot(motion, row + dRow, col + dCol, rows, cols);
}

const spriteWalkFrames = 6;
const spriteIdleFrames = 4;
const spriteWalkFrameMs = 140;
const spriteIdleFrameMs = 200;
const _inputStandingRows = 3;
const _standCells = 4.0;
const _promptTint = 0.22;

/// 학생 꾸밈이 입혀진 화면. 바뀐 행만 새 List 라 접기·레이아웃 캐시가 나머지를 그대로 쓴다.
class StyledGrid implements GridLines {
  StyledGrid(this.live, this.lines, this.slots, {required this.animated});

  final GridLines live;
  @override
  final List<List<Run>> lines;
  @override
  final List<SpriteSlot> slots;

  /// 스피너 glow·도트 걸음처럼 시간이 흐르면 다시 그려야 하는가.
  final bool animated;

  @override
  int get cols => live.cols;
  @override
  int get rows => lines.length;
  @override
  int get cursorRow => live.cursorRow;
  @override
  int get cursorCol => live.cursorCol;
  @override
  bool get cursorVisible => live.cursorVisible;
}

class StudentStyle {
  const StudentStyle({
    required this.slug,
    required this.accent,
    required this.bg,
    this.hasWalk = true,
    this.hasIdle = true,
  });

  /// 도트 파일명의 학생 슬러그 — 없으면 색만 입힌다.
  final String? slug;
  final Color accent;
  final Color bg;
  final bool hasWalk;
  final bool hasIdle;
}

class _Cell {
  _Cell(this.rune, this.fg, this.bg, this.flags);
  int rune;
  CellColor fg;
  CellColor bg;
  int flags;

  bool get blank => rune == 0x20 || rune == 0;
}

List<_Cell> _cells(List<Run> runs) => [
  for (final r in runs)
    for (final rune in r.text.runes) _Cell(rune, r.fg, r.bg, r.flags),
];

bool _sameColor(CellColor a, CellColor b) => switch ((a, b)) {
  (DefaultColor(), DefaultColor()) => true,
  (IndexColor(index: final x), IndexColor(index: final y)) => x == y,
  (
    RgbColor(r: final r1, g: final g1, b: final b1),
    RgbColor(r: final r2, g: final g2, b: final b2),
  ) =>
    r1 == r2 && g1 == g2 && b1 == b2,
  _ => false,
};

List<Run> _runs(List<_Cell> cells) {
  final out = <Run>[];
  _Cell? style;
  final buf = StringBuffer();
  void flush() {
    final s = style;
    if (s != null && buf.isNotEmpty) {
      out.add(Run(buf.toString(), s.fg, s.bg, s.flags));
    }
    buf.clear();
  }

  for (final c in cells) {
    final s = style;
    if (s == null ||
        !_sameColor(s.fg, c.fg) ||
        !_sameColor(s.bg, c.bg) ||
        s.flags != c.flags) {
      flush();
      style = c;
    }
    buf.writeCharCode(c.rune);
  }
  flush();
  return out;
}

/// 셀 i 의 열 — 두 칸 글자 앞이면 그만큼 밀린다(데스크톱 격자는 열 단위다).
int _colOf(List<_Cell> cells, int i) {
  var col = 0;
  for (var k = 0; k < i; k++) {
    col += cellWidth(cells[k].rune);
  }
  return col;
}

int? _firstGlyph(List<_Cell> cells) {
  for (var i = 0; i < cells.length; i++) {
    if (!cells[i].blank) return i;
  }
  return null;
}

int? _lastGlyph(List<_Cell> cells) {
  for (var i = cells.length - 1; i >= 0; i--) {
    if (!cells[i].blank) return i;
  }
  return null;
}

String _text(List<_Cell> cells, [int from = 0]) => String.fromCharCodes([
  for (final c in cells.skip(from)) c.rune == 0 ? 0x20 : c.rune,
]);

int? _lastNonBlankRow(List<List<_Cell>> rows) {
  for (var r = rows.length - 1; r >= 0; r--) {
    if (_firstGlyph(rows[r]) != null) return r;
  }
  return null;
}

/// claude 스피너의 앞머리 글리프 — 별(Dingbats)·점·ASCII `*`(윈도우)·`●`(reduce motion).
bool isSpinnerHead(int rune) =>
    (rune >= 0x2720 && rune <= 0x274F) ||
    rune == 0xB7 ||
    rune == 0x2A ||
    rune == 0x25CF;

bool _hasElapsed(String head) {
  final b = head.codeUnits;
  for (var i = 1; i < b.length; i++) {
    if ((b[i] == 0x73 || b[i] == 0x6d) &&
        b[i - 1] >= 0x30 &&
        b[i - 1] <= 0x39) {
      return true;
    }
  }
  return false;
}

/// 스피너 행이면 앞머리 셀 index.
int? _spinnerRowCol(List<_Cell> row) {
  final first = _firstGlyph(row);
  if (first == null || _colOf(row, first) >= 8) return null;
  final rest = _text(row, first + 1);
  if (rest.contains('esc to interrupt')) return first;
  final g = row[first].rune;
  if (g >= 0x2800 && g <= 0x28FF) return first;
  if (!isSpinnerHead(g)) return null;
  if (rest.contains('ompacting')) return first;
  final dots = rest.indexOf('…');
  if (dots < 0) return null;
  final tail = rest.substring(dots + 1);
  final paren = tail.indexOf('(');
  if (paren < 0) return null;
  final inside = tail.substring(paren + 1);
  final close = inside.indexOf(')');
  final head = close < 0 ? inside : inside.substring(0, close);
  return _hasElapsed(head) ? first : null;
}

int? _spinnerTipRescue(List<List<_Cell>> rows, int r) {
  final row = rows[r];
  final first = _firstGlyph(row);
  if (first == null || _colOf(row, first) >= 8) return null;
  if (!isSpinnerHead(row[first].rune)) return null;
  if (!_text(row, first + 1).contains('…')) return null;
  for (var k = r + 1; k < rows.length && k <= r + 2; k++) {
    final fi = _firstGlyph(rows[k]);
    if (fi == null) continue;
    final g = rows[k][fi].rune;
    if (g != 0x23BF && g != 0x2514 && g != 0x2570) return null;
    return _text(rows[k], fi + 1).contains('Tip:') ? first : null;
  }
  return null;
}

const _widgetHeads = {
  0x25FB,
  0x25FC,
  0x25A1,
  0x25A0,
  0x2610,
  0x2611,
  0x2714,
  0x2718,
  0x2716,
  0x25C9,
  0x25CB,
  0x25CF,
};

bool _spinnerIsLive(List<List<_Cell>> rows, int r) {
  for (final row in rows.skip(r + 1)) {
    final fi = _firstGlyph(row);
    if (fi == null) continue;
    final g = row[fi].rune;
    if (g == 0x23FA) return false;
    if (g == 0x23BF) {
      final text = _text(row, fi + 1).trimLeft();
      final widget = text.isNotEmpty && _widgetHeads.contains(text.runes.first);
      if (!(text.contains('Tip:') || widget)) return false;
    }
  }
  return true;
}

/// (행, 셀 index) — 화면 아래 30행 안의 살아 있는 스피너 자리.
(int, int)? _findClaudeSpinner(List<List<_Cell>> rows) {
  final last = _lastNonBlankRow(rows);
  if (last == null) return null;
  final start = math.max(0, last + 1 - 30);
  for (var r = last; r >= start; r--) {
    final c = _spinnerRowCol(rows[r]) ?? _spinnerTipRescue(rows, r);
    if (c != null && _spinnerIsLive(rows, r)) return (r, c);
  }
  return null;
}

sealed class _PromptBox {
  const _PromptBox();
  Iterable<int> get rowsIn;
}

class _Bordered extends _PromptBox {
  const _Bordered(this.top, this.bottom);
  final int top;
  final int bottom;
  @override
  Iterable<int> get rowsIn => [for (var r = top + 1; r < bottom; r++) r];
}

class _Filled extends _PromptBox {
  const _Filled(this.start, this.end);
  final int start;
  final int end;
  @override
  Iterable<int> get rowsIn => [for (var r = start; r < end; r++) r];
}

bool _isBorder(List<_Cell> r) {
  var dash = 0, glyph = 0;
  for (final c in r) {
    if (c.blank) continue;
    glyph++;
    if (c.rune == 0x2500) dash++;
  }
  return dash >= 10 && dash * 2 >= glyph;
}

bool _markerRow(List<_Cell> r) {
  final fi = _firstGlyph(r);
  return fi != null && (r[fi].rune == 0x276F || r[fi].rune == 0x203A);
}

CellColor? _uniformFill(List<_Cell> r) {
  CellColor? fill;
  var glyphs = 0;
  for (final c in r) {
    if (c.rune == 0) continue;
    if (c.bg is DefaultColor) return null;
    if (fill != null && !_sameColor(fill, c.bg)) return null;
    fill = c.bg;
    glyphs++;
  }
  return glyphs >= 8 ? fill : null;
}

_PromptBox? _promptBox(List<List<_Cell>> rows) {
  var b2 = -1;
  for (var r = rows.length - 1; r >= 0; r--) {
    if (_isBorder(rows[r])) {
      b2 = r;
      break;
    }
  }
  if (b2 > 0) {
    for (var b1 = b2 - 1; b1 >= 0; b1--) {
      if (!_isBorder(rows[b1])) continue;
      if (b1 + 1 < b2) {
        for (var r = b1 + 1; r < b2; r++) {
          if (_markerRow(rows[r])) return _Bordered(b1, b2);
        }
      }
      break;
    }
  }
  for (var f = rows.length - 1; f >= 0; f--) {
    if (!_markerRow(rows[f])) continue;
    final fill = _uniformFill(rows[f]);
    if (fill == null) continue;
    bool same(List<_Cell> r) {
      final u = _uniformFill(r);
      return u != null && _sameColor(u, fill);
    }

    var start = f;
    while (start > 0 && same(rows[start - 1])) {
      start--;
    }
    var end = f + 1;
    while (end < rows.length && same(rows[end])) {
      end++;
    }
    return _Filled(start, end);
  }
  return null;
}

RgbColor _rgb(Color c) {
  final v = c.toARGB32();
  return RgbColor((v >> 16) & 0xff, (v >> 8) & 0xff, v & 0xff);
}

/// `base` 를 `accent` 쪽으로 `amount` 만큼 — 셀 배경엔 알파가 없어 미리 섞는다.
RgbColor tintToward(Color base, Color accent, double amount) {
  final b = base.toARGB32(), a = accent.toARGB32();
  int ch(int s) {
    final x = (b >> s) & 0xff, y = (a >> s) & 0xff;
    return (x + (y - x) * amount).round().clamp(0, 255);
  }

  return RgbColor(ch(16), ch(8), ch(0));
}

/// 입력상자 — 테두리 줄은 배경을 터미널색으로 되돌리고 글리프만 학생색, ❯ 도 학생색.
/// codex 의 칠해진 입력행은 배경을 학생색 쪽으로 22% 끌어당긴다.
void _stylePromptBox(List<List<_Cell>> rows, Set<int> touched, Color accent) {
  final bx = _promptBox(rows);
  if (bx == null) return;
  final fg = _rgb(accent);
  switch (bx) {
    case _Bordered(:final top, :final bottom):
      for (final i in [top, bottom]) {
        touched.add(i);
        for (final c in rows[i]) {
          c.bg = const DefaultColor();
          if (!c.blank) c.fg = fg;
        }
      }
    case _Filled(:final start, :final end):
      for (var i = start; i < end; i++) {
        touched.add(i);
        for (final c in rows[i]) {
          if (c.bg case RgbColor(:final r, :final g, :final b)) {
            c.bg = tintToward(
              Color.fromARGB(255, r, g, b),
              accent,
              _promptTint,
            );
          }
        }
      }
  }
  for (final r in bx.rowsIn) {
    final fi = _firstGlyph(rows[r]);
    if (fi == null) continue;
    final g = rows[r][fi].rune;
    if (g == 0x276F || g == 0x203A || g == 0x3E) {
      rows[r][fi].fg = fg;
      touched.add(r);
    }
  }
}

CellColor? _bandBg(List<_Cell> row) {
  if (row.isEmpty) return null;
  final bg = row.first.bg;
  if (bg is DefaultColor) return null;
  for (final c in row) {
    if (!_sameColor(c.bg, bg)) return null;
  }
  return bg;
}

CellColor? _userPromptBand(List<_Cell> row) {
  final first = _firstGlyph(row);
  if (first == null || _colOf(row, first) > 1 || row[first].rune != 0x276F) {
    return null;
  }
  return _bandBg(row);
}

/// 프롬프트 띠 한 행 — 띠는 본문 폭까지만(꼬리는 기본 배경), 바탕은 `fill`, ❯ 는 학생색.
void _restyleUserPromptRow(List<_Cell> row, RgbColor fill, RgbColor accent) {
  final last = _lastGlyph(row) ?? 0;
  final padEnd = math.min(last + 2, row.length);
  for (var i = 0; i < row.length; i++) {
    final c = row[i];
    if (i < padEnd) {
      c.bg = fill;
      if (i <= 1 && c.rune == 0x276F) c.fg = accent;
    } else {
      c.bg = const DefaultColor();
    }
  }
}

/// statusline 의 프사 자리(U+FFFC 연속 셀) — 아래→위 스캔. (행, 시작 index, 칸수).
(int, int, int)? _findStatuslineFace(List<List<_Cell>> rows) {
  for (var r = rows.length - 1; r >= 0; r--) {
    final row = rows[r];
    for (var i = 0; i < row.length; i++) {
      if (row[i].rune != 0xFFFC) continue;
      var n = 0;
      while (i + n < row.length && row[i + n].rune == 0xFFFC) {
        n++;
      }
      return (r, i, n);
    }
  }
  return null;
}

bool _isRule(List<_Cell> row, int maxLabel) {
  var dashes = 0, label = 0, contentW = 0;
  for (var i = 0; i < row.length; i++) {
    final g = row[i].rune;
    if (g == 0x2500) {
      dashes++;
      contentW = _colOf(row, i) + 1;
    } else if (g == 0x20 || g == 0) {
      continue;
    } else {
      label++;
      contentW = _colOf(row, i) + 1;
      if (label > maxLabel) return false;
    }
  }
  return dashes >= 8 && dashes > contentW ~/ 2;
}

/// 입력상자 위 서 있는 도트의 앵커 — (앵커 행, 왼쪽 열). statusline 바로 위가 아래
/// 테두리, 그 위 첫 rule 이 윗 테두리; 도트는 윗 테두리에 발이 닿게 선다.
(int, double)? _findStandingAnchor(
  List<List<_Cell>> rows,
  int faceRow,
  int cols,
) {
  if (faceRow < 4 || !_isRule(rows[faceRow - 1], 0)) return null;
  int? tr;
  for (var r = faceRow - 2; r >= math.max(1, faceRow - 16); r--) {
    if (_isRule(rows[r], 24)) {
      tr = r;
      break;
    }
  }
  if (tr == null) return null;
  final anchor = tr - 1;
  final first = _firstGlyph(rows[anchor]);
  final rightC = first == null ? cols - 1.0 : _colOf(rows[anchor], first) - 1.5;
  final leftC = rightC - _standCells;
  return leftC > 2.0 ? (anchor, leftC) : null;
}

/// 데스크톱과 같은 순서로 꾸민다. `t` 는 초 단위 애니 시계.
StyledGrid restyleClaude(GridLines live, StudentStyle st, double t) {
  final rows = <List<_Cell>>[for (final r in live.lines) _cells(r)];
  final touched = <int>{};
  final slots = <SpriteSlot>[];
  var animated = false;
  final accent = st.accent;
  final canWalk = st.slug != null && st.hasWalk;
  final canStand = st.slug != null && st.hasIdle;

  // 통째로 파고들지 않도록 프레임 단위 규칙만 — 학생 프사 자리는 항상 비운다.
  final face = _findStatuslineFace(rows);
  if (face != null) {
    final (fr, fc, n) = face;
    for (var i = fc; i < fc + n; i++) {
      rows[fr][i]
        ..rune = 0x20
        ..fg = const DefaultColor()
        ..bg = const DefaultColor()
        ..flags = 0;
    }
    touched.add(fr);
  }

  var busy = false;
  final hit = _findClaudeSpinner(rows);
  if (hit != null) {
    busy = true;
    animated = true;
    final (sr, sc) = hit;
    final row = rows[sr];
    touched.add(sr);
    var end = row.length;
    for (var i = 0; i < row.length; i++) {
      if (row[i].rune == 0x2026) {
        end = i + 1;
        break;
      }
    }
    if (end == row.length) {
      for (var i = 0; i < row.length; i++) {
        if (row[i].rune == 0x28) {
          end = i;
          break;
        }
      }
    }
    var first = 0, lastc = 0;
    for (var i = 0; i < end; i++) {
      if (!row[i].blank) {
        first = i;
        break;
      }
    }
    for (var i = end - 1; i >= 0; i--) {
      if (!row[i].blank) {
        lastc = i;
        break;
      }
    }
    final span = math.max(1, lastc - first).toDouble();
    const period = 2.0, sigma = 2.0, glow = 0.9;
    final sweep = (t / period) - (t / period).floorToDouble();
    final center = first - sigma * 2.0 + sweep * (span + sigma * 4.0);
    final a = accent.toARGB32();
    final ar = (a >> 16) & 0xff, ag = (a >> 8) & 0xff, ab = a & 0xff;
    for (var i = 0; i < end; i++) {
      final c = row[i];
      if (c.blank) continue;
      final d = i - center;
      final g = math.exp(-(d * d) / (2.0 * sigma * sigma)) * glow;
      int mix(int b) => (b + (255.0 - b) * g).round();
      c.fg = RgbColor(mix(ar), mix(ag), mix(ab));
    }
    final tail = tintToward(st.bg, accent, 0.6);
    for (var i = end; i < row.length; i++) {
      if (!row[i].blank) row[i].fg = tail;
    }
    if (canWalk) {
      row[sc]
        ..rune = 0x20
        ..fg = const DefaultColor()
        ..bg = const DefaultColor()
        ..flags = 0;
      final topR = math.max(0, sr - 1);
      slots.add(
        SpriteSlot(
          'walk',
          topR.toDouble(),
          _colOf(row, sc).toDouble(),
          (sr - topR + 1).toDouble(),
          2.0,
        ),
      );
    }
  }

  if (!busy && canStand && face != null) {
    final anchor = _findStandingAnchor(rows, face.$1, live.cols);
    if (anchor != null) {
      final (ar, leftC) = anchor;
      final h = math.min(_inputStandingRows, live.rows).toDouble();
      slots.add(
        SpriteSlot('idle', math.max(0.0, (ar + 1) - h), leftC, h, _standCells),
      );
      animated = true;
    }
  }

  _stylePromptBox(rows, touched, accent);

  final base = st.bg.toARGB32();
  final light =
      ((base >> 16) & 0xff) + ((base >> 8) & 0xff) + (base & 0xff) > 380;
  final fill = tintToward(st.bg, accent, light ? 0.10 : 0.18);
  final accentRgb = _rgb(accent);
  var r = 0;
  while (r < rows.length) {
    final band = _userPromptBand(rows[r]);
    if (band == null) {
      r++;
      continue;
    }
    while (true) {
      _restyleUserPromptRow(rows[r], fill, accentRgb);
      touched.add(r);
      r++;
      if (r >= rows.length) break;
      final b = _bandBg(rows[r]);
      if (b == null || !_sameColor(b, band)) break;
    }
  }

  final lines = <List<Run>>[
    for (var i = 0; i < rows.length; i++)
      touched.contains(i) ? _runs(rows[i]) : live.lines[i],
  ];
  return StyledGrid(live, lines, slots, animated: animated);
}
