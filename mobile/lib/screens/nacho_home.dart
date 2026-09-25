import 'dart:async';

import 'package:flutter/material.dart';

import '../hub_prefs.dart';
import '../nacho.dart';
import '../server.dart';
import 'hub.dart';
import 'nacho_task.dart';
import 'nacho_typing.dart';

/// 카사모바일 첫 화면 — 나쵸와의 대화와, 그 대화에서 맡은 일의 목록.
///
/// 나쵸와 얘기하는 주 창구는 여기다(2026-09-25) — 디스코드·슬랙은 보조 창구로 남는다. 학생 터미널은
/// 여기서 한 번 더 들어가는 자리(오른쪽 위)로 남는다 — 지운 것이 아니다.
class NachoHome extends StatefulWidget {
  const NachoHome({
    super.key,
    required this.server,
    required this.onChangeAddress,
    this.desk,
  });

  final Server server;
  final Future<void> Function() onChangeAddress;

  /// 검사용 — 가짜 창구를 넣는다.
  final NachoDesk? desk;

  @override
  State<NachoHome> createState() => _NachoHomeState();
}

class _NachoHomeState extends State<NachoHome> with WidgetsBindingObserver {
  late final NachoDesk _desk = widget.desk ?? NachoDesk(widget.server);

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    unawaited(_desk.start());
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    if (widget.desk == null) _desk.dispose();
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    switch (state) {
      case AppLifecycleState.resumed:
        unawaited(_desk.start());
      case AppLifecycleState.paused:
      case AppLifecycleState.hidden:
      case AppLifecycleState.detached:
        _desk.stop();
      case AppLifecycleState.inactive:
        break;
    }
  }

  void _openStudents() {
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => HubScreen(
          server: widget.server,
          onChangeAddress: widget.onChangeAddress,
          prefs: const HubPrefs(),
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) => DefaultTabController(
    length: 2,
    child: ListenableBuilder(
      listenable: _desk,
      builder: (context, _) {
        final attention = _desk.groups['attention'] ?? 0;
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
                const Text('카사모바일'),
              ],
            ),
            actions: [
              IconButton(
                tooltip: _desk.linkedPet == null
                    ? '펫 연결 — 지금은 폰에만'
                    : '펫 연결 — ${_desk.linkedPet!.replaceFirst('kasapet:', '')}',
                onPressed: _openPets,
                icon: Icon(_desk.linkedPet == null ? Icons.link_off_rounded : Icons.link_rounded),
              ),
              IconButton(
                tooltip: '학생 화면',
                onPressed: _openStudents,
                icon: const Icon(Icons.terminal_rounded),
              ),
            ],
            bottom: TabBar(
              tabs: [
                const Tab(text: '대화'),
                Tab(
                  child: Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      const Text('작업'),
                      if (attention > 0) ...[
                        const SizedBox(width: 6),
                        Badge.count(count: attention),
                      ],
                    ],
                  ),
                ),
              ],
            ),
          ),
          body: Column(
            children: [
              if (_desk.problem != null) _Banner(text: _desk.problem!),
              Expanded(
                child: TabBarView(
                  children: [
                    NachoChat(desk: _desk, onOpenTask: _openTask),
                    NachoTasks(desk: _desk, onOpenTask: _openTask),
                  ],
                ),
              ),
            ],
          ),
        );
      },
    ),
  );

  void _openPets() {
    showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (_) => NachoPetSheet(desk: _desk),
    );
  }

  void _openTask(String id) {
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => NachoTaskScreen(
          desk: _desk,
          taskId: id,
          onOpenStudents: _openStudents,
        ),
      ),
    );
  }
}

/// 어느 펫에서 이 대화를 이어 볼지 — 사람이 한 대를 고른다. 모든 바탕화면에 뿌리지 않는다.
class NachoPetSheet extends StatefulWidget {
  const NachoPetSheet({super.key, required this.desk});

  final NachoDesk desk;

  @override
  State<NachoPetSheet> createState() => _NachoPetSheetState();
}

class _NachoPetSheetState extends State<NachoPetSheet> {
  late Future<(String?, List<NachoPet>)> _load = widget.desk.pets();
  bool _busy = false;

  Future<void> _link(String? conv) async {
    setState(() => _busy = true);
    try {
      await widget.desk.linkPet(conv);
      final next = widget.desk.pets();
      if (mounted) {
        setState(() {
          _load = next;
        });
      }
    } on NachoError catch (e) {
      if (mounted) ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(e.message)));
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return SafeArea(
      child: FutureBuilder<(String?, List<NachoPet>)>(
        future: _load,
        builder: (context, snap) {
          final data = snap.data;
          final children = <Widget>[
            const ListTile(
              title: Text('펫과 이어 보기', style: TextStyle(fontWeight: FontWeight.w700)),
              subtitle: Text('고른 펫 한 대만 이 대화를 말풍선으로 받고, 그 펫에서 물으면 같은 대화로 이어져요.'),
            ),
          ];
          if (snap.hasError) {
            children.add(ListTile(title: Text('${snap.error}', style: TextStyle(color: scheme.error))));
          } else if (data == null) {
            children.add(const Padding(padding: EdgeInsets.all(24), child: Center(child: CircularProgressIndicator())));
          } else if (data.$2.isEmpty) {
            children.add(const ListTile(
              title: Text('이을 수 있는 펫이 없어요'),
              subtitle: Text('컴퓨터에서 카사텀 펫을 켜면 여기에 나와요. 그 전까지 대화는 폰에만 있어요.'),
            ));
          } else {
            for (final p in data.$2) {
              children.add(ListTile(
                leading: Icon(p.alive ? Icons.desktop_mac_rounded : Icons.desktop_access_disabled_rounded,
                    color: p.alive ? scheme.primary : scheme.onSurfaceVariant),
                title: Text('펫(${p.place})'),
                subtitle: Text(p.linked ? '연결됨 · ${p.status}' : p.status),
                trailing: p.linked
                    ? TextButton(onPressed: _busy ? null : () => _link(null), child: const Text('끊기'))
                    : FilledButton(onPressed: _busy ? null : () => _link(p.conv), child: const Text('잇기')),
              ));
            }
          }
          return ListView(shrinkWrap: true, children: children);
        },
      ),
    );
  }
}

class _Banner extends StatelessWidget {
  const _Banner({required this.text});

  final String text;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Semantics(
      liveRegion: true,
      child: Container(
        width: double.infinity,
        color: scheme.errorContainer,
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
        child: Text(
          text,
          style: TextStyle(color: scheme.onErrorContainer, fontSize: 13),
        ),
      ),
    );
  }
}

// ── 대화 ─────────────────────────────────────────────────────────────────
class NachoChat extends StatefulWidget {
  const NachoChat({super.key, required this.desk, required this.onOpenTask});

  final NachoDesk desk;
  final ValueChanged<String> onOpenTask;

  @override
  State<NachoChat> createState() => _NachoChatState();
}

class _NachoChatState extends State<NachoChat> {
  final _input = TextEditingController();
  bool _sending = false;

  @override
  void dispose() {
    _input.dispose();
    super.dispose();
  }

  Future<void> _send() async {
    final text = _input.text.trim();
    if (text.isEmpty || _sending) return;
    setState(() => _sending = true);
    try {
      await widget.desk.send(text);
      _input.clear();
    } on NachoError catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(e.message)));
      }
    } finally {
      if (mounted) setState(() => _sending = false);
    }
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: widget.desk,
    builder: (context, _) {
      final rows = _rows(widget.desk);
      return Column(
        children: [
          Expanded(
            child: rows.isEmpty
                ? _Empty(
                    text: widget.desk.loaded
                        ? '나쵸에게 말을 걸어 보세요.\n맡긴 일은 작업 탭에 정리돼요.'
                        : '대화를 불러오는 중',
                  )
                : ListView.builder(
                    reverse: true,
                    padding: const EdgeInsets.symmetric(vertical: 12),
                    itemCount: rows.length,
                    itemBuilder: (context, i) => rows[rows.length - 1 - i],
                  ),
          ),
          _Composer(
            controller: _input,
            hint: '나쵸에게 말하기',
            sending: _sending,
            onSend: _send,
          ),
        ],
      );
    },
  );

  List<Widget> _rows(NachoDesk desk) {
    final out = <Widget>[];
    for (final e in desk.events) {
      switch (e.kind) {
        case 'message':
          final id = e.id ?? '';
          final state = desk.stateOf(id);
          out.add(
            _UserBubble(
              text: e.text,
              state: state,
              note: desk.noteOf(id),
              direction: e.task != null,
              origin: e.origin,
            ),
          );
        case 'reply':
          final work = desk.workOf(e.task);
          out.add(
            _NachoBubble(
              event: e,
              desk: desk,
              work: work,
              onOpenTask: widget.onOpenTask,
            ),
          );
        case 'notice':
          out.add(
            _NoticeRow(
              event: e,
              onOpenTask: desk.workOf(e.task) == null ? null : widget.onOpenTask,
            ),
          );
      }
    }
    for (final o in desk.outgoing) {
      out.add(
        _UserBubble(
          text: o.text,
          state: o.state,
          note: o.error,
          onRetry: o.state == 'unsent' ? () => desk.retry(o.id) : null,
        ),
      );
    }
    final awaiting = desk.awaiting;
    if (awaiting != null) {
      out.add(
        NachoTyping(
          key: const ValueKey('nacho-typing'),
          progress: desk.stateOf(awaiting) == 'running'
              ? desk.progressOf(awaiting)
              : null,
        ),
      );
    }
    return out;
  }
}

class _Empty extends StatelessWidget {
  const _Empty({required this.text});

  final String text;

  @override
  Widget build(BuildContext context) => Center(
    child: Padding(
      padding: const EdgeInsets.all(32),
      child: Text(
        text,
        textAlign: TextAlign.center,
        style: TextStyle(color: Theme.of(context).colorScheme.onSurfaceVariant),
      ),
    ),
  );
}

class _UserBubble extends StatelessWidget {
  const _UserBubble({
    required this.text,
    required this.state,
    this.note = '',
    this.direction = false,
    this.onRetry,
    this.origin,
  });

  final String text;
  final String state;
  final String note;
  final bool direction;
  final VoidCallback? onRetry;
  final String? origin;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final bad = const {'failed', 'refused', 'interrupted', 'unsent'}.contains(state);
    // 도는 중은 나쵸 자리의 점이 말한다 — 같은 뜻을 내 말 밑에 한 번 더 적지 않는다.
    final meta = [
      ?origin,
      if (direction) '방향 수정',
      if (state != 'running') receiptLabel(state),
      if (note.isNotEmpty && bad) note,
    ].join(' · ');
    return Align(
      alignment: Alignment.centerRight,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(56, 4, 12, 4),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.end,
          children: [
            Container(
              padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
              decoration: BoxDecoration(
                color: scheme.primary,
                borderRadius: BorderRadius.circular(16),
              ),
              child: SelectableText(
                text,
                style: TextStyle(color: scheme.onPrimary, fontSize: 15, height: 1.35),
              ),
            ),
            if (meta.isNotEmpty) ...[
              const SizedBox(height: 3),
              GestureDetector(
                onTap: onRetry,
                child: Text(
                  meta,
                  style: TextStyle(
                    fontSize: 12,
                    color: bad ? scheme.error : scheme.onSurfaceVariant,
                    decoration: onRetry == null ? null : TextDecoration.underline,
                  ),
                ),
              ),
            ],
          ],
        ),
      ),
    );
  }
}

class _NachoBubble extends StatelessWidget {
  const _NachoBubble({
    required this.event,
    required this.desk,
    required this.work,
    required this.onOpenTask,
  });

  final NachoEvent event;
  final NachoDesk desk;
  final NachoTaskCard? work;
  final ValueChanged<String> onOpenTask;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = event.text.trim().isEmpty ? '(할 말 없이 끝냈다)' : event.text;
    final origin = event.origin;
    return Align(
      alignment: Alignment.centerLeft,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(12, 4, 40, 4),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Container(
              padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
              decoration: BoxDecoration(
                color: scheme.surfaceContainerHighest,
                borderRadius: BorderRadius.circular(16),
              ),
              child: SelectableText(
                text,
                style: TextStyle(color: scheme.onSurface, fontSize: 15, height: 1.4),
              ),
            ),
            if (origin != null)
              Padding(
                padding: const EdgeInsets.only(top: 3),
                child: Text(origin, style: TextStyle(fontSize: 12, color: scheme.onSurfaceVariant)),
              ),
            if (desk.deliveryOf(event.seq) case final d?)
              Padding(
                padding: const EdgeInsets.only(top: 3),
                child: Text(
                  deliveryLabel(d.state, d.target),
                  style: TextStyle(
                    fontSize: 12,
                    color: d.state == 'expired' || d.state == 'failed' ? scheme.error : scheme.onSurfaceVariant,
                  ),
                ),
              ),
            for (var i = 0; i < event.files.length; i++)
              if (_isImage(event.files[i]))
                Padding(
                  padding: const EdgeInsets.only(top: 6),
                  child: ClipRRect(
                    borderRadius: BorderRadius.circular(12),
                    child: Image.network(
                      desk.fileUri(event.seq, i, task: event.task).toString(),
                      width: 240,
                      fit: BoxFit.cover,
                      semanticLabel: event.files[i],
                      errorBuilder: (_, _, _) => Text(
                        '${event.files[i]} (지금은 못 불러온다)',
                        style: TextStyle(fontSize: 12, color: scheme.onSurfaceVariant),
                      ),
                    ),
                  ),
                )
              else
                Text(
                  '파일: ${event.files[i]}',
                  style: TextStyle(fontSize: 12, color: scheme.onSurfaceVariant),
                ),
            if (work != null)
              Padding(
                padding: const EdgeInsets.only(top: 6),
                child: ActionChip(
                  avatar: const Icon(Icons.task_alt_rounded, size: 18),
                  label: Text('작업 보기 · ${work!.stateLabel}'),
                  onPressed: () => onOpenTask(work!.id),
                ),
              ),
          ],
        ),
      ),
    );
  }
}

bool _isImage(String name) {
  final n = name.toLowerCase();
  return n.endsWith('.png') || n.endsWith('.jpg') || n.endsWith('.jpeg') || n.endsWith('.gif') || n.endsWith('.webp');
}

class _NoticeRow extends StatelessWidget {
  const _NoticeRow({required this.event, this.onOpenTask});

  final NachoEvent event;
  final ValueChanged<String>? onOpenTask;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final warn = event.notice == 'confirm_needed' || event.notice == 'failed';
    final icon = switch (event.notice) {
      'confirm_needed' => Icons.gpp_maybe_rounded,
      'hop' => Icons.swap_horiz_rounded,
      'link' => Icons.link_rounded,
      'restart' => Icons.restart_alt_rounded,
      'watch' => Icons.visibility_outlined,
      _ => Icons.info_outline_rounded,
    };
    final row = Container(
      margin: const EdgeInsets.symmetric(horizontal: 24, vertical: 6),
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
      decoration: BoxDecoration(
        color: warn ? scheme.errorContainer : scheme.surfaceContainerHigh,
        borderRadius: BorderRadius.circular(12),
      ),
      child: Row(
        children: [
          Icon(icon, size: 18, color: warn ? scheme.onErrorContainer : scheme.onSurfaceVariant),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              event.text,
              style: TextStyle(
                fontSize: 13,
                color: warn ? scheme.onErrorContainer : scheme.onSurfaceVariant,
              ),
            ),
          ),
        ],
      ),
    );
    final task = event.task;
    if (onOpenTask == null || task == null) return row;
    return InkWell(onTap: () => onOpenTask!(task), child: row);
  }
}

class _Composer extends StatelessWidget {
  const _Composer({
    required this.controller,
    required this.hint,
    required this.sending,
    required this.onSend,
  });

  final TextEditingController controller;
  final String hint;
  final bool sending;
  final VoidCallback onSend;

  @override
  Widget build(BuildContext context) => SafeArea(
    top: false,
    child: Padding(
      padding: const EdgeInsets.fromLTRB(12, 6, 8, 8),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.end,
        children: [
          Expanded(
            child: TextField(
              controller: controller,
              minLines: 1,
              maxLines: 5,
              // 16px 아래로 내리면 iOS 가 포커스 때 화면을 확대한다.
              style: const TextStyle(fontSize: 16),
              textInputAction: TextInputAction.newline,
              decoration: InputDecoration(hintText: hint, isDense: true),
            ),
          ),
          const SizedBox(width: 6),
          IconButton.filled(
            tooltip: '보내기',
            onPressed: sending ? null : onSend,
            icon: sending
                ? const SizedBox(width: 18, height: 18, child: CircularProgressIndicator(strokeWidth: 2))
                : const Icon(Icons.arrow_upward_rounded),
          ),
        ],
      ),
    ),
  );
}

/// 다른 화면(작업 상세)도 같은 입력칸을 쓴다.
Widget nachoComposer({
  required TextEditingController controller,
  required String hint,
  required bool sending,
  required VoidCallback onSend,
}) => _Composer(controller: controller, hint: hint, sending: sending, onSend: onSend);

// ── 작업 ─────────────────────────────────────────────────────────────────
class NachoTasks extends StatefulWidget {
  const NachoTasks({super.key, required this.desk, required this.onOpenTask});

  final NachoDesk desk;
  final ValueChanged<String> onOpenTask;

  @override
  State<NachoTasks> createState() => _NachoTasksState();
}

class _NachoTasksState extends State<NachoTasks> {
  String? _project;

  static const _order = ['attention', 'active', 'closed'];
  static const _label = {'attention': '판단 필요', 'active': '진행 중', 'closed': '끝남·실패'};

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: widget.desk,
    builder: (context, _) {
      final desk = widget.desk;
      final projects = {for (final t in desk.tasks) t.project}.toList()..sort();
      final shown = [
        for (final t in desk.tasks)
          if (_project == null || t.project == _project) t,
      ];
      final children = <Widget>[
        if (desk.tasksProblem != null) _Banner(text: desk.tasksProblem!),
        if (projects.length > 1)
          SizedBox(
            height: 48,
            child: ListView(
              scrollDirection: Axis.horizontal,
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
              children: [
                _chip('전체', _project == null, () => setState(() => _project = null)),
                for (final p in projects)
                  _chip(p, _project == p, () => setState(() => _project = p)),
              ],
            ),
          ),
      ];
      for (final g in _order) {
        final rows = [for (final t in shown) if (t.group == g) t];
        if (rows.isEmpty) continue;
        children.add(_SectionHeader(text: '${_label[g]} ${rows.length}'));
        for (final t in rows) {
          children.add(_TaskTile(card: t, onTap: () => widget.onOpenTask(t.id)));
        }
      }
      if (shown.isEmpty && desk.tasksProblem == null) {
        children.add(
          const SizedBox(
            height: 320,
            child: _Empty(text: '아직 맡긴 일이 없어요.\n대화에서 일을 시키면 여기로 분류돼요.'),
          ),
        );
      }
      return RefreshIndicator(
        onRefresh: desk.loadTasks,
        child: ListView(
          physics: const AlwaysScrollableScrollPhysics(),
          padding: const EdgeInsets.only(bottom: 24),
          children: children,
        ),
      );
    },
  );

  Widget _chip(String text, bool on, VoidCallback onTap) => Padding(
    padding: const EdgeInsets.only(right: 6),
    child: FilterChip(label: Text(text), selected: on, onSelected: (_) => onTap()),
  );
}

class _SectionHeader extends StatelessWidget {
  const _SectionHeader({required this.text});

  final String text;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(16, 16, 16, 6),
    child: Semantics(
      header: true,
      child: Text(
        text,
        style: Theme.of(context).textTheme.titleSmall?.copyWith(
          color: Theme.of(context).colorScheme.onSurfaceVariant,
          fontWeight: FontWeight.w700,
        ),
      ),
    ),
  );
}

class _TaskTile extends StatelessWidget {
  const _TaskTile({required this.card, required this.onTap});

  final NachoTaskCard card;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final sub = card.attention.isNotEmpty ? card.attention : card.step;
    return Card(
      margin: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
      child: InkWell(
        borderRadius: BorderRadius.circular(12),
        onTap: onTap,
        child: Padding(
          padding: const EdgeInsets.all(14),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                card.goal,
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
                style: const TextStyle(fontSize: 15, fontWeight: FontWeight.w600),
              ),
              const SizedBox(height: 6),
              Wrap(
                spacing: 6,
                runSpacing: 4,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: [
                  _Pill(text: card.stateLabel, strong: card.group == 'attention'),
                  _Pill(text: card.project),
                  if (card.place.isNotEmpty && card.place != '카사모바일') _Pill(text: card.place),
                  Text(
                    agoLabel(card.updatedMs),
                    style: TextStyle(fontSize: 12, color: scheme.onSurfaceVariant),
                  ),
                ],
              ),
              if (sub.isNotEmpty) ...[
                const SizedBox(height: 6),
                Text(
                  sub,
                  maxLines: 2,
                  overflow: TextOverflow.ellipsis,
                  style: TextStyle(fontSize: 13, color: scheme.onSurfaceVariant),
                ),
              ],
            ],
          ),
        ),
      ),
    );
  }
}

class _Pill extends StatelessWidget {
  const _Pill({required this.text, this.strong = false});

  final String text;
  final bool strong;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 3),
      decoration: BoxDecoration(
        color: strong ? scheme.errorContainer : scheme.surfaceContainerHigh,
        borderRadius: BorderRadius.circular(999),
      ),
      child: Text(
        text,
        style: TextStyle(
          fontSize: 12,
          color: strong ? scheme.onErrorContainer : scheme.onSurfaceVariant,
        ),
      ),
    );
  }
}

Widget nachoPill(String text, {bool strong = false}) => _Pill(text: text, strong: strong);

String agoLabel(int ms, {DateTime? now}) {
  if (ms <= 0) return '';
  final d = (now ?? DateTime.now()).difference(DateTime.fromMillisecondsSinceEpoch(ms));
  if (d.inMinutes < 1) return '방금';
  if (d.inHours < 1) return '${d.inMinutes}분 전';
  if (d.inDays < 1) return '${d.inHours}시간 전';
  return '${d.inDays}일 전';
}
