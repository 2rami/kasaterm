import 'package:flutter/material.dart';

import '../hub_model.dart';
import '../server.dart';
import 'settings.dart';
import 'terminal.dart';

/// 첫 화면 — 기계·방별 학생 목록. 기다리는 학생이 맨 위에 선다.
class HubScreen extends StatefulWidget {
  const HubScreen({
    super.key,
    required this.server,
    required this.onChangeAddress,
  });

  final Server server;
  final Future<void> Function() onChangeAddress;

  @override
  State<HubScreen> createState() => _HubScreenState();
}

class _HubScreenState extends State<HubScreen> with WidgetsBindingObserver {
  late final HubModel _model = HubModel(widget.server);

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _model.start();
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _model.dispose();
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    switch (state) {
      case AppLifecycleState.resumed:
        _model.start();
      case AppLifecycleState.paused:
      case AppLifecycleState.detached:
      case AppLifecycleState.hidden:
        _model.stop();
      case AppLifecycleState.inactive:
        break;
    }
  }

  void _open(Pane pane) {
    _model.clearBadge();
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => TerminalScreen(server: widget.server, pane: pane),
      ),
    );
  }

  void _openSettings() {
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => SettingsScreen(
          server: widget.server,
          onChangeAddress: widget.onChangeAddress,
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: _model,
    builder: (context, _) {
      final theme = Theme.of(context);
      return Scaffold(
        appBar: AppBar(
          title: const Text('학생'),
          actions: [
            if (_model.newWaiting > 0)
              Padding(
                padding: const EdgeInsets.only(right: 4),
                child: Badge.count(
                  count: _model.newWaiting,
                  child: const Icon(Icons.notifications_outlined),
                ),
              ),
            IconButton(
              onPressed: _openSettings,
              icon: const Icon(Icons.settings_outlined),
              tooltip: '설정',
            ),
          ],
        ),
        body: RefreshIndicator(onRefresh: _model.refresh, child: _body(theme)),
      );
    },
  );

  Widget _body(ThemeData theme) {
    final sections = _model.sections;
    final children = <Widget>[];
    if (_model.error != null) {
      children.add(
        _Notice(text: _model.error!, color: theme.colorScheme.error),
      );
    }
    if (sections.isEmpty && _model.error == null) {
      children.add(const _Notice(text: '학생 목록을 받는 중…'));
    }
    for (final s in sections) {
      final title = s.machine ?? '이 기계';
      children.add(
        _SectionHeader(
          title: title,
          trailing: s.machine == null
              ? null
              : (s.online ? '${s.paneCount}명' : '안 닿음'),
          muted: !s.online,
        ),
      );
      if (s.online && s.rooms.isEmpty) {
        children.add(const _Notice(text: '학생이 없다'));
      }
      for (final room in s.rooms) {
        children.add(_RoomHeader(title: room.title));
        for (final p in room.panes) {
          children.add(
            _PaneTile(
              server: widget.server,
              pane: p,
              onTap: s.online ? () => _open(p) : null,
            ),
          );
        }
      }
    }
    return ListView(
      physics: const AlwaysScrollableScrollPhysics(),
      padding: const EdgeInsets.fromLTRB(12, 0, 12, 24),
      children: children,
    );
  }
}

class _SectionHeader extends StatelessWidget {
  const _SectionHeader({
    required this.title,
    this.trailing,
    this.muted = false,
  });

  final String title;
  final String? trailing;
  final bool muted;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final color = muted
        ? theme.colorScheme.onSurfaceVariant
        : theme.colorScheme.onSurface;
    return Padding(
      padding: const EdgeInsets.fromLTRB(4, 18, 4, 6),
      child: Row(
        children: [
          Icon(Icons.computer_outlined, size: 18, color: color),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              title,
              style: theme.textTheme.titleMedium?.copyWith(color: color),
              overflow: TextOverflow.ellipsis,
            ),
          ),
          if (trailing != null)
            Text(
              trailing!,
              style: theme.textTheme.labelMedium?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
              ),
            ),
        ],
      ),
    );
  }
}

class _RoomHeader extends StatelessWidget {
  const _RoomHeader({required this.title});

  final String title;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(4, 10, 4, 4),
      child: Text(
        title,
        style: theme.textTheme.labelLarge?.copyWith(
          color: theme.colorScheme.onSurfaceVariant,
        ),
      ),
    );
  }
}

class _Notice extends StatelessWidget {
  const _Notice({required this.text, this.color});

  final String text;
  final Color? color;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(4, 12, 4, 4),
      child: Text(
        text,
        style: theme.textTheme.bodyMedium?.copyWith(
          color: color ?? theme.colorScheme.onSurfaceVariant,
        ),
      ),
    );
  }
}

Color? parseHexColor(String? hex) {
  if (hex == null) return null;
  final h = hex.replaceFirst('#', '');
  if (h.length != 6) return null;
  final v = int.tryParse(h, radix: 16);
  return v == null ? null : Color(0xff000000 | v);
}

class _PaneTile extends StatelessWidget {
  const _PaneTile({required this.server, required this.pane, this.onTap});

  final Server server;
  final Pane pane;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final accent = parseHexColor(pane.color) ?? scheme.primary;
    final slug = pane.slug;
    return Padding(
      padding: const EdgeInsets.only(bottom: 6),
      child: Card(
        clipBehavior: Clip.antiAlias,
        child: InkWell(
          onTap: onTap,
          child: Row(
            children: [
              Container(width: 4, height: 60, color: accent),
              const SizedBox(width: 10),
              _Avatar(
                url: slug == null
                    ? null
                    : server.avatar(slug, machine: pane.machine),
              ),
              const SizedBox(width: 10),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Text(
                      pane.name.isEmpty ? '이름 없는 학생' : pane.name,
                      style: theme.textTheme.titleSmall,
                      overflow: TextOverflow.ellipsis,
                    ),
                    if (pane.title.isNotEmpty)
                      Text(
                        pane.title,
                        style: theme.textTheme.bodySmall?.copyWith(
                          color: scheme.onSurfaceVariant,
                        ),
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                      ),
                  ],
                ),
              ),
              const SizedBox(width: 8),
              _StatusChip(pane: pane, accent: accent),
              const SizedBox(width: 10),
            ],
          ),
        ),
      ),
    );
  }
}

class _Avatar extends StatelessWidget {
  const _Avatar({required this.url});

  final Uri? url;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final fallback = Container(
      width: 40,
      height: 40,
      color: scheme.surfaceContainerHighest,
      child: Icon(Icons.person_outline, color: scheme.onSurfaceVariant),
    );
    return ClipOval(
      child: url == null
          ? fallback
          : Image.network(
              url.toString(),
              width: 40,
              height: 40,
              fit: BoxFit.cover,
              errorBuilder: (_, _, _) => fallback,
            ),
    );
  }
}

class _StatusChip extends StatelessWidget {
  const _StatusChip({required this.pane, required this.accent});

  final Pane pane;
  final Color accent;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final label = pane.isWaiting ? '대기' : (pane.isIdle ? '쉼' : '작업 중');
    final filled = pane.isWaiting;
    final fg = filled ? scheme.onPrimary : scheme.onSurfaceVariant;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 9, vertical: 4),
      decoration: BoxDecoration(
        color: filled ? scheme.primary : null,
        border: filled ? null : Border.all(color: scheme.outline),
        borderRadius: BorderRadius.circular(999),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (!filled && !pane.isIdle) ...[
            Container(
              width: 7,
              height: 7,
              decoration: BoxDecoration(color: accent, shape: BoxShape.circle),
            ),
            const SizedBox(width: 5),
          ],
          Text(
            label,
            style: Theme.of(context).textTheme.labelMedium?.copyWith(
              color: fg,
              fontWeight: filled ? FontWeight.w600 : FontWeight.w500,
            ),
          ),
        ],
      ),
    );
  }
}
