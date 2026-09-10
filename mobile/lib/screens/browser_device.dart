import 'package:flutter/material.dart';

import '../hub_model.dart';
import '../server.dart';

/// 하단바 「브라우저 기기」의 폰 판 — 학생이 사람에게 보여 주려 여는 페이지가 어디로
/// 갈지. 「이 폰」이면 쪽지+알림으로 오고, 기계를 고르면 그 기계 크롬으로 간다.
///
/// 목록은 주소가 가리키는 기계의 것을 받는다(하단바 팝오버와 같은 값). 고른 것은
/// 명부의 모든 기계에 같이 보낸다 — `open` 은 학생이 도는 기계가 처리하므로 본진에
/// 안 전해지면 거기 학생이 연 페이지는 여전히 그쪽 브라우저로 간다.
Future<void> showBrowserDeviceSheet(
  BuildContext context, {
  required Server server,
  required HubModel model,
}) => showModalBottomSheet<void>(
  context: context,
  showDragHandle: true,
  builder: (_) => _BrowserDeviceSheet(server: server, model: model),
);

class _BrowserDeviceSheet extends StatefulWidget {
  const _BrowserDeviceSheet({required this.server, required this.model});

  final Server server;
  final HubModel model;

  @override
  State<_BrowserDeviceSheet> createState() => _BrowserDeviceSheetState();
}

class _BrowserDeviceSheetState extends State<_BrowserDeviceSheet> {
  BrowserTarget? _target;
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
      final t = await widget.server.browserTarget();
      if (!mounted) return;
      setState(() {
        _target = t;
        _loading = false;
        _error = t == null ? '옛 데스크톱이라 브라우저 기기를 못 받았다' : null;
      });
    } on ServerException catch (e) {
      if (!mounted) return;
      setState(() {
        _loading = false;
        _error = e.message;
      });
    }
  }

  /// 명부 기계 전부에 같은 액션 — 루트는 `machine: null`, 나머지는 route 로.
  Future<void> _everywhere(
    Future<void> Function(String? machine, bool root) send,
  ) async {
    final failed = <String>[];
    await send(null, true);
    for (final s in widget.model.sections) {
      if (s.machine == null || !s.online) continue;
      try {
        await send(s.route ?? s.machine, false);
      } on ServerException {
        failed.add(s.machine!);
      }
    }
    if (failed.isNotEmpty && mounted) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('${failed.join(', ')} 에는 전하지 못했다')),
      );
    }
  }

  Future<void> _pickPhone() => _apply(
    (machine, _) => widget.server.settingsAction(
      'set-open-target',
      id: 'phone',
      machine: machine,
    ),
  );

  /// 빈 라벨 = 루트 기계 자신. 다른 기계에는 루트의 명부 이름으로 말해야 그쪽이 찾는다.
  Future<void> _pickMachine(String label) {
    final local = _target?.local ?? '';
    return _apply(
      (machine, root) => widget.server.settingsAction(
        'set-browser-target',
        label: root ? label : (label.isEmpty ? local : label),
        machine: machine,
      ),
    );
  }

  Future<void> _apply(
    Future<void> Function(String? machine, bool root) send,
  ) async {
    setState(() => _busy = true);
    try {
      await _everywhere(send);
      if (mounted) Navigator.of(context).pop();
    } on ServerException catch (e) {
      if (!mounted) return;
      setState(() {
        _busy = false;
        _error = e.message;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final t = _target;
    final rootName = widget.model.rootName ?? '이 기계';
    return SafeArea(
      child: Padding(
        padding: const EdgeInsets.fromLTRB(16, 0, 16, 16),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Icon(Icons.public, color: theme.colorScheme.primary, size: 20),
                const SizedBox(width: 10),
                Text('브라우저 기기', style: theme.textTheme.titleMedium),
              ],
            ),
            const SizedBox(height: 4),
            Text(
              '학생이 보여 주려 여는 페이지가 갈 곳.',
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
              ),
            ),
            const SizedBox(height: 8),
            if (_loading)
              const Padding(
                padding: EdgeInsets.all(24),
                child: Center(child: CircularProgressIndicator()),
              )
            else ...[
              _row(
                icon: Icons.smartphone,
                label: '이 폰',
                hint: '쪽지와 알림으로 받는다. 학생이 조작하는 크롬은 그대로.',
                selected: t?.phone ?? false,
                onTap: _pickPhone,
              ),
              _row(
                icon: Icons.computer,
                label: '$rootName · 연결된 기계',
                selected: t != null && !t.phone && t.machine.isEmpty,
                onTap: () => _pickMachine(''),
              ),
              for (final c in t?.candidates ?? const <String>[])
                _row(
                  icon: Icons.desktop_windows_outlined,
                  label: c,
                  selected: t != null && !t.phone && t.machine == c,
                  onTap: () => _pickMachine(c),
                ),
              if (_error != null)
                Padding(
                  padding: const EdgeInsets.only(top: 8),
                  child: Text(
                    _error!,
                    style: theme.textTheme.bodySmall?.copyWith(
                      color: theme.colorScheme.error,
                    ),
                  ),
                ),
            ],
          ],
        ),
      ),
    );
  }

  Widget _row({
    required IconData icon,
    required String label,
    String? hint,
    required bool selected,
    required VoidCallback onTap,
  }) {
    final scheme = Theme.of(context).colorScheme;
    return ListTile(
      contentPadding: EdgeInsets.zero,
      enabled: !_busy,
      leading: Icon(icon, color: selected ? scheme.primary : scheme.outline),
      title: Text(
        label,
        style: TextStyle(
          fontWeight: selected ? FontWeight.w700 : FontWeight.w400,
        ),
      ),
      subtitle: hint == null ? null : Text(hint),
      trailing: selected ? Icon(Icons.check, color: scheme.primary) : null,
      onTap: onTap,
    );
  }
}
