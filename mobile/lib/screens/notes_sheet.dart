import 'package:flutter/material.dart';

import '../hub_model.dart';
import '../server.dart';
import '../status_style.dart';
import '../student_art.dart';

/// 종 아이콘을 누르면 뜨는 학생 쪽지 목록 — 나쵸가 알림마다 남긴 「시킨 것 → 한 것」
/// 한 줄(2026-09-08 지시). 쪽지를 누르면 그 학생 화면으로 간다.
class NotesSheet extends StatelessWidget {
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
  }) async {
    await showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (_) => NotesSheet(model: model, server: server, onOpen: onOpen),
    );
    // 열어 봤으면 읽은 것이다 — 닫을 때 한 번에 표시한다.
    if (model.unread > 0) await model.markAllRead();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return DraggableScrollableSheet(
      expand: false,
      initialChildSize: 0.6,
      minChildSize: 0.3,
      maxChildSize: 0.92,
      builder: (context, controller) => ListenableBuilder(
        listenable: model,
        builder: (context, _) {
          final notes = model.notes;
          return Column(
            children: [
              Padding(
                padding: const EdgeInsets.fromLTRB(20, 0, 12, 4),
                child: Row(
                  children: [
                    Text('학생 쪽지', style: theme.textTheme.titleMedium),
                    const Spacer(),
                    if (model.unread > 0)
                      TextButton(
                        onPressed: model.markAllRead,
                        child: const Text('모두 읽음'),
                      ),
                  ],
                ),
              ),
              Expanded(
                child: notes.isEmpty
                    ? Center(
                        child: Text(
                          '나쵸가 남긴 쪽지가 아직 없다',
                          style: theme.textTheme.bodyMedium?.copyWith(
                            color: theme.colorScheme.onSurfaceVariant,
                          ),
                        ),
                      )
                    : ListView.separated(
                        controller: controller,
                        itemCount: notes.length,
                        separatorBuilder: (_, _) => const Divider(height: 1),
                        itemBuilder: (context, i) => _NoteRow(
                          note: notes[i],
                          pane: model.paneOfNote(notes[i]),
                          server: server,
                          onOpen: onOpen,
                        ),
                      ),
              ),
            ],
          );
        },
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
  });

  final Note note;
  final Pane? pane;
  final Server server;
  final void Function(Pane) onOpen;

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
    return ListTile(
      onTap: p == null
          ? () => ScaffoldMessenger.of(
              context,
            ).showSnackBar(const SnackBar(content: Text('그 pane 은 이제 없다')))
          : () {
              Navigator.of(context).pop();
              onOpen(p);
            },
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
