import 'dart:async';

import 'package:flutter/material.dart';

import '../character_stage.dart';
import '../nacho.dart';
import '../nacho_student.dart';
import '../server.dart';
import '../status_style.dart';
import '../student_art.dart';
import '../workboard.dart';

/// 나쵸 「작업」 탭 — 거노 차례를 맨 위에, 그 아래 진행·검증·완료. 프로젝트로 거르고, 기기와
/// 학생은 머리의 「기기」 단추로 본다(2026-09-25 B안 — 할 일 먼저).
class WorkBoardView extends StatefulWidget {
  const WorkBoardView({
    super.key,
    required this.desk,
    required this.server,
    required this.students,
    required this.onOpenTask,
    required this.onOpenPane,
    required this.onGoChat,
    this.demo = const bool.fromEnvironment('KASA_DEMO_BOARD'),
    this.roster,
  });

  final NachoDesk desk;
  final Server server;
  final StudentLookup students;
  final ValueChanged<String> onOpenTask;
  final void Function(Pane pane) onOpenPane;

  /// 승인은 아직 대화에서 한다 — 승인 시트의 「대화에서 답하기」가 부른다.
  final VoidCallback onGoChat;

  /// 예시 데이터로 그린다(검사·시뮬레이터 확인용). 화면이 늘 「예시」라고 밝힌다.
  final bool demo;

  @visibleForTesting
  final LiveRoster? roster;

  @override
  State<WorkBoardView> createState() => _WorkBoardViewState();
}

class _WorkBoardViewState extends State<WorkBoardView>
    with WidgetsBindingObserver {
  late final LiveRoster _roster = widget.roster ?? LiveRoster(widget.server);
  String? _project;
  bool _showDone = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    if (!widget.demo) _roster.start();
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    if (widget.roster == null) _roster.dispose();
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (widget.demo) return;
    switch (state) {
      case AppLifecycleState.resumed:
        _roster.start();
      case AppLifecycleState.paused:
      case AppLifecycleState.hidden:
      case AppLifecycleState.detached:
        _roster.stop();
      case AppLifecycleState.inactive:
        break;
    }
  }

  WorkBoard _board() => widget.demo
      ? demoBoard()
      : buildBoard(
          tasks: widget.desk.tasks,
          devices: _roster.devices,
          seatOf: widget.students.seat,
        );

  Future<void> _refresh() async {
    await Future.wait([widget.desk.loadTasks(), _roster.refresh()]);
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: Listenable.merge([widget.desk, _roster, widget.students]),
    builder: (context, _) {
      final board = _board();
      final project = board.projects.any((p) => p.name == _project)
          ? _project
          : null;
      final yours = board.lane(WorkLane.yours, project: project);
      final running = board.lane(WorkLane.running, project: project);
      final verifying = board.lane(WorkLane.verifying, project: project);
      final done = board.lane(WorkLane.done, project: project);
      final children = <Widget>[
        if (board.demo)
          const _Band(
            text: '예시 데이터 — 실제 작업이 아니에요',
            icon: Icons.science_outlined,
          ),
        // 원장이 끊긴 것은 나쵸 홈 머리가 이미 말한다 — 같은 말을 두 번 세우지 않는다.
        if (!board.demo &&
            widget.desk.tasksProblem != null &&
            widget.desk.tasksProblem != widget.desk.problem)
          _Band(text: '나쵸 장부: ${widget.desk.tasksProblem}'),
        if (!board.demo && _roster.problem != null)
          _Band(
            text:
                '실시간 판: ${_roster.problem}'
                '${_roster.okAt == null ? '' : ' · 마지막 ${freshLabel(_roster.okAt!.millisecondsSinceEpoch)}'}',
          ),
        _Head(
          yours: yours.length,
          running: running.length,
          verifying: verifying.length,
          devices: board.devices,
          onDevices: () => _openDevices(board),
        ),
        if (board.projects.length > 1)
          _ProjectChips(
            projects: board.projects,
            selected: project,
            onPick: (p) => setState(() => _project = p),
          ),
      ];
      if (yours.isNotEmpty) {
        children.add(
          _Section(text: '거노 차례 ${yours.length}', color: StatusStyle.attention),
        );
        for (final i in yours) {
          children.add(_row(i, attention: true));
        }
      }
      void lane(String name, List<WorkItem> rows) {
        if (rows.isEmpty) return;
        children.add(_Section(text: '$name ${rows.length}'));
        for (final i in rows) {
          children.add(_row(i));
        }
      }

      lane('진행', running);
      lane('검증', verifying);
      if (done.isNotEmpty) {
        children.add(
          _DoneToggle(
            count: done.length,
            open: _showDone,
            onTap: () => setState(() => _showDone = !_showDone),
          ),
        );
        if (_showDone) {
          for (final i in done) {
            children.add(_row(i));
          }
        }
      }
      if (yours.isEmpty && running.isEmpty && verifying.isEmpty) {
        children.add(
          const Padding(
            padding: EdgeInsets.fromLTRB(24, 40, 24, 24),
            child: Text(
              '지금 거노 차례도, 도는 일도 없어요.\n대화에서 일을 맡기면 여기로 와요.',
              textAlign: TextAlign.center,
            ),
          ),
        );
      }
      return RefreshIndicator(
        onRefresh: board.demo ? () async {} : _refresh,
        child: ListView(
          physics: const AlwaysScrollableScrollPhysics(),
          padding: const EdgeInsets.only(bottom: 24),
          children: children,
        ),
      );
    },
  );

  Widget _row(WorkItem i, {bool attention = false}) {
    VoidCallback? open;
    if (i.taskId != null && i.source != WorkSource.demo) {
      open = () => widget.onOpenTask(i.taskId!);
    } else if (i.pane != null && i.source == WorkSource.live) {
      open = () => widget.onOpenPane(i.pane!);
    }
    Widget? action;
    if (i.yours == YoursKind.approval && i.taskId != null) {
      action = _ActionButton(label: '검토', onTap: () => _openApproval(i));
    } else if (i.yours != null && i.pane != null) {
      action = _ActionButton(
        label: '답하기',
        onTap: i.source == WorkSource.demo
            ? null
            : () => widget.onOpenPane(i.pane!),
      );
    }
    return _WorkRow(
      item: i,
      server: widget.server,
      attention: attention,
      onTap: open,
      action: action,
    );
  }

  void _openDevices(WorkBoard board) {
    showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      isScrollControlled: true,
      builder: (_) =>
          DevicesSheet(devices: board.devices, server: widget.server),
    );
  }

  void _openApproval(WorkItem i) {
    showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      isScrollControlled: true,
      builder: (sheet) => ApprovalSheet(
        item: i,
        load: i.source == WorkSource.demo
            ? Future.value(ApprovalView.demo)
            : widget.desk.task(i.taskId!).then(ApprovalView.fromDetail),
        onGoChat: () {
          Navigator.of(sheet).pop();
          widget.onGoChat();
        },
        onOpenTask: i.source == WorkSource.demo
            ? null
            : () {
                Navigator.of(sheet).pop();
                widget.onOpenTask(i.taskId!);
              },
      ),
    );
  }
}

class _Band extends StatelessWidget {
  const _Band({required this.text, this.icon = Icons.error_outline});

  final String text;
  final IconData icon;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      width: double.infinity,
      color: scheme.surfaceContainerHighest,
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
      child: Row(
        children: [
          Icon(icon, size: 16, color: scheme.onSurfaceVariant),
          const SizedBox(width: 8),
          Expanded(child: Text(text, style: const TextStyle(fontSize: 13))),
        ],
      ),
    );
  }
}

/// 캐릭터 한 명과 한 줄 요약, 기기 단추.
class _Head extends StatelessWidget {
  const _Head({
    required this.yours,
    required this.running,
    required this.verifying,
    required this.devices,
    required this.onDevices,
  });

  final int yours;
  final int running;
  final int verifying;
  final List<WorkDevice> devices;
  final VoidCallback onDevices;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final online = devices.where((d) => d.online).length;
    final line = yours > 0
        ? '거노 차례 $yours · 진행 $running · 검증 $verifying'
        : '거노 차례 없음 · 진행 $running · 검증 $verifying';
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 6, 12, 0),
      child: Row(
        children: [
          const CharacterStage(slug: 'arona', size: 44),
          const SizedBox(width: 10),
          Expanded(
            child: Text(
              line,
              style: theme.textTheme.titleSmall?.copyWith(
                color: yours > 0 ? StatusStyle.attentionInk : scheme.onSurface,
                fontWeight: FontWeight.w600,
              ),
            ),
          ),
          OutlinedButton.icon(
            onPressed: onDevices,
            style: OutlinedButton.styleFrom(
              minimumSize: const Size(0, 44),
              padding: const EdgeInsets.symmetric(horizontal: 12),
            ),
            icon: _DeviceDots(devices: devices),
            label: Text('기기 $online/${devices.length}'),
          ),
        ],
      ),
    );
  }
}

class _DeviceDots extends StatelessWidget {
  const _DeviceDots({required this.devices});

  final List<WorkDevice> devices;

  @override
  Widget build(BuildContext context) {
    final off = Theme.of(context).colorScheme.outline;
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        for (final d in devices.take(4))
          Padding(
            padding: const EdgeInsets.only(right: 3),
            child: _Dot(
              color: d.online ? StatusStyle.success : off,
              hollow: !d.online,
            ),
          ),
      ],
    );
  }
}

class _Dot extends StatelessWidget {
  const _Dot({required this.color, this.hollow = false});

  static const size = 8.0;

  final Color color;
  final bool hollow;

  @override
  Widget build(BuildContext context) => Container(
    width: size,
    height: size,
    decoration: BoxDecoration(
      shape: BoxShape.circle,
      color: hollow ? null : color,
      border: hollow ? Border.all(color: color, width: 1.4) : null,
    ),
  );
}

class _ProjectChips extends StatelessWidget {
  const _ProjectChips({
    required this.projects,
    required this.selected,
    required this.onPick,
  });

  final List<({String name, int yours, int total})> projects;
  final String? selected;
  final ValueChanged<String?> onPick;

  @override
  Widget build(BuildContext context) {
    Widget chip(String label, int yours, bool on, VoidCallback tap) => Padding(
      padding: const EdgeInsets.only(right: 6),
      child: FilterChip(
        selected: on,
        onSelected: (_) => tap(),
        showCheckmark: false,
        label: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(label),
            if (yours > 0) ...[
              const SizedBox(width: 6),
              Text(
                '$yours',
                style: const TextStyle(
                  color: StatusStyle.attentionInk,
                  fontWeight: FontWeight.w700,
                ),
              ),
            ],
          ],
        ),
      ),
    );
    final all = projects.fold(0, (n, p) => n + p.yours);
    return SizedBox(
      height: 48,
      child: ListView(
        scrollDirection: Axis.horizontal,
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
        children: [
          chip('전체', all, selected == null, () => onPick(null)),
          for (final p in projects)
            chip(p.name, p.yours, selected == p.name, () => onPick(p.name)),
        ],
      ),
    );
  }
}

class _Section extends StatelessWidget {
  const _Section({required this.text, this.color});

  final String text;
  final Color? color;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 10, 16, 4),
      child: Text(
        text,
        style: theme.textTheme.labelLarge?.copyWith(
          color: color == null
              ? theme.colorScheme.onSurfaceVariant
              : StatusStyle.attentionInk,
          fontWeight: FontWeight.w700,
        ),
      ),
    );
  }
}

class _DoneToggle extends StatelessWidget {
  const _DoneToggle({
    required this.count,
    required this.open,
    required this.onTap,
  });

  final int count;
  final bool open;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final dim = Theme.of(context).colorScheme.onSurfaceVariant;
    return InkWell(
      onTap: onTap,
      child: SizedBox(
        height: 52,
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 16),
          child: Row(
            children: [
              const Icon(
                Icons.check_rounded,
                size: 18,
                color: StatusStyle.successInk,
              ),
              const SizedBox(width: 8),
              Text(
                '완료 $count',
                style: TextStyle(color: dim, fontWeight: FontWeight.w600),
              ),
              const Spacer(),
              Text(open ? '접기' : '펼치기', style: TextStyle(color: dim)),
            ],
          ),
        ),
      ),
    );
  }
}

class _ActionButton extends StatelessWidget {
  const _ActionButton({required this.label, required this.onTap});

  final String label;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) => OutlinedButton(
    onPressed: onTap,
    style: OutlinedButton.styleFrom(
      minimumSize: const Size(64, 44),
      foregroundColor: StatusStyle.attentionInk,
      side: const BorderSide(color: StatusStyle.attentionInk),
    ),
    child: Text(label, style: const TextStyle(fontWeight: FontWeight.w700)),
  );
}

class _WorkRow extends StatelessWidget {
  const _WorkRow({
    required this.item,
    required this.server,
    this.attention = false,
    this.onTap,
    this.action,
  });

  final WorkItem item;
  final Server server;
  final bool attention;
  final VoidCallback? onTap;
  final Widget? action;

  static String sourceLabel(WorkSource s) => switch (s) {
    WorkSource.ledger => '장부',
    WorkSource.live => '실시간',
    WorkSource.demo => '예시',
  };

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final i = item;
    final pane = i.pane;
    final Widget lead = pane == null
        ? _LaneIcon(item: i)
        : StudentFace(
            slug: pane.slug,
            url: pane.slug == null || i.source == WorkSource.demo
                ? null
                : server.avatar(pane.slug!, machine: pane.machine),
            size: 32,
          );
    final meta = [
      i.project,
      ?i.machine,
      if (pane != null && i.source != WorkSource.demo) pane.displayName,
      freshLabel(i.updatedMs),
    ].join(' · ');
    final done = i.lane == WorkLane.done;
    return Material(
      color: attention
          ? StatusStyle.attention.withValues(alpha: 0.08)
          : Colors.transparent,
      child: InkWell(
        onTap: onTap,
        child: Container(
          constraints: const BoxConstraints(minHeight: 64),
          padding: const EdgeInsets.fromLTRB(16, 8, 12, 8),
          decoration: BoxDecoration(
            border: Border(bottom: BorderSide(color: scheme.outline)),
          ),
          child: Row(
            children: [
              SizedBox(width: 32, child: Center(child: lead)),
              const SizedBox(width: 12),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      i.title,
                      maxLines: 2,
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(
                        fontSize: 15,
                        fontWeight: attention ? FontWeight.w600 : null,
                        color: done
                            ? scheme.onSurfaceVariant
                            : scheme.onSurface,
                        decoration: i.failed
                            ? TextDecoration.lineThrough
                            : null,
                      ),
                    ),
                    if (i.detail.isNotEmpty)
                      Text(
                        i.detail,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: TextStyle(
                          fontSize: 12.5,
                          color: scheme.onSurfaceVariant,
                        ),
                      ),
                    Row(
                      children: [
                        Flexible(
                          child: Text(
                            meta,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: TextStyle(
                              fontSize: 12,
                              color: scheme.onSurfaceVariant,
                            ),
                          ),
                        ),
                        const SizedBox(width: 6),
                        _SourceTag(text: sourceLabel(i.source)),
                      ],
                    ),
                  ],
                ),
              ),
              if (action != null) ...[const SizedBox(width: 8), action!],
            ],
          ),
        ),
      ),
    );
  }
}

class _SourceTag extends StatelessWidget {
  const _SourceTag({required this.text});

  final String text;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 1),
      decoration: BoxDecoration(
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: scheme.outline),
      ),
      child: Text(
        text,
        style: TextStyle(fontSize: 10.5, color: scheme.onSurfaceVariant),
      ),
    );
  }
}

class _LaneIcon extends StatelessWidget {
  const _LaneIcon({required this.item});

  final WorkItem item;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return switch (item.lane) {
      WorkLane.yours => Icon(
        item.yours == YoursKind.question
            ? Icons.help_outline_rounded
            : Icons.warning_amber_rounded,
        color: StatusStyle.attentionInk,
      ),
      WorkLane.running => Icon(
        Icons.play_circle_outline_rounded,
        color: scheme.primary,
      ),
      WorkLane.verifying => Icon(
        Icons.fact_check_outlined,
        color: scheme.primary,
      ),
      WorkLane.done => Icon(
        item.failed ? Icons.close_rounded : Icons.check_rounded,
        color: item.failed ? scheme.error : StatusStyle.successInk,
      ),
    };
  }
}

/// 기기 — 연결·마지막 확인·그 기기의 학생.
class DevicesSheet extends StatelessWidget {
  const DevicesSheet({super.key, required this.devices, required this.server});

  final List<WorkDevice> devices;
  final Server server;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return DraggableScrollableSheet(
      expand: false,
      initialChildSize: 0.6,
      maxChildSize: 0.92,
      builder: (context, controller) => ListView(
        controller: controller,
        padding: const EdgeInsets.fromLTRB(16, 0, 16, 24),
        children: [
          Text('기기', style: theme.textTheme.titleMedium),
          const SizedBox(height: 8),
          for (final d in devices) ...[
            Padding(
              padding: const EdgeInsets.only(top: 12, bottom: 4),
              child: Row(
                children: [
                  _Dot(
                    color: d.online ? StatusStyle.success : scheme.outline,
                    hollow: !d.online,
                  ),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      d.here ? '${d.label} (이 주소)' : d.label,
                      style: const TextStyle(fontWeight: FontWeight.w700),
                    ),
                  ),
                  Text(
                    [
                      d.online ? '연결됨' : '끊김',
                      d.here ? '지금' : freshSecsLabel(d.agoSecs),
                      if (d.rttMs != null) '${d.rttMs}ms',
                    ].join(' · '),
                    style: TextStyle(
                      fontSize: 12,
                      color: scheme.onSurfaceVariant,
                    ),
                  ),
                ],
              ),
            ),
            if (d.students.isEmpty)
              Padding(
                padding: const EdgeInsets.only(left: 16, bottom: 4),
                child: Text(
                  d.online ? '학생 없음' : '연결이 끊겨 학생을 못 봐요',
                  style: TextStyle(
                    fontSize: 12.5,
                    color: scheme.onSurfaceVariant,
                  ),
                ),
              ),
            for (final p in d.students)
              if (!p.isShell && !p.closed)
                Container(
                  constraints: const BoxConstraints(minHeight: 48),
                  padding: const EdgeInsets.only(left: 16),
                  decoration: BoxDecoration(
                    border: Border(bottom: BorderSide(color: scheme.outline)),
                  ),
                  child: Row(
                    children: [
                      StudentFace(slug: p.slug, size: 28),
                      const SizedBox(width: 10),
                      Expanded(child: Text(p.displayName)),
                      Builder(
                        builder: (context) {
                          final st = StatusStyle.of(p, scheme);
                          return Text(
                            st.label,
                            style: TextStyle(
                              fontSize: 12.5,
                              color: st.color,
                              fontWeight: FontWeight.w600,
                            ),
                          );
                        },
                      ),
                    ],
                  ),
                ),
          ],
        ],
      ),
    );
  }
}

/// 승인 시트가 그리는 것. 서버가 범위·만료·1회용을 주지 않으면 [scoped] 가 거짓이다.
class ApprovalView {
  const ApprovalView({
    required this.what,
    this.note = '',
    this.origin,
    this.where,
    this.scoped = false,
  });

  factory ApprovalView.fromDetail(NachoTaskDetail t) {
    final a = t.approval ?? const {};
    final hops = t.hops;
    final from = hops.isEmpty ? null : hops.first['from']?.toString();
    return ApprovalView(
      what: (a['what'] as String? ?? '').trim().isEmpty
          ? t.goal
          : a['what'] as String,
      note: a['note'] as String? ?? '',
      origin: from ?? (t.place.isEmpty ? null : t.place),
      where: t.student?['host']?.toString(),
      // 지금 계약엔 id·범위·만료·1회용이 없다. 생기면 여기서 읽고, 그 전엔 거짓.
      scoped: false,
    );
  }

  static const demo = ApprovalView(
    what: 'launchctl kickstart -k gui/501/com.kasaterm.request-journal',
    note: '예시 — 실제 요청이 아니에요',
    origin: '디코 DM',
    where: '맥북',
  );

  final String what;
  final String note;
  final String? origin;
  final String? where;
  final bool scoped;
}

/// 한 번의 승인을 보이는 시트. 서버가 그 요청의 범위·만료·1회용을 검증해 주는 창구가
/// 생기기 전에는 허용 단추를 켜지 않는다 — Face ID 는 이 폰의 잠금 확인일 뿐 서버 권한이
/// 아니다. 그동안 승인은 원래 길(나쵸 대화)로 한다.
class ApprovalSheet extends StatelessWidget {
  const ApprovalSheet({
    super.key,
    required this.item,
    required this.load,
    required this.onGoChat,
    this.onOpenTask,
  });

  final WorkItem item;
  final Future<ApprovalView> load;
  final VoidCallback onGoChat;
  final VoidCallback? onOpenTask;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return SafeArea(
      child: FutureBuilder<ApprovalView>(
        future: load,
        builder: (context, snap) {
          final v = snap.data;
          Widget kv(String k, String value, {bool mono = false}) => Padding(
            padding: const EdgeInsets.only(bottom: 10),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                SizedBox(
                  width: 52,
                  child: Text(
                    k,
                    style: TextStyle(
                      fontSize: 13,
                      color: scheme.onSurfaceVariant,
                    ),
                  ),
                ),
                Expanded(
                  child: SelectableText(
                    value,
                    style: TextStyle(
                      fontSize: mono ? 13 : 15,
                      fontFamily: mono ? 'TermMono' : null,
                    ),
                  ),
                ),
              ],
            ),
          );
          return Padding(
            padding: const EdgeInsets.fromLTRB(20, 0, 20, 16),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Text(item.title, style: theme.textTheme.titleLarge),
                const SizedBox(height: 12),
                if (snap.hasError)
                  Text('${snap.error}', style: TextStyle(color: scheme.error))
                else if (v == null)
                  const Padding(
                    padding: EdgeInsets.all(24),
                    child: Center(child: CircularProgressIndicator.adaptive()),
                  )
                else ...[
                  kv('무엇', v.what, mono: true),
                  kv('어디', v.where ?? item.machine ?? '모름'),
                  if (v.origin != null) kv('출처', v.origin!),
                  kv(
                    '범위',
                    v.scoped ? '서버가 확인한 한 번' : '서버가 아직 범위·만료·1회용을 주지 않아요',
                  ),
                  if (v.note.isNotEmpty) kv('메모', v.note),
                ],
                const SizedBox(height: 8),
                OutlinedButton.icon(
                  onPressed: null,
                  style: OutlinedButton.styleFrom(
                    minimumSize: const Size.fromHeight(50),
                  ),
                  icon: const Icon(Icons.lock_outline_rounded),
                  label: const Text('한 번 허용 — 아직 못 켜요'),
                ),
                const SizedBox(height: 6),
                Text(
                  '서버가 이 요청의 범위·만료·1회용을 검증하는 창구가 생기면 켜져요. '
                  'Face ID 는 이 폰의 잠금 확인일 뿐 서버 권한을 만들지 않아요.',
                  style: TextStyle(
                    fontSize: 12.5,
                    color: scheme.onSurfaceVariant,
                    height: 1.5,
                  ),
                ),
                const SizedBox(height: 14),
                Row(
                  children: [
                    Expanded(
                      child: OutlinedButton(
                        onPressed: onGoChat,
                        style: OutlinedButton.styleFrom(
                          minimumSize: const Size.fromHeight(48),
                        ),
                        child: const Text('나쵸 대화에서 답하기'),
                      ),
                    ),
                    if (onOpenTask != null) ...[
                      const SizedBox(width: 8),
                      Expanded(
                        child: OutlinedButton(
                          onPressed: onOpenTask,
                          style: OutlinedButton.styleFrom(
                            minimumSize: const Size.fromHeight(48),
                          ),
                          child: const Text('작업 상세'),
                        ),
                      ),
                    ],
                  ],
                ),
              ],
            ),
          );
        },
      ),
    );
  }
}
