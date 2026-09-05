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
  const RowReflow(this.chunks, this.starts);
  final List<List<Run>> chunks;
  final List<int> starts;
}

RowReflow reflowRow(List<Run> runs, int cols) {
  final cells = <_Cell>[
    for (final r in runs)
      for (final rune in r.text.runes) _Cell(rune, r),
  ];
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
  if (width <= cols) return RowReflow([_runs(trimmed)], const [0]);

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

  final chunks = <List<Run>>[];
  final starts = <int>[];
  var line = <_Cell>[];
  var lineWidth = 0;
  var col = 0;
  for (final c in trimmed) {
    if (lineWidth + c.width > cols && line.isNotEmpty) {
      chunks.add(_runs(line));
      line = [];
      lineWidth = 0;
    }
    if (line.isEmpty) starts.add(col);
    line.add(c);
    lineWidth += c.width;
    col += c.width;
  }
  if (line.isNotEmpty) chunks.add(_runs(line));
  return RowReflow(chunks, starts);
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
  _Entry(this.cols, this.row);
  final int cols;
  final RowReflow row;
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

  ReflowedGrid apply(GridLines grid, int cols) {
    final lines = <List<Run>>[];
    int? cursorRow;
    var cursorCol = 0;
    final lineStart = <int>[];
    final starts = <List<int>>[];
    for (var r = 0; r < grid.lines.length; r++) {
      final src = grid.lines[r];
      var e = _rows[src];
      if (e == null || e.cols != cols) {
        e = _Entry(cols, reflowRow(src, cols));
        _rows[src] = e;
      }
      lineStart.add(lines.length);
      starts.add(e.row.starts);
      if (r == grid.cursorRow) {
        final starts = e.row.starts;
        var k = 0;
        for (var i = 0; i < starts.length; i++) {
          if (starts[i] <= grid.cursorCol) k = i;
        }
        cursorRow = lines.length + k;
        cursorCol = (grid.cursorCol - starts[k]).clamp(0, cols - 1);
      }
      lines.addAll(e.row.chunks);
    }
    var end = lines.length;
    while (end > 0 && lines[end - 1].isEmpty && (cursorRow ?? -1) < end - 1) {
      end--;
    }
    // 스프라이트 자리도 같은 자로 옮긴다 — 앵커 열이 든 조각의 줄로 가고, 열은 그
    // 조각 시작만큼 뺀다. 도트가 앉는 행은 접히지 않는 짧은 행이라 폭은 그대로다.
    final slots = <SpriteSlot>[
      for (final s in grid.slots)
        if (s.row.floor() < lineStart.length) _mapSlot(s, lineStart, starts),
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

SpriteSlot _mapSlot(SpriteSlot s, List<int> lineStart, List<List<int>> starts) {
  final r = s.row.floor();
  // 머리가 화면 위로 밀린 배너는 행이 음수다 — 첫 줄 위로 그만큼 삐져나가게 둔다.
  if (r < 0) {
    return SpriteSlot(s.motion, lineStart[0] + s.row, s.col, s.rows, s.cols);
  }
  final frac = s.row - r;
  final st = starts[r];
  var k = 0;
  for (var i = 0; i < st.length; i++) {
    if (st[i] <= s.col) k = i;
  }
  return SpriteSlot(
    s.motion,
    lineStart[r] + k + frac,
    s.col - st[k],
    s.rows,
    s.cols,
  );
}
