import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../hub_model.dart';
import '../server.dart';
import '../student_art.dart';
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
          title: Row(
            children: [
              Image.asset(
                'assets/students/schale-logo.png',
                height: 22,
                errorBuilder: (_, _, _) => const SizedBox.shrink(),
              ),
              const SizedBox(width: 8),
              const Text('학생'),
            ],
          ),
          actions: [
            if (_model.waiting > 0)
              Padding(
                padding: const EdgeInsets.only(right: 4),
                child: Badge.count(
                  count: _model.waiting,
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
        body: Stack(
          fit: StackFit.expand,
          children: [
            // 데스크톱이 학생을 고르는 화면 뒤에 까는 교실. 목록이 읽히게 배경색으로 덮는다.
            Image.asset(
              'assets/schale-classroom.png',
              fit: BoxFit.cover,
              alignment: Alignment.topCenter,
              errorBuilder: (_, _, _) => const SizedBox.shrink(),
            ),
            DecoratedBox(
              decoration: BoxDecoration(
                gradient: LinearGradient(
                  begin: Alignment.topCenter,
                  end: Alignment.bottomCenter,
                  colors: [
                    theme.scaffoldBackgroundColor.withValues(alpha: 0.72),
                    theme.scaffoldBackgroundColor.withValues(alpha: 0.96),
                  ],
                ),
              ),
            ),
            RefreshIndicator(onRefresh: _model.refresh, child: _body(theme)),
          ],
        ),
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
        if (room.rects.isNotEmpty) {
          children.add(
            _MiniMap(
              server: widget.server,
              room: room,
              onOpen: s.online ? _open : null,
            ),
          );
        }
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

/// 데스크톱 창을 축소한 지도 — 방 안에서 누가 어디에 어떤 크기로 앉아 있는지.
/// 칸을 누르면 목록의 타일과 같은 화면으로 간다.
class _MiniMap extends StatelessWidget {
  const _MiniMap({required this.server, required this.room, this.onOpen});

  final Server server;
  final HubRoom room;
  final void Function(Pane)? onOpen;

  static const _height = 108.0;
  static const _gap = 1.5;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Padding(
      padding: const EdgeInsets.fromLTRB(2, 0, 2, 8),
      child: Container(
        height: _height,
        decoration: BoxDecoration(
          color: scheme.surfaceContainerHighest.withValues(alpha: 0.5),
          borderRadius: BorderRadius.circular(8),
        ),
        clipBehavior: Clip.antiAlias,
        child: LayoutBuilder(
          builder: (context, box) {
            final w = box.maxWidth;
            final h = box.maxHeight;
            return Stack(
              children: [
                for (final r in room.rects)
                  Positioned(
                    left: r.x / 100 * w + _gap,
                    top: r.y / 100 * h + _gap,
                    width: math.max(0, r.w / 100 * w - _gap * 2),
                    height: math.max(0, r.h / 100 * h - _gap * 2),
                    child: _MiniCell(
                      server: server,
                      pane: room.paneOf(r.surface),
                      onOpen: onOpen,
                    ),
                  ),
              ],
            );
          },
        ),
      ),
    );
  }
}

class _MiniCell extends StatelessWidget {
  const _MiniCell({required this.server, required this.pane, this.onOpen});

  final Server server;
  final Pane? pane;
  final void Function(Pane)? onOpen;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final p = pane;
    final accent = p == null
        ? scheme.outline
        : (parseHexColor(p.color) ?? scheme.primary);
    final waiting = p?.isWaiting ?? false;
    final busy = p?.isBusy ?? false;
    return LayoutBuilder(
      builder: (context, box) {
        final roomy = box.maxWidth >= 64 && box.maxHeight >= 44;
        final face = math.min(box.maxHeight * 0.55, 30.0);
        return Material(
          color: accent.withValues(alpha: waiting ? 0.32 : 0.12),
          shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(5),
            side: BorderSide(
              color: accent.withValues(alpha: waiting ? 0.9 : 0.45),
              width: waiting ? 1.5 : 0.8,
            ),
          ),
          clipBehavior: Clip.antiAlias,
          child: InkWell(
            onTap: p == null || onOpen == null ? null : () => onOpen!(p),
            child: Stack(
              children: [
                Center(
                  child: Column(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      if (p != null && face >= 14)
                        StudentFace(
                          slug: p.slug,
                          url: p.slug == null
                              ? null
                              : server.avatar(p.slug!, machine: p.machine),
                          shell: p.isShell,
                          size: face,
                        ),
                      if (p != null && roomy)
                        Padding(
                          padding: const EdgeInsets.only(top: 2),
                          child: Text(
                            p.displayName,
                            style: Theme.of(context).textTheme.labelSmall
                                ?.copyWith(color: scheme.onSurface),
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                          ),
                        ),
                    ],
                  ),
                ),
                if (waiting || busy)
                  Positioned(
                    top: 4,
                    right: 4,
                    child: Container(
                      width: 7,
                      height: 7,
                      decoration: BoxDecoration(
                        color: waiting ? scheme.primary : accent,
                        shape: BoxShape.circle,
                      ),
                    ),
                  ),
              ],
            ),
          ),
        );
      },
    );
  }
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
              StudentFace(
                slug: slug,
                url: slug == null
                    ? null
                    : server.avatar(slug, machine: pane.machine),
                shell: pane.isShell,
              ),
              const SizedBox(width: 10),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Text(
                      pane.displayName,
                      style: theme.textTheme.titleSmall,
                      overflow: TextOverflow.ellipsis,
                    ),
                    if (pane.subtitle.isNotEmpty)
                      Text(
                        pane.subtitle,
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

class _StatusChip extends StatelessWidget {
  const _StatusChip({required this.pane, required this.accent});

  final Pane pane;
  final Color accent;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final label = pane.kindLabel;
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
          if (!filled && pane.isBusy) ...[
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
