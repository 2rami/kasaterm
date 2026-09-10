import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../server.dart';
import 'notes_sheet.dart' show timeAgo;

/// 데스크톱 하단바 「최근 복사」의 폰 판(2026-09-10 지시 「카사텀 pc 에도 붙고 폰에도
/// 붙게」). 줄을 누르면 그 글이 **폰 클립보드**로 오고, 「폰에서 올리기」는 폰에 복사해
/// 둔 것을 데스크톱 클립보드로 보낸다 — 어느 쪽에서 복사했든 양쪽에서 붙는다.
/// 비밀값은 목록에서 가려진 채 오고, 가져갈 때만 본문이 온다.
Future<void> showClipboardSheet(
  BuildContext context, {
  required Server server,
}) => showModalBottomSheet<void>(
  context: context,
  isScrollControlled: true,
  showDragHandle: true,
  builder: (_) => _ClipboardSheet(server: server),
);

class _ClipboardSheet extends StatefulWidget {
  const _ClipboardSheet({required this.server});

  final Server server;

  @override
  State<_ClipboardSheet> createState() => _ClipboardSheetState();
}

class _ClipboardSheetState extends State<_ClipboardSheet> {
  List<ClipItem> _items = const [];
  bool _loading = true;
  bool _busy = false;
  String? _error;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    try {
      final items = await widget.server.clipboard();
      if (!mounted) return;
      setState(() {
        _items = items;
        _loading = false;
        _error = null;
      });
    } on ServerException catch (e) {
      if (!mounted) return;
      setState(() {
        _loading = false;
        _error = e.message;
      });
    }
  }

  void _say(String text) {
    if (!mounted) return;
    ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(text)));
  }

  /// 그 칸을 폰 클립보드로 — 데스크톱 쪽도 그것이 맨 위(지금 것)가 된다.
  Future<void> _take(ClipItem it) async {
    setState(() => _busy = true);
    try {
      final text = await widget.server.clipboardItem(it.id);
      await Clipboard.setData(ClipboardData(text: text));
      await widget.server.clipboardPick(it.id);
      if (!mounted) return;
      Navigator.of(context).pop();
      _say(it.secret ? '비밀값을 폰에 복사했다' : '폰에 복사했다 · ${it.preview}');
    } on ServerException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  /// 폰에 복사해 둔 것을 데스크톱 클립보드로.
  Future<void> _push() async {
    final data = await Clipboard.getData(Clipboard.kTextPlain);
    final text = data?.text ?? '';
    if (text.trim().isEmpty) {
      _say('폰 클립보드가 비어 있다');
      return;
    }
    setState(() => _busy = true);
    try {
      await widget.server.clipboardPush(text);
      await _load();
      _say('데스크톱 클립보드에 넣었다');
    } on ServerException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return DraggableScrollableSheet(
      expand: false,
      initialChildSize: 0.55,
      minChildSize: 0.3,
      maxChildSize: 0.92,
      builder: (context, controller) => Column(
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(20, 0, 12, 4),
            child: Row(
              children: [
                Text('클립보드', style: theme.textTheme.titleMedium),
                const Spacer(),
                TextButton.icon(
                  onPressed: _busy ? null : _push,
                  icon: const Icon(Icons.upload_outlined, size: 18),
                  label: const Text('폰에서 올리기'),
                ),
              ],
            ),
          ),
          Padding(
            padding: const EdgeInsets.fromLTRB(20, 0, 20, 6),
            child: Align(
              alignment: Alignment.centerLeft,
              child: Text(
                '누르면 폰에 복사된다. 맨 위가 데스크톱의 지금 것.',
                style: theme.textTheme.bodySmall?.copyWith(
                  color: scheme.onSurfaceVariant,
                ),
              ),
            ),
          ),
          if (_error != null)
            Padding(
              padding: const EdgeInsets.fromLTRB(20, 0, 20, 6),
              child: Text(
                _error!,
                style: theme.textTheme.bodySmall?.copyWith(color: scheme.error),
              ),
            ),
          Expanded(
            child: _loading
                ? const Center(child: CircularProgressIndicator())
                : _items.isEmpty
                ? Center(
                    child: Text(
                      '아직 복사한 것이 없다',
                      style: theme.textTheme.bodyMedium?.copyWith(
                        color: scheme.onSurfaceVariant,
                      ),
                    ),
                  )
                : ListView.separated(
                    controller: controller,
                    itemCount: _items.length,
                    separatorBuilder: (_, _) => const Divider(height: 1),
                    itemBuilder: (context, i) {
                      final it = _items[i];
                      return ListTile(
                        enabled: !_busy,
                        onTap: () => _take(it),
                        leading: Icon(
                          it.secret ? Icons.lock_outline : Icons.notes,
                          color: it.secret ? scheme.tertiary : scheme.outline,
                        ),
                        title: Text(
                          it.preview,
                          maxLines: 2,
                          overflow: TextOverflow.ellipsis,
                          style: TextStyle(
                            fontFamily: it.secret ? null : 'TermMono',
                            fontSize: 13,
                            fontWeight: i == 0
                                ? FontWeight.w700
                                : FontWeight.w400,
                          ),
                        ),
                        subtitle: Text(
                          '${it.chars}자 · ${timeAgo(it.when)}',
                          style: theme.textTheme.labelSmall?.copyWith(
                            color: scheme.onSurfaceVariant,
                          ),
                        ),
                        trailing: const Icon(Icons.download_outlined, size: 18),
                      );
                    },
                  ),
          ),
        ],
      ),
    );
  }
}
