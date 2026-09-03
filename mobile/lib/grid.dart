/// 서버(`gridwire.rs`)가 보내는 격자 프레임을 그대로 담는 순수 Dart 모델.
/// Flutter 를 import 하지 않아 `dart test` 급으로 빨리 검증한다.
library;

const flagBold = 1;
const flagItalic = 2;
const flagUnderline = 4;
const flagInverse = 8;
const flagDim = 16;

/// 색은 파싱 때 굳히지 않는다 — 테마(다크/라이트)는 프레임 없이도 바뀌므로 그릴 때 푼다.
sealed class CellColor {
  const CellColor();

  static CellColor parse(Object? v) {
    if (v is int) return IndexColor(v);
    if (v is List && v.length == 3) {
      return RgbColor(v[0] as int, v[1] as int, v[2] as int);
    }
    return const DefaultColor();
  }
}

class DefaultColor extends CellColor {
  const DefaultColor();
}

class IndexColor extends CellColor {
  const IndexColor(this.index);
  final int index;
}

class RgbColor extends CellColor {
  const RgbColor(this.r, this.g, this.b);
  final int r, g, b;
}

class Run {
  const Run(this.text, this.fg, this.bg, this.flags);
  final String text;
  final CellColor fg;
  final CellColor bg;
  final int flags;

  static Run parse(List<Object?> raw) => Run(
        raw[0] as String,
        CellColor.parse(raw[1]),
        CellColor.parse(raw[2]),
        (raw[3] as num?)?.toInt() ?? 0,
      );

  /// 이 런이 차지하는 칸 수 — 서버는 wide 글자의 스페이서 칸을 빼고 보낸다.
  int get cells {
    var n = 0;
    for (final r in text.runes) {
      n += cellWidth(r);
    }
    return n;
  }
}

class Grid {
  int cols = 0;
  int rows = 0;
  List<List<Run>> lines = const [];
  int cursorRow = 0;
  int cursorCol = 0;
  bool cursorVisible = true;
  bool appCursor = false;
  bool bracketedPaste = false;

  /// 프레임마다 오른다 — 위젯이 「다시 그릴 것이 있나」를 이 숫자로 안다.
  int version = 0;

  /// `{"t":"grid", …}` 한 프레임을 반영한다. 크기가 바뀌면 서버가 전 행을 다시
  /// 보내므로 행을 비우고 시작해도 빠지는 줄이 없다. 바뀐 행은 새 List 객체로
  /// 갈아 끼운다 — 렌더러의 행 캐시가 `identical` 로 무효화를 알아본다.
  void apply(Map<String, Object?> m) {
    final c = (m['cols'] as num?)?.toInt() ?? cols;
    final r = (m['rows'] as num?)?.toInt() ?? rows;
    if (c != cols || r != rows || lines.length != r) {
      cols = c;
      rows = r;
      lines = List<List<Run>>.filled(r, const []);
    }
    final dirty = m['dirty'];
    if (dirty is List) {
      for (final entry in dirty) {
        if (entry is! List || entry.length < 2) continue;
        final row = (entry[0] as num).toInt();
        if (row < 0 || row >= rows) continue;
        final runs = entry[1];
        if (runs is! List) continue;
        lines[row] = [
          for (final raw in runs)
            if (raw is List && raw.length >= 4) Run.parse(raw),
        ];
      }
    }
    final cursor = m['cursor'];
    if (cursor is List && cursor.length == 2) {
      cursorRow = (cursor[0] as num).toInt();
      cursorCol = (cursor[1] as num).toInt();
    }
    cursorVisible = m['cursorVisible'] as bool? ?? cursorVisible;
    appCursor = m['appCursor'] as bool? ?? appCursor;
    bracketedPaste = m['bracketedPaste'] as bool? ?? bracketedPaste;
    version++;
  }

  /// 한 행의 글자를 이어 붙인다 — 테스트와 접근성 낭독용.
  String rowText(int row) =>
      row < lines.length ? lines[row].map((r) => r.text).join() : '';
}

/// 서버의 `unicode_width` 와 같은 답을 내야 한다. 표가 어긋나면 그 행의 뒤 열이
/// 통째로 밀린다.
int cellWidth(int rune) {
  if (rune == 0) return 0;
  if (rune < 0x20 || (rune >= 0x7f && rune < 0xa0)) return 0;
  if (_isZeroWidth(rune)) return 0;
  if (_isWide(rune)) return 2;
  return 1;
}

bool _isZeroWidth(int r) =>
    (r >= 0x0300 && r <= 0x036f) ||
    (r >= 0x0483 && r <= 0x0489) ||
    (r >= 0x0591 && r <= 0x05bd) ||
    (r >= 0x0610 && r <= 0x061a) ||
    (r >= 0x064b && r <= 0x065f) ||
    (r >= 0x1ab0 && r <= 0x1aff) ||
    (r >= 0x1dc0 && r <= 0x1dff) ||
    r == 0x200b ||
    r == 0x200c ||
    r == 0x200d ||
    (r >= 0x20d0 && r <= 0x20ff) ||
    (r >= 0xfe00 && r <= 0xfe0f) ||
    (r >= 0xfe20 && r <= 0xfe2f) ||
    r == 0xfeff ||
    (r >= 0xe0100 && r <= 0xe01ef);

bool _isWide(int r) =>
    (r >= 0x1100 && r <= 0x115f) ||
    (r >= 0x231a && r <= 0x231b) ||
    (r >= 0x2329 && r <= 0x232a) ||
    (r >= 0x23e9 && r <= 0x23ec) ||
    r == 0x23f0 ||
    r == 0x23f3 ||
    (r >= 0x25fd && r <= 0x25fe) ||
    (r >= 0x2614 && r <= 0x2615) ||
    (r >= 0x2648 && r <= 0x2653) ||
    r == 0x267f ||
    r == 0x2693 ||
    r == 0x26a1 ||
    (r >= 0x26aa && r <= 0x26ab) ||
    (r >= 0x26bd && r <= 0x26be) ||
    (r >= 0x26c4 && r <= 0x26c5) ||
    r == 0x26ce ||
    r == 0x26d4 ||
    r == 0x26ea ||
    (r >= 0x26f2 && r <= 0x26f3) ||
    r == 0x26f5 ||
    r == 0x26fa ||
    r == 0x26fd ||
    r == 0x2705 ||
    (r >= 0x270a && r <= 0x270b) ||
    r == 0x2728 ||
    r == 0x274c ||
    r == 0x274e ||
    (r >= 0x2753 && r <= 0x2755) ||
    r == 0x2757 ||
    (r >= 0x2795 && r <= 0x2797) ||
    r == 0x27b0 ||
    r == 0x27bf ||
    (r >= 0x2b1b && r <= 0x2b1c) ||
    r == 0x2b50 ||
    r == 0x2b55 ||
    (r >= 0x2e80 && r <= 0x303e) ||
    (r >= 0x3041 && r <= 0x33ff) ||
    (r >= 0x3400 && r <= 0x4dbf) ||
    (r >= 0x4e00 && r <= 0x9fff) ||
    (r >= 0xa000 && r <= 0xa4cf) ||
    (r >= 0xa960 && r <= 0xa97f) ||
    (r >= 0xac00 && r <= 0xd7a3) ||
    (r >= 0xf900 && r <= 0xfaff) ||
    (r >= 0xfe10 && r <= 0xfe19) ||
    (r >= 0xfe30 && r <= 0xfe6f) ||
    (r >= 0xff00 && r <= 0xff60) ||
    (r >= 0xffe0 && r <= 0xffe6) ||
    (r >= 0x16fe0 && r <= 0x16fe4) ||
    (r >= 0x17000 && r <= 0x18aff) ||
    (r >= 0x1b000 && r <= 0x1b2ff) ||
    (r >= 0x1f004 && r <= 0x1f004) ||
    r == 0x1f0cf ||
    r == 0x1f18e ||
    (r >= 0x1f191 && r <= 0x1f19a) ||
    (r >= 0x1f200 && r <= 0x1f251) ||
    (r >= 0x1f300 && r <= 0x1f320) ||
    (r >= 0x1f32d && r <= 0x1f335) ||
    (r >= 0x1f337 && r <= 0x1f37c) ||
    (r >= 0x1f37e && r <= 0x1f393) ||
    (r >= 0x1f3a0 && r <= 0x1f3ca) ||
    (r >= 0x1f3cf && r <= 0x1f3d3) ||
    (r >= 0x1f3e0 && r <= 0x1f3f0) ||
    r == 0x1f3f4 ||
    (r >= 0x1f3f8 && r <= 0x1f43e) ||
    r == 0x1f440 ||
    (r >= 0x1f442 && r <= 0x1f4fc) ||
    (r >= 0x1f4ff && r <= 0x1f53d) ||
    (r >= 0x1f54b && r <= 0x1f54e) ||
    (r >= 0x1f550 && r <= 0x1f567) ||
    r == 0x1f57a ||
    (r >= 0x1f595 && r <= 0x1f596) ||
    r == 0x1f5a4 ||
    (r >= 0x1f5fb && r <= 0x1f64f) ||
    (r >= 0x1f680 && r <= 0x1f6c5) ||
    r == 0x1f6cc ||
    (r >= 0x1f6d0 && r <= 0x1f6d2) ||
    (r >= 0x1f6d5 && r <= 0x1f6d7) ||
    (r >= 0x1f6dc && r <= 0x1f6df) ||
    (r >= 0x1f6eb && r <= 0x1f6ec) ||
    (r >= 0x1f6f4 && r <= 0x1f6fc) ||
    (r >= 0x1f7e0 && r <= 0x1f7eb) ||
    r == 0x1f7f0 ||
    (r >= 0x1f90c && r <= 0x1f93a) ||
    (r >= 0x1f93c && r <= 0x1f945) ||
    (r >= 0x1f947 && r <= 0x1f9ff) ||
    (r >= 0x1fa70 && r <= 0x1faff) ||
    (r >= 0x20000 && r <= 0x2fffd) ||
    (r >= 0x30000 && r <= 0x3fffd);

/// xterm 기본 16색 — 웹 격자(`grid.js`)와 같은 자리. 테마의 fg/bg 는 null 로 오므로
/// 여기 없다. 라이트 판은 같은 색상을 흰 바탕에서 읽히게 어둡혀 둔 것.
const base16Dark = <int>[
  0xff12161c, 0xfff7768e, 0xff9ece6a, 0xffe0af68, 0xff7aa2f7, 0xffbb9af7, 0xff7dcfff, 0xffa9b1d6,
  0xff414868, 0xffff7a93, 0xffb9f27c, 0xffff9e64, 0xff7da6ff, 0xffc0a3ff, 0xff0db9d7, 0xffc0caf5,
];

const base16Light = <int>[
  0xff15294a, 0xffc4304f, 0xff3f8a2a, 0xffa26a12, 0xff2f63c4, 0xff7a4fd1, 0xff1183b0, 0xff5b6b8a,
  0xff8a97b3, 0xffd8385a, 0xff4d9d34, 0xffc27a1f, 0xff3b72d6, 0xff8d67e3, 0xff0d97b3, 0xff2c3e5f,
];

/// 256 팔레트를 ARGB 정수로. 0–15 는 테마별 표, 그 뒤는 xterm 의 계산식이다.
int palette256(int n, {required bool dark}) {
  if (n < 0) return dark ? base16Dark[7] : base16Light[7];
  if (n < 16) return dark ? base16Dark[n] : base16Light[n];
  if (n < 232) {
    final i = n - 16;
    final r = i ~/ 36, g = (i % 36) ~/ 6, b = i % 6;
    int v(int c) => c == 0 ? 0 : 55 + c * 40;
    return 0xff000000 | (v(r) << 16) | (v(g) << 8) | v(b);
  }
  final v = (8 + (n.clamp(232, 255) - 232) * 10).clamp(0, 255);
  return 0xff000000 | (v << 16) | (v << 8) | v;
}
