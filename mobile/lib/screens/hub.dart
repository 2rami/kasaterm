import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../app_release.dart';
import '../background_grace.dart';
import '../hub_model.dart';
import '../hub_prefs.dart';
import '../look.dart';
import '../machine_look.dart';
import '../server.dart';
import '../status_style.dart';
import '../student_art.dart';
import '../twins_loading.dart';
import '../weather/card.dart';
import '../weather/model.dart';
import '../weather/scene.dart';
import '../wide_layout.dart';
import 'clipboard_sheet.dart';
import 'controls.dart';
import 'notes_sheet.dart';
import 'pane_actions.dart';
import 'settings.dart';
import 'share_screen.dart';
import 'terminal.dart';

export '../machine_look.dart' show parseHexColor;

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

  /// 새 판을 묻지 못했거나(끊김·관리자 아님) 붙들어 주지 않는 옛 관문이면 이만큼 쉬고 다시 묻는다.
  static const releaseRetry = Duration(seconds: 30);

  @override
  State<HubScreen> createState() => _HubScreenState();
}

class _HubScreenState extends State<HubScreen> {
  late final HubModel _model = HubModel(widget.server, prefs: widget.prefs);

  /// 목록과 기기 색표 — 색표는 관문 기준 기기 설정에서 따로 온다.
  late final Listenable _shown = Listenable.merge([_model, machineLooks]);

  @override
  void initState() {
    super.initState();
    BackgroundGrace.instance.addListener(_graceChanged);
    _model.start();
    unawaited(_watchReleases());
  }

  /// 이 실행에서 이미 알린 빌드 — 앱으로 돌아올 때마다 같은 판을 또 알리지 않는다.
  String? _offered;

  // 앞에 있는 동안 관문의 「판이 바뀌면 곧바로 답하는」 길에 늘 하나 매달려 있다 — 쓰는 중에 올라온 판도 그 순간 알린다.
  /// 지금 도는 지켜보기의 번호 — 뒤로 가면 올려 앞 것을 멈춘다.
  int _releaseRun = 0;
  Timer? _releaseNap;

  Future<void> _watchReleases() async {
    final run = ++_releaseRun;
    String? seen;
    while (mounted && run == _releaseRun) {
      final w = await widget.server.watchRelease(have: seen);
      if (!mounted || run != _releaseRun) return;
      if (w != null) {
        seen = w.release?.build ?? '';
        final r = w.release;
        if (r != null) _offerRelease(r);
      }
      if (w == null || !w.waits) {
        final nap = Completer<void>();
        _releaseNap = Timer(HubScreen.releaseRetry, nap.complete);
        await nap.future;
      }
    }
  }

  void _stopWatchingReleases() {
    _releaseRun++;
    _releaseNap?.cancel();
  }

  void _offerRelease(AppRelease r) {
    if (!r.newer || r.build == _offered) return;
    _offered = r.build;
    ScaffoldMessenger.of(context).showSnackBar(SnackBar(
      content: Text('새 판 ${r.version} (${r.build})이 있어요'),
      duration: const Duration(seconds: 10),
      action: SnackBarAction(label: '설치', onPressed: () => unawaited(installRelease(context, r, from: '허브 띠'))),
    ));
  }

  @override
  void dispose() {
    BackgroundGrace.instance.removeListener(_graceChanged);
    _stopWatchingReleases();
    _model.dispose();
    super.dispose();
  }

  /// 다른 앱에 다녀오는 동안은 목록 받기·롱폴을 그대로 둔다 — 그 시간이 다 됐을 때만 닫는다.
  void _graceChanged() {
    if (BackgroundGrace.instance.live) {
      _model.start();
      unawaited(_watchReleases());
    } else {
      _model.stop();
      _stopWatchingReleases();
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
  );

  Future<void> _newRoom(HubSection s) => newRoom(
    context,
    server: widget.server,
    machine: s.route,
    onChanged: _model.refresh,
  );

  void _openShare() {
    Navigator.of(context).push(
      MaterialPageRoute<void>(builder: (_) => ShareScreen(server: widget.server)),
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

  /// 새로 고치는 중 — 앱바 쌍둥이가 뛴다.
  bool _refreshing = false;

  Future<void> _refresh() async {
    setState(() => _refreshing = true);
    try {
      await _model.refresh();
    } finally {
      if (mounted) setState(() => _refreshing = false);
    }
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: _shown,
    builder: (context, _) {
      final theme = Theme.of(context);
      return TwinBackdrop(
        child: Scaffold(
          backgroundColor: Colors.transparent,
          appBar: AppBar(
            backgroundColor: Colors.transparent,
            // 제목 글자 없이 쌍둥이 표 — 목록이 학생이라는 것은 화면이 말한다(2026-10-01).
            title: TwinsMark(hopping: _refreshing || _model.showingCached),
            actions: [
              // 클립보드 = 데스크톱 「최근 복사」. 폰으로 가져오거나 폰 것을 올린다
              // (2026-09-10 지시 「카사텀 pc 에도 붙고 폰에도 붙게」).
              IconButton(
                tooltip: 'KASA-share',
                onPressed: _openShare,
                icon: const Icon(Icons.folder_outlined),
              ),
              IconButton(
                tooltip: '클립보드',
                onPressed: () =>
                    showClipboardSheet(context, server: widget.server),
                icon: const Icon(Icons.content_paste_outlined),
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
              WeatherScene(child: _body(theme)),
              // 지난번 목록을 먼저 그렸다 — 새 목록이 닿을 때까지 위에 얇게 「확인 중」.
              if (_model.showingCached)
                const Positioned(top: 0, left: 0, right: 0, child: TwinBar()),
            ],
          ),
        ),
      );
    },
  );

  Widget _body(ThemeData theme) {
    final sections = _model.visible;
    final shape = _model.view.shape;
    final children = <Widget>[];
    if (_model.error != null) {
      children.add(ErrorBand(text: _model.error!, onRetry: _refresh));
    }
    if (sections.isEmpty && _model.error == null) {
      children.add(
        const Padding(
          padding: EdgeInsets.only(top: Look.groupGap * 2),
          child: TwinsLoading(label: '학생 목록을 받는 중', size: Look.twinsSmall),
        ),
      );
    }
    for (final s in sections) {
      final title = s.machine ?? _model.rootName ?? '이 기계';
      final tint = machineColor(title, local: s.machine == null);
      final icon = machineIcon(title);
      final folded = _model.view.isFolded(s.machine);
      children.add(
        _SectionHeader(
          title: machineName(title, route: s.route, local: s.machine == null),
          icon: icon,
          color: tint,
          root: s.machine == null,
          online: s.online,
          folded: folded,
          count: s.studentCount,
          onTap: () => _model.toggleFold(s),
          onAdd: s.online ? () => _newRoom(s) : null,
        ),
      );
      if (folded) continue;
      if (s.online && s.rooms.isEmpty) {
        children.add(
          TwinsNotice(
            text: '아직 학생이 없어요',
            action: FilledButton.icon(
              onPressed: () => _newRoom(s),
              icon: const Icon(Icons.add, size: Look.iconSize),
              label: const Text('새 방'),
            ),
          ),
        );
      }
      // 아이패드·가로 화면에선 방 상자가 여러 열로 선다 — 한 방이 화면 폭을 다 먹으면
      // 한 칸짜리 방의 지도가 화면 반을 차지한다.
      final rooms = <Widget>[];
      for (final room in s.rooms) {
        final inside = <Widget>[
          _RoomHeader(
            title: room.title,
            icon: icon,
            color: tint,
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
        rooms.add(
          _RoomBox(
            child: WeatherCard(
              id: 'room:${s.machine ?? ''}|${room.title}',
              mood: moodOf(
                room.panes.map(
                  (p) => weatherMood(StatusStyle.of(p, theme.colorScheme).mood),
                ),
              ),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: inside,
              ),
            ),
          ),
        );
      }
      if (rooms.isNotEmpty) children.add(Masonry(children: rooms));
    }
    // 당기면 쌍둥이가 내려온다 — 머티리얼 빙글이 대신(design.md 「쌍둥이 결」).
    return CustomScrollView(
      physics: twinsScroll,
      slivers: [
        twinsRefreshSliver(_refresh),
        SliverPadding(
          padding: const EdgeInsets.fromLTRB(
            Look.pagePad,
            0,
            Look.pagePad,
            Look.groupGap,
          ),
          sliver: SliverList.list(children: children),
        ),
      ],
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
    // 고른 값은 기기 제 이름으로 적어 두고(보기 기억이 이름을 바꿔도 안 풀린다) 계정 이름으로 보인다.
    final machines = [
      for (final s in model.sections)
        if (s.machine != null) (s.machine!, machineName(s.machine!, route: s.route)),
    ];
    final rootName = machineName(model.rootName ?? '이 기계', local: true);
    final current = view.machine == null
        ? '전체'
        : (view.machine!.isEmpty
              ? rootName
              : machines.where((m) => m.$1 == view.machine).firstOrNull?.$2 ?? view.machine!);
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
        for (final (label, name) in machines)
          _pick(name, view.machine == label, () {
            model.setView(view.copyWith(machine: label));
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

/// 기기 머리글 — 기기색 물 동그라미 안 아이콘 · 이름(16/600) · 「기준 기기 · N명」 두 줄과 그 아래 기기색 2px 알약 선.
/// 데스크톱 사이드바 기기 머리(기기색 아이콘 + 굵은 이름 + 사정 한 줄)와 같은 모양이고, 선이 기기 경계다 — 흐린 묶음
/// 제목 한 줄로는 어디서 다른 기기 학생이 시작하는지 안 보였다(2026-10-01 지적). 기준 기기 칸도 같은 머리를 단다.
class _SectionHeader extends StatelessWidget {
  const _SectionHeader({
    required this.title,
    required this.icon,
    required this.color,
    this.root = false,
    this.online = true,
    this.folded = false,
    this.count = 0,
    this.onTap,
    this.onAdd,
  });

  final String title;
  final IconData icon;

  /// 그 기기의 색 — 데스크톱 설정 「기기 색」 그대로.
  final Color color;

  /// 관문이 고른 기준 기기(주소가 가리키는 기기).
  final bool root;
  final bool online;
  final bool folded;
  final int count;

  /// 머리글 자체를 누르면 접고 편다. 「새 방」 단추는 자기 탭을 먼저 먹는다.
  final VoidCallback? onTap;

  /// 「새 방」 — 그 기계에 빈 창 하나. 안 닿는 기계엔 안 단다.
  final VoidCallback? onAdd;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final dim = scheme.onSurfaceVariant;
    final sub = theme.textTheme.bodySmall;
    final line = online ? color : scheme.outline;
    final wash = Color.alphaBlend(
      line.withValues(alpha: 0.16),
      theme.scaffoldBackgroundColor,
    );
    return Padding(
      padding: EdgeInsets.only(
        top: Look.groupGap,
        bottom: folded ? 0 : Look.cardGap,
      ),
      child: InkWell(
        onTap: onTap,
        borderRadius: Look.corners,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            SizedBox(
              height: Look.row2,
              child: Row(
                children: [
                  Container(
                    width: Look.machineBadge,
                    height: Look.machineBadge,
                    decoration: BoxDecoration(
                      color: wash,
                      shape: BoxShape.circle,
                    ),
                    child: Icon(
                      icon,
                      size: Look.iconSize,
                      color: machineInk(line, wash),
                    ),
                  ),
                  const SizedBox(width: 12),
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        Text(
                          title,
                          style: theme.textTheme.titleMedium,
                          overflow: TextOverflow.ellipsis,
                        ),
                        const SizedBox(height: Look.rowGap),
                        Text.rich(
                          TextSpan(
                            children: [
                              if (!online)
                                TextSpan(
                                  text: '연결 안 됨',
                                  style: TextStyle(color: scheme.error),
                                )
                              else
                                TextSpan(
                                  text: '${root ? '기준 기기' : '연결됨'} · 학생 $count',
                                ),
                            ],
                          ),
                          style: sub?.copyWith(color: dim),
                          overflow: TextOverflow.ellipsis,
                        ),
                      ],
                    ),
                  ),
                  if (onAdd != null)
                    IconButton(
                      tooltip: '새 방',
                      onPressed: onAdd,
                      icon: Icon(Icons.add, color: dim),
                    ),
                  AnimatedRotation(
                    turns: folded ? -0.25 : 0,
                    duration: const Duration(milliseconds: 160),
                    child: Icon(
                      Icons.expand_more,
                      size: Look.iconSize,
                      color: dim,
                    ),
                  ),
                ],
              ),
            ),
            Container(
              height: Look.twinBarH,
              decoration: BoxDecoration(
                color: line,
                borderRadius: BorderRadius.circular(Look.twinBarH),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// 방 하나 = 둥근 판 하나(쌍둥이 결). 날씨 유리가 판 모서리 밖으로 안 나가게 같은 모서리로 자른다.
class _RoomBox extends StatelessWidget {
  const _RoomBox({required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) => Container(
    margin: const EdgeInsets.only(bottom: Look.cardGap),
    decoration: TwinTone.of(context).cardBox(),
    child: ClipRRect(borderRadius: Look.cardCorners, child: child),
  );
}

/// 방 제목 줄 44 — 기기색 아이콘 · 방 이름 15/600 · 경로 13 흐림. 서버는 「이름 · 경로」 한 줄로 준다.
/// 아이콘은 머리글이 화면 위로 지나간 뒤에도 어느 기기 방인지 말한다.
class _RoomHeader extends StatelessWidget {
  const _RoomHeader({
    required this.title,
    required this.icon,
    required this.color,
    this.onMenu,
  });

  final String title;
  final IconData icon;
  final Color color;

  /// 방 메뉴(pane 추가·이름·닫기). 안 닿는 기계엔 안 단다.
  final VoidCallback? onMenu;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final cut = title.indexOf(' · ');
    final name = cut < 0 ? title : title.substring(0, cut);
    final path = cut < 0 ? '' : title.substring(cut + 3);
    return Container(
      height: Look.roomHeadH,
      padding: const EdgeInsets.only(left: Look.cardPad, right: 4),
      child: Row(
        children: [
          Icon(
            icon,
            size: Look.iconSize,
            color: machineInk(color, TwinTone.of(context).card),
          ),
          const SizedBox(width: 8),
          Flexible(
            child: Text(
              name,
              style: theme.textTheme.titleSmall,
              overflow: TextOverflow.ellipsis,
            ),
          ),
          if (path.isNotEmpty) ...[
            const SizedBox(width: 8),
            Expanded(
              child: Text(
                path,
                style: theme.textTheme.bodySmall?.copyWith(
                  color: theme.colorScheme.onSurfaceVariant,
                ),
                overflow: TextOverflow.ellipsis,
              ),
            ),
          ] else
            const Spacer(),
          if (onMenu != null)
            IconButton(
              tooltip: '방 메뉴',
              onPressed: onMenu,
              icon: Icon(
                Icons.more_horiz,
                color: theme.colorScheme.onSurfaceVariant,
              ),
            ),
        ],
      ),
    );
  }
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
      padding: const EdgeInsets.fromLTRB(Look.cardGap, 0, Look.cardGap, Look.cardGap),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          // 지도가 첫 화면을 먹으면 학생 줄이 화면 절반 아래로 밀린다(475pt) — 높이에 상한.
          LayoutBuilder(
            builder: (context, box) => SizedBox(
              height: math.min(
                box.maxWidth / (room.aspect ?? _defaultAspect).clamp(1.0, 3.2),
                Look.isPad(context) ? Look.mapMaxPad : Look.mapMaxPhone,
              ),
              child: _board(scheme),
            ),
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
        borderRadius: Look.corners,
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
                  server: widget.server,
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
            color: accent.withValues(alpha: 0.12),
            borderRadius: Look.smallCorners,
            border: waiting ? Border.all(color: edge, width: 1.5) : null,
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
                      bottom: 3,
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
                ],
              ),
            ),
          ),
        );
        // 작업 중은 학생색 빛 조각이 칸 윤곽을 한 바퀴씩 돈다 — 바닥에 흐르던 막대를 걷었다(2026-10-07).
        // 기다림의 고정 테(상태색)와는 상태가 배타적이라 같이 서지 않는다.
        final cell = OrbitEdge(
          live: busy && !waiting,
          color: accent,
          radius: Look.smallCorners,
          child: front,
        );
        if (!tabbed) return cell;
        // 좌우로 쓸면 다음·이전 탭. 겹친 뒷장은 두지 않는다 — 몇째인지는 밑의 점이
        // 말하고, 칸이 작아 덱까지 들어가면 얼굴이 밀린다(2026-09-08 지시).
        return GestureDetector(
          behavior: HitTestBehavior.opaque,
          onHorizontalDragEnd: (d) {
            final v = d.primaryVelocity ?? 0;
            if (v.abs() < 120) return;
            _flip(v < 0 ? 1 : -1);
          },
          child: cell,
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
                        server: server,
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
    final slug = pane.slug;
    final st = StatusStyle.of(pane, scheme);
    // 왼쪽 알약 띠는 데스크톱 사이드바와 같은 뜻 — 내 차례만 주황. 하는 중은 띠가 아니라 줄 윤곽을 빛
    // 조각이 돈다(지도 칸·데스크톱 목록 줄과 같은 결, 2026-10-07).
    final stripe = st.needsYou ? StatusStyle.attention : null;
    // 글은 세션 이름과 도는 시간뿐 — 데스크톱 사이드바 목록 줄과 같다(2026-10-01). 학생 이름은
    // 얼굴이, 상태는 띠와 도는 윤곽이 말한다.
    final time = st.live ? elapsedLabel(pane.busySecs) : null;
    return OrbitEdge(
      live: st.live,
      color: scheme.primary,
      radius: Look.smallCorners,
      child: InkWell(
        onTap: onTap,
        onLongPress: onLongPress,
        child: CustomPaint(
          // 줄 사이 선은 글자 시작점부터 — 판 안의 안쪽 선(design.md 「행」).
          painter: _InsetLine(scheme.outline, Look.cardPad + Look.face + 12),
          child: Stack(
            children: [
              if (stripe != null)
                Positioned(
                  left: Look.stripeX,
                  top: 0,
                  bottom: 0,
                  child: Center(
                    child: Container(
                      width: Look.stripe,
                      height: Look.stripeH,
                      decoration: BoxDecoration(
                        color: stripe,
                        borderRadius: BorderRadius.circular(Look.stripe),
                      ),
                    ),
                  ),
                ),
              Container(
                constraints: const BoxConstraints(minHeight: Look.row2),
                padding: const EdgeInsets.fromLTRB(
                  Look.cardPad,
                  8,
                  Look.cardPad,
                  8,
                ),
                child: Row(
                  children: [
                    Hero(
                      tag: 'face-${pane.machine}-${pane.id}',
                      child: StudentFace(
                        server: server,
                        slug: slug,
                        url: slug == null
                            ? null
                            : server.avatar(slug, machine: pane.machine),
                        shell: pane.isShell,
                        size: Look.face,
                      ),
                    ),
                    const SizedBox(width: 12),
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          Row(
                            children: [
                              Flexible(
                                child: Text(
                                  pane.rowTitle,
                                  style: theme.textTheme.titleSmall,
                                  maxLines: 1,
                                  overflow: TextOverflow.ellipsis,
                                ),
                              ),
                              if ((pane.mirrorOf ?? '').isNotEmpty) ...[
                                const SizedBox(width: 6),
                                MirrorTag(pane.mirrorOf!),
                              ],
                            ],
                          ),
                          const SizedBox(height: Look.rowGap),
                          // 둘째 줄 자리는 늘 잡아 둔다 — 일이 시작·끝날 때마다 이름이 위아래로 튀지 않게.
                          SizedBox(
                            height: Look.subLine,
                            child: Row(
                              children: [
                                const Spacer(),
                                if (time != null) ...[
                                  const SizedBox(width: 6),
                                  Text(
                                    time,
                                    style: elapsedStyle(pane.busySecs!, theme),
                                  ),
                                ],
                              ],
                            ),
                          ),
                        ],
                      ),
                    ),
                  ],
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// 위쪽 1px 선을 [inset] 부터 오른쪽 끝까지.
class _InsetLine extends CustomPainter {
  const _InsetLine(this.color, this.inset);

  final Color color;
  final double inset;

  @override
  void paint(Canvas canvas, Size size) {
    canvas.drawRect(
      Rect.fromLTWH(inset, 0, size.width - inset, 1),
      Paint()..color = color,
    );
  }

  @override
  bool shouldRepaint(_InsetLine old) =>
      old.color != color || old.inset != inset;
}

/// PC 상태줄과 같은 조각들 — 하네스 로고 · 모델 · 브랜치 · 컨텍스트% · effort.
/// 컨텍스트가 많이 찼으면 그 숫자만 주황(경고색은 상태색과 같은 값).
