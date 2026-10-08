/// 셸 칸의 「명령 + 결과」 묶음 — 데스크톱 `/term/blocks` 답을 쌓는 순수 Dart 모델.
/// PC 거울(`app/kasaterm/src/shell_view.rs`)과 같은 합치기 규칙이다. 정본은
/// `docs/mirror-render.md`.
library;

import 'grid.dart';

/// 와이어 색 수 → 격자와 같은 색. 0~255 는 팔레트 번호, `0x1000000 | rgb` 는 트루컬러.
CellColor wireColor(Object? v) {
  if (v is! int) return const DefaultColor();
  if (v < 256) return IndexColor(v);
  return RgbColor((v >> 16) & 0xff, (v >> 8) & 0xff, v & 0xff);
}

List<Run> _line(Object? raw) => raw is List
    ? [
        for (final s in raw)
          if (s is Map)
            Run(
              s['t'] is String ? s['t'] as String : '',
              wireColor(s['f']),
              wireColor(s['b']),
              (s['s'] as num?)?.toInt() ?? 0,
            ),
      ]
    : const [];

/// 결과 속 폴더 고리 — [line] 번째 줄의 [start]부터 [length] 글자가 [path] 폴더다.
/// 원본 기계가 실제로 있는 폴더만 준다(`kasa-mcp shell_blocks.rs dir_links`).
class ShellLink {
  const ShellLink(this.line, this.start, this.length, this.path);
  final int line;
  final int start;
  final int length;
  final String path;
}

/// 셸에 그대로 칠 수 있게 작은따옴표로 감싼다.
String shellQuote(String path) => "'${path.replaceAll("'", r"'\''")}'";

class ShellBlock {
  ShellBlock({
    required this.id,
    required this.cmd,
    required this.startMs,
    required this.running,
    required this.lines,
    this.exit,
    this.ms,
    this.tui = false,
    this.dropped = 0,
    this.gapAt,
    this.gap = 0,
    this.links = const [],
  });

  final int id;
  final String cmd;
  final int? exit;
  final int? ms;
  final int startMs;
  final bool running;

  /// 전체 화면 프로그램(vim 등)이었다 — 그 화면은 원본 칸에만 있다.
  final bool tui;

  /// 너무 길어 원본이 앞에서 버린 줄 수.
  final int dropped;

  /// 가운데가 빠졌을 수 있다 — [gapAt] 자리에 [gap] 줄.
  final List<List<Run>> lines;
  final int? gapAt;
  final int gap;
  final List<ShellLink> links;

  static ShellBlock? parse(Object? raw) {
    if (raw is! Map || raw['id'] is! int) return null;
    int? n(String k) => (raw[k] as num?)?.toInt();
    return ShellBlock(
      id: raw['id'] as int,
      cmd: raw['cmd'] is String ? raw['cmd'] as String : '',
      exit: n('exit'),
      ms: n('ms'),
      startMs: n('start_ms') ?? 0,
      running: raw['running'] == true,
      tui: raw['tui'] == true,
      dropped: n('dropped') ?? 0,
      lines: raw['lines'] is List
          ? [for (final l in raw['lines'] as List) _line(l)]
          : const [],
      gapAt: n('gap_at'),
      gap: n('gap') ?? 0,
      links: [
        if (raw['links'] is List)
          for (final l in raw['links'] as List)
            if (l is List &&
                l.length >= 4 &&
                l[0] is int &&
                l[1] is int &&
                l[2] is int &&
                l[3] is String)
              ShellLink(l[0] as int, l[1] as int, l[2] as int, l[3] as String),
      ],
    );
  }

  /// 결과 줄을 평문으로 — 복사용.
  String get plain => lines.map((l) => l.map((r) => r.text).join()).join('\n');
}

class ShellFeed {
  int? since;
  bool loaded = false;
  bool integration = false;
  bool alt = false;
  final List<ShellBlock> blocks = [];

  /// 바뀔 때마다 오른다 — 화면이 이것만 보고 다시 그린다.
  int version = 0;

  bool get running => blocks.any((b) => b.running);

  /// 다 끝난 블록 중 가장 큰 id — 그 아래 끝난 블록은 다시 안 받는다.
  int get have => blocks
      .where((b) => !b.running)
      .fold(0, (m, b) => b.id > m ? b.id : m);

  /// 원본 답 하나를 합친다. 원본 셸이 새로 떠 번호가 처음부터 다시 매겨졌으면 버린다.
  void merge(Map<String, Object?> answer) {
    since = (answer['since'] as num?)?.toInt();
    integration = answer['integration'] == true;
    alt = answer['alt'] == true;
    final oldest = (answer['oldest'] as num?)?.toInt() ?? 0;
    final incoming = ((answer['blocks'] as List?) ?? const [])
        .map(ShellBlock.parse)
        .whereType<ShellBlock>()
        .toList();
    // 원본이 가진 가장 새 번호가 보는 쪽 것보다 작으면 원본 셸이 새로 떠 번호가 처음부터다.
    final newest = (answer['newest'] as num?)?.toInt() ?? 0;
    if (newest < have) blocks.clear();
    blocks.removeWhere((b) => b.id < oldest);
    for (final b in incoming) {
      final at = blocks.indexWhere((x) => x.id == b.id);
      if (at >= 0) {
        blocks[at] = b;
      } else {
        blocks.add(b);
      }
    }
    blocks.sort((a, b) => a.id.compareTo(b.id));
    loaded = true;
    version++;
  }

  /// 펼치기로 통째 받은 블록을 갈아 끼운다.
  void replace(ShellBlock whole) {
    final at = blocks.indexWhere((b) => b.id == whole.id);
    if (at < 0) return;
    blocks[at] = whole;
    version++;
  }
}

/// 사람이 읽는 걸린 시간. 눈 깜짝할 새(10ms 미만)는 빈 글이다.
String tookLabel(int ms) {
  if (ms < 10) return '';
  if (ms < 1000) return '${(ms / 1000).toStringAsFixed(2)}초';
  if (ms < 10000) return '${(ms / 1000).toStringAsFixed(1)}초';
  final s = ms ~/ 1000;
  if (s < 60) return '$s초';
  if (s < 3600) return '${s ~/ 60}분 ${s % 60}초';
  return '${s ~/ 3600}시간 ${s % 3600 ~/ 60}분';
}
