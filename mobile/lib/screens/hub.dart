import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../control_session.dart';
import '../device_info.dart';
import '../hub_model.dart';
import '../hub_prefs.dart';
import '../server.dart';
import '../status_style.dart';
import '../student_art.dart';
import 'notes_sheet.dart';
import 'pane_actions.dart';
import 'settings.dart';
import 'terminal.dart';
import 'web.dart';

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

  /// 폰 제어 채널 — 학생의 「폰에서 보라」(`open-url`)를 받는다. 허브가 살아 있는
  /// 동안(학생 화면을 위에 올려도 허브는 남는다) 붙어 있고, 앱이 뒤로 가면 닫는다.
  late final ControlSession _control = ControlSession(
    widget.server,
    onOpenUrl: (f) => openControlUrl(Navigator.of(context), f),
  );
  bool _registered = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _model.start();
    _control.connect();
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    if (_registered) return;
    _registered = true;
    _registerDevice();
  }

  /// 이 폰의 모양(논리 크기·배율·플랫폼)을 서버에 맡긴다 — 학생 쪽 KasaChrome 이
  /// 그 크기를 흉내 내고, 브라우징 기기 목록에 「이 폰」이 선다. 실패는 조용히 —
  /// 옛 서버엔 이 창구가 없다.
  Future<void> _registerDevice() async {
    final info = describeDevice(
      logical: MediaQuery.sizeOf(context),
      devicePixelRatio: MediaQuery.devicePixelRatioOf(context),
    );
    try {
      await widget.server.registerDevice(info);
    } on ServerException {
      // 옛 데스크톱이거나 연결이 끊겼다 — 다음 허브 진입에 다시.
    }
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _control.dispose();
    _model.dispose();
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    switch (state) {
      case AppLifecycleState.resumed:
        _model.start();
        _control.resume();
      case AppLifecycleState.paused:
      case AppLifecycleState.detached:
      case AppLifecycleState.hidden:
        _model.stop();
        _control.pause();
      case AppLifecycleState.inactive:
        break;
    }
  }

  void _open(Pane pane) {
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => TerminalScreen(
          server: widget.server,
          pane: pane,
          // 세션 화면에서 pane 을 추가하면 허브 목록도 폴링 전에 갱신되게.
          onPaneCreated: _model.refresh,
        ),
      ),
    );
  }

  Future<void> _paneSheet(HubSection s, HubRoom room, Pane p) => showPaneSheet(
    context,
    server: widget.server,
    room: room,
    pane: p,
    machine: s.route,
    onOpen: _open,
    onChanged: _model.refresh,
  );

  Future<void> _roomSheet(HubSection s, HubRoom room) => showRoomSheet(
    context,
    server: widget.server,
    room: room,
    machine: s.route,
    onChanged: _model.refresh,
    onOpen: _open,
  );

  Future<void> _newRoom(HubSection s) => newRoom(
    context,
    server: widget.server,
    machine: s.route,
    onChanged: _model.refresh,
    onOpen: _open,
    model: _model,
  );

  /// 앱바의 「+」 — 기계·방을 골라 pane 하나. 만든 자리로 바로 들어간다.
  Future<void> _addPane() => showAddPaneSheet(
    context,
    server: widget.server,
    model: _model,
    onOpen: _open,
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
            IconButton(
              tooltip: 'pane 추가',
              onPressed: _addPane,
              icon: const Icon(Icons.add_box_outlined),
            ),
            // 종 = 나쵸가 남긴 학생 쪽지. 배지는 안 읽은 쪽지 수(2026-09-08 지시 —
            // 전엔 기다리는 학생 수만 세고 눌러도 아무것도 없었다).
            IconButton(
              tooltip: '학생 쪽지',
              onPressed: () => NotesSheet.show(
                context,
                model: _model,
                server: widget.server,
                onOpen: _open,
              ),
              icon: AnimatedSwitcher(
                duration: const Duration(milliseconds: 280),
                transitionBuilder: (child, anim) => ScaleTransition(
                  scale: CurvedAnimation(
                    parent: anim,
                    curve: Curves.easeOutBack,
                  ),
                  child: child,
                ),
                child: _model.unread > 0
                    ? Badge.count(
                        key: ValueKey(_model.unread),
                        count: _model.unread,
                        backgroundColor: StatusStyle.attention,
                        textColor: Colors.white,
                        child: const Icon(Icons.notifications_rounded),
                      )
                    : const Icon(
                        Icons.notifications_outlined,
                        key: ValueKey(0),
                      ),
              ),
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
      final tint = machineColor(title, theme.colorScheme);
      final folded = _model.view.isFolded(s.machine);
      children.add(
        _SectionHeader(
          title: title,
          color: tint,
          online: s.online,
          folded: folded,
          // 접힌 기계는 안이 안 보이니 주소 기계라도 몇 명인지는 남긴다.
          trailing: !s.online || (s.machine == null && !folded)
              ? null
              : '${s.paneCount}명',
          onTap: () => _model.toggleFold(s),
          onAdd: s.online ? () => _newRoom(s) : null,
        ),
      );
      if (folded) continue;
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
        children.add(_RoomBox(accent: tint, children: inside));
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

/// 기계 머리글 — 기계색 띠에 그 기계 모양 아이콘. 안 닿는 기계는 빨간 「연결 안 됨」
/// 칩을 달아, 밖에서 열었을 때 어느 기계가 빠졌는지 한눈에 갈린다.
class _SectionHeader extends StatelessWidget {
  const _SectionHeader({
    required this.title,
    required this.color,
    this.online = true,
    this.folded = false,
    this.trailing,
    this.onTap,
    this.onAdd,
  });

  final String title;

  /// 그 기계의 색 — 아래 방 상자의 왼쪽 줄과 같은 색.
  final Color color;
  final bool online;
  final bool folded;
  final String? trailing;

  /// 머리글 자체를 누르면 접고 편다. 「새 방」 단추는 자기 탭을 먼저 먹는다.
  final VoidCallback? onTap;

  /// 「새 방」 — 그 기계에 빈 창 하나. 안 닿는 기계엔 안 단다.
  final VoidCallback? onAdd;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final ink = online ? scheme.onSurface : scheme.onSurfaceVariant;
    final tint = online ? color : scheme.outline;
    final radius = BorderRadius.circular(10);
    return Container(
      margin: EdgeInsets.fromLTRB(0, 14, 0, folded ? 0 : 8),
      decoration: BoxDecoration(
        color: tint.withValues(alpha: online ? 0.13 : 0.08),
        borderRadius: radius,
      ),
      child: InkWell(
        onTap: onTap,
        borderRadius: radius,
        child: Padding(
          padding: const EdgeInsets.fromLTRB(10, 7, 8, 7),
          child: Row(
            children: [
              Icon(machineIcon(title), size: 18, color: tint),
              const SizedBox(width: 8),
              Expanded(
                child: Text(
                  title,
                  style: theme.textTheme.titleMedium?.copyWith(
                    color: ink,
                    fontWeight: FontWeight.w600,
                  ),
                  overflow: TextOverflow.ellipsis,
                ),
              ),
              if (!online)
                Container(
                  padding: const EdgeInsets.symmetric(
                    horizontal: 7,
                    vertical: 2,
                  ),
                  decoration: BoxDecoration(
                    color: scheme.error.withValues(alpha: 0.14),
                    borderRadius: BorderRadius.circular(8),
                  ),
                  child: Text(
                    '연결 안 됨',
                    style: theme.textTheme.labelSmall?.copyWith(
                      color: scheme.error,
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                ),
              if (trailing != null)
                Text(
                  trailing!,
                  style: theme.textTheme.labelMedium?.copyWith(
                    color: scheme.onSurfaceVariant,
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
                    icon: Icon(Icons.add_box_outlined, size: 20, color: ink),
                  ),
                ),
              const SizedBox(width: 4),
              AnimatedRotation(
                turns: folded ? -0.25 : 0,
                duration: const Duration(milliseconds: 160),
                child: Icon(Icons.expand_more, size: 20, color: ink),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// 방 하나를 한 판으로 묶는다 — 방이 여럿 펼쳐졌을 때 어디까지가 한 방인지 보이게.
class _RoomBox extends StatelessWidget {
  const _RoomBox({required this.children, this.accent});

  final List<Widget> children;

  /// 기계색 — 왼쪽 줄로 「어느 기계의 방」인지 머리글과 잇는다.
  final Color? accent;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    // 색줄은 따로 얹는다 — 둥근 모서리 상자엔 변마다 다른 색의 테를 못 준다.
    return Container(
      margin: const EdgeInsets.only(bottom: 10),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerLow.withValues(alpha: 0.85),
        borderRadius: BorderRadius.circular(14),
        border: Border.all(color: scheme.outlineVariant.withValues(alpha: 0.6)),
      ),
      clipBehavior: Clip.antiAlias,
      child: Stack(
        children: [
          if (accent != null)
            Positioned(
              left: 0,
              top: 0,
              bottom: 0,
              width: 3,
              child: ColoredBox(color: accent!.withValues(alpha: 0.9)),
            ),
          Padding(
            padding: const EdgeInsets.fromLTRB(8, 4, 8, 4),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: children,
            ),
          ),
        ],
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
    final hidden = room.unplaced;
    final undocked = room.undocked;
    return Padding(
      padding: const EdgeInsets.fromLTRB(2, 0, 2, 8),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          AspectRatio(
            aspectRatio: (room.aspect ?? _defaultAspect).clamp(1.0, 3.2),
            child: _board(scheme),
          ),
          if (hidden.isNotEmpty)
            Padding(
              padding: const EdgeInsets.fromLTRB(2, 6, 2, 0),
              child: Row(
                children: [
                  Text(
                    '탭 안',
                    style: Theme.of(context).textTheme.labelSmall?.copyWith(
                      color: scheme.onSurfaceVariant,
                    ),
                  ),
                  const SizedBox(width: 6),
                  Expanded(
                    child: _MiniTabRow(
                      server: server,
                      tabs: hidden,
                      active: null,
                      size: 24,
                      onOpen: onOpen,
                      marks: true,
                    ),
                  ),
                ],
              ),
            ),
          // 별도 OS 창으로 뗀 학생 — 데스크톱 배치도의 점선 칸과 같은 말(점선 테).
          if (undocked.isNotEmpty)
            Padding(
              padding: const EdgeInsets.fromLTRB(2, 6, 2, 0),
              child: Row(
                children: [
                  Icon(Icons.open_in_new, size: 11, color: scheme.onSurfaceVariant),
                  const SizedBox(width: 3),
                  Text(
                    '별도창',
                    style: Theme.of(context).textTheme.labelSmall?.copyWith(
                      color: scheme.onSurfaceVariant,
                    ),
                  ),
                  const SizedBox(width: 6),
                  Expanded(
                    child: _MiniTabRow(
                      server: server,
                      tabs: undocked,
                      active: null,
                      size: 24,
                      onOpen: onOpen,
                      marks: true,
                      dashed: true,
                    ),
                  ),
                ],
              ),
            ),
        ],
      ),
    );
  }

  Widget _board(ColorScheme scheme) {
    return Container(
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
                    // 탭이 둘 이상이면 그 자리의 학생 전부 — 칸 하나에 탭 줄로.
                    tabs: [for (final t in r.tabs) room.paneOf(t)],
                    tabActive: r.tabActive,
                    onOpen: onOpen,
                    onMore: onMore,
                  ),
                ),
            ],
          );
        },
      ),
    );
  }
}

class _MiniCell extends StatefulWidget {
  const _MiniCell({
    required this.server,
    required this.pane,
    this.tabs = const [],
    this.tabActive,
    this.onOpen,
    this.onMore,
  });

  final Server server;
  final Pane? pane;

  /// 이 자리의 탭들(순서대로). 데스크톱 pane 머리의 탭 줄과 같은 것 — 탭 안에 있는
  /// 학생도 지도에 보여야 한다(2026-09-08 지시). 빈 목록이면 탭이 하나다.
  final List<Pane?> tabs;
  final int? tabActive;
  final void Function(Pane)? onOpen;
  final void Function(Pane)? onMore;

  @override
  State<_MiniCell> createState() => _MiniCellState();
}

/// 탭이 여럿인 칸은 좌우로 넘겨 본다 — 탭마다 작은 얼굴을 줄 세우면 손가락으로 못
/// 집는다(2026-09-08 지적 「터치하기 너무 작잖아」). 한 번에 한 학생, 밑에 점으로 몇째인지.
/// 넘기면 새 학생이 아래에서 올라온다.
class _MiniCellState extends State<_MiniCell> {
  late int _page = _initialPage;

  int get _initialPage {
    final a = widget.tabActive;
    return a != null && a >= 0 && a < widget.tabs.length ? a : 0;
  }

  @override
  void didUpdateWidget(_MiniCell old) {
    super.didUpdateWidget(old);
    // 데스크톱에서 앞 탭이 바뀌면 따라간다 — 손으로 넘겨 둔 자리는 그때만 밀린다.
    if (old.tabActive != widget.tabActive ||
        old.tabs.length != widget.tabs.length) {
      _page = _initialPage;
    }
  }

  void _flip(int dir) {
    final n = widget.tabs.length;
    if (n < 2) return;
    setState(() => _page = (_page + dir + n) % n);
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final tabs = widget.tabs;
    final tabbed = tabs.length > 1;
    // 넘겨서 보고 있는 탭이 이 칸의 얼굴 — 자리 pane 은 대개 첫 탭이라, 그대로 두면
    // 뒤에 숨은 학생이 지도에 안 나온다.
    final shown = tabbed && _page < tabs.length ? tabs[_page] : null;
    final p = shown ?? widget.pane;
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
        final ch = box.maxHeight;
        final roomy = box.maxWidth >= 64 && ch >= 44;
        final dots = tabbed && ch >= 36;
        final face = math.min((ch - (dots ? 8 : 0)) * 0.55, 30.0);
        Widget student(Pane? q) => Center(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              if (q == null && face >= 14)
                // 학생이 안 앉은 칸(맨 셸)도 빈 상자로 두지 않는다 — 깨진 칸으로
                // 읽힌다(2026-09-08 폰 실물 점검).
                Icon(
                  Icons.terminal,
                  size: face * 0.6,
                  color: scheme.outline.withValues(alpha: 0.7),
                ),
              if (q != null && face >= 14)
                StudentFace(
                  slug: q.slug,
                  url: q.slug == null
                      ? null
                      : widget.server.avatar(q.slug!, machine: q.machine),
                  shell: q.isShell,
                  size: face,
                ),
              if (q != null && roomy)
                Padding(
                  padding: const EdgeInsets.only(top: 2),
                  child: Text(
                    q.displayName,
                    style: Theme.of(
                      context,
                    ).textTheme.labelSmall?.copyWith(color: scheme.onSurface),
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                  ),
                ),
              if (dots) const SizedBox(height: 8),
            ],
          ),
        );
        final front = AnimatedContainer(
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
              onTap: p == null || widget.onOpen == null
                  ? null
                  : () => widget.onOpen!(p),
              onLongPress: p == null || widget.onMore == null
                  ? null
                  : () => widget.onMore!(p),
              child: Stack(
                children: [
                  // 넘길 때 새 학생이 아래에서 올라온다 — 카드가 위로 올라오는 느낌
                  // (2026-09-08 지시).
                  AnimatedSwitcher(
                    duration: const Duration(milliseconds: 260),
                    switchInCurve: Curves.easeOutCubic,
                    switchOutCurve: Curves.easeIn,
                    transitionBuilder: (child, anim) => FadeTransition(
                      opacity: anim,
                      child: SlideTransition(
                        position: Tween(
                          begin: const Offset(0, 0.35),
                          end: Offset.zero,
                        ).animate(anim),
                        child: child,
                      ),
                    ),
                    child: KeyedSubtree(
                      key: ValueKey('face-${p?.id ?? 'shell'}'),
                      child: student(p),
                    ),
                  ),
                  if (dots)
                    Positioned(
                      left: 0,
                      right: 0,
                      bottom: busy ? 5 : 3,
                      child: Row(
                        mainAxisAlignment: MainAxisAlignment.center,
                        children: [
                          for (var i = 0; i < tabs.length; i++)
                            Container(
                              width: i == _page ? 6 : 4,
                              height: 4,
                              margin: const EdgeInsets.symmetric(
                                horizontal: 1.5,
                              ),
                              decoration: BoxDecoration(
                                color: i == _page
                                    ? scheme.onSurface
                                    : scheme.onSurface.withValues(alpha: 0.3),
                                borderRadius: BorderRadius.circular(2),
                              ),
                            ),
                        ],
                      ),
                    ),
                  if (waiting)
                    Positioned(
                      top: 3,
                      right: 3,
                      child: Icon(st!.icon, size: 12, color: st.color),
                    ),
                  // 작업 중은 칸 바닥에 흐르는 막대 — 허브 타일은 얼굴 테가 돌아
                  // 같은 뜻을 두 번 말하지 않는다(2026-09-07 지시 「둘 중 하나만」).
                  if (busy)
                    Positioned(
                      left: 0,
                      right: 0,
                      bottom: 0,
                      child: WorkingBar(style: st!),
                    ),
                ],
              ),
            ),
          ),
        );
        if (!tabbed) return front;
        // 좌우로 쓸면 다음·이전 탭. 겹친 뒷장은 두지 않는다 — 몇째인지는 밑의 점이
        // 말하고, 칸이 작아 덱까지 들어가면 얼굴이 밀린다(2026-09-08 지시).
        return GestureDetector(
          behavior: HitTestBehavior.opaque,
          onHorizontalDragEnd: (d) {
            final v = d.primaryVelocity ?? 0;
            if (v.abs() < 120) return;
            _flip(v < 0 ? 1 : -1);
          },
          child: front,
        );
      },
    );
  }
}

/// 칸 위쪽의 탭 줄 — 탭마다 작은 얼굴, 앞에 나온 탭은 테. 누르면 그 탭이 열린다.
/// 데스크톱 pane 머리의 탭 줄을 지도 크기로 줄인 것.
class _MiniTabRow extends StatelessWidget {
  const _MiniTabRow({
    required this.server,
    required this.tabs,
    required this.active,
    required this.size,
    this.onOpen,
    this.marks = false,
    this.dashed = false,
  });

  final Server server;
  final List<Pane?> tabs;

  /// 테를 점선으로 — 별도 OS 창에 나가 있는 학생(데스크톱 배치도와 같은 표시).
  final bool dashed;
  final int? active;
  final double size;
  final void Function(Pane)? onOpen;

  /// 테를 상태색으로 — 칸 밖에 홀로 선 줄에선 얼굴만으론 「답 기다림」이 안 보인다.
  final bool marks;

  Color _edge(Pane? p, int i, ColorScheme scheme) {
    if (marks && p != null) {
      final st = StatusStyle.of(p, scheme);
      if (st.needsYou) return StatusStyle.attention;
      if (st.live) return st.color;
    }
    return i == active
        ? scheme.onSurface
        : scheme.outline.withValues(alpha: 0.35);
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Row(
      children: [
        for (var i = 0; i < tabs.length; i++)
          Padding(
            padding: const EdgeInsets.only(right: 3),
            child: GestureDetector(
              behavior: HitTestBehavior.opaque,
              onTap: tabs[i] == null || onOpen == null
                  ? null
                  : () => onOpen!(tabs[i]!),
              child: Container(
                padding: const EdgeInsets.all(1),
                foregroundDecoration: dashed
                    ? _DashRing(_edge(tabs[i], i, scheme))
                    : null,
                decoration: BoxDecoration(
                  shape: BoxShape.circle,
                  border: dashed
                      ? null
                      : Border.all(
                          color: _edge(tabs[i], i, scheme),
                          width: i == active || (marks && tabs[i] != null)
                              ? 1.4
                              : 0.8,
                        ),
                ),
                child: tabs[i] == null
                    ? SizedBox(
                        width: size,
                        height: size,
                        child: DecoratedBox(
                          decoration: BoxDecoration(
                            shape: BoxShape.circle,
                            color: scheme.outlineVariant,
                          ),
                        ),
                      )
                    : StudentFace(
                        slug: tabs[i]!.slug,
                        url: tabs[i]!.slug == null
                            ? null
                            : server.avatar(
                                tabs[i]!.slug!,
                                machine: tabs[i]!.machine,
                              ),
                        shell: tabs[i]!.isShell,
                        size: size,
                      ),
              ),
            ),
          ),
      ],
    );
  }
}

/// 점선 원 테 — 패키지 없이 `Path` 를 잘라 긋는다(3px 긋고 2px 쉼).
class _DashRing extends Decoration {
  const _DashRing(this.color);
  final Color color;

  @override
  BoxPainter createBoxPainter([VoidCallback? onChanged]) =>
      _DashRingPainter(color);
}

class _DashRingPainter extends BoxPainter {
  _DashRingPainter(this.color);
  final Color color;

  @override
  void paint(Canvas canvas, Offset offset, ImageConfiguration configuration) {
    final size = configuration.size ?? Size.zero;
    if (size.isEmpty) return;
    final rect = (offset & size).deflate(0.7);
    final ring = Path()..addOval(rect);
    final paint = Paint()
      ..color = color
      ..style = PaintingStyle.stroke
      ..strokeWidth = 1.4;
    const on = 3.0;
    const off = 2.0;
    for (final metric in ring.computeMetrics()) {
      var at = 0.0;
      while (at < metric.length) {
        canvas.drawPath(
          metric.extractPath(at, math.min(at + on, metric.length)),
          paint,
        );
        at += on + off;
      }
    }
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
    final st = StatusStyle.of(pane, scheme);
    return Padding(
      padding: const EdgeInsets.only(bottom: 6),
      child: Card(
        clipBehavior: Clip.antiAlias,
        child: InkWell(
          onTap: onTap,
          onLongPress: onLongPress,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Row(
                children: [
                  Container(width: 4, height: 60, color: accent),
                  const SizedBox(width: 10),
                  // 프사에 상태를 얹는다 — 작업 중이면 테가 돌고, 기다리면 주황 테.
                  Hero(
                    tag: 'face-${pane.machine}-${pane.id}',
                    child: StatusRing(
                      style: st,
                      size: 40,
                      child: StudentFace(
                        slug: slug,
                        url: slug == null
                            ? null
                            : server.avatar(slug, machine: pane.machine),
                        shell: pane.isShell,
                        size: 40,
                      ),
                    ),
                  ),
                  const SizedBox(width: 10),
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        Row(
                          children: [
                            Flexible(
                              child: Text(
                                pane.displayName,
                                style: theme.textTheme.titleSmall,
                                overflow: TextOverflow.ellipsis,
                              ),
                            ),
                            if ((pane.session ?? '').isNotEmpty) ...[
                              const SizedBox(width: 6),
                              Flexible(child: SessionTag(pane.session!)),
                            ],
                            if ((pane.mirrorOf ?? '').isNotEmpty) ...[
                              const SizedBox(width: 6),
                              MirrorTag(pane.mirrorOf!),
                            ],
                          ],
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
                        if (pane.briefStatusParts.isNotEmpty)
                          PaneStatusLine(pane: pane, brief: true),
                      ],
                    ),
                  ),
                  const SizedBox(width: 8),
                  StatusChip(pane: pane),
                  const SizedBox(width: 10),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// PC 상태줄과 같은 조각들 — 하네스 로고 · 모델 · 브랜치 · 컨텍스트% · effort.
/// 컨텍스트가 많이 찼으면 그 숫자만 주황(경고색은 상태색과 같은 값).
