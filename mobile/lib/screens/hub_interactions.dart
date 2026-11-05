part of 'hub.dart';

mixin HubInteractions on State<HubScreen> {
  HubModel get hubModel;
  Future<void> _paneSheet(HubSection section, HubRoom room, Pane pane);
  void _open(Pane pane);

  final _chosen = <String>{};
  HubDragData? _dragData;
  bool _manipulating = false;
  final _hubScroll = ScrollController();
  final _scrollViewport = GlobalKey();
  Timer? _edgeScroll;
  Offset? _dragPoint;

  List<Pane> get _allHubPanes => [
    for (final section in hubModel.sections) ...[
      for (final room in section.rooms) ...room.panes,
      ...section.webShells,
    ],
  ];
  List<Pane> get _chosenPanes =>
      _allHubPanes.where((p) => _chosen.contains(p.viewKey)).toList();

  @override
  void dispose() {
    _edgeScroll?.cancel();
    _hubScroll.dispose();
    super.dispose();
  }

  void _notice(String text) {
    if (mounted) {
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(text)));
    }
  }

  void _actionNotice(SnackBar notice) {
    final messenger = ScaffoldMessenger.of(context);
    messenger.clearSnackBars();
    messenger.showSnackBar(notice);
  }

  void _tapPane(Pane pane) {
    if (_manipulating) return;
    if (_chosen.isEmpty) {
      _open(pane);
      return;
    }
    _select([pane], toggle: true);
  }

  void _select(Iterable<Pane> panes, {bool toggle = false}) {
    final current = _chosenPanes;
    final added = panes.toList();
    if (added.isEmpty) return;
    final machine = current.isEmpty
        ? added.first.machine
        : current.first.machine;
    if (added.any((pane) => pane.machine != machine)) {
      _notice('기기 하나씩 골라 주세요.');
      return;
    }
    setState(() {
      for (final pane in added) {
        if (!toggle || !_chosen.remove(pane.viewKey)) _chosen.add(pane.viewKey);
      }
    });
  }

  void _dragStart(HubDragData data) {
    // Floating undo messages otherwise intercept drops on the action dock.
    final messenger = ScaffoldMessenger.of(context);
    messenger.clearSnackBars();
    messenger.removeCurrentSnackBar();
    setState(() => _dragData = data);
    _dragPoint = null;
    _edgeScroll?.cancel();
    _edgeScroll = Timer.periodic(const Duration(milliseconds: 24), (_) {
      final lifecycle = WidgetsBinding.instance.lifecycleState;
      if (lifecycle != null && lifecycle != AppLifecycleState.resumed) {
        _dragEnd();
        return;
      }
      final point = _dragPoint;
      final box = _scrollViewport.currentContext?.findRenderObject();
      if (point == null || box is! RenderBox || !_hubScroll.hasClients) return;
      final y = box.globalToLocal(point).dy;
      final bottom = box.size.height - 156;
      final step = y < 64
          ? -16.0
          : y > bottom - 56 && y < bottom
          ? 16.0
          : 0.0;
      if (step == 0) return;
      final next = (_hubScroll.offset + step).clamp(
        0.0,
        _hubScroll.position.maxScrollExtent,
      );
      if (next != _hubScroll.offset) _hubScroll.jumpTo(next);
    });
  }

  void _dragEnd() {
    _edgeScroll?.cancel();
    _dragPoint = null;
    if (mounted) setState(() => _dragData = null);
  }

  Future<void> _showPaneMenu(Pane pane) async {
    for (final section in hubModel.sections) {
      for (final room in section.rooms) {
        if (room.panes.any((p) => p.viewKey == pane.viewKey)) {
          await _paneSheet(section, room, pane);
          return;
        }
      }
    }
  }

  Widget _wrapPane(Pane pane, Widget child, {required bool online}) {
    final data = HubDragData(
      _chosen.contains(pane.viewKey) ? _chosenPanes : [pane],
    );
    return HubDropDestination(
      key: ValueKey('hub-destination-${pane.viewKey}'),
      target: pane,
      enabled: online && !_manipulating,
      onDrop: _movePanes,
      child: HubDragItem(
        key: ValueKey('hub-drag-${pane.viewKey}'),
        server: widget.server,
        pane: pane,
        data: data,
        selected: _chosen.contains(pane.viewKey),
        enabled: !_manipulating,
        onStart: _dragStart,
        onUpdate: (point) => _dragPoint = point,
        onEnd: _dragEnd,
        onMenu: () => _showPaneMenu(pane),
        child: child,
      ),
    );
  }

  void _dropAction(HubDropAction action, HubDragData? data) {
    if (_manipulating) return;
    final panes = data?.panes ?? _chosenPanes;
    if (panes.isEmpty) return;
    _dragEnd();
    switch (action) {
      case HubDropAction.select:
        _select(panes);
      case HubDropAction.hide:
        unawaited(_hidePanes(panes));
      case HubDropAction.trash:
        unawaited(_trashPanes(panes));
      case HubDropAction.more:
        if (panes.length == 1) {
          unawaited(_showPaneMenu(panes.first));
        } else {
          _notice('더보기는 칸 하나를 골라 주세요.');
        }
    }
  }

  Future<void> _hidePanes(List<Pane> panes) async {
    final newlyHidden = panes
        .where((pane) => !hubModel.view.hidden.contains(pane.viewKey))
        .toList();
    await hubModel.hide(panes, true);
    if (!mounted) return;
    setState(_chosen.clear);
    _actionNotice(
      SnackBar(
        content: Text('${newlyHidden.length}개를 이 폰에서 숨겼어요. 작업은 계속 실행돼요.'),
        action: SnackBarAction(
          label: '되돌리기',
          onPressed: () => hubModel.hide(newlyHidden, false),
        ),
      ),
    );
  }

  Future<void> _movePanes(
    HubDragData data,
    Pane target,
    String direction,
  ) async {
    if (_manipulating) return;
    _dragEnd();
    setState(() => _manipulating = true);
    try {
      await HubActions(widget.server).move(data.panes, target, direction);
      if (mounted) setState(_chosen.clear);
    } catch (error) {
      _notice(desktopError(error));
    } finally {
      await hubModel.refresh();
      if (mounted) setState(() => _manipulating = false);
    }
  }

  Future<void> _trashPanes(List<Pane> panes) async {
    if (_manipulating) return;
    setState(() => _manipulating = true);
    try {
      final actions = HubActions(widget.server);
      final targets = await actions.prepare(panes);
      if (!mounted) return;
      final names = panes.map((p) => p.rowTitle).take(4).join('\n');
      Future<bool> confirm() => desktopConfirm(
        context,
        '${panes.length}개 칸을 닫을까요? 진행 중인 작업도 영향을 받아요.\n\n$names',
        action: '닫기',
      );
      final shells = panes.every((p) => p.isShell);
      if (!shells && !await confirm()) return;
      var result = await actions.close(targets, idleOnly: shells);
      if (result.needsConfirmation) {
        if (!mounted || !await confirm()) return;
        result = await actions.close(targets, idleOnly: false);
      }
      if (!mounted) return;
      setState(_chosen.clear);
      _actionNotice(
        SnackBar(
          content: Text('${panes.length}개 칸을 닫았어요.'),
          duration: const Duration(seconds: 8),
          action: result.restores.isEmpty
              ? null
              : SnackBarAction(
                  label: '되살리기',
                  onPressed: () async {
                    try {
                      await actions.restore(result.restores);
                    } catch (error) {
                      _notice(desktopError(error));
                    }
                    await hubModel.refresh();
                  },
                ),
        ),
      );
    } catch (error) {
      if (error is HubCloseException && error.restores.isNotEmpty && mounted) {
        _actionNotice(
          SnackBar(
            content: Text(error.message),
            duration: const Duration(seconds: 8),
            action: SnackBarAction(
              label: '닫힌 칸 되살리기',
              onPressed: () async {
                try {
                  await HubActions(widget.server).restore(error.restores);
                } catch (failure) {
                  _notice(desktopError(failure));
                }
                await hubModel.refresh();
              },
            ),
          ),
        );
      } else {
        _notice(desktopError(error));
      }
    } finally {
      await hubModel.refresh();
      if (mounted) setState(() => _manipulating = false);
    }
  }

  void _showHidden() {
    showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (context) => SafeArea(
        child: SizedBox(
          height: MediaQuery.sizeOf(context).height * .65,
          child: ListenableBuilder(
            listenable: hubModel,
            builder: (context, _) {
              final hidden = _allHubPanes
                  .where((p) => hubModel.view.hidden.contains(p.viewKey))
                  .toList();
              return ListView(
                children: [
                  ListTile(
                    title: Text('숨긴 칸 ${hidden.length}개'),
                    subtitle: const Text('이 폰에서만 숨겨져 있어요. 작업은 계속 실행돼요.'),
                  ),
                  TextButton.icon(
                    icon: const Icon(Icons.visibility_outlined),
                    label: const Text('전부 다시 보이기'),
                    onPressed: () =>
                        hubModel.setView(hubModel.view.copyWith(hidden: {})),
                  ),
                  for (final pane in hidden)
                    ListTile(
                      key: ValueKey('hub-restore-${pane.viewKey}'),
                      title: Text(pane.rowTitle),
                      subtitle: Text(pane.displayName),
                      trailing: const Icon(Icons.visibility_outlined),
                      onTap: () => hubModel.hide([pane], false),
                    ),
                ],
              );
            },
          ),
        ),
      ),
    );
  }

  void _showSubagents(Pane pane) {
    showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      isScrollControlled: true,
      builder: (context) => SafeArea(
        child: ConstrainedBox(
          constraints: BoxConstraints(
            maxHeight: MediaQuery.sizeOf(context).height * .65,
          ),
          child: ListenableBuilder(
            listenable: hubModel,
            builder: (context, _) {
              final current = _allHubPanes
                  .where((p) => p.viewKey == pane.viewKey)
                  .firstOrNull;
              final tasks = current?.hasRunningSubagents == true
                  ? current!.subagents
                  : <String>[];
              return ListView(
                shrinkWrap: true,
                children: [
                  ListTile(
                    title: Text('실행 중인 서브에이전트 ${tasks.length}개'),
                    subtitle: Text(current?.rowTitle ?? pane.rowTitle),
                  ),
                  if (tasks.isEmpty)
                    const ListTile(title: Text('실행 중인 하위 작업이 없어요.')),
                  for (final task in tasks)
                    ListTile(
                      leading: Icon(
                        Icons.account_tree_outlined,
                        color: StatusStyle.successText(
                          Theme.of(context).colorScheme,
                        ),
                      ),
                      title: Text(task),
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
