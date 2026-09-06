import 'claude_style.dart';
import 'grid.dart';

/// 폰 폭에 맞춰 다시 접은 화면. 미러 pane 의 크기는 못 바꾸므로(데스크톱이 같이
/// 좁아진다) 받은 행을 폰의 열 수로 접는다 — 데스크톱은 제 크기 그대로, 폰은 제 폭으로
/// 따로 산다. 뒤쪽 빈 칸은 접기 전에 잘라 짧은 줄이 한 줄로 남고, 아래쪽 빈 행도 잘라
/// 마지막 글자 줄이 바닥에 앉는다.
class ReflowedGrid implements GridLines {
  const ReflowedGrid({
    required this.cols,
    required this.lines,
    required this.cursorRow,
    required this.cursorCol,
    required this.cursorVisible,
    this.slots = const [],
  });

  @override
  final int cols;
  @override
  final List<List<Run>> lines;
  @override
  int get rows => lines.length;
  @override
  final int cursorRow;
  @override
  final int cursorCol;
  @override
  final bool cursorVisible;
  @override
  final List<SpriteSlot> slots;
}

class _Cell {
  _Cell(this.rune, this.run) : width = cellWidth(rune);
  final int rune;
  final int width;
  final Run run;

  /// 색 없는 빈 칸 — 뒤에 달려 있으면 잘라도 화면이 안 바뀐다.
  bool get blank =>
      rune == 0x20 &&
      run.bg is DefaultColor &&
      run.flags & (flagInverse | flagUnderline) == 0;

  /// 빈칸이나 선 그리기 글자(─ ━ ═ ░ …) — 글줄이 아니라 상자의 채움이다.
  bool get filler => rune == 0x20 || (rune >= 0x2500 && rune <= 0x259f);

  /// 같은 모양의 채움 글자인가 — 빈 칸은 글자색이 안 보이니 배경만 본다.
  bool sameFill(_Cell o) =>
      filler &&
      rune == o.rune &&
      width == 1 &&
      run.bg == o.run.bg &&
      run.flags == o.run.flags &&
      (rune == 0x20 || run.fg == o.run.fg);
}

/// 한 행을 접은 결과. `starts` 는 각 조각이 원본의 몇 번째 열에서 시작하는지 —
/// 커서를 어느 조각으로 옮길지 여기서 찾는다.
class RowReflow {
  const RowReflow(this.chunks, this.starts, {this.indent = 0});
  final List<List<Run>> chunks;
  final List<int> starts;

  /// 둘째 조각부터 앞에 붙인 빈 칸 수 — 글머리(- • ⎿ 1.)와 들여쓰기 아래로 이어져
  /// 문단이 한 덩어리로 읽힌다.
  final int indent;
}

/// `paneCols` 는 원본 pane 의 폭 — 폰보다 좁은 pane(맥미니 창을 다섯으로 쪼갠 30열)의
/// 테두리·상자 줄을 폰 폭까지 늘이는 데 쓴다. 0 이면 안 늘인다.
RowReflow reflowRow(List<Run> runs, int cols, {int paneCols = 0}) =>
    _reflowCells(_cellsOf(runs), cols, paneCols: paneCols);

List<_Cell> _cellsOf(List<Run> runs) => [
  for (final r in runs)
    for (final rune in r.text.runes) _Cell(rune, r),
];

const _blank = Run(' ', DefaultColor(), DefaultColor(), 0);

RowReflow _reflowCells(List<_Cell> cells, int cols, {int paneCols = 0}) {
  var end = cells.length;
  while (end > 0 && cells[end - 1].blank) {
    end--;
  }
  if (end == 0) {
    return const RowReflow([[]], [0]);
  }
  final trimmed = cells.sublist(0, end);
  var width = 0;
  for (final c in trimmed) {
    width += c.width;
  }
  if (width <= cols) {
    // pane 을 꽉 채운 테두리·상자 줄은 폰 폭까지 — 글은 되이어 넓어졌는데 상자만 좁으면
    // 데스크톱과 다른 물건으로 보인다.
    final wide = paneCols > 0 && paneCols < cols && width == paneCols
        ? _stretch(trimmed, cols - paneCols)
        : null;
    return RowReflow([_runs(wide ?? trimmed)], const [0]);
  }

  // 테두리·상자 줄(╭────╮, │ 글 …빈칸… │)은 접으면 네 줄짜리 선이 된다. 앞 cols-1 칸
  // 뒤가 같은 글자로만 이어지다 마지막 한 칸으로 끝나면 가운데를 빼고 폭에 맞춘다.
  final head = <_Cell>[];
  var headWidth = 0;
  for (final c in trimmed) {
    if (headWidth + c.width > cols - 1) break;
    head.add(c);
    headWidth += c.width;
  }
  if (head.length < trimmed.length - 1) {
    final filler = trimmed.sublist(head.length, trimmed.length - 1);
    final last = trimmed.last;
    if (last.width == 1 && filler.every((c) => c.sameFill(filler.first))) {
      return RowReflow(
        [
          _runs([...head, last]),
        ],
        const [0],
      );
    }
  }

  // 「──────── mobile ─」 처럼 라벨이 낀 테두리는 위 규칙에 안 걸린다 — 선 그리기
  // 채움을 줄여 한 줄에 맞춘다. 데스크톱에선 한 줄인 상자 윗변이 두 줄로 갈리지 않게.
  final shrunk = _shrinkLines(trimmed, width - cols);
  if (shrunk != null) return RowReflow([_runs(shrunk)], const [0]);

  // 이어지는 조각은 글머리·들여쓰기 아래로 — 폭의 절반까지만(들여쓰기가 너무 깊으면
  // 한 줄에 낱말 하나씩 남는다).
  final indent = _hangingIndent(trimmed).clamp(0, cols ~/ 2);
  final pad = <_Cell>[for (var p = 0; p < indent; p++) _Cell(0x20, _blank)];
  final chunks = <List<Run>>[];
  final starts = <int>[];
  void emit(List<_Cell> line) {
    chunks.add(_runs(chunks.isEmpty ? line : [...pad, ...line]));
  }

  var i = 0;
  var col = 0;
  while (i < trimmed.length) {
    final start = col;
    final line = <_Cell>[];
    var lineWidth = 0;
    var j = i;
    final budget = chunks.isEmpty ? cols : cols - indent;
    while (j < trimmed.length && lineWidth + trimmed[j].width <= budget) {
      line.add(trimmed[j]);
      lineWidth += trimmed[j].width;
      j++;
    }
    if (line.isEmpty) {
      line.add(trimmed[j]);
      lineWidth += trimmed[j].width;
      j++;
    }
    if (j < trimmed.length) {
      // 넘쳤다 — 낱말 가운데가 갈리지 않게 가까운 빈칸에서 끊고 그 빈칸은 버린다.
      if (trimmed[j].rune == 0x20) {
        emit(line);
        starts.add(start);
        i = j + 1;
        col = start + lineWidth + 1;
        continue;
      }
      var k = line.length - 1;
      var back = 0;
      while (k > 0 && back < _wordBackoff && line[k].rune != 0x20) {
        back += line[k].width;
        k--;
      }
      if (k > 0 && line[k].rune == 0x20) {
        final kept = line.sublist(0, k);
        var keptWidth = 0;
        for (final c in kept) {
          keptWidth += c.width;
        }
        emit(kept);
        starts.add(start);
        i += k + 1;
        col = start + keptWidth + 1;
        continue;
      }
    }
    emit(line);
    starts.add(start);
    i = j;
    col = start + lineWidth;
  }
  return RowReflow(chunks, starts, indent: indent);
}

/// 글머리 기호 — 이 뒤의 빈칸까지가 이어지는 줄의 들여쓰기다.
const _marks = {
  0x2d, // -
  0x2a, // *
  0x3e, // >
  0x2022, // •
  0x25cf, // ●
  0x23fa, // ⏺
  0x23bf, // ⎿
  0x276f, // ❯
  0x25b8, // ▸
  0x25aa, // ▪
};

/// 행의 들여쓰기 — 앞 빈칸에, 글머리(- • ⎿ ❯ 「1.」「2)」)가 있으면 그 뒤 빈칸까지.
int _hangingIndent(List<_Cell> cells) {
  var i = 0;
  while (i < cells.length && cells[i].rune == 0x20) {
    i++;
  }
  if (i >= cells.length) return 0;
  final lead = i;
  var j = i;
  final r = cells[j].rune;
  if (_marks.contains(r)) {
    j++;
  } else if (r >= 0x30 && r <= 0x39) {
    var k = j;
    while (k < cells.length && cells[k].rune >= 0x30 && cells[k].rune <= 0x39) {
      k++;
    }
    if (k >= cells.length || (cells[k].rune != 0x2e && cells[k].rune != 0x29)) {
      return lead;
    }
    j = k + 1;
  } else {
    return lead;
  }
  if (j >= cells.length || cells[j].rune != 0x20) return lead;
  while (j < cells.length && cells[j].rune == 0x20) {
    j++;
  }
  var w = 0;
  for (var k = 0; k < j; k++) {
    w += cells[k].width;
  }
  return w;
}

/// 행 하나의 접기 정보 — 문단 되잇기 판정용. 행 객체마다 한 번만 잰다.
class _Info {
  _Info(
    this.width,
    this.lead,
    this.indent,
    this.prose,
    this.words,
    this.firstWord,
  );

  /// 뒤 빈칸을 뺀 폭.
  final int width;
  final int lead;
  final int indent;

  /// 글줄인가 — 빈 행·선 그리기(테두리)·상자 옆선은 아니다.
  final bool prose;

  /// 낱말 수 — 빈칸 없는 긴 토큰(주소·해시) 하나는 접어 둔 문단이 아니다.
  final int words;

  /// 들여쓰기 뒤 첫 낱말의 폭 — 앞줄에 이게 들어갈 자리가 있었다면 거기서 접었을
  /// 리가 없다.
  final int firstWord;

  bool get marked => indent > lead;
}

_Info _infoOf(List<Run> runs) {
  final cells = _cellsOf(runs);
  var end = cells.length;
  while (end > 0 && cells[end - 1].blank) {
    end--;
  }
  final trimmed = end == cells.length ? cells : cells.sublist(0, end);
  var width = 0;
  for (final c in trimmed) {
    width += c.width;
  }
  var i = 0;
  while (i < trimmed.length && trimmed[i].rune == 0x20) {
    i++;
  }
  final prose =
      i < trimmed.length &&
      !(trimmed[i].rune >= 0x2500 && trimmed[i].rune <= 0x259f);
  var words = 0;
  var inWord = false;
  var firstWord = 0;
  for (var k = i; k < trimmed.length; k++) {
    final space = trimmed[k].rune == 0x20;
    if (!space && !inWord) words++;
    if (!space && words == 1) firstWord += trimmed[k].width;
    inWord = !space;
  }
  return _Info(width, i, _hangingIndent(trimmed), prose, words, firstWord);
}

/// 데스크톱이 제 폭에서 낱말 끝으로 접어 둔 줄은 이만큼까지 짧을 수 있다.
const _joinSlack = 12;

/// `b` 가 `a` 의 이어지는 줄인가 — `a` 가 pane 폭을 거의 채우고, `b` 가 `a` 의
/// 들여쓰기 자리에서 글머리 없이 시작하면 데스크톱(Ink)이 한 문단을 접어 둔 것이다.
bool _continues(_Info a, _Info b, int gridCols) =>
    a.prose &&
    b.prose &&
    a.words > 1 &&
    a.width >= gridCols - _joinSlack &&
    a.width <= gridCols &&
    // 뒷줄 첫 낱말이 앞줄 남은 자리에 들어갔다면 거기서 접혔을 리가 없다.
    a.width + 1 + b.firstWord > gridCols &&
    b.lead == a.indent &&
    !b.marked;

/// 접어 둔 줄들을 한 줄로 되잇는다 — 앞줄 뒤 빈칸과 뒷줄 들여쓰기를 빼고 빈칸 하나로.
/// `offsets[i]` 는 i 번째 줄의 글이 되이은 줄의 몇 번째 칸에서 시작하는지.
(List<_Cell>, List<int>) _joinRows(
  List<List<Run>> rows,
  List<_Info> infos,
  int gridCols,
) {
  final out = <_Cell>[];
  final offsets = <int>[];
  for (var i = 0; i < rows.length; i++) {
    final cells = _cellsOf(rows[i]);
    var end = cells.length;
    while (end > 0 && cells[end - 1].blank) {
      end--;
    }
    var from = 0;
    if (i > 0) {
      from = infos[i].lead;
      // 앞줄이 마지막 칸까지 찼고 그 자리가 경로·식별자 한가운데면 글자 단위로 잘린
      // 것이다(`recall.p` + `y`) — 빈칸을 끼우면 없던 낱말이 생긴다.
      final glued =
          infos[i - 1].width == gridCols && _tokenBreak(out, cells, from, end);
      if (!glued) out.add(_Cell(0x20, _blank));
    }
    var w = 0;
    for (final c in out) {
      w += c.width;
    }
    offsets.add(w);
    out.addAll(cells.sublist(from, end));
  }
  return (out, offsets);
}

/// 앞줄 꼬리와 뒷줄 머리가 한 토큰(경로·식별자·주소)의 두 동강인가 — 양쪽 다 ASCII
/// 토큰 글자뿐이고 붙인 것에 `_ . / : -` 가 든다. 「word」+「continued」 같은 낱말
/// 둘은 여기 안 걸려 빈칸으로 잇는다.
bool _tokenBreak(List<_Cell> prev, List<_Cell> next, int from, int end) {
  bool tokenChar(int r) =>
      (r >= 0x30 && r <= 0x39) ||
      (r >= 0x41 && r <= 0x5a) ||
      (r >= 0x61 && r <= 0x7a) ||
      r == 0x5f ||
      r == 0x2e ||
      r == 0x2f ||
      r == 0x3a ||
      r == 0x2d ||
      r == 0x40 ||
      r == 0x7e;
  bool joiner(int r) =>
      r == 0x5f || r == 0x2e || r == 0x2f || r == 0x3a || r == 0x2d;
  var i = prev.length;
  while (i > 0 && prev[i - 1].rune != 0x20) {
    if (!tokenChar(prev[i - 1].rune)) return false;
    i--;
  }
  var j = from;
  while (j < end && next[j].rune != 0x20) {
    if (!tokenChar(next[j].rune)) return false;
    j++;
  }
  if (i == prev.length || j == from) return false;
  for (var k = i; k < prev.length; k++) {
    if (joiner(prev[k].rune)) return true;
  }
  for (var k = from; k < j; k++) {
    if (joiner(next[k].rune)) return true;
  }
  return false;
}

/// 낱말을 지키려고 되돌아가는 최대 칸 수 — 이보다 긴 낱말은 그냥 자른다.
const _wordBackoff = 12;

bool _vertical(int rune) => rune == 0x2502 || rune == 0x2503 || rune == 0x2551;

/// `_shrinkLines` 의 반대 — 선 채움이 있으면 가장 긴 채움을 `deficit` 만큼 늘이고,
/// 양끝이 세로선(│)인 상자 줄이면 오른쪽 세로선 앞 빈칸을 늘인다. 글줄이면 null.
List<_Cell>? _stretch(List<_Cell> cells, int deficit) {
  var bestAt = -1;
  var bestLen = 0;
  var i = 0;
  while (i < cells.length) {
    var j = i + 1;
    if (cells[i].rune != 0x20 && cells[i].filler) {
      while (j < cells.length && cells[j].sameFill(cells[i])) {
        j++;
      }
    }
    if (j - i >= 2 && j - i > bestLen) {
      bestAt = i;
      bestLen = j - i;
    }
    i = j;
  }
  if (bestAt >= 0) {
    return [
      ...cells.sublist(0, bestAt),
      for (var k = 0; k < deficit; k++) cells[bestAt],
      ...cells.sublist(bestAt),
    ];
  }
  if (cells.length >= 2 &&
      _vertical(cells.first.rune) &&
      _vertical(cells.last.rune)) {
    // 안쪽 빈칸의 모양(배경)을 따른다 — 입력상자 바탕색이 세로선까지 이어지게.
    final inner = cells[cells.length - 2];
    final blank = inner.rune == 0x20 ? inner : _Cell(0x20, _blank);
    return [
      ...cells.sublist(0, cells.length - 1),
      for (var k = 0; k < deficit; k++) blank,
      cells.last,
    ];
  }
  return null;
}

/// 선 그리기 채움(─ ━ ═ …)이 두 칸 이상 이어진 자리를 줄여 `excess` 칸을 덜어 낸다.
/// 덜어 낼 자리가 모자라면 null — 글줄이라는 뜻이니 접는 쪽으로 간다.
List<_Cell>? _shrinkLines(List<_Cell> cells, int excess) {
  final runs = <List<int>>[];
  var i = 0;
  while (i < cells.length) {
    var j = i + 1;
    if (cells[i].rune != 0x20 && cells[i].filler) {
      while (j < cells.length && cells[j].sameFill(cells[i])) {
        j++;
      }
    }
    if (j - i >= 2) runs.add([i, j - i]);
    i = j;
  }
  var room = 0;
  for (final r in runs) {
    room += r[1] - 1;
  }
  if (runs.isEmpty || room < excess) return null;
  var left = excess;
  while (left > 0) {
    runs.sort((a, b) => b[1].compareTo(a[1]));
    runs.first[1]--;
    left--;
  }
  final keep = <int, int>{for (final r in runs) r[0]: r[1]};
  final out = <_Cell>[];
  i = 0;
  while (i < cells.length) {
    final n = keep[i];
    if (n == null) {
      out.add(cells[i++]);
      continue;
    }
    var j = i + 1;
    while (j < cells.length && cells[j].sameFill(cells[i])) {
      j++;
    }
    out.addAll(cells.sublist(i, i + n));
    i = j;
  }
  return out;
}

/// 같은 모양(색·속성)의 이웃 칸을 한 런으로 되묶는다.
List<Run> _runs(List<_Cell> cells) {
  final out = <Run>[];
  Run? style;
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
        s.fg != c.run.fg ||
        s.bg != c.run.bg ||
        s.flags != c.run.flags) {
      flush();
      style = c.run;
    }
    buf.writeCharCode(c.rune);
  }
  flush();
  return out;
}

class _Entry {
  _Entry(this.cols, this.row, this.members, this.offsets);
  final int cols;
  final RowReflow row;

  /// 이 결과에 든 원본 행들(되이은 문단이면 여럿) — 하나라도 바뀌면 다시 접는다.
  final List<List<Run>> members;

  /// 각 원본 행의 글이 되이은 줄의 몇 번째 칸에서 시작하나(첫 행은 0).
  final List<int> offsets;

  bool matches(List<List<Run>> lines, int r, int n) {
    if (members.length != n) return false;
    for (var i = 0; i < n; i++) {
      if (!identical(members[i], lines[r + i])) return false;
    }
    return true;
  }
}

/// 지난 줄(스크롤백) 위에 살아 있는 화면을 이어 붙인 한 장 — 접는 쪽은 이걸 하나의
/// 화면으로 본다. 커서는 살아 있는 화면 안에 있으므로 지난 줄 수만큼 내려간다.
class CombinedGrid implements GridLines {
  CombinedGrid(this.history, this.live, {this.historySlots = const []})
    : lines = history.isEmpty ? live.lines : [...history, ...live.lines];

  final List<List<Run>> history;
  final GridLines live;

  /// 지난 줄 안의 도트 자리(지난 줄 기준 행) — 시작 배너가 넘어간 자리.
  final List<SpriteSlot> historySlots;

  @override
  final List<List<Run>> lines;
  @override
  int get cols => live.cols;
  @override
  int get rows => lines.length;
  @override
  int get cursorRow => history.length + live.cursorRow;
  @override
  int get cursorCol => live.cursorCol;
  @override
  bool get cursorVisible => live.cursorVisible;
  @override
  List<SpriteSlot> get slots => history.isEmpty
      ? live.slots
      : [
          ...historySlots,
          for (final s in live.slots) s.shifted(history.length.toDouble(), 0),
        ];
}

/// 행별 결과를 **행 객체에** 매달아 두고(Expando) 그 객체가 바뀐 때만 다시 접는다 —
/// 서버는 바뀐 행만 새 List 로 보내므로 객체가 같으면 내용도 같다. 행 번호로 캐시하면
/// 지난 줄이 한 줄 늘 때마다 전부 밀려 통째로 다시 접게 된다. 조각 List 도 그대로
/// 재사용되어 렌더러의 행 캐시가 같은 규칙으로 살아남는다.
class Reflow {
  final _rows = Expando<_Entry>();
  final _infos = Expando<_Info>();

  _Info _info(List<Run> row) => _infos[row] ??= _infoOf(row);

  ReflowedGrid apply(GridLines grid, int cols) {
    final src = grid.lines;
    final lines = <List<Run>>[];
    int? cursorRow;
    var cursorCol = 0;
    final lineStart = <int>[];
    final starts = <List<int>>[];
    // 원본 행의 열 → 접힌 줄의 열: 되이은 문단이면 앞 행들 뒤로 밀리고(offset) 그 행의
    // 들여쓰기(lead)는 빠진다. 둘째 조각부터는 indent 만큼 앞이 채워진다.
    final shift = <int>[];
    final indents = <int>[];
    // 폭이 같으면 데스크톱과 같은 줄 나눔이 정답이다. 다르면 — 넓든 좁든 — 데스크톱이
    // 제 폭에서 접어 둔 문단을 되이어 폰 폭으로 다시 접는다(2026-09-07 지시: 맥미니의
    // 좁은 pane 도 「폰에 했던 것처럼」 기기 폭에 맞게).
    final rewrap = grid.cols != cols;
    var r = 0;
    while (r < src.length) {
      var n = 1;
      if (rewrap) {
        var last = _info(src[r]);
        while (r + n < src.length) {
          final next = _info(src[r + n]);
          if (!_continues(last, next, grid.cols)) break;
          last = next;
          n++;
        }
      }
      var e = _rows[src[r]];
      if (e == null || e.cols != cols || !e.matches(src, r, n)) {
        final members = src.sublist(r, r + n);
        if (n == 1) {
          e = _Entry(
            cols,
            reflowRow(src[r], cols, paneCols: grid.cols),
            members,
            const [0],
          );
        } else {
          final (cells, offsets) = _joinRows(members, [
            for (final m in members) _info(m),
          ], grid.cols);
          e = _Entry(cols, _reflowCells(cells, cols), members, offsets);
        }
        _rows[src[r]] = e;
      }
      for (var i = 0; i < n; i++) {
        lineStart.add(lines.length);
        starts.add(e.row.starts);
        shift.add(i == 0 ? 0 : e.offsets[i] - _info(src[r + i]).lead);
        indents.add(e.row.indent);
        if (r + i == grid.cursorRow) {
          final st = e.row.starts;
          final col = grid.cursorCol + shift.last;
          var k = 0;
          for (var q = 0; q < st.length; q++) {
            if (st[q] <= col) k = q;
          }
          cursorRow = lines.length + k;
          cursorCol = (col - st[k] + (k > 0 ? e.row.indent : 0)).clamp(
            0,
            cols - 1,
          );
        }
      }
      lines.addAll(e.row.chunks);
      r += n;
    }
    var end = lines.length;
    while (end > 0 && lines[end - 1].isEmpty && (cursorRow ?? -1) < end - 1) {
      end--;
    }
    // 스프라이트 자리도 같은 자로 옮긴다 — 앵커 열이 든 조각의 줄로 가고, 열은 그
    // 조각 시작만큼 뺀다. 도트가 앉는 행은 접히지 않는 짧은 행이라 폭은 그대로다.
    final slots = <SpriteSlot>[
      for (final s in grid.slots)
        if (s.row.floor() < lineStart.length)
          _mapSlot(s, lineStart, starts, shift, indents),
    ];
    return ReflowedGrid(
      cols: cols,
      lines: end == lines.length ? lines : lines.sublist(0, end),
      cursorRow: cursorRow ?? 0,
      cursorCol: cursorCol,
      cursorVisible: grid.cursorVisible,
      slots: slots,
    );
  }
}

SpriteSlot _mapSlot(
  SpriteSlot s,
  List<int> lineStart,
  List<List<int>> starts,
  List<int> shift,
  List<int> indents,
) {
  final r = s.row.floor();
  // 머리가 화면 위로 밀린 배너는 행이 음수다 — 첫 줄 위로 그만큼 삐져나가게 둔다.
  if (r < 0) {
    return SpriteSlot(s.motion, lineStart[0] + s.row, s.col, s.rows, s.cols);
  }
  final frac = s.row - r;
  final st = starts[r];
  final col = s.col + shift[r];
  var k = 0;
  for (var i = 0; i < st.length; i++) {
    if (st[i] <= col) k = i;
  }
  return SpriteSlot(
    s.motion,
    lineStart[r] + k + frac,
    col - st[k] + (k > 0 ? indents[r] : 0),
    s.rows,
    s.cols,
  );
}
