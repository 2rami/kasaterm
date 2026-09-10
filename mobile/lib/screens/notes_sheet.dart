import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:url_launcher/url_launcher.dart';

import '../hub_model.dart';
import '../server.dart';
import '../status_style.dart';
import '../student_art.dart';

/// 종 아이콘을 누르면 뜨는 학생 쪽지 목록 — 나쵸가 알림마다 남긴 「시킨 것 → 한 것」
/// 한 줄(2026-09-08 지시). 안 읽은 것만 선다: 누르면 그 학생 화면으로 가며 읽음,
/// 옆으로 밀면 빨간 바탕이 드러나며 지움, 「모두 읽음」은 한 장씩 스르륵 밀려
/// 나간다(2026-09-10 지시). 열어 본 것만으로는 읽음이 안 된다 — 그러면 다음에 열었을
/// 때 목록이 비어 있어 뭘 놓쳤는지 모른다.
class NotesSheet extends StatefulWidget {
  const NotesSheet({
    super.key,
    required this.model,
    required this.server,
    required this.onOpen,
  });

  final HubModel model;
  final Server server;
  final void Function(Pane) onOpen;

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

  Future<void> _readAll(List<Note> notes) async {
    if (_clearing || notes.isEmpty) return;
    setState(() => _clearing = true);
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
    return DraggableScrollableSheet(
      expand: false,
      initialChildSize: 0.6,
      minChildSize: 0.3,
      maxChildSize: 0.92,
      builder: (context, controller) => ListenableBuilder(
        listenable: model,
        builder: (context, _) {
          final notes = [
            for (final n in model.notes)
              if (!n.read) n,
          ];
          return Column(
            children: [
              Padding(
                padding: const EdgeInsets.fromLTRB(20, 0, 12, 4),
                child: Row(
                  children: [
                    Text('학생 쪽지', style: theme.textTheme.titleMedium),
                    const Spacer(),
                    if (notes.isNotEmpty)
                      TextButton(
                        onPressed: _clearing ? null : () => _readAll(notes),
                        child: const Text('모두 읽음'),
                      ),
                  ],
                ),
              ),
              Expanded(
                child: notes.isEmpty
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
                        itemCount: notes.length,
                        separatorBuilder: (_, _) => const Divider(height: 1),
                        itemBuilder: (context, i) {
                          final n = notes[i];
                          return _NoteExit(
                            leaving: _leaving.contains(n.key),
                            child: Dismissible(
                              key: ValueKey('note-${n.key}'),
                              direction: DismissDirection.endToStart,
                              background: const _DeleteBackground(),
                              onDismissed: (_) => model.deleteNote(n),
                              child: _NoteRow(
                                note: n,
                                pane: model.paneOfNote(n),
                                server: widget.server,
                                onOpen: (p) {
                                  model.markRead(n);
                                  widget.onOpen(p);
                                },
                                onLink: (u) {
                                  model.markRead(n);
                                  launchUrl(
                                    u,
                                    mode: LaunchMode.externalApplication,
                                  );
                                },
                              ),
                            ),
                          );
                        },
                      ),
              ),
            ],
          );
        },
      ),
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

/// 옆으로 밀 때 드러나는 빨간 바탕 — 메일 앱의 그것.
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
              fontWeight: FontWeight.w700,
            ),
          ),
        ],
      ),
    );
  }
}

/// 쪽지 종류 → 말·색. 상태 칩과 같은 색 체계다.
(String, Color) noteKindStyle(String kind, ColorScheme scheme) =>
    switch (kind) {
      'permission' => ('승인 기다림', StatusStyle.attention),
      'question' => ('질문', StatusStyle.attention),
      'waiting' => ('답 기다림', StatusStyle.attention),
      'idle' => ('오래 기다림', StatusStyle.attention),
      'done_ok' => ('끝냄', StatusStyle.success),
      'done_fail' => ('실패', scheme.error),
      'dead' => ('사라짐', scheme.outline),
      'link' => ('페이지', scheme.primary),
      _ => (kind, scheme.onSurfaceVariant),
    };

String timeAgo(DateTime when, [DateTime? now]) {
  final d = (now ?? DateTime.now()).difference(when);
  if (d.inMinutes < 1) return '방금';
  if (d.inHours < 1) return '${d.inMinutes}분 전';
  if (d.inDays < 1) return '${d.inHours}시간 전';
  return '${d.inDays}일 전';
}

class _NoteRow extends StatelessWidget {
  const _NoteRow({
    required this.note,
    required this.pane,
    required this.server,
    required this.onOpen,
    required this.onLink,
  });

  final Note note;
  final Pane? pane;
  final Server server;
  final void Function(Pane) onOpen;
  final void Function(Uri) onLink;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final (label, color) = noteKindStyle(note.kind, scheme);
    final p = pane;
    final detail = [
      if (note.asked.isNotEmpty) note.asked,
      if (note.did.isNotEmpty) note.did,
    ].join('  →  ');
    final link = Uri.tryParse(note.url);
    final hasLink =
        link != null && (link.scheme == 'http' || link.scheme == 'https');
    return ListTile(
      // 페이지 쪽지는 학생 화면이 아니라 그 주소로 — 「폰」을 골라 둔 동안 학생이
      // 보여 주려 연 것이라 사파리가 맞다.
      onTap: hasLink
          ? () => onLink(link)
          : p == null
          ? () => ScaffoldMessenger.of(
              context,
            ).showSnackBar(const SnackBar(content: Text('그 pane 은 이제 없다')))
          : () {
              Navigator.of(context).pop();
              onOpen(p);
            },
      trailing: hasLink ? const Icon(Icons.open_in_new, size: 18) : null,
      leading: StudentFace(
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
            padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
            decoration: BoxDecoration(
              color: color.withValues(alpha: 0.14),
              borderRadius: BorderRadius.circular(999),
            ),
            child: Text(
              label,
              style: theme.textTheme.labelSmall?.copyWith(
                color: color,
                fontWeight: FontWeight.w700,
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
          Text(
            note.summary,
            style: theme.textTheme.bodyMedium?.copyWith(
              fontWeight: note.read ? FontWeight.w400 : FontWeight.w700,
            ),
          ),
          if (detail.isNotEmpty)
            Text(
              detail,
              style: theme.textTheme.bodySmall?.copyWith(
                color: scheme.onSurfaceVariant,
              ),
              maxLines: 2,
              overflow: TextOverflow.ellipsis,
            ),
          if (note.image)
            Padding(
              padding: const EdgeInsets.only(top: 6),
              child: GestureDetector(
                onTap: () => showDialog<void>(
                  context: context,
                  builder: (_) => Dialog(
                    child: InteractiveViewer(
                      child: Image.network(
                        server
                            .noteImage(note.id, machine: note.machine)
                            .toString(),
                      ),
                    ),
                  ),
                ),
                child: ClipRRect(
                  borderRadius: BorderRadius.circular(6),
                  child: Image.network(
                    server.noteImage(note.id, machine: note.machine).toString(),
                    height: 72,
                    width: 128,
                    fit: BoxFit.cover,
                    alignment: Alignment.topLeft,
                    errorBuilder: (_, _, _) => const SizedBox.shrink(),
                  ),
                ),
              ),
            ),
        ],
      ),
      isThreeLine: detail.isNotEmpty || note.image,
    );
  }
}
