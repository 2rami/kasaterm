import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../hub_model.dart';
import '../look.dart';
import '../server.dart';
import '../status_style.dart';
import '../student_art.dart';
import '../term_session.dart';
import 'conversation_view.dart';

/// 종 아이콘을 누르면 뜨는 학생 쪽지 목록 — 사람에게 간 알림 두 가지(고를 것·끝냄)의 기록이다. 데스크톱이 폰·맥
/// 알림을 낼 때 같은 자리에서 한 줄 남긴다(2026-11-13). 안 읽은 것만 선다: 누르면 그 학생 화면으로 가며 읽음,
/// 왼쪽으로 밀면 빨간 바탕이 드러나며 지움, 「모두 읽음」은 한 장씩 스르륵 밀려
/// 나간다(2026-09-10 지시). 열어 본 것만으로는 읽음이 안 된다 — 그러면 다음에 열었을
/// 때 목록이 비어 있어 뭘 놓쳤는지 모른다.
///
/// 학생 화면으로 가지 않고 그 자리에서 답한다(2026-11-13 「그 종 아이콘에서 바로 답 가능하게 하자, 창 안 가도」):
/// 오른쪽으로 밀거나 「답」을 누르면 답 칸이 열리고, 승인·질문을 기다리는 쪽지는 고를 것이 쪽지 안에 선다.
/// 「데스크톱에서 보기」는 그 기기 앱의 초점을 그 칸으로 옮긴다.
class NotesSheet extends StatefulWidget {
  const NotesSheet({
    super.key,
    required this.model,
    required this.server,
    required this.onOpen,
    this.openSession = _connect,
  });

  final HubModel model;
  final Server server;
  final void Function(Pane) onOpen;

  /// 쪽지 안 고를 것이 읽고 칠 격자 소켓 — 시험은 붙지 않는 것을 끼운다.
  final TermSession Function(Server, Pane) openSession;
  static TermSession _connect(Server server, Pane pane) => TermSession(server, pane)..connect();

  static Future<void> show(
    BuildContext context, {
    required HubModel model,
    required Server server,
    required void Function(Pane) onOpen,
  }) => showModalBottomSheet<void>(
    context: context,
    isScrollControlled: true,
    showDragHandle: true,
    builder: (_) => NotesSheet(model: model, server: server, onOpen: onOpen),
  );

  @override
  State<NotesSheet> createState() => _NotesSheetState();
}

class _NotesSheetState extends State<NotesSheet> {
  /// 「모두 읽음」으로 떠나는 중인 쪽지 — 위에서부터 차례로 밀려 나간다.
  final Set<String> _leaving = {};
  bool _clearing = false;

  /// 답 칸을 연 쪽지 — 한 번에 하나라 자판이 비켜 줄 쪽지도 하나다.
  String? _replying;
  final _drafts = <String, TextEditingController>{};
  final _sending = <String>{};
  final _sheet = DraggableScrollableController();
  static const _maxSize = 0.92;

  @override
  void dispose() {
    for (final d in _drafts.values) {
      d.dispose();
    }
    _sheet.dispose();
    super.dispose();
  }

  TextEditingController _draft(Note n) => _drafts.putIfAbsent(n.key, TextEditingController.new);

  /// 답 칸을 열면 시트를 끝까지 올린다 — 자판이 올라와도 쪽지와 답 칸이 함께 보일 높이.
  void _openReply(Note n) {
    setState(() => _replying = n.key);
    if (_sheet.isAttached && _sheet.size < _maxSize) {
      unawaited(_sheet.animateTo(_maxSize, duration: const Duration(milliseconds: 220), curve: Curves.easeOut));
    }
  }

  void _toggleReply(Note n) => _replying == n.key ? setState(() => _replying = null) : _openReply(n);

  Future<void> _send(Note n) async {
    final draft = _draft(n);
    final text = draft.text.trim();
    if (text.isEmpty || _sending.contains(n.key)) return;
    HapticFeedback.selectionClick();
    setState(() => _sending.add(n.key));
    final r = await widget.model.replyToNote(n, text);
    if (!mounted) return;
    setState(() {
      _sending.remove(n.key);
      // 못 보냈으면 쓴 글과 답 칸을 그대로 둔다 — 까닭을 보고 다시 누르면 된다.
      if (r.sent) {
        draft.clear();
        if (_replying == n.key) _replying = null;
      }
    });
  }

  /// 데스크톱에서 그 칸으로 — 그 기기 앱이 그 칸의 방으로 넘어가 초점을 준다(`surface.focus`). 폰 화면은 그대로다.
  Future<void> _focusDesktop(Note n) async {
    final messenger = ScaffoldMessenger.of(context);
    HapticFeedback.selectionClick();
    try {
      await widget.server.cmd('surface.focus', {'surface_id': n.pane}, machine: n.machine);
      messenger.showSnackBar(const SnackBar(content: Text('데스크톱에서 그 칸을 열었어요')));
    } on ServerException catch (e) {
      messenger.showSnackBar(SnackBar(content: Text('데스크톱을 못 옮겼어요 · ${e.message}')));
    }
  }

  Future<void> _readAll(List<Note> notes) async {
    if (_clearing || notes.isEmpty) return;
    setState(() {
      _clearing = true;
      _replying = null;
    });
    // 스무 장이 넘어도 전체가 1초 안에 끝나게 간격을 줄인다.
    final gap = Duration(
      milliseconds: math.max(24, math.min(70, 900 ~/ notes.length)),
    );
    for (final n in notes) {
      if (!mounted) return;
      setState(() => _leaving.add(n.key));
      await Future<void>.delayed(gap);
    }
    await Future<void>.delayed(_NoteExit.duration);
    await widget.model.markAllRead();
    if (!mounted) return;
    setState(() {
      _leaving.clear();
      _clearing = false;
    });
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final model = widget.model;
    // 아래 시트는 자판에 밀려 올라가지 않는다 — 자판 높이만큼 비워 답 칸이 그 위에 선다.
    return Padding(
      padding: EdgeInsets.only(bottom: MediaQuery.viewInsetsOf(context).bottom),
      child: DraggableScrollableSheet(
        controller: _sheet,
        expand: false,
        initialChildSize: 0.6,
        minChildSize: 0.3,
        maxChildSize: _maxSize,
        builder: (context, controller) => ListenableBuilder(
          listenable: model,
          builder: (context, _) {
            final lines = model.noteList;
            // 같은 칸의 쪽지가 여럿이면 고를 것은 맨 위(최근) 쪽지에만 — 같은 창이 여러 번 서지 않게.
            final asksAt = <String>{};
            final asking = <String>{};
            for (final l in lines) {
              final n = l.note;
              if (n.kind != 'permission' && n.kind != 'question') continue;
              if (asksAt.add('${n.machine ?? ''}|${n.pane}')) asking.add(n.key);
            }
            return Column(
              children: [
                Padding(
                  padding: const EdgeInsets.fromLTRB(20, 0, 12, 4),
                  child: Row(
                    children: [
                      Text('학생 쪽지', style: theme.textTheme.titleMedium),
                      const Spacer(),
                      if (lines.isNotEmpty)
                        TextButton(
                          onPressed: _clearing ? null : () => _readAll([for (final l in lines) l.note]),
                          child: const Text('모두 읽음'),
                        ),
                    ],
                  ),
                ),
                Expanded(
                  child: lines.isEmpty
                      ? Center(
                          child: Text(
                            '안 읽은 쪽지가 없다',
                            style: theme.textTheme.bodyMedium?.copyWith(
                              color: theme.colorScheme.onSurfaceVariant,
                            ),
                          ),
                        )
                      : ListView.separated(
                          controller: controller,
                          itemCount: lines.length,
                          separatorBuilder: (_, _) => const Divider(height: 1),
                          itemBuilder: (context, i) {
                            final line = lines[i];
                            final n = line.note;
                            final p = model.paneOfNote(n);
                            final online = model.noteOnline(n);
                            void open(Pane p) {
                              model.markRead(n);
                              widget.onOpen(p);
                            }

                            return _NoteExit(
                              leaving: _leaving.contains(n.key),
                              child: Column(
                                crossAxisAlignment: CrossAxisAlignment.stretch,
                                children: [
                                  // 밀기는 쪽지 줄에만 — 답 칸에서 글을 고르다 옆으로 끌어도 쪽지가 지워지지 않게.
                                  Dismissible(
                                    key: ValueKey('note-${n.key}'),
                                    background: const _ReplyBackground(),
                                    secondaryBackground: const _DeleteBackground(),
                                    dismissThresholds: const {DismissDirection.startToEnd: 0.25},
                                    // 오른쪽 밀기는 답 칸만 열고 쪽지는 제자리로 돌아온다.
                                    confirmDismiss: (d) async {
                                      if (d == DismissDirection.endToStart) return true;
                                      _openReply(n);
                                      return false;
                                    },
                                    onDismissed: (_) => model.deleteNote(n),
                                    child: _NoteRow(
                                      note: n,
                                      pane: p,
                                      server: widget.server,
                                      sent: line.sent,
                                      before: line.before,
                                      replying: _replying == n.key,
                                      onReply: () => _toggleReply(n),
                                      onOpen: open,
                                      onDesk: p != null && online ? () => unawaited(_focusDesktop(n)) : null,
                                    ),
                                  ),
                                  if (p != null && online && p.needsYou && asking.contains(n.key))
                                    _NoteAsks(
                                      key: ValueKey('asks-${n.key}'),
                                      session: () => widget.openSession(widget.server, p),
                                      server: widget.server,
                                      pane: p,
                                      draft: _draft(n),
                                      onTerminal: () {
                                        Navigator.of(context).pop();
                                        open(p);
                                      },
                                    ),
                                  if (_replying == n.key)
                                    _ReplyBox(
                                      key: ValueKey('reply-${n.key}'),
                                      blocked: noteReplyBlock(p, online: online),
                                      draft: _draft(n),
                                      sending: _sending.contains(n.key),
                                      hint: '${n.character.isEmpty ? '학생' : n.character}에게 답',
                                      onSend: () => unawaited(_send(n)),
                                    ),
                                ],
                              ),
                            );
                          },
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

/// 답 칸을 막는 까닭 — 닿을 수 없거나 받을 학생이 없는 칸. 막히지 않으면 null.
String? noteReplyBlock(Pane? p, {required bool online}) {
  if (!online) return '기기가 끊겨 답할 수 없어요';
  if (p == null) return '칸이 닫혀 답할 수 없어요';
  // 학생이 나가고 셸만 남은 칸에 글을 넣으면 명령으로 돈다.
  if (p.isShell) return '학생이 나가 셸만 남았어요';
  return null;
}

/// 승인·질문을 기다리는 쪽지 안의 고를 것 — 대화 보기와 같은 판정·보내기(`ConversationView.asksOnly`)를
/// 이 쪽지만의 격자 소켓으로 돌린다. 쪽지가 목록에서 빠지거나 학생이 더 안 기다리면 소켓도 닫는다.
class _NoteAsks extends StatefulWidget {
  const _NoteAsks({super.key, required this.session, required this.server, required this.pane, required this.draft, required this.onTerminal});

  final TermSession Function() session;
  final Server server;
  final Pane pane;

  /// 답 칸의 글 — 「거절」이 그것을 까닭으로 함께 보낸다.
  final TextEditingController draft;
  final VoidCallback onTerminal;

  @override
  State<_NoteAsks> createState() => _NoteAsksState();
}

class _NoteAsksState extends State<_NoteAsks> {
  late final TermSession _session = widget.session();

  @override
  void dispose() {
    _session.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.only(bottom: 6),
    child: ListenableBuilder(
      listenable: _session,
      builder: (context, _) =>
          ConversationView(server: widget.server, pane: widget.pane, session: _session, accent: StatusStyle.attention, onTerminal: widget.onTerminal, draft: widget.draft, asksOnly: true),
    ),
  );
}

/// 쪽지 밑 답 칸 — 대화 보기 입력줄 그대로(한 줄에서 6줄까지 자란다). 자판이 오르내리면 이 쪽지를 자판 위로 당긴다.
class _ReplyBox extends StatefulWidget {
  const _ReplyBox({super.key, required this.blocked, required this.draft, required this.sending, required this.hint, required this.onSend});

  final String? blocked;
  final TextEditingController draft;
  final bool sending;
  final String hint;
  final VoidCallback onSend;

  @override
  State<_ReplyBox> createState() => _ReplyBoxState();
}

class _ReplyBoxState extends State<_ReplyBox> {
  final _focus = FocusNode();
  double? _inset;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final inset = MediaQuery.viewInsetsOf(context).bottom;
    if (inset == _inset) return;
    _inset = inset;
    WidgetsBinding.instance.addPostFrameCallback((_) => _reveal());
  }

  /// 답 칸의 아래 끝을 보이는 칸 아래 끝에 맞춘다 — 답 칸이 자판 바로 위에, 쪽지와 고를 것이 그 위에 선다.
  void _reveal() {
    if (mounted) unawaited(Scrollable.ensureVisible(context, alignment: 1));
  }

  @override
  void dispose() {
    _focus.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final why = widget.blocked;
    if (why != null) {
      return Padding(
        padding: const EdgeInsets.fromLTRB(Look.pagePad, 0, Look.pagePad, 12),
        child: Row(
          children: [
            Icon(Icons.block, size: 16, color: scheme.onSurfaceVariant),
            const SizedBox(width: 6),
            Expanded(
              child: Text(
                why,
                style: TextStyle(fontSize: Look.sub, color: scheme.onSurfaceVariant),
              ),
            ),
          ],
        ),
      );
    }
    // 보내는 중에도 칸은 살려 둔다 — 끄면 자판이 내려가 못 보냈을 때 다시 쓰려면 또 눌러야 한다. 두 번 보내기는 단추가 막는다.
    return ChatComposer(
      controller: widget.draft,
      focusNode: _focus,
      enabled: true,
      busy: widget.sending,
      autofocus: true,
      hint: widget.hint,
      onSend: widget.onSend,
    );
  }
}

/// 「모두 읽음」의 퇴장 — 오른쪽으로 미끄러지며 옅어진다. 자리는 목록이 갱신될 때
/// 한꺼번에 접힌다(한 장씩 접으면 아래 줄이 계속 튀어 오른다).
class _NoteExit extends StatelessWidget {
  const _NoteExit({required this.leaving, required this.child});

  static const duration = Duration(milliseconds: 280);

  final bool leaving;
  final Widget child;

  @override
  Widget build(BuildContext context) => AnimatedSlide(
    offset: leaving ? const Offset(1.1, 0) : Offset.zero,
    duration: duration,
    curve: Curves.easeInCubic,
    child: AnimatedOpacity(
      opacity: leaving ? 0 : 1,
      duration: duration,
      curve: Curves.easeIn,
      child: IgnorePointer(ignoring: leaving, child: child),
    ),
  );
}

/// 오른쪽으로 밀 때 드러나는 답 바탕 — 메일 앱의 「답장」 자리.
class _ReplyBackground extends StatelessWidget {
  const _ReplyBackground();

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      color: scheme.primary,
      alignment: Alignment.centerLeft,
      padding: const EdgeInsets.only(left: 22),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(Icons.reply_rounded, color: scheme.onPrimary),
          Text(
            '답',
            style: Theme.of(context).textTheme.labelSmall?.copyWith(color: scheme.onPrimary, fontWeight: FontWeight.w600),
          ),
        ],
      ),
    );
  }
}

/// 왼쪽으로 밀 때 드러나는 빨간 바탕 — 메일 앱의 그것.
class _DeleteBackground extends StatelessWidget {
  const _DeleteBackground();

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      color: scheme.error,
      alignment: Alignment.centerRight,
      padding: const EdgeInsets.only(right: 22),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(Icons.delete_outline, color: scheme.onError),
          Text(
            '지우기',
            style: Theme.of(context).textTheme.labelSmall?.copyWith(
              color: scheme.onError,
              fontWeight: FontWeight.w600,
            ),
          ),
        ],
      ),
    );
  }
}

/// 쪽지 종류 → 말·색. 상태 칩과 같은 색 체계 — 고를 것은 주의색, 끝냄은 성공색.
(String, Color) noteKindStyle(String kind, ColorScheme scheme) =>
    switch (kind) {
      'permission' => ('승인', StatusStyle.attention),
      'question' => ('질문', StatusStyle.attention),
      'blocked' => ('막힘', StatusStyle.attention),
      'done' => ('끝냄', StatusStyle.success),
      _ => (kind, scheme.onSurfaceVariant),
    };

String timeAgo(DateTime when, [DateTime? now]) {
  final d = (now ?? DateTime.now()).difference(when);
  if (d.inMinutes < 1) return '방금';
  if (d.inHours < 1) return '${d.inMinutes}분 전';
  if (d.inDays < 1) return '${d.inHours}시간 전';
  return '${d.inDays}일 전';
}

/// 보낸 답의 시각 — 오늘 안의 일이라 시:분만.
String clockLabel(DateTime t) => '${t.hour.toString().padLeft(2, '0')}:${t.minute.toString().padLeft(2, '0')}';

class _NoteRow extends StatelessWidget {
  const _NoteRow({
    required this.note,
    required this.pane,
    required this.server,
    required this.onOpen,
    required this.onReply,
    this.onDesk,
    this.sent,
    this.before,
    this.replying = false,
  });

  final Note note;
  final Pane? pane;
  final Server server;
  final void Function(Pane) onOpen;
  final VoidCallback onReply;

  /// 데스크톱에서 그 칸으로 초점을 옮긴다. 닿지 않는 칸이면 null.
  final VoidCallback? onDesk;

  /// 이 쪽지에서 보낸 답.
  final NoteReply? sent;

  /// 이 쪽지가 학생의 다음 답이라 앞에 다는 내 답.
  final NoteReply? before;
  final bool replying;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final (label, color) = noteKindStyle(note.kind, scheme);
    final p = pane;
    return ListTile(
      onTap: p == null
          ? () => ScaffoldMessenger.of(
              context,
            ).showSnackBar(const SnackBar(content: Text('그 pane 은 이제 없다')))
          : () {
              Navigator.of(context).pop();
              onOpen(p);
            },
      // 「답」은 오른쪽 끝 — 엄지가 닿는 자리. 데스크톱으로 옮기기는 그 앞.
      trailing: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (onDesk != null)
            IconButton(
              tooltip: '데스크톱에서 보기',
              onPressed: onDesk,
              icon: const Icon(Icons.desktop_windows_outlined, size: Look.iconSize),
              color: scheme.onSurfaceVariant,
            ),
          IconButton(
            tooltip: replying ? '답 칸 닫기' : '답',
            onPressed: onReply,
            isSelected: replying,
            icon: const Icon(Icons.reply_rounded, size: Look.iconSize),
            color: scheme.onSurfaceVariant,
            selectedIcon: Icon(Icons.reply_rounded, size: Look.iconSize, color: scheme.primary),
          ),
        ],
      ),
      contentPadding: const EdgeInsets.only(left: Look.pagePad, right: 4),
      leading: StudentFace(
        server: server,
        slug: p?.slug,
        url: p?.slug == null
            ? null
            : server.avatar(p!.slug!, machine: note.machine),
        shell: p == null || p.isShell,
        size: 40,
      ),
      title: Row(
        children: [
          Flexible(
            child: Text(
              note.character.isEmpty ? note.pane : note.character,
              style: theme.textTheme.titleSmall,
              overflow: TextOverflow.ellipsis,
            ),
          ),
          const SizedBox(width: 6),
          Container(
            padding: const EdgeInsets.symmetric(horizontal: Look.chipPadX, vertical: 2),
            decoration: ShapeDecoration(
              shape: const StadiumBorder(),
              color: color.withValues(alpha: 0.16),
            ),
            child: Text(
              label,
              style: theme.textTheme.labelSmall?.copyWith(
                color: color,
                fontWeight: FontWeight.w600,
              ),
            ),
          ),
          const Spacer(),
          Text(
            timeAgo(note.when),
            style: theme.textTheme.labelSmall?.copyWith(
              color: scheme.onSurfaceVariant,
            ),
          ),
        ],
      ),
      subtitle: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          const SizedBox(height: 2),
          if (before case final r?) _ReplyQuote(reply: r, earlier: true),
          Text(
            note.summary,
            maxLines: 3,
            overflow: TextOverflow.ellipsis,
            style: theme.textTheme.bodyMedium?.copyWith(
              fontWeight: note.read ? FontWeight.w400 : FontWeight.w600,
            ),
          ),
          if (sent case final r?) _ReplyQuote(reply: r, earlier: false),
        ],
      ),
      isThreeLine: true,
    );
  }
}

/// 쪽지 안의 내 답 — 이 쪽지에서 보낸 것은 아래에 「보냈어요 · 시각」(못 갔으면 까닭), 학생의 다음 쪽지가 이어 받은 것은
/// 위에 「내 답 · 시각」. 무엇에 대한 답인지 쪽지만 보고 알게 한다.
class _ReplyQuote extends StatelessWidget {
  const _ReplyQuote({required this.reply, required this.earlier});

  final NoteReply reply;
  final bool earlier;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final failed = !reply.sent;
    final ink = failed ? scheme.error : (earlier ? scheme.onSurfaceVariant : StatusStyle.successText(scheme));
    final head = failed
        ? '못 보냈어요 · ${reply.error}'
        : earlier
        ? '내 답 · ${clockLabel(reply.at)}'
        : '보냈어요 · ${clockLabel(reply.at)}';
    return Padding(
      padding: EdgeInsets.only(top: earlier ? 2 : 6, bottom: earlier ? 4 : 0),
      child: Container(
        padding: const EdgeInsets.fromLTRB(10, 6, 10, 6),
        decoration: BoxDecoration(color: scheme.surfaceContainerHigh, borderRadius: Look.smallCorners),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Icon(failed ? Icons.error_outline : (earlier ? Icons.reply_rounded : Icons.check_circle_outline), size: 16, color: ink),
                const SizedBox(width: 4),
                Expanded(
                  child: Text(
                    head,
                    maxLines: 2,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(fontSize: Look.sub, fontWeight: FontWeight.w600, color: ink),
                  ),
                ),
              ],
            ),
            Text(
              reply.text,
              maxLines: earlier ? 1 : 3,
              overflow: TextOverflow.ellipsis,
              style: TextStyle(fontSize: Look.sub, color: scheme.onSurface),
            ),
          ],
        ),
      ),
    );
  }
}
