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
    this.codex = false,
    this.session,
    this.branch,
    this.project,
  });

  /// 도트 파일명의 학생 슬러그 — 없으면 색만 입힌다.
  final String? slug;

  /// 표시 이름(「아리스」) — 시작 배너의 제목·환영문에 들어간다.
  final String? name;
  final Color accent;
  final Color bg;
  final bool hasWalk;
  final bool hasIdle;

  /// codex pane 인가 — 바닥 상태줄 되그리기와 세션 배지는 codex 에만 있다.
  final bool codex;

  /// 우리가 붙인 세션 이름 — codex 입력창 첫 줄 오른쪽 배지(데스크톱
  /// `overlay_codex_session_label` 과 같은 자리).
  final String? session;

  /// 상태줄에 보탤 브랜치·폴더 이름 — 데스크톱은 cwd/Git 에서 알고, 폰은 허브가
  /// 준 값을 넣는다.
  final String? branch;
  final String? project;
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

/// 스크롤을 올렸을 때 뷰포트 바닥에 붙잡아 둘 **첫 행** — 입력상자의 위 테두리(codex 는
/// 입력행 자신). 여기서부터 화면 끝까지를 통째로 붙잡는다: 상자 아래 힌트 줄까지 함께
/// 가야 붙잡아 둔 상자 밑으로 지나간 대화가 비치지 않는다(데스크톱 `pinned_input_top`).
int? pinnedInputTop(List<List<Run>> lines) {
  final rows = <List<_Cell>>[for (final r in lines) _cells(r)];
  return switch (_promptBox(rows)) {
    _Bordered(:final top) => top,
    _Filled(:final start) => start,
    null => null,
  };
}

void _blankFacePlaceholders(List<List<_Cell>> rows, Set<int> touched) {
  for (var r = 0; r < rows.length; r++) {
    var hit = false;
    for (final c in rows[r]) {
      if (c.rune == 0xFFFC) {
        _blankCell(c);
        hit = true;
      }
    }
    if (hit) touched.add(r);
  }
}

/// Clawd 그림 자리마다 도트 슬롯을 세우고 제목·환영문·상자 선을 학생 것으로. 하나라도
/// 세웠으면 true(애니가 돈다).
bool _restyleBanners(
  List<List<_Cell>> rows,
  Set<int> touched,
  List<SpriteSlot> slots,
  StudentStyle st,
) {
  final name = st.name;
  if (name == null) return false;
  var any = false;
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
    any = true;
    final acc = _rgb(st.accent);
    _replaceBannerTitle(rows, touched, br, bc, name, acc);
    _replaceWelcome(rows, touched, br, name, acc);
    _tintWelcomeBox(rows, touched, math.max(0, br), br + clawdRows, acc);
  }
  return any;
}

void _restyleUserPromptBands(
  List<List<_Cell>> rows,
  Set<int> touched,
  StudentStyle st,
) {
  final base = st.bg.toARGB32();
  final light =
      ((base >> 16) & 0xff) + ((base >> 8) & 0xff) + (base & 0xff) > 380;
  final fill = tintToward(st.bg, st.accent, light ? 0.10 : 0.18);
  final accentRgb = _rgb(st.accent);
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
}

/// 지난 줄(스크롤백)의 꾸밈 — 데스크톱은 넘겨 본 줄에도 같은 규칙을 입힌다. 스피너·
/// 입력상자·서 있는 도트는 살아 있는 화면 몫이라 여기엔 없다: 프사 자리표 지우기,
/// 사용자 프롬프트 띠, 시작 배너(도트·이름·인사·상자 선)만. 돌려주는 자리는 지난 줄
/// 기준 행이다.
(List<List<Run>>, List<SpriteSlot>) restyleHistory(
  List<List<Run>> history,
  StudentStyle st,
) {
  final rows = <List<_Cell>>[for (final r in history) _cells(r)];
  final touched = <int>{};
  final slots = <SpriteSlot>[];
  _blankFacePlaceholders(rows, touched);
  if (st.slug != null && st.hasIdle && st.name != null) {
    _restyleBanners(rows, touched, slots, st);
  }
  _restyleUserPromptBands(rows, touched, st);
  return (
    [
      for (var i = 0; i < rows.length; i++)
        touched.contains(i) ? _runs(rows[i]) : history[i],
    ],
    slots,
  );
}

/// 데스크톱과 같은 순서로 꾸민다. `t` 는 초 단위 애니 시계.
const _efforts = {'none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max'};

/// 데스크톱 `restyle_codex_status_line` 의 색 — Tokyo Night 계열, claude statusline 과 같다.
const _cModel = RgbColor(0x7a, 0xa2, 0xf7);
const _cGit = RgbColor(0x73, 0xda, 0xca);
const _cDir = RgbColor(0xbb, 0x9a, 0xf7);
const _cCtx = RgbColor(0xff, 0x9e, 0x64);
const _cSep = RgbColor(0x56, 0x5f, 0x89);
const _cDanger = RgbColor(0xf7, 0x76, 0x8e);

RgbColor _effortColor(String level) => switch (level) {
  'low' => const RgbColor(0x56, 0x5f, 0x89),
  'medium' => _cModel,
  'high' => const RgbColor(0xe0, 0xaf, 0x68),
  'xhigh' => _cDanger,
  'max' => _cDir,
  _ => _cModel,
};

String _titleCase(String part) =>
    part.isEmpty ? part : part[0].toUpperCase() + part.substring(1);

/// `gpt-5.6-sol` → `GPT-5.6 Sol` — 데스크톱과 같은 표기.
String prettyModel(String raw) {
  final lower = raw.toLowerCase();
  switch (lower) {
    case 'gpt-5.6' || 'gpt-5.6-sol':
      return 'GPT-5.6 Sol';
    case 'gpt-5.6-terra':
      return 'GPT-5.6 Terra';
    case 'gpt-5.6-luna':
      return 'GPT-5.6 Luna';
  }
  if (lower.startsWith('gpt-')) {
    final pieces = lower.split('-');
    final version = pieces.length > 1 ? pieces[1] : '';
    final suffix = pieces.skip(2).map(_titleCase).join(' ');
    return suffix.isEmpty ? 'GPT-$version' : 'GPT-$version $suffix';
  }
  if (lower.startsWith('claude-') || lower.startsWith('codex-')) {
    return lower.split('-').map(_titleCase).join(' ');
  }
  return raw;
}

class _Span {
  const _Span(this.text, {this.fg, this.bold = false, this.dim = false});
  final String text;
  final CellColor? fg;
  final bool bold;
  final bool dim;
}

int _rowCols(List<_Cell> row) {
  var w = 0;
  for (final c in row) {
    w += cellWidth(c.rune);
  }
  return w;
}

/// codex 바닥줄(「gpt-5.6-sol xhigh · main · kasaterm · Context 16% used」)을 claude
/// statusline 과 같은 말로 다시 쓴다 — 데스크톱 `restyle_codex_status_line` 과 같은
/// 규칙·색. codex 는 항목 순서만 받고 아이콘·구분자를 못 바꾸므로 PTY 가 준 값을
/// 읽어 표현만 바꾸고, 못 읽으면 원본을 둔다. 모델 표식은 로고 자리표로 심어
/// 뒤의 표식 치환이 codex 로고를 앉힌다.
bool _restyleCodexStatusLine(
  List<List<_Cell>> rows,
  Set<int> touched, {
  required int cols,
  String? branch,
  String? project,
}) {
  for (var r = rows.length - 1; r >= 0; r--) {
    final parts = _text(rows[r])
        .trim()
        .split(' · ')
        .map((s) => s.trim())
        .where((s) => s.isNotEmpty)
        .toList();
    if (parts.isEmpty) continue;
    final words = parts.first.split(RegExp(r'\s+'));
    final effort = words.last;
    if (!_efforts.contains(effort)) continue;
    final model = words.sublist(0, words.length - 1).join(' ');
    final lower = model.toLowerCase();
    final known =
        lower.startsWith('gpt-') ||
        lower.startsWith('codex-') ||
        lower.startsWith('claude-') ||
        (lower.length > 1 &&
            lower[0] == 'o' &&
            lower.codeUnitAt(1) >= 0x30 &&
            lower.codeUnitAt(1) <= 0x39);
    if (model.isEmpty || !known) continue;
    String? context;
    for (final part in parts) {
      for (final word in part.split(RegExp(r'\s+'))) {
        final m = RegExp(r'^\D*(\d+%)\D*$').firstMatch(word);
        if (m != null) {
          context = m.group(1);
          break;
        }
      }
      if (context != null) break;
    }
    final middle = parts
        .skip(1)
        .where(
          (p) =>
              !p.contains('%') &&
              !const {
                'never',
                'on-request',
                'untrusted',
                'on-failure',
              }.contains(p),
        )
        .toList();
    var b = (branch ?? '').isEmpty ? null : branch;
    var d = (project ?? '').isEmpty ? null : project;
    if (b == null && d == null) {
      b = middle.isNotEmpty ? middle[0] : null;
      d = middle.length > 1 ? middle[1] : null;
    }
    final marker = lower.startsWith('claude-')
        ? statusModelClaude
        : statusModelGpt;
    List<_Span> line(
      bool window,
      bool showBranch,
      bool showProject,
      bool showCtx,
    ) {
      final out = <_Span>[
        const _Span(' '),
        _Span(String.fromCharCode(marker), fg: _cModel, bold: true),
        const _Span(' '),
        _Span(prettyModel(model), fg: _cModel, bold: true),
        if (window && lower.startsWith('gpt-5.6'))
          const _Span(' 1M', dim: true),
      ];
      void sep() => out
        ..add(const _Span(' '))
        ..add(const _Span('┃', fg: _cSep, dim: true))
        ..add(const _Span(' '));
      if (showBranch && b != null) {
        sep();
        out.add(_Span(' $b', fg: _cGit));
      }
      if (showProject && d != null) {
        sep();
        out.add(_Span(' $d', fg: _cDir));
      }
      if (showCtx && context != null) {
        sep();
        final pct = int.tryParse(context.substring(0, context.length - 1)) ?? 0;
        out.add(_Span(context, fg: pct >= 90 ? _cDanger : _cCtx));
      }
      sep();
      out.add(_Span(' $effort', fg: _effortColor(effort)));
      return out;
    }

    // 서버가 뒤 빈칸을 잘라 보내므로 행의 칸 수가 아니라 화면 폭이 기준이다.
    final width = cols;
    // 좁으면 폴더 → 브랜치 → 1M → 컨텍스트 순으로 뺀다. 컨텍스트%는 폰에서 제일
    // 자주 보는 값이라 마지막까지 남긴다(claude 상태줄도 같은 순서).
    final candidates = [
      line(true, true, true, true),
      line(true, true, false, true),
      line(true, false, false, true),
      line(false, false, false, true),
      line(false, false, false, false),
    ];
    for (final spans in candidates) {
      final cells = <_Cell>[
        for (final sp in spans)
          for (final rune in sp.text.runes)
            _Cell(
              rune,
              sp.fg ?? const DefaultColor(),
              const DefaultColor(),
              (sp.bold ? flagBold : 0) | (sp.dim ? flagDim : 0),
            ),
      ];
      final used = _rowCols(cells);
      if (used > width) continue;
      for (var pad = used; pad < width; pad++) {
        cells.add(_Cell(0x20, const DefaultColor(), const DefaultColor(), 0));
      }
      rows[r]
        ..clear()
        ..addAll(cells);
      touched.add(r);
      return true;
    }
    return false;
  }
  return false;
}

/// codex 입력창 첫 줄 오른쪽에 세션 이름 배지 — 데스크톱 `overlay_codex_session_label`.
/// claude 는 CLI 가 위보더 끝에 `/rename` 이름을 스스로 그리지만 codex 는 안 그려서,
/// 화면만 보고는 무슨 일을 하는 자리인지 몰랐다. 그 구간이 비어 있을 때만 심는다.
void _overlayCodexSessionLabel(
  List<List<_Cell>> rows,
  Set<int> touched,
  String name,
  Color accent,
  int width,
) {
  final bx = _promptBox(rows);
  if (bx is! _Filled) return;
  final row = rows[bx.start];
  name = name.trim();
  if (name.isEmpty || row.isEmpty) return;
  // 배지는 **폰 화면의** 오른쪽 끝에 — pane 폭(171칸) 끝에 두면 되잇기가 그 행을
  // 일곱 줄로 접고 들여쓰기 자리가 검게 뜬다(2026-09-08 지적 「입력창이 이상해」).
  // 서버가 뒤 빈칸을 잘라 보내므로 모자란 칸은 띠 바탕으로 채운다.
  final w = width;
  while (_rowCols(row) < w) {
    row.add(_Cell(0x20, const DefaultColor(), row.last.bg, 0));
  }
  // 이름이 길다고 줄을 통째로 먹으면 배지가 아니라 문장이다 — 폭의 절반까지.
  final budget = w ~/ 2;
  if (budget < 6) return;
  final shown = <int>[];
  var used = 0;
  for (final ch in name.runes) {
    final cw = cellWidth(ch).clamp(1, 2);
    if (used + cw > budget - 1) {
      shown.add(0x2026);
      used += 1;
      break;
    }
    shown.add(ch);
    used += cw;
  }
  // 오른쪽 한 칸은 비워 둔다 — claude 가 위보더 끝에 대시 한 칸을 남기듯.
  final end = w - 1;
  final start = end - used;
  if (start <= 0 || used == 0) return;
  final head = <_Cell>[];
  final tail = <_Cell>[];
  _Cell? sample;
  var col = 0;
  for (final c in row) {
    final at = col;
    col += cellWidth(c.rune);
    // 앞 한 칸까지 함께 본다 — 옆 글자에 딱 붙으면 배지로 안 읽힌다.
    if (at >= start - 1 && at < end && !c.blank) return;
    if (at < start) {
      head.add(c);
    } else if (at >= end) {
      tail.add(c);
    } else {
      sample ??= c;
    }
  }
  final fg = _rgb(accent);
  final bg = sample?.bg ?? const DefaultColor();
  row
    ..clear()
    ..addAll(head)
    ..addAll([for (final r in shown) _Cell(r, fg, bg, 0)])
    ..addAll(tail);
  touched.add(bx.start);
}

int _trimmedCols(List<_Cell> row) {
  var end = row.length;
  while (end > 0 && row[end - 1].blank) {
    end--;
  }
  return _rowCols(row.sublist(0, end));
}

/// 상태줄 조각의 종류 — 아이콘 글리프로 안다(statusline.py 가 찍는 Nerd 글리프).
const _glyphBranch = 0xE0A0;
const _glyphFolder = 0xF07B;

/// ` ┃ ` 로 나뉜 조각의 [시작, 끝) 목록. 첫 조각은 모델이다.
List<(int, int)> _statusSegments(List<_Cell> row) {
  final segs = <(int, int)>[];
  var start = 0;
  for (var i = 1; i + 1 < row.length; i++) {
    if (row[i].rune == 0x2503 && row[i - 1].blank && row[i + 1].blank) {
      segs.add((start, i - 1));
      start = i + 2;
    }
  }
  segs.add((start, row.length));
  return segs;
}

/// claude statusline(「모델 1M ┃ 브랜치 ┃ 폴더 ┃ 42% ┃ xhigh」)을 폰 폭에 맞춘다 —
/// 폴더 → 브랜치 → 1M → 컨텍스트 순으로 빼고 모델·effort 는 남긴다. 칸을 지울 뿐
/// 색·글꼴은 원래 것이라 데스크톱과 같은 옷이다.
bool _shrinkStatusRow(List<_Cell> row, int width) {
  var changed = false;
  bool has(List<_Cell> seg, int rune) => seg.any((c) => c.rune == rune);
  bool dropSeg(bool Function(List<_Cell>) pick) {
    final segs = _statusSegments(row);
    for (var k = segs.length - 1; k >= 1; k--) {
      final (a, b) = segs[k];
      if (!pick(row.sublist(a, b))) continue;
      // 앞의 ` ┃ ` 까지 함께 걷는다.
      row.removeRange(a - 3, b);
      return true;
    }
    return false;
  }

  bool dropWindow() {
    final (a, b) = _statusSegments(row).first;
    final t = _text(row.sublist(a, b));
    final at = t.lastIndexOf(' 1M');
    if (at < 0) return false;
    row.removeRange(a + at, a + at + 3);
    return true;
  }

  final steps = <bool Function()>[
    () => dropSeg((seg) => has(seg, _glyphFolder)),
    () => dropSeg((seg) => has(seg, _glyphBranch)),
    dropWindow,
    () => dropSeg((seg) => _text(seg).contains('%')),
  ];
  for (final step in steps) {
    if (_trimmedCols(row) <= width) break;
    if (step()) changed = true;
  }
  return changed;
}

/// 힌트 줄(「⏵⏵ bypass permissions on (shift+tab to cycle)」)은 괄호 힌트부터 떼고,
/// 그래도 넘치면 「…」로 자른다 — 접혀서 두 줄이 되느니 한 줄에 요점만.
bool _fitPlainRow(List<_Cell> row, int width) {
  var end = row.length;
  while (end > 0 && row[end - 1].blank) {
    end--;
  }
  if (end > 0 && row[end - 1].rune == 0x29) {
    var open = end - 1;
    while (open > 0 && row[open].rune != 0x28) {
      open--;
    }
    if (open > 0 && row[open].rune == 0x28 && row[open - 1].blank) {
      row.removeRange(open - 1, row.length);
      if (_trimmedCols(row) <= width) return true;
      end = row.length;
    }
  }
  var keep = 0;
  var used = 0;
  while (keep < end && used + cellWidth(row[keep].rune) <= width - 1) {
    used += cellWidth(row[keep].rune);
    keep++;
  }
  final last = keep > 0 ? row[keep - 1] : null;
  row.removeRange(keep, row.length);
  row.add(
    _Cell(0x2026, last?.fg ?? const DefaultColor(), const DefaultColor(), 0),
  );
  return true;
}

/// 입력상자 아래 바닥줄(상태줄·힌트)은 폰에서 접지 않는다 — 넘치면 덜 중요한 조각부터
/// 뺀다(2026-09-08 지시 「상태줄이랑 밑에 여러 줄 안 되게」). 대화 본문은 안 건드린다.
void _fitFooterRows(List<List<_Cell>> rows, Set<int> touched, int width) {
  final bx = _promptBox(rows);
  if (bx == null) return;
  final from = switch (bx) {
    _Bordered(:final bottom) => bottom + 1,
    _Filled(:final end) => end,
  };
  for (var r = from; r < rows.length; r++) {
    final row = rows[r];
    if (_trimmedCols(row) <= width) continue;
    final status = row.any(
      (c) => c.rune == statusModelClaude || c.rune == statusModelGpt,
    );
    final changed = status
        ? _shrinkStatusRow(row, width)
        : _fitPlainRow(row, width);
    if (changed) touched.add(r);
  }
}

/// [wrapCols] 는 폰이 행을 접는 열 수 — codex 상태줄의 단계와 세션 배지 자리는 pane
/// 폭이 아니라 이걸 본다.
StyledGrid restyleClaude(
  GridLines live,
  StudentStyle st,
  double t, {
  int? wrapCols,
}) {
  final rows = <List<_Cell>>[for (final r in live.lines) _cells(r)];
  final width = wrapCols == null || wrapCols > live.cols ? live.cols : wrapCols;
  final touched = <int>{};
  final slots = <SpriteSlot>[];
  var animated = false;
  final accent = st.accent;
  final canWalk = st.slug != null && st.hasWalk;
  final canStand = st.slug != null && st.hasIdle;

  // 학생 프사 자리표(U+FFFC)는 어느 행이든 비운다 — 글꼴에 없어 빈 상자로 뜬다.
  final face = _findStatuslineFace(rows);
  _blankFacePlaceholders(rows, touched);

  // codex 바닥줄은 claude statusline 의 말로 — 표식 치환보다 먼저라야 로고가 앉는다.
  if (st.codex) {
    _restyleCodexStatusLine(
      rows,
      touched,
      cols: width,
      branch: st.branch,
      project: st.project,
    );
  }

  _fitFooterRows(rows, touched, width);

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
  if (canStand && st.name != null) {
    if (_restyleBanners(rows, touched, slots, st)) animated = true;
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
  if (st.codex && (st.session ?? '').isNotEmpty) {
    _overlayCodexSessionLabel(rows, touched, st.session!, accent, width);
  }

  _restyleUserPromptBands(rows, touched, st);

  final lines = <List<Run>>[
    for (var i = 0; i < rows.length; i++)
      touched.contains(i) ? _runs(rows[i]) : live.lines[i],
  ];
  return StyledGrid(live, lines, slots, animated: animated);
}
