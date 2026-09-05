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
    this.name,
    this.hasWalk = true,
    this.hasIdle = true,
  });

  /// 도트 파일명의 학생 슬러그 — 없으면 색만 입힌다.
  final String? slug;

  /// 표시 이름(「아리스」) — 시작 배너의 제목·환영문에 들어간다.
  final String? name;
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

/// claude 시작 배너의 Clawd 블록 그림 — 9칸×3줄. 위 줄이 화면 밖으로 밀리면 행이 음수다.
const clawdCols = 9;
const clawdRows = 3;
const _clawdTitle = 'Claude Code';

/// 상태줄 모델 표식 — 글자가 아니라 로고를 앉힐 한 칸 자리표(데스크톱 `STATUS_MODEL_*_MARKER`).
const statusModelClaude = 0xE0C0;
const statusModelGpt = 0xE0C1;
const statusModelColor = Color(0xff7aa2f7);

int? _idxAtCol(List<_Cell> row, int col) {
  var c = 0;
  for (var i = 0; i < row.length; i++) {
    if (c == col) return i;
    if (c > col) return null;
    c += cellWidth(row[i].rune);
  }
  return c == col ? row.length : null;
}

bool _matchesAt(List<_Cell> row, int at, List<int> pat) {
  if (at < 0 || at + pat.length > row.length) return false;
  for (var i = 0; i < pat.length; i++) {
    if (row[at + i].rune != pat[i]) return false;
  }
  return true;
}

/// (시작 행, 시작 index) 목록. 두 세대의 그림을 다 본다; 행 -1·-2 는 머리가 밀려난 것.
List<(int, int)> _findClawdBanners(List<List<_Cell>> rows) {
  const gens = [
    (
      [0x2590, 0x259B, 0x2588, 0x2588, 0x2588, 0x259B, 0x2588],
      [0x259D, 0x259C, 0x2588, 0x2588, 0x2588, 0x2588, 0x2588, 0x2588, 0x2580],
      [0x259D, 0x259D, 0x20, 0x259D, 0x259D],
    ),
    (
      [0x2590, 0x259B, 0x2588, 0x2588, 0x2588, 0x259C, 0x258C],
      [0x259D, 0x259C, 0x2588, 0x2588, 0x2588, 0x2588, 0x2588, 0x259B, 0x2598],
      [0x2598, 0x2598, 0x20, 0x259D, 0x259D],
    ),
  ];
  final out = <(int, int)>[];
  for (final (head, body, feet) in gens) {
    for (var r = 0; r < rows.length; r++) {
      final row = rows[r];
      var c = 0;
      while (c + body.length <= row.length) {
        if (_matchesAt(row, c, body)) {
          if (r == 0) {
            out.add((-1, c));
            c += body.length;
            continue;
          }
          if (_matchesAt(rows[r - 1], c + 1, head)) {
            out.add((r - 1, c));
            c += body.length;
            continue;
          }
        }
        c++;
      }
    }
    if (rows.isNotEmpty) {
      final row = rows.first;
      var p = 2;
      while (p + feet.length + 2 <= row.length) {
        if (_matchesAt(row, p, feet) &&
            row[p - 2].blank &&
            row[p - 1].blank &&
            row[p + 5].blank &&
            row[p + 6].blank) {
          out.add((-2, p - 2));
          p += feet.length;
          continue;
        }
        p++;
      }
    }
  }
  return out;
}

void _blankCell(_Cell c) {
  c
    ..rune = 0x20
    ..fg = const DefaultColor()
    ..bg = const DefaultColor()
    ..flags = 0;
}

/// 「Claude Code」를 학생 이름으로 — 이름 글자마다 빈 칸 하나(두 칸 글자의 자리)를 붙이고,
/// 뒤따르는 버전 글은 왼쪽으로 당긴다. 여섯 칸을 넘는 이름은 원문을 둔다.
void _replaceBannerTitle(
  List<List<_Cell>> rows,
  Set<int> touched,
  int br,
  int bc,
  String name,
  RgbColor accent,
) {
  final title = _clawdTitle.runes.toList();
  final r0 = math.max(0, br), r1 = math.min(rows.length, br + clawdRows);
  for (var r = r0; r < r1; r++) {
    final row = rows[r];
    final start = bc + clawdCols;
    if (start >= row.length) continue;
    int? tc;
    for (var c = start; c + title.length <= row.length; c++) {
      if (_matchesAt(row, c, title)) {
        tc = c;
        break;
      }
    }
    if (tc == null) continue;
    final style = row[tc];
    final repl = <_Cell>[];
    for (final ch in name.runes) {
      repl.add(_Cell(ch, accent, style.bg, style.flags));
      if (cellWidth(ch) == 1) {
        repl.add(_Cell(0x20, accent, style.bg, style.flags));
      }
    }
    final replCols = repl.fold(0, (n, c) => n + cellWidth(c.rune));
    if (replCols > title.length) return;
    var end = tc + title.length;
    var probe = end;
    while (probe < row.length) {
      if (row[probe].blank) {
        if (probe + 1 >= row.length || row[probe + 1].blank) break;
      } else {
        end = probe + 1;
      }
      probe++;
    }
    final tail = row.sublist(tc + title.length, end);
    final rebuilt = [...row.sublist(0, tc), ...repl, ...tail];
    // 빈 칸으로 채워 폭을 지킨다 — 원본은 칸 단위라 뒤 글자가 안 밀린다.
    var cols = rebuilt.fold(0, (n, c) => n + cellWidth(c.rune));
    final want = _colOf(row, end);
    while (cols < want) {
      rebuilt.add(_Cell(0x20, const DefaultColor(), const DefaultColor(), 0));
      cols++;
    }
    rebuilt.addAll(row.sublist(end));
    rows[r] = rebuilt;
    touched.add(r);
    return;
  }
}

String _welcomeFor(String name, String user) => switch (name) {
  '아로나' => '어서 오세요 $user 선생님!',
  '프라나' => '$user 선생님, 오셨군요.',
  '미도리' => '$user 선생님, 오셨어요.',
  '모모이' => '$user 선생님, 어서 오세요!',
  '유즈' => '$user 선생님… 오셨네요.',
  '아리스' => '$user 선생님, 돌아왔구나!',
  '유우카' => '$user 선생님, 오셨네요.',
  '시로코' => '$user 선생님, 오셨어요.',
  '호시노' => '$user 선생님~ 왔구나~',
  '코하루' => '어, 어서오세요 $user 선생님…!',
  '히마리' => '$user 선생님, 어서 오세요.',
  '아루' => '훗, 왔군 $user 선생님!',
  _ => '$user 선생님, 어서 오세요.',
};

/// 「Welcome back `<user>`!」 를 학생 말투의 인사로. 배너 위 네 줄만 본다.
void _replaceWelcome(
  List<List<_Cell>> rows,
  Set<int> touched,
  int br,
  String name,
  RgbColor accent,
) {
  final prefix = 'Welcome back '.runes.toList();
  final hi = br.clamp(0, rows.length), lo = math.max(0, br - 4);
  for (var r = lo; r < hi; r++) {
    final row = rows[r];
    int? wc;
    for (var c = 0; c + prefix.length <= row.length; c++) {
      if (_matchesAt(row, c, prefix)) {
        wc = c;
        break;
      }
    }
    if (wc == null) continue;
    final nameStart = wc + prefix.length;
    var excl = -1;
    for (var i = nameStart; i < row.length; i++) {
      if (row[i].rune == 0x21) {
        excl = i;
        break;
      }
    }
    if (excl <= nameStart) continue;
    final user = String.fromCharCodes([
      for (var i = nameStart; i < excl; i++) row[i].rune,
    ]).trim();
    var limit = row.length;
    for (var i = excl + 1; i < row.length; i++) {
      if (!row[i].blank) {
        limit = i;
        break;
      }
    }
    final greet = _welcomeFor(name, user);
    final cells = <_Cell>[
      for (final ch in greet.runes)
        _Cell(ch, accent, row[wc].bg, row[wc].flags),
    ];
    final width = cells.fold(0, (n, c) => n + cellWidth(c.rune));
    final room = _colOf(row, limit) - _colOf(row, wc);
    if (width > room) return;
    final rebuilt = [...row.sublist(0, wc), ...cells];
    var cols = width;
    while (cols < room) {
      rebuilt.add(_Cell(0x20, const DefaultColor(), row[wc].bg, 0));
      cols++;
    }
    rebuilt.addAll(row.sublist(limit));
    rows[r] = rebuilt;
    touched.add(r);
    return;
  }
}

/// 환영 상자의 선을 학생색으로 — 배너 위의 ╭ 줄부터 그림 아래 첫 ╰ 줄까지.
void _tintWelcomeBox(
  List<List<_Cell>> rows,
  Set<int> touched,
  int welcomeRow,
  int artBottom,
  RgbColor accent,
) {
  bool hasAny(List<_Cell> row, Set<int> set) =>
      row.any((c) => set.contains(c.rune));
  int? top;
  for (var r = welcomeRow - 1; r >= 0; r--) {
    if (hasAny(rows[r], const {0x256D, 0x256E, 0x250C, 0x2510})) {
      top = r;
      break;
    }
  }
  int? bottom;
  for (var r = math.min(artBottom, rows.length); r < rows.length; r++) {
    if (hasAny(rows[r], const {0x2570, 0x256F, 0x2514, 0x2518})) {
      bottom = r;
      break;
    }
  }
  if (top == null || bottom == null) return;
  for (var r = top; r <= bottom; r++) {
    for (final c in rows[r]) {
      if (c.rune >= 0x2500 && c.rune <= 0x257F) c.fg = accent;
    }
    touched.add(r);
  }
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
      _blankCell(rows[fr][i]);
    }
    touched.add(fr);
  }

  // 상태줄 모델 표식 — 글리프 대신 로고. 아래→위, 마지막 상태줄이 이긴다.
  for (var r = rows.length - 1; r >= 0; r--) {
    final row = rows[r];
    var done = false;
    for (var i = 0; i < row.length; i++) {
      final g = row[i].rune;
      if (g != statusModelClaude && g != statusModelGpt) continue;
      final col = _colOf(row, i);
      _blankCell(row[i]);
      touched.add(r);
      slots.add(
        SpriteSlot(
          g == statusModelClaude ? 'icon:claude' : 'icon:codex',
          r.toDouble(),
          col.toDouble(),
          1,
          2,
        ),
      );
      done = true;
      break;
    }
    if (done) break;
  }

  // 시작 배너 — Clawd 그림 자리에 학생 도트, 제목·환영문은 학생 것으로.
  final name = st.name;
  if (canStand && name != null) {
    for (final (br, bc) in _findClawdBanners(rows)) {
      final r0 = math.max(0, br), r1 = math.min(rows.length, br + clawdRows);
      for (var r = r0; r < r1; r++) {
        final i0 = _idxAtCol(rows[r], bc);
        if (i0 == null) continue;
        for (var i = i0; i < math.min(rows[r].length, i0 + clawdCols); i++) {
          _blankCell(rows[r][i]);
        }
        touched.add(r);
      }
      slots.add(
        SpriteSlot(
          'idle',
          br.toDouble(),
          bc.toDouble(),
          clawdRows.toDouble(),
          clawdCols.toDouble(),
        ),
      );
      animated = true;
      final acc = _rgb(accent);
      _replaceBannerTitle(rows, touched, br, bc, name, acc);
      _replaceWelcome(rows, touched, br, name, acc);
      _tintWelcomeBox(rows, touched, math.max(0, br), br + clawdRows, acc);
    }
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
      _blankCell(row[sc]);
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
