import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import '../contrast.dart';
import '../file_link.dart';
import '../grid.dart';
import '../grid_canvas.dart';
import '../look.dart';
import '../server.dart';
import '../shell_blocks.dart';

/// 셸 폴더 트리 — 셸 화면의 폴더 줄을 누르면 아래에서 올라온다. 명령을 친 뒤에도 언제든 연다(PC 는 왼쪽 파일트리 칼럼).
/// 목록은 원본 기기 `/term/tree?git=1` 을 한 층씩 받는다. 폴더 줄은 그 셸을 `cd` 하고 시트는 남아 새 폴더를 편다,
/// 화살표는 그 자리에서 펼치기, 파일은 원본에서 받아 미리보기. 치수는 `docs/design.md` 「거울 셸 명령 묶음」.
Future<void> showShellTree(
  BuildContext context, {
  required Server server,
  String? machine,
  required ValueListenable<String?> root,
  required TerminalPalette palette,
  void Function(String dir)? onCd,
}) => showModalBottomSheet<void>(
  context: context,
  isScrollControlled: true,
  showDragHandle: true,
  backgroundColor: palette.bg,
  builder: (context) => DraggableScrollableSheet(
    expand: false,
    initialChildSize: 0.6,
    minChildSize: 0.3,
    maxChildSize: 0.92,
    builder: (context, scroll) => ValueListenableBuilder<String?>(
      valueListenable: root,
      builder: (context, dir, _) => dir == null
          ? const Center(child: Text('셸이 선 폴더를 아직 몰라요'))
          : ShellTree(server: server, machine: machine, root: dir, palette: palette, onCd: onCd, scroll: scroll),
    ),
  ),
);

const _mono = TextStyle(
  fontFamily: 'TermMono',
  fontFamilyFallback: Look.flowMonoFallback,
  fontSize: Look.sub,
  height: 1.35,
);

class ShellTree extends StatefulWidget {
  const ShellTree({
    super.key,
    required this.server,
    this.machine,
    required this.root,
    required this.palette,
    this.onCd,
    this.scroll,
  });

  final Server server;
  final String? machine;
  final String root;
  final TerminalPalette palette;
  final void Function(String dir)? onCd;
  final ScrollController? scroll;

  @override
  State<ShellTree> createState() => _ShellTreeState();
}

class _ShellTreeState extends State<ShellTree> {
  /// 폴더마다 받은 목록·못 받은 까닭·받은 때, 펼친 폴더, 받는 중.
  final _dirs = <String, List<TreeEntry>>{};
  final _errors = <String, String>{};
  final _at = <String, DateTime>{};
  final _open = <String>{};
  final _asking = <String>{};

  @override
  void initState() {
    super.initState();
    _want();
  }

  @override
  void didUpdateWidget(ShellTree old) {
    super.didUpdateWidget(old);
    if (old.root == widget.root) return;
    // 셸이 다른 폴더로 갔다 — 펼침은 접고 새 뿌리 밖 목록은 버린다.
    _open.clear();
    for (final m in [_dirs, _errors, _at]) {
      m.removeWhere((dir, _) => !dir.startsWith(widget.root));
    }
    _want();
  }

  /// 뿌리와 펼친 폴더 가운데 없거나 5초 넘게 묵은 목록을 받는다.
  void _want() {
    final root = widget.root;
    final now = DateTime.now();
    for (final dir in [root, ..._open.where((d) => d.startsWith(root))]) {
      final at = _at[dir];
      if (at != null && now.difference(at) < const Duration(seconds: 5)) continue;
      if (!_asking.add(dir)) continue;
      widget.server.shellTree(dir, machine: widget.machine).then(
        (entries) {
          _dirs[dir] = entries;
          _errors.remove(dir);
        },
        onError: (Object e) {
          _errors[dir] = e is ServerException ? e.message : '폴더 목록을 못 받았어요';
        },
      ).whenComplete(() {
        _asking.remove(dir);
        _at[dir] = DateTime.now();
        if (mounted) setState(() {});
      });
    }
  }

  void _toggle(String dir) {
    setState(() => _open.contains(dir) ? _open.remove(dir) : _open.add(dir));
    _want();
  }

  /// 펼친 대로 줄을 편다 — (깊이, 경로, 줄) 이고, 줄이 null 이면 [경로] 자리에 안내 글.
  List<(int, String, TreeEntry?)> _rows(String dir, int depth) {
    final entries = _dirs[dir];
    if (entries == null) return [(depth, _errors[dir] ?? '목록을 받는 중…', null)];
    if (entries.isEmpty) return [(depth, '빈 폴더예요', null)];
    return [
      for (final e in entries) ...[
        (depth, joinDir(dir, e.name), e),
        if (e.dir && _open.contains(joinDir(dir, e.name))) ..._rows(joinDir(dir, e.name), depth + 1),
      ],
    ];
  }

  @override
  Widget build(BuildContext context) {
    final palette = widget.palette;
    final up = parentDir(widget.root);
    final cd = widget.onCd;
    return ListView(
      controller: widget.scroll,
      padding: const EdgeInsets.fromLTRB(Look.pagePad, 0, Look.pagePad, Look.pagePad),
      children: [
        Padding(
          padding: const EdgeInsets.only(bottom: 4),
          child: Text(
            tildePath(widget.root),
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: _mono.copyWith(color: mixToward(palette.fg, palette.bg, 0.4)),
          ),
        ),
        if (up != null)
          _TreeRow(depth: 0, name: '..', icon: Icons.arrow_upward, palette: palette, onTap: cd == null ? null : () => cd(up)),
        for (final (depth, path, entry) in _rows(widget.root, 0))
          if (entry == null)
            _TreeRow(depth: depth, name: path, palette: palette, note: true)
          else
            _TreeRow(
              depth: depth,
              name: entry.name,
              icon: entry.dir
                  ? (entry.repo ? Icons.account_tree_outlined : Icons.folder_outlined)
                  : Icons.insert_drive_file_outlined,
              open: entry.dir ? _open.contains(path) : null,
              mark: entry.ignored ? null : entry.git,
              dim: entry.ignored,
              palette: palette,
              onExpand: entry.dir ? () => _toggle(path) : null,
              onTap: entry.dir
                  ? (cd == null ? null : () => cd(path))
                  : () => previewResultFile(context, widget.server, path, machine: widget.machine),
            ),
      ],
    );
  }
}

/// 트리 한 줄 — 높이 [Look.tap]. 폴더면 앞에 펼침 화살표, 끝에 git 표시 한 글자.
class _TreeRow extends StatelessWidget {
  const _TreeRow({
    required this.depth,
    required this.name,
    required this.palette,
    this.icon,
    this.open,
    this.mark,
    this.dim = false,
    this.note = false,
    this.onTap,
    this.onExpand,
  });

  final int depth;
  final String name;
  final TerminalPalette palette;
  final IconData? icon;

  /// 폴더면 펼쳤나, 파일·윗 폴더 줄이면 null.
  final bool? open;
  final String? mark;
  final bool dim;

  /// 받는 중·빈 폴더 같은 안내 글.
  final bool note;
  final VoidCallback? onTap;
  final VoidCallback? onExpand;

  Color _markColor(String m) => switch (m) {
    'M' => palette.resolve(const IndexColor(3), foreground: true),
    'A' || 'U' => palette.resolve(const IndexColor(2), foreground: true),
    'D' => palette.resolve(const IndexColor(1), foreground: true),
    _ => mixToward(palette.fg, palette.bg, 0.4),
  };

  @override
  Widget build(BuildContext context) {
    final mute = mixToward(palette.fg, palette.bg, 0.5);
    final ink = note || dim ? mute : (open != null ? palette.fg : mixToward(palette.fg, palette.bg, 0.2));
    final m = mark;
    return InkWell(
      onTap: onTap,
      borderRadius: BorderRadius.circular(Look.radiusSm),
      child: SizedBox(
        height: Look.tap,
        child: Row(
          children: [
            SizedBox(width: depth * 16.0),
            SizedBox(
              width: 28,
              height: Look.tap,
              child: open == null
                  ? null
                  : InkResponse(
                      onTap: onExpand,
                      radius: 18,
                      child: Icon(open! ? Icons.expand_more : Icons.chevron_right, size: 18, color: mute),
                    ),
            ),
            if (icon != null) ...[
              Icon(icon, size: 18, color: ink),
              const SizedBox(width: 8),
            ],
            Expanded(
              child: Text(
                name,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: _mono.copyWith(color: m == null ? ink : _markColor(m)),
              ),
            ),
            if (m != null)
              Padding(
                padding: const EdgeInsets.only(left: 8, right: 4),
                child: Text(m, style: _mono.copyWith(color: _markColor(m))),
              ),
          ],
        ),
      ),
    );
  }
}

