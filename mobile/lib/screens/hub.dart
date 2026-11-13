import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../app_release.dart';
import '../connection.dart';
import '../background_grace.dart';
import '../hub_model.dart';
import '../conversation_cache.dart';
import '../hub_actions.dart';
import '../hub_prefs.dart';
import '../hub_snapshot.dart';
import '../look.dart';
import '../machine_look.dart';
import '../server.dart';
import '../shared_rooms_repository.dart';
import '../status_style.dart';
import '../student_art.dart';
import '../student_walk.dart';
import '../twins_loading.dart';
import '../weather/card.dart';
import '../weather/model.dart';
import '../weather/scene.dart';
import '../wide_layout.dart';
import 'controls.dart';
import 'notes_sheet.dart';
import 'offline_hub.dart';
import 'pane_actions.dart';
import 'settings.dart';
import 'shared_rooms.dart';
import 'terminal.dart';
import 'desktop_tools.dart';
import 'desktop_controls.dart';
import 'hub_drag.dart';

export '../machine_look.dart' show parseHexColor;

part 'hub_interactions.dart';

/// 첫 화면 — 기계·방별 학생 목록. 기다리는 학생이 맨 위에 선다.
class HubScreen extends StatefulWidget {
  const HubScreen({
    super.key,
    required this.server,
    required this.onChangeAddress,
    this.prefs,
    this.shared,
    this.snapshots,
    this.offline = false,
    this.connection,
  });

  final Server server;
  final Future<void> Function() onChangeAddress;

  /// 닿는 데스크톱이 없다 — 지난 목록만 흐리게 보이고, 학생을 누르면 저장한 대화를 연다. 연결이 서면 새 허브로 바뀐다.
  final bool offline;

  /// 계정 연결 — 끊긴 허브의 띠·계정 시트가 로그인 상태·기기 목록·다시 확인을 여기서 읽는다.
  final ConnectionController? connection;

  /// 지난 목록 저장소. 없으면 앱 전체의 것.
  final HubSnapshots? snapshots;

  /// 계정의 공유방 — 기기 목록과 별도 묶음으로 아래에 선다. 옛 주소 연결이면 없다.
  final SharedRoomsRepository? shared;

  /// 보기 설정 저장소. 없으면(테스트) 고른 것이 이 화면에서만 산다.
  final HubPrefs? prefs;

  /// 새 판을 묻지 못했거나(끊김·관리자 아님) 붙들어 주지 않는 옛 관문이면 이만큼 쉬고 다시 묻는다.
  static const releaseRetry = Duration(seconds: 30);

  @override
  State<HubScreen> createState() => _HubScreenState();
}

class _HubScreenState extends State<HubScreen> with HubInteractions {
  late final HubModel _model = HubModel(
    widget.server,
    prefs: widget.prefs,
    snapshots: widget.snapshots,
    offline: widget.offline,
  );
  @override
  HubModel get hubModel => _model;

  /// 목록과 기기 색표 — 색표는 관문 기준 기기 설정에서 따로 온다.
  late final Listenable _shown = Listenable.merge([_model, machineLooks]);

  @override
  void initState() {
    super.initState();
    BackgroundGrace.instance.addListener(_graceChanged);
    _model.addListener(_warmConversations);
    _model.start();
    _warmConversations();
    if (!widget.offline) unawaited(_watchReleases());
  }

  void _warmConversations() {
    ConversationCache.of(widget.server).warm([
      for (final section in _model.visible.where((s) => s.online))
        for (final room in section.rooms) ...room.panes,
    ]);
  }

  /// 이 실행에서 이미 알린 빌드 — 앱으로 돌아올 때마다 같은 판을 또 알리지 않는다.
  String? _offered;

  // 앞에 있는 동안 관문의 「판이 바뀌면 곧바로 답하는」 길에 늘 하나 매달려 있다 — 쓰는 중에 올라온 판도 그 순간 알린다.
  /// 지금 도는 지켜보기의 번호 — 뒤로 가면 올려 앞 것을 멈춘다.
  int _releaseRun = 0;
  Timer? _releaseNap;
  Completer<void>? _releaseWake;

  Future<void> _watchReleases() async {
    _stopWatchingReleases();
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
        _releaseWake = nap;
        _releaseNap = Timer(HubScreen.releaseRetry, nap.complete);
        await nap.future;
      }
    }
  }

  void _stopWatchingReleases() {
    _releaseRun++;
    _releaseNap?.cancel();
    _releaseNap = null;
    final wake = _releaseWake;
    _releaseWake = null;
    if (wake != null && !wake.isCompleted) wake.complete();
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
    _model.removeListener(_warmConversations);
    _model.dispose();
    super.dispose();
  }

  /// 다른 앱에 다녀오는 동안은 목록 받기·롱폴을 그대로 둔다 — 그 시간이 다 됐을 때만 닫는다.
  void _graceChanged() {
    if (BackgroundGrace.instance.live) {
      _model.start();
      if (!widget.offline) unawaited(_watchReleases());
    } else {
      _model.stop();
      _stopWatchingReleases();
    }
  }

  @override
  void _open(Pane pane) {
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => TerminalScreen(server: widget.server, pane: pane),
      ),
    );
  }

  @override
  Future<void> _paneSheet(HubSection s, HubRoom room, Pane p) => showPaneSheet(
    context,
    server: widget.server,
    room: room,
    rooms: s.rooms,
    pane: p,
    machine: s.route,
    onOpen: _open,
    onChanged: _model.refresh,
    onHide: () => _hidePanes([p]),
    onSelect: () => _select([p]),
    onSubagents: () => _showSubagents(p),
  );

  Future<void> _roomSheet(HubSection s, HubRoom room) => showRoomSheet(
    context,
    server: widget.server,
    room: room,
    machine: s.route,
    onChanged: _model.refresh,
    onHide: () => _hidePanes(room.panes),
  );

  Future<void> _newRoom(HubSection s) => newRoom(
    context,
    server: widget.server,
    machine: s.route,
    onChanged: _model.refresh,
  );

  void _openAccount() {
    final connection = widget.connection;
    if (connection == null) return;
    unawaited(showAccountSheet(context, connection: connection, shared: widget.shared));
  }

  void _openFrozen(Pane pane) => unawaited(openFrozenPane(context, widget.server, pane));

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
      await (widget.offline ? (widget.connection?.retry() ?? Future<void>.value()) : _model.refresh());
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
            title: TwinsMark(
              hopping: _refreshing || _model.showingCached || widget.connection?.phase == ConnectionPhase.checking,
            ),
            actions: [
              // 끊긴 허브엔 데스크톱이 받아야 하는 쪽지를 달지 않는다. 설정 자리는 계정·기기 시트(저장한 대화 입구 포함)다.
              if (widget.offline) ...[
                _ViewMenu(model: _model),
                IconButton(
                  tooltip: '계정·연결',
                  onPressed: _openAccount,
                  icon: const Icon(Icons.manage_accounts_outlined),
                ),
              ] else ...[
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
            ],
          ),
          body: Stack(
            fit: StackFit.expand,
            children: [
              WeatherScene(child: _body(theme)),
              // 지난번 목록을 먼저 그렸다 — 새 목록이 닿을 때까지 위에 얇게 「확인 중」.
              if (_model.showingCached)
                const Positioned(top: 0, left: 0, right: 0, child: TwinBar()),
              if (_manipulating)
                const Positioned(top: 0, left: 0, right: 0, child: LinearProgressIndicator()),
              if (_dragData != null || _chosen.isNotEmpty)
                Positioned(left: 12, right: 12, bottom: 8, child: SafeArea(
                  child: HubActionDock(
                    count: _chosenPanes.length, drag: _dragData, busy: _manipulating,
                    onAction: _dropAction,
                    onCancel: () { _dragEnd(); setState(_chosen.clear); },
                  ),
                )),
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
    if (_model.view.hidden.isNotEmpty) {
      children.add(TextButton.icon(
        key: const Key('hub-hidden'),
        onPressed: _showHidden, icon: const Icon(Icons.visibility_off_outlined),
        label: Text('숨긴 칸 ${_allHubPanes.where((p) => _model.view.hidden.contains(p.viewKey)).length}개 · 다시 보이기'),
      ));
    }
    if (widget.offline && widget.connection != null) {
      children.add(OfflineBand(connection: widget.connection!, onOpen: _openAccount));
    }
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
          seenAt: s.seenAt,
          checking: widget.offline && widget.connection?.phase == ConnectionPhase.checking,
          folded: folded,
          count: s.studentCount,
          onTap: () => _model.toggleFold(s),
          onAdd: s.online ? () => _newRoom(s) : null,
          onScreen: s.online
              ? () => openDesktopTools(context, server: widget.server, machine: s.route,
                  title: machineName(title, route: s.route, local: s.machine == null))
              : null,
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
      for (final sourceRoom in s.rooms) {
        final room = sourceRoom.visible(_model.view.hidden);
        if (room.panes.isEmpty && sourceRoom.panes.isNotEmpty) continue;
        final inside = <Widget>[
          _RoomHeader(
            title: room.title,
            icon: icon,
            color: tint,
            onMenu: s.online ? () => _roomSheet(s, sourceRoom) : () => explainOffline(context, '방을 고칠'),
          ),
        ];
        final hasMap = room.rects.isNotEmpty && shape != HubShape.list;
        if (hasMap) {
          inside.add(
            _MiniMap(
              server: widget.server,
              room: room,
              // 끊긴 방은 끌기·고르기 없이 누르면 저장한 대화 — 닿지 않는 기기에 화면을 열지 않는다.
              onOpen: !s.online ? _openFrozen : _tapPane,
              wrapPane: s.online ? (p, child) => _wrapPane(p, child, online: true) : null,
              onSubagents: _showSubagents,
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
                child: s.online
                    ? _wrapPane(p, _PaneTile(server: widget.server, pane: p, onTap: () => _tapPane(p)), online: true)
                    : _PaneTile(server: widget.server, pane: p, onTap: () => _openFrozen(p)),
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
      // 웹 터미널·도구가 연 셸은 데스크톱 방 밖에 산다 — 첫 방에 섞으면 정체 모를 「셸」로 보인다.
      final webShells = s.webShells.where((p) => !_model.view.hidden.contains(p.viewKey)).toList();
      if (webShells.isNotEmpty) {
        rooms.add(
          _RoomBox(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                _RoomHeader(
                  title: '창 밖 셸 · 데스크톱 창에 없는 셸',
                  icon: icon,
                  color: tint,
                ),
                for (final (i, p) in webShells.indexed)
                  Appear(
                    key: ValueKey('web-${p.machine}-${p.id}'),
                    delayIndex: i,
                    child: _PaneTile(
                      server: widget.server,
                      pane: p,
                      onTap: s.online ? () => _open(p) : () => _openFrozen(p),
                    ),
                  ),
              ],
            ),
          ),
        );
      }
      if (rooms.isNotEmpty) {
        final board = Masonry(children: rooms);
        children.add(s.online ? board : Opacity(key: ValueKey('stale-${s.route ?? ''}'), opacity: Look.staleFade, child: board));
      }
    }
    if (widget.shared case final shared?) children.add(_SharedSection(repository: shared));
    // 당기면 쌍둥이가 내려온다 — 머티리얼 빙글이 대신(design.md 「쌍둥이 결」).
    return CustomScrollView(
      key: _scrollViewport,
      controller: _hubScroll,
      physics: twinsScroll,
      slivers: [
        twinsRefreshSliver(_refresh),
        SliverPadding(
          padding: EdgeInsets.fromLTRB(
            Look.pagePad,
            0,
            Look.pagePad,
            Look.groupGap + (_dragData != null || _chosen.isNotEmpty ? 160 : 0),
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
    this.onScreen,
    this.detail,
    this.seenAt,
    this.checking = false,
    this.addTip = '새 방',
  });

  /// 끊긴 기기를 마지막으로 본 때 — 「끊김 · 15:32」.
  final DateTime? seenAt;

  /// 앱을 켜고 아직 데스크톱을 확인하는 중 — 끊겼다고 단정하지 않고 「확인 중 · 15:32」.
  final bool checking;

  final String title;
  final IconData icon;

  /// 기기가 아닌 묶음(공유방)의 둘째 줄. 없으면 기기 사정을 쓴다.
  final String? detail;
  final String addTip;

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

  /// 「화면 거울」 — 그 기기의 화면을 본다. 안 닿는 기계엔 안 단다.
  final VoidCallback? onScreen;

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
              height: Look.row2 * math.max(1, MediaQuery.textScalerOf(context).scale(14) / 14),
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
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                        ),
                        const SizedBox(height: Look.rowGap),
                        Text.rich(
                          TextSpan(
                            children: [
                              if (detail != null)
                                TextSpan(text: detail)
                              else if (!online) ...[
                                TextSpan(
                                  text: checking ? '확인 중' : '끊김',
                                  style: checking ? null : TextStyle(color: scheme.error),
                                ),
                                TextSpan(text: ' · ${lastSeenLabel(seenAt)}'),
                              ]
                              else
                                TextSpan(
                                  text: '${root ? '기준 기기' : '연결됨'} · 학생 $count',
                                ),
                            ],
                          ),
                          style: sub?.copyWith(color: dim),
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                        ),
                      ],
                    ),
                  ),
                  if (onScreen != null)
                    IconButton(
                      tooltip: 'PC 기능',
                      onPressed: onScreen,
                      icon: Icon(Icons.more_horiz, color: dim),
                    ),
                  if (onAdd != null)
                    IconButton(
                      tooltip: addTip,
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

/// 공유방 묶음 — 기기 목록 아래 따로 선다. 방을 보거나 참여해도 셸·창을 만들지 않는다(docs/shared-rooms.md).
class _SharedSection extends StatefulWidget {
  const _SharedSection({required this.repository});
  final SharedRoomsRepository repository;
  @override
  State<_SharedSection> createState() => _SharedSectionState();
}

class _SharedSectionState extends State<_SharedSection> {
  bool _folded = false;

  @override
  Widget build(BuildContext context) => SharedRoomsWatch(
    every: const Duration(seconds: 30),
    onTick: widget.repository.load,
    child: ListenableBuilder(
      listenable: widget.repository,
      builder: (context, _) {
        final repo = widget.repository;
        if (repo.locked) return const SizedBox.shrink();
        final scheme = Theme.of(context).colorScheme;
        final mine = repo.rooms.where((r) => r.owner).length;
        return Column(
          key: const Key('hub-shared-rooms'),
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            _SectionHeader(
              title: '공유방',
              icon: Icons.groups_outlined,
              color: scheme.primary,
              folded: _folded,
              detail: repo.loading && repo.rooms.isEmpty ? '받는 중' : '내 방 $mine · 참여 ${repo.rooms.length - mine}',
              onTap: () => setState(() => _folded = !_folded),
              addTip: '새 공유방',
              onAdd: () => createSharedRoom(context, repo),
            ),
            if (!_folded)
              _RoomBox(
                child: Material(
                  type: MaterialType.transparency,
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    children: [
                      if (repo.error != null && repo.rooms.isEmpty)
                        Padding(
                          padding: const EdgeInsets.all(Look.cardPad),
                          child: Text(repo.error!, style: Theme.of(context).textTheme.bodySmall?.copyWith(color: scheme.error)),
                        ),
                      for (final (i, r) in repo.rooms.indexed)
                        SharedRoomRow(tone: i, room: r, onTap: () => openSharedRoom(context, repo, r.id)),
                      SettingsRow(
                        tone: repo.rooms.length,
                        icon: Icons.login,
                        title: '초대 코드로 참여',
                        onTap: () => joinSharedRoom(context, repo),
                      ),
                    ],
                  ),
                ),
              ),
          ],
        );
      },
    ),
  );
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
    this.wrapPane,
    this.onSubagents,
  });

  final Server server;
  final HubRoom room;
  final void Function(Pane)? onOpen;

  final Widget Function(Pane, Widget)? wrapPane;
  final void Function(Pane)? onSubagents;

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
          for (final pane in room.panes.where((p) => p.hasRunningSubagents))
            Padding(
              padding: const EdgeInsets.only(top: 6),
              child: Material(
                color: StatusStyle.success.withValues(alpha: .08),
                shape: RoundedRectangleBorder(borderRadius: Look.corners,
                  side: const BorderSide(color: StatusStyle.success)),
                child: ListTile(
                  key: ValueKey('subagents-${pane.viewKey}'),
                  dense: true,
                  leading: Icon(Icons.account_tree_outlined, color: StatusStyle.successText(scheme)),
                  title: Text('${pane.rowTitle} · 하위 작업 ${pane.subagents.length}개'),
                  subtitle: Text(pane.subagents.take(2).join('\n'), maxLines: 3, overflow: TextOverflow.ellipsis),
                  onTap: () => onSubagents?.call(pane),
                ),
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
                      wrapPane: wrapPane,
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
                      wrapPane: wrapPane,
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
                    wrapPane: wrapPane,
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
    this.wrapPane,
  });

  final Server server;
  final Pane? pane;

  /// 이 자리의 탭들(순서대로). 데스크톱 pane 머리의 탭 줄과 같은 것 — 탭 안에 있는
  /// 학생도 지도에 보여야 한다(2026-09-08 지시). 빈 목록이면 탭이 하나다.
  final List<Pane?> tabs;
  final int? tabActive;
  final void Function(Pane)? onOpen;
  final Widget Function(Pane, Widget)? wrapPane;

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
    final childrenRunning = p?.hasRunningSubagents ?? false;
    // 뒤에서 도는 중 — 이 학생의 백그라운드 일이거나, 같은 칸 뒤 탭의 학생이 일하는 중(데스크톱
    // `bg_tab_busy`). 일하는 중·기다림·서브에이전트 표시가 먼저다.
    final behind = !busy && !waiting && !childrenRunning &&
        ((p?.bgActive ?? false) || tabs.any((t) => t != null && t.id != p?.id && t.isBusy));
    // 칸 바탕은 학생색, 테두리·점은 상태색 — 「누구」와 「어떤 상태」를 다른 채널로.
    final edge = childrenRunning ? StatusStyle.success : waiting ? StatusStyle.attention : accent;
    return LayoutBuilder(
      builder: (context, box) {
        final ch = box.maxHeight;
        final roomy = box.maxWidth >= 64 && ch >= 44;
        final dots = tabbed && ch >= 36;
        final face = math.min((ch - (dots ? 8 : 0)) * 0.55, 30.0);
        Widget portrait(Pane q) {
          final still = StudentFace(
            server: widget.server,
            slug: q.slug,
            url: q.slug == null
                ? null
                : widget.server.avatar(q.slug!, machine: q.machine),
            shell: q.isShell,
            size: face,
          );
          // 하는 중인 학생은 데스크톱 배치도 칸처럼 얼굴과 같은 상자에서 걷는다.
          return busy && q.slug != null && !q.isShell
              ? StudentWalk(
                  server: widget.server,
                  slug: q.slug!,
                  machine: q.machine,
                  size: face,
                  fallback: still,
                )
              : still;
        }
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
              if (q != null && face >= 14) portrait(q),
              if (q != null && roomy)
                Padding(
                  padding: const EdgeInsets.only(top: 2),
                  child: Text(
                    q.mapTitle,
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
            border: waiting || childrenRunning ? Border.all(color: edge, width: childrenRunning ? 2 : 1.5) : null,
          ),
          clipBehavior: Clip.antiAlias,
          child: Material(
            color: Colors.transparent,
            child: InkWell(
              onTap: p == null || widget.onOpen == null
                  ? null
                  : () => widget.onOpen!(p),
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
                  if (childrenRunning && ch >= 32)
                    Positioned(top: 3, left: 3, child: IgnorePointer(child: Semantics(
                      label: '서브에이전트 ${p!.subagents.length}개 실행 중',
                      child: Container(
                        key: ValueKey('subagent-badge-${p.viewKey}'),
                        padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 1),
                        decoration: BoxDecoration(color: scheme.surface, borderRadius: BorderRadius.circular(6)),
                        child: Text('${p.subagents.length}', style: TextStyle(
                          fontSize: 12, fontWeight: FontWeight.w600, color: StatusStyle.successText(scheme))),
                      ),
                    ))),
                ],
              ),
            ),
          ),
        );
        // 작업 중은 학생색 빛 조각이 칸 윤곽을 한 바퀴씩 돈다 — 바닥에 흐르던 막대를 걷었다(2026-10-07).
        // 기다림의 고정 테(상태색)와는 상태가 배타적이라 같이 서지 않는다.
        final cell = OrbitEdge(
          live: (busy && !waiting) || childrenRunning || behind,
          dashed: behind,
          color: childrenRunning ? StatusStyle.success : accent,
          radius: Look.smallCorners,
          child: front,
        );
        Widget wrap(Widget child) => p == null ? child : widget.wrapPane?.call(p, child) ?? child;
        if (!tabbed) return wrap(cell);
        // 좌우로 쓸면 다음·이전 탭. 겹친 뒷장은 두지 않는다 — 몇째인지는 밑의 점이
        // 말하고, 칸이 작아 덱까지 들어가면 얼굴이 밀린다(2026-09-08 지시).
        return wrap(GestureDetector(
          behavior: HitTestBehavior.opaque,
          onHorizontalDragEnd: (d) {
            final v = d.primaryVelocity ?? 0;
            if (v.abs() < 120) return;
            _flip(v < 0 ? 1 : -1);
          },
          child: cell,
        ));
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
    this.wrapPane,
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
  final Widget Function(Pane, Widget)? wrapPane;

  /// 테를 상태색으로 — 칸 밖에 홀로 선 줄에선 얼굴만으론 「답 기다림」이 안 보인다.
  final bool marks;

  Color _edge(Pane? p, int i, ColorScheme scheme) {
    if (p?.hasRunningSubagents == true) return StatusStyle.success;
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
    Widget wrap(int i, Widget child) => tabs[i] == null ? child : wrapPane?.call(tabs[i]!, child) ?? child;
    return Wrap(
      spacing: 3,
      runSpacing: 3,
      children: [
        for (var i = 0; i < tabs.length; i++)
          Padding(
            padding: const EdgeInsets.only(right: 3),
            child: wrap(i, GestureDetector(
              behavior: HitTestBehavior.opaque,
              onTap: tabs[i] == null || onOpen == null
                  ? null
                  : () => onOpen!(tabs[i]!),
              child: SizedBox(width: 44, height: 44, child: Center(child: Container(
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
              ))),
            )),
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
  });

  final Server server;
  final Pane pane;
  final VoidCallback? onTap;

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
    final behind = !st.live && !st.needsYou && !pane.hasRunningSubagents && pane.bgActive;
    return OrbitEdge(
      live: st.live || pane.hasRunningSubagents || behind,
      dashed: behind,
      color: pane.hasRunningSubagents ? StatusStyle.success : scheme.primary,
      radius: Look.smallCorners,
      child: InkWell(
        onTap: onTap,
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
                                if (pane.hasRunningSubagents)
                                  Text('하위 작업 ${pane.subagents.length}개', style: theme.textTheme.labelSmall?.copyWith(color: StatusStyle.successText(scheme))),
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
