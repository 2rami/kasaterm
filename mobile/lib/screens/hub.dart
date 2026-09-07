import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../hub_model.dart';
import '../hub_prefs.dart';
import '../server.dart';
import '../status_style.dart';
import '../student_art.dart';
import 'pane_actions.dart';
import 'settings.dart';
import 'terminal.dart';

/// 첫 화면 — 기계·방별 학생 목록. 기다리는 학생이 맨 위에 선다.
class HubScreen extends StatefulWidget {
  const HubScreen({
    super.key,
    required this.server,
    required this.onChangeAddress,
    this.prefs,
  });

  final Server server;
  final Future<void> Function() onChangeAddress;

  /// 보기 설정 저장소. 없으면(테스트) 고른 것이 이 화면에서만 산다.
  final HubPrefs? prefs;

  @override
  State<HubScreen> createState() => _HubScreenState();
}

class _HubScreenState extends State<HubScreen> with WidgetsBindingObserver {
  late final HubModel _model = HubModel(widget.server, prefs: widget.prefs);

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

  Future<void> _paneSheet(HubSection s, HubRoom room, Pane p) => showPaneSheet(
    context,
    server: widget.server,
    room: room,
    pane: p,
    machine: s.machine,
    onOpen: _open,
    onChanged: _model.refresh,
  );

  Future<void> _roomSheet(HubSection s, HubRoom room) => showRoomSheet(
    context,
    server: widget.server,
    room: room,
    machine: s.machine,
    onChanged: _model.refresh,
  );

  Future<void> _newRoom(HubSection s) => newRoom(
    context,
    server: widget.server,
    machine: s.machine,
    onChanged: _model.refresh,
  );

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
            AnimatedSwitcher(
              duration: const Duration(milliseconds: 280),
              transitionBuilder: (child, anim) => ScaleTransition(
                scale: CurvedAnimation(parent: anim, curve: Curves.easeOutBack),
                child: child,
              ),
              child: _model.waiting > 0
                  ? Padding(
                      key: ValueKey(_model.waiting),
                      padding: const EdgeInsets.only(right: 4),
                      child: Badge.count(
                        count: _model.waiting,
                        backgroundColor: StatusStyle.attention,
                        textColor: Colors.white,
                        child: const Icon(Icons.notifications_outlined),
                      ),
                    )
                  : const SizedBox.shrink(),
            ),
            _ViewMenu(model: _model),
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
    final sections = _model.visible;
    final shape = _model.view.shape;
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
      final title = s.machine ?? _model.rootName ?? '이 기계';
      children.add(
        _SectionHeader(
          title: title,
          trailing: s.machine == null
              ? null
              : (s.online ? '${s.paneCount}명' : '안 닿음'),
          muted: !s.online,
          onAdd: s.online ? () => _newRoom(s) : null,
        ),
      );
      if (s.online && s.rooms.isEmpty) {
        children.add(const _Notice(text: '학생이 없다'));
      }
      for (final room in s.rooms) {
        final inside = <Widget>[
          _RoomHeader(
            title: room.title,
            onMenu: s.online ? () => _roomSheet(s, room) : null,
          ),
        ];
        final hasMap = room.rects.isNotEmpty && shape != HubShape.list;
        if (hasMap) {
          inside.add(
            _MiniMap(
              server: widget.server,
              room: room,
              onOpen: s.online ? _open : null,
              onMore: s.online ? (p) => _paneSheet(s, room, p) : null,
            ),
          );
        }
        // 「지도만」이라도 지도를 못 그리는 방(옛 서버)은 목록으로 — 학생이 사라지면 안 된다.
        if (!(shape == HubShape.map && hasMap)) {
          for (final (i, p) in room.panes.indexed) {
            inside.add(
              Appear(
                key: ValueKey('tile-${p.machine}-${p.id}'),
                delayIndex: i,
                child: _PaneTile(
                  server: widget.server,
                  pane: p,
                  onTap: s.online ? () => _open(p) : null,
                  onLongPress: s.online ? () => _paneSheet(s, room, p) : null,
                ),
              ),
            );
          }
        }
        children.add(_RoomBox(children: inside));
      }
    }
    return ListView(
      physics: const AlwaysScrollableScrollPhysics(),
      padding: const EdgeInsets.fromLTRB(12, 0, 12, 24),
      children: children,
    );
  }
}

/// 앱바의 「보기」 메뉴 — 어느 기기를 따라갈지, 지도·목록 중 무엇을 볼지.
class _ViewMenu extends StatelessWidget {
  const _ViewMenu({required this.model});

  final HubModel model;

  @override
  Widget build(BuildContext context) {
    final view = model.view;
    final machines = [
      for (final s in model.sections)
        if (s.machine != null) s.machine!,
    ];
    final rootName = model.rootName ?? '이 기계';
    final current = view.machine == null
        ? '전체'
        : (view.machine!.isEmpty ? rootName : view.machine!);
    return PopupMenuButton<VoidCallback>(
      tooltip: '보기',
      onSelected: (fn) => fn(),
      icon: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          const Icon(Icons.devices_outlined),
          const SizedBox(width: 4),
          Text(
            current,
            style: Theme.of(context).textTheme.labelLarge,
            overflow: TextOverflow.ellipsis,
          ),
        ],
      ),
      itemBuilder: (context) => [
        const PopupMenuItem(enabled: false, height: 32, child: Text('기기')),
        _pick('전체', view.machine == null, () {
          model.setView(view.copyWith(clearMachine: true));
        }),
        _pick(rootName, view.machine == '', () {
          model.setView(view.copyWith(machine: ''));
        }),
        for (final m in machines)
          _pick(m, view.machine == m, () {
            model.setView(view.copyWith(machine: m));
          }),
        const PopupMenuDivider(),
        const PopupMenuItem(enabled: false, height: 32, child: Text('모양')),
        _pick('지도와 목록', view.shape == HubShape.both, () {
          model.setView(view.copyWith(shape: HubShape.both));
        }),
        _pick('목록만', view.shape == HubShape.list, () {
          model.setView(view.copyWith(shape: HubShape.list));
        }),
        _pick('지도만', view.shape == HubShape.map, () {
          model.setView(view.copyWith(shape: HubShape.map));
        }),
      ],
    );
  }

  PopupMenuEntry<VoidCallback> _pick(
    String label,
    bool checked,
    VoidCallback fn,
  ) => CheckedPopupMenuItem<VoidCallback>(
    value: fn,
    checked: checked,
    child: Text(label),
  );
}

class _SectionHeader extends StatelessWidget {
  const _SectionHeader({
    required this.title,
    this.trailing,
    this.muted = false,
    this.onAdd,
  });

  final String title;
  final String? trailing;
  final bool muted;

  /// 「새 방」 — 그 기계에 빈 창 하나. 안 닿는 기계엔 안 단다.
  final VoidCallback? onAdd;

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
          if (onAdd != null)
            // 줄 높이를 안 바꾸는 크기 — 기본 IconButton 은 48px 라 목록 전체가 밀린다.
            SizedBox.square(
              dimension: 22,
              child: IconButton(
                tooltip: '새 방',
                padding: EdgeInsets.zero,
                constraints: const BoxConstraints(),
                onPressed: onAdd,
                icon: Icon(Icons.add_box_outlined, size: 20, color: color),
              ),
            ),
        ],
      ),
    );
  }
}

/// 방 하나를 한 판으로 묶는다 — 방이 여럿 펼쳐졌을 때 어디까지가 한 방인지 보이게.
class _RoomBox extends StatelessWidget {
  const _RoomBox({required this.children});

  final List<Widget> children;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      margin: const EdgeInsets.only(bottom: 10),
      padding: const EdgeInsets.fromLTRB(8, 4, 8, 4),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerLow.withValues(alpha: 0.85),
        borderRadius: BorderRadius.circular(14),
        border: Border.all(color: scheme.outlineVariant.withValues(alpha: 0.6)),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: children,
      ),
    );
  }
}

class _RoomHeader extends StatelessWidget {
  const _RoomHeader({required this.title, this.onMenu});

  final String title;

  /// 방 메뉴(pane 추가·이름·닫기). 안 닿는 기계엔 안 단다.
  final VoidCallback? onMenu;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    // 방 이름은 판의 제목이다 — 회색 한 줄로는 방이 여럿일 때 경계가 안 보였다.
    return Padding(
      padding: const EdgeInsets.fromLTRB(2, 6, 2, 6),
      child: Row(
        children: [
          Container(
            width: 3,
            height: 14,
            decoration: BoxDecoration(
              color: theme.colorScheme.primary,
              borderRadius: BorderRadius.circular(2),
            ),
          ),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              title,
              style: theme.textTheme.titleSmall?.copyWith(
                color: theme.colorScheme.onSurface,
                fontWeight: FontWeight.w700,
              ),
              overflow: TextOverflow.ellipsis,
            ),
          ),
          if (onMenu != null)
            SizedBox.square(
              dimension: 20,
              child: IconButton(
                tooltip: '방 메뉴',
                padding: EdgeInsets.zero,
                constraints: const BoxConstraints(),
                onPressed: onMenu,
                icon: Icon(
                  Icons.more_horiz,
                  size: 18,
                  color: theme.colorScheme.onSurfaceVariant,
                ),
              ),
            ),
        ],
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
  const _MiniMap({
    required this.server,
    required this.room,
    this.onOpen,
    this.onMore,
  });

  final Server server;
  final HubRoom room;
  final void Function(Pane)? onOpen;

  /// 길게 누름 — pane 판(닫기·추가·자리 바꾸기). 「지도만」 보기에서도 닿게.
  final void Function(Pane)? onMore;

  static const _gap = 1.5;

  /// 흔한 데스크톱 창 모양. 서버가 비율을 안 주면 이걸로.
  static const _defaultAspect = 16 / 10;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Padding(
      padding: const EdgeInsets.fromLTRB(2, 0, 2, 8),
      child: AspectRatio(
        aspectRatio: (room.aspect ?? _defaultAspect).clamp(1.0, 3.2),
        child: Container(
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
                        onMore: onMore,
                      ),
                    ),
                ],
              );
            },
          ),
        ),
      ),
    );
  }
}

class _MiniCell extends StatelessWidget {
  const _MiniCell({
    required this.server,
    required this.pane,
    this.onOpen,
    this.onMore,
  });

  final Server server;
  final Pane? pane;
  final void Function(Pane)? onOpen;
  final void Function(Pane)? onMore;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final p = pane;
    final accent = p == null
        ? scheme.outline
        : (parseHexColor(p.color) ?? scheme.primary);
    final st = p == null ? null : StatusStyle.of(p, scheme);
    final waiting = st?.needsYou ?? false;
    final busy = st?.live ?? false;
    // 칸 바탕은 학생색, 테두리·점은 상태색 — 「누구」와 「어떤 상태」를 다른 채널로.
    final edge = waiting ? StatusStyle.attention : accent;
    return LayoutBuilder(
      builder: (context, box) {
        final roomy = box.maxWidth >= 64 && box.maxHeight >= 44;
        final face = math.min(box.maxHeight * 0.55, 30.0);
        return AnimatedContainer(
          duration: const Duration(milliseconds: 300),
          curve: Curves.easeOut,
          decoration: BoxDecoration(
            color: accent.withValues(alpha: waiting ? 0.26 : 0.12),
            borderRadius: BorderRadius.circular(5),
            border: Border.all(
              color: edge.withValues(alpha: waiting ? 0.95 : 0.45),
              width: waiting ? 1.5 : 0.8,
            ),
          ),
          clipBehavior: Clip.antiAlias,
          child: Material(
            color: Colors.transparent,
            child: InkWell(
              onTap: p == null || onOpen == null ? null : () => onOpen!(p),
              onLongPress: p == null || onMore == null
                  ? null
                  : () => onMore!(p),
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
                      top: 3,
                      right: 3,
                      child: busy
                          ? PulseDot(color: st!.color, size: 7)
                          : Icon(st!.icon, size: 12, color: st.color),
                    ),
                ],
              ),
            ),
          ),
        );
      },
    );
  }
}

class _PaneTile extends StatelessWidget {
  const _PaneTile({
    required this.server,
    required this.pane,
    this.onTap,
    this.onLongPress,
  });

  final Server server;
  final Pane pane;
  final VoidCallback? onTap;
  final VoidCallback? onLongPress;

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
          onLongPress: onLongPress,
          child: Row(
            children: [
              Container(width: 4, height: 60, color: accent),
              const SizedBox(width: 10),
              Hero(
                tag: 'face-${pane.machine}-${pane.id}',
                child: StudentFace(
                  slug: slug,
                  url: slug == null
                      ? null
                      : server.avatar(slug, machine: pane.machine),
                  shell: pane.isShell,
                ),
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
              StatusChip(pane: pane),
              const SizedBox(width: 10),
            ],
          ),
        ),
      ),
    );
  }
}
