import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:url_launcher/url_launcher.dart';

import 'grid.dart';

/// 격자 글자에서 URL 을 찾는다 — 데스크톱 `links.rs` 와 같은 규칙. 한 칸 == 한 글자라
/// 열 번호가 곧 스캔 위치다(두 칸 글자 뒤에는 빈 칸을 하나 둔다). 폭을 꽉 채운 줄의
/// 끝과 다음 줄 머리가 모두 URL 글자면 접힌 한 주소로 잇는다.
class LinkHit {
  const LinkHit(this.url, this.segments);

  final String url;

  /// (행, 시작 열, 끝 열(exclusive)) — 접힌 주소는 여러 행에 걸친다.
  final List<(int, int, int)> segments;

  bool covers(int row, int col) => segments.any(
    (s) => s.$1 == row && col >= s.$2 && col < s.$3,
  );
}

bool _isUrlChar(int r) {
  if (r <= 0x20 || r == 0x7f) return false;
  return switch (String.fromCharCode(r)) {
    '"' || "'" || '`' || '<' || '>' || '{' || '}' || '|' || '\\' || '^' => false,
    _ => true,
  };
}

int _trimTrailing(List<int> chars, int start, int end) {
  while (end > start) {
    final c = String.fromCharCode(chars[end - 1]);
    final body = chars.sublist(start, end);
    final strip = switch (c) {
      '.' || ',' || ';' || ':' || '!' || '?' => true,
      ')' => !body.contains('('.codeUnitAt(0)),
      ']' => !body.contains('['.codeUnitAt(0)),
      '}' => !body.contains('{'.codeUnitAt(0)),
      _ => false,
    };
    if (!strip) break;
    end--;
  }
  return end;
}

/// 한 행을 칸 단위 글자 배열로 — 두 칸 글자 뒤엔 빈 칸(스페이서).
List<int> _cellsOf(List<Run> runs) {
  final out = <int>[];
  for (final run in runs) {
    for (final r in run.text.runes) {
      out.add(r);
      if (cellWidth(r) == 2) out.add(0x20);
    }
  }
  return out;
}

bool _joins(List<int> prev, List<int> next, int cols) {
  if (prev.isEmpty || next.isEmpty || cols <= 0) return false;
  // 폭을 꽉 채운 줄만 접힌 것이다 — 짧은 줄 끝의 주소는 거기서 끝난다.
  if (prev.length < cols) return false;
  return _isUrlChar(prev.last) && _isUrlChar(next.first);
}

/// `lines[from..to)` 에서 URL 을 찾는다. 행 번호는 `lines` 기준 그대로.
List<LinkHit> detectLinks(
  List<List<Run>> lines,
  int cols, {
  int from = 0,
  int? to,
}) {
  final end = to ?? lines.length;
  final chars = <int>[];
  final pos = <(int, int)?>[];
  List<int>? prev;
  for (var r = from; r < end; r++) {
    final cells = _cellsOf(lines[r]);
    if (prev != null && !_joins(prev, cells, cols)) {
      chars.add(0x20);
      pos.add(null);
    }
    for (var c = 0; c < cells.length; c++) {
      chars.add(cells[c]);
      pos.add((r, c));
    }
    prev = cells;
  }
  final n = chars.length;
  final out = <LinkHit>[];
  var i = 0;
  while (i < n) {
    final head = chars[i];
    if (head == 0x68 || head == 0x66 || head == 0x77) {
      final tail = String.fromCharCodes(
        chars.sublist(i, i + 8 > n ? n : i + 8),
      );
      final isWww = tail.startsWith('www.');
      final scheme =
          tail.startsWith('https://') ||
          tail.startsWith('http://') ||
          tail.startsWith('file://');
      if (scheme || isWww) {
        var j = i;
        while (j < n && _isUrlChar(chars[j])) {
          j++;
        }
        final stop = _trimTrailing(chars, i, j);
        if (stop > i + 8 || (isWww && stop > i + 5)) {
          final body = String.fromCharCodes(chars.sublist(i, stop));
          final url = isWww ? 'https://$body' : body;
          final segs = <(int, int, int)>[];
          for (final p in pos.sublist(i, stop)) {
            if (p == null) continue;
            final last = segs.isEmpty ? null : segs.last;
            if (last != null && last.$1 == p.$1 && last.$3 == p.$2) {
              segs[segs.length - 1] = (last.$1, last.$2, last.$3 + 1);
            } else {
              segs.add((p.$1, p.$2, p.$2 + 1));
            }
          }
          out.add(LinkHit(url, segs));
        }
        i = j > i ? j : i + 1;
        continue;
      }
    }
    i++;
  }
  return out;
}

/// 누른 칸에 걸린 주소. 접힌 주소가 앞뒤 줄에 걸쳐 있어 근처 몇 줄을 같이 본다.
LinkHit? linkAt(List<List<Run>> lines, int cols, int row, int col) {
  if (row < 0 || row >= lines.length) return null;
  final from = row - 6 < 0 ? 0 : row - 6;
  final to = row + 7 > lines.length ? lines.length : row + 7;
  for (final hit in detectLinks(lines, cols, from: from, to: to)) {
    if (hit.covers(row, col)) return hit;
  }
  return null;
}

/// 눌린 주소 — 열거나 복사한다. 데스크톱은 클릭 즉시 열지만 폰은 손가락이 굵어 오누름이
/// 잦고, 학생 화면의 주소는 옮겨 붙일 일도 많다.
Future<void> showLinkSheet(BuildContext context, String url) =>
    showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (sheet) {
        final theme = Theme.of(sheet);
        final uri = Uri.tryParse(url);
        final openable =
            uri != null && (uri.scheme == 'http' || uri.scheme == 'https');
        return SafeArea(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Padding(
                padding: const EdgeInsets.fromLTRB(20, 0, 20, 8),
                child: SelectableText(
                  url,
                  style: theme.textTheme.bodyMedium?.copyWith(
                    fontFamily: 'TermMono',
                  ),
                ),
              ),
              ListTile(
                leading: const Icon(Icons.open_in_new),
                title: const Text('브라우저로 열기'),
                enabled: openable,
                onTap: () {
                  Navigator.of(sheet).pop();
                  launchUrl(uri!, mode: LaunchMode.externalApplication);
                },
              ),
              ListTile(
                leading: const Icon(Icons.copy),
                title: const Text('주소 복사'),
                onTap: () async {
                  await Clipboard.setData(ClipboardData(text: url));
                  if (sheet.mounted) Navigator.of(sheet).pop();
                  if (context.mounted) {
                    ScaffoldMessenger.of(
                      context,
                    ).showSnackBar(const SnackBar(content: Text('주소를 복사했다')));
                  }
                },
              ),
              const SizedBox(height: 4),
            ],
          ),
        );
      },
    );

/// 길게 누른 줄 — 글자를 통째로 복사한다(주소가 아니어도).
Future<void> copyLine(BuildContext context, List<Run> runs) async {
  final text = runs.map((r) => r.text).join().trimRight();
  if (text.isEmpty) return;
  await Clipboard.setData(ClipboardData(text: text));
  if (context.mounted) {
    ScaffoldMessenger.of(
      context,
    ).showSnackBar(const SnackBar(content: Text('한 줄을 복사했다')));
  }
}
