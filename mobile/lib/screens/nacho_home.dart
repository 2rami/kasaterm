import 'dart:async';

import 'package:flutter/material.dart';
import 'package:url_launcher/url_launcher.dart';

import '../hub_prefs.dart';
import '../look.dart';
import 'controls.dart';
import '../nacho.dart';
import '../nacho_reply.dart';
import '../nacho_student.dart';
import '../server.dart';
import '../server_image.dart';
import '../weather/scene.dart';
import '../wide_layout.dart';
import 'hub.dart';
import 'nacho_reply_view.dart';
import 'nacho_task.dart';
import 'nacho_typing.dart';
import 'share_screen.dart';
import 'terminal.dart';
import 'workboard_view.dart';

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
  late final StudentLookup _students = StudentLookup(widget.server);

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    unawaited(_desk.start());
    unawaited(_offerRelease());
  }

  /// 이 실행에서 이미 알린 빌드 — 앱으로 돌아올 때마다 같은 판을 또 알리지 않는다.
  String? _offered;

  Future<void> _offerRelease() async {
    final r = await widget.server.latestRelease();
    if (r == null || !r.newer || r.build == _offered || !mounted) return;
    _offered = r.build;
    ScaffoldMessenger.of(context).showSnackBar(SnackBar(
      content: Text('새 판 ${r.version} (${r.build})이 있어요'),
      duration: const Duration(seconds: 10),
      action: SnackBarAction(label: '설치', onPressed: () => unawaited(r.open())),
    ));
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    if (widget.desk == null) _desk.dispose();
    _students.dispose();
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    switch (state) {
      case AppLifecycleState.resumed:
        unawaited(_desk.start());
        unawaited(_offerRelease());
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
          prefs: HubPrefs(scope: widget.server.account == null ? '' :
            '${widget.server.account!.origin}|${widget.server.account!.account}'),
        ),
      ),
    );
  }

  /// 가로로 돌리면 탭과 나란히 보기가 바뀐다 — 쓰던 글·스크롤이 날아가지 않게 같은 것을 옮겨 단다.
  final _chatKey = GlobalKey();
  final _boardKey = GlobalKey();

  @override
  Widget build(BuildContext context) => DefaultTabController(
    length: 2,
    child: ListenableBuilder(
      listenable: _desk,
      builder: (context, _) {
        final attention = _desk.groups['attention'] ?? 0;
        // 아이패드 가로·큰 아이패드 세로는 대화와 작업을 한 화면에 — 탭을 오가며 맡긴 일을 확인하지 않게.
        final wide = sideBySide(MediaQuery.sizeOf(context).width);
        final chat = NachoChat(
          key: _chatKey,
          desk: _desk,
          students: _students,
          onOpenTask: _openTask,
          onOpenPane: _openPane,
          onLink: _openLink,
        );
        final board = Builder(
          builder: (tabs) => WorkBoardView(
            key: _boardKey,
            desk: _desk,
            server: widget.server,
            students: _students,
            onOpenTask: _openTask,
            onOpenPane: _openPane,
            onGoChat: () => DefaultTabController.of(tabs).animateTo(0),
          ),
        );
        return Scaffold(
          appBar: AppBar(
            title: Row(
              children: [
                Image.asset(
                  'assets/icons/kasa.png',
                  width: 22,
                  height: 22,
                  errorBuilder: (_, _, _) => const SizedBox.shrink(),
                ),
                const SizedBox(width: 8),
                const Flexible(child: Text('KASA Mobile', overflow: TextOverflow.ellipsis)),
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
                tooltip: 'KASA-share',
                onPressed: _openShare,
                icon: const Icon(Icons.folder_outlined),
              ),
              // 학생 목록 입구가 터미널 그림 하나라 무엇인지 안 읽혔다 — 이름을 붙인다.
              TextButton.icon(
                onPressed: _openStudents,
                icon: const Icon(Icons.people_outline_rounded, size: Look.iconSize),
                label: const Text('학생'),
              ),
              const SizedBox(width: 4),
            ],
            bottom: wide ? null : TabBar(
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
                child: WeatherScene(
                  child: wide
                    ? Row(
                        children: [
                          Expanded(
                            child: Center(
                              child: ConstrainedBox(
                                constraints: const BoxConstraints(maxWidth: readColumnMaxWidth),
                                child: chat,
                              ),
                            ),
                          ),
                          const VerticalDivider(width: 1),
                          SizedBox(width: workColumnWidth, child: board),
                        ],
                      )
                    : TabBarView(children: [chat, board]),
                ),
              ),
            ],
          ),
        );
      },
    ),
  );

  void _openShare() {
    Navigator.of(context).push(
      MaterialPageRoute<void>(builder: (_) => ShareScreen(server: widget.server)),
    );
  }

  void _openPets() {
    showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (_) => ModalLook(child: NachoPetSheet(desk: _desk)),
    );
  }

  void _openTask(String id) {
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => NachoTaskScreen(
          desk: _desk,
          taskId: id,
          onOpenStudents: _openStudents,
          students: _students,
          onOpenPane: _openPane,
          onLink: _openLink,
        ),
      ),
    );
  }

  void _openPane(Pane pane) {
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => TerminalScreen(server: widget.server, pane: pane),
      ),
    );
  }

  /// 답 속 링크. 이 서버의 학생 화면 링크면 앱 안에서 그 학생을 열고, 아니면 밖(사파리)으로.
  Future<void> _openLink(String url) async {
    final messenger = ScaffoldMessenger.of(context);
    final term = termLinkOf(url, widget.server.root);
    if (term != null) {
      Pane? pane;
      try {
        pane = await _students.fresh(term.machine, term.pane);
      } on ServerException catch (e) {
        messenger.showSnackBar(SnackBar(content: Text(e.message)));
        return;
      }
      if (!mounted) return;
      if (pane == null) {
        messenger.showSnackBar(const SnackBar(content: Text('그 창을 지금 목록에서 못 찾았어요 — 닫혔을 수 있어요')));
        return;
      }
      _openPane(pane);
      return;
    }
    final uri = externalUri(url);
    if (uri == null || !await launchUrl(uri, mode: LaunchMode.externalApplication)) {
      messenger.showSnackBar(const SnackBar(content: Text('링크를 열지 못했어요')));
    }
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
              title: Text('펫과 이어 보기', style: TextStyle(fontWeight: FontWeight.w600)),
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
        padding: const EdgeInsets.symmetric(horizontal: Look.pagePad, vertical: 10),
        child: Text(
          text,
          style: TextStyle(color: scheme.onErrorContainer, fontSize: Look.sub, fontWeight: FontWeight.w600),
        ),
      ),
    );
  }
}

// ── 대화 ─────────────────────────────────────────────────────────────────
class NachoChat extends StatefulWidget {
  const NachoChat({
    super.key,
    required this.desk,
    required this.students,
    required this.onOpenTask,
    required this.onOpenPane,
    required this.onLink,
  });

  final NachoDesk desk;
  final StudentLookup students;
  final ValueChanged<String> onOpenTask;
  final void Function(Pane pane) onOpenPane;
  final ValueChanged<String> onLink;

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
    // 학생 카드는 그 일의 첫 답(띄웠다는 보고) 밑에만 — 뒤의 답은 「작업 보기」 칩으로 충분하다.
    final carded = <String>{};
    for (final e in timeline(desk.events)) {
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
          final seated = work != null && work.student != null && carded.add(work.id);
          out.add(
            _NachoBubble(
              event: e,
              desk: desk,
              work: work,
              card: seated,
              students: widget.students,
              onOpenTask: widget.onOpenTask,
              onOpenPane: widget.onOpenPane,
              onLink: widget.onLink,
            ),
          );
        case 'notice':
          out.add(
            _NoticeRow(
              event: e,
              onLink: widget.onLink,
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
        padding: const EdgeInsets.fromLTRB(Look.bubbleFar, 4, Look.pagePad, 4),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.end,
          children: [
            SpeechBubble(
              mine: true,
              child: SelectableText(
                text,
                style: TextStyle(
                  color: SpeechBubble.ink(context, mine: true),
                  fontSize: Look.body,
                  height: 1.4,
                ),
              ),
            ),
            if (meta.isNotEmpty) ...[
              const SizedBox(height: 4),
              GestureDetector(
                onTap: onRetry,
                child: Text(
                  meta,
                  style: Theme.of(context).textTheme.bodySmall?.copyWith(
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
    required this.card,
    required this.students,
    required this.onOpenTask,
    required this.onOpenPane,
    required this.onLink,
  });

  final NachoEvent event;
  final NachoDesk desk;
  final NachoTaskCard? work;

  /// 이 답 밑에 맡은 학생 카드를 세우나.
  final bool card;
  final StudentLookup students;
  final ValueChanged<String> onOpenTask;
  final void Function(Pane pane) onOpenPane;
  final ValueChanged<String> onLink;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final view = splitReply(
      event.text,
      root: desk.server.root,
      seatPane: card ? (work?.student?['surface'] as String?) : null,
    );
    final text = event.text.trim().isEmpty ? '(할 말 없이 끝냈다)' : view.body;
    final origin = event.origin;
    return Align(
      alignment: Alignment.centerLeft,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(Look.pagePad, 4, Look.bubbleFar, 4),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            if (text.isNotEmpty)
              SpeechBubble(
                mine: false,
                child: ReplyText(
                  text: text,
                  onLink: onLink,
                  style: TextStyle(color: scheme.onSurface, fontSize: Look.body, height: 1.4),
                ),
              ),
            if (view.meta != null) ReplyMetaLine(meta: view.meta!),
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
                    borderRadius: BorderRadius.circular(6),
                    child: ServerImage(
                      server: desk.server,
                      uri: desk.fileUri(event.seq, i, task: event.task),
                      width: 240,
                      fit: BoxFit.cover,
                      semanticLabel: event.files[i],
                      fallback: Text(
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
            if (card)
              StudentWorkCard(
                work: work!,
                lookup: students,
                onOpenPane: onOpenPane,
                onOpenTask: onOpenTask,
              )
            else if (work != null)
              Padding(
                padding: const EdgeInsets.only(top: 6),
                child: ActionChip(
                  avatar: const Icon(Icons.task_alt_rounded, size: 18),
                  label: Text('작업 보기 · ${work!.stateLabel}'),
                  onPressed: () => onOpenTask(work!.id),
                ),
              ),
            if (view.folded) ReplyDetails(view: view),
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
  const _NoticeRow({required this.event, required this.onLink, this.onOpenTask});

  final NachoEvent event;
  final ValueChanged<String> onLink;
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
      margin: const EdgeInsets.symmetric(horizontal: Look.pagePad, vertical: 6),
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 10),
      decoration: BoxDecoration(
        color: warn ? scheme.errorContainer : null,
        borderRadius: Look.corners,
        border: warn ? null : Border.all(color: scheme.outline),
      ),
      child: Row(
        children: [
          Icon(icon, size: 18, color: warn ? scheme.onErrorContainer : scheme.onSurfaceVariant),
          const SizedBox(width: 8),
          Expanded(
            child: ReplyText(
              text: event.text,
              onLink: onLink,
              selectable: false,
              style: TextStyle(
                fontSize: Look.sub,
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
      padding: const EdgeInsets.fromLTRB(Look.pagePad, 8, Look.pagePad, 8),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.end,
        children: [
          Expanded(
            child: TextField(
              controller: controller,
              minLines: 1,
              maxLines: Look.inputMaxLines,
              // 16px 아래로 내리면 iOS 가 포커스 때 화면을 확대한다.
              style: const TextStyle(fontSize: 16),
              textInputAction: TextInputAction.newline,
              decoration: InputDecoration(hintText: hint),
            ),
          ),
          const SizedBox(width: 8),
          ListenableBuilder(
            listenable: controller,
            builder: (context, _) => SendButton(
              ready: controller.text.trim().isNotEmpty,
              busy: sending,
              onPressed: onSend,
              round: true,
            ),
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

Widget nachoPill(String text, {bool strong = false}) => _Pill(text: text, strong: strong);

String agoLabel(int ms, {DateTime? now}) {
  if (ms <= 0) return '';
  final d = (now ?? DateTime.now()).difference(DateTime.fromMillisecondsSinceEpoch(ms));
  if (d.inMinutes < 1) return '방금';
  if (d.inHours < 1) return '${d.inMinutes}분 전';
  if (d.inDays < 1) return '${d.inHours}시간 전';
  return '${d.inDays}일 전';
}

class _Pill extends StatelessWidget {
  const _Pill({required this.text, this.strong = false});

  final String text;
  final bool strong;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: Look.chipPadX, vertical: 3),
      decoration: BoxDecoration(
        borderRadius: Look.corners,
        border: Border.all(color: strong ? scheme.error : scheme.outline),
      ),
      child: Text(
        text,
        style: TextStyle(
          fontSize: Look.chip,
          fontWeight: FontWeight.w600,
          color: strong ? scheme.error : scheme.onSurfaceVariant,
        ),
      ),
    );
  }
}
