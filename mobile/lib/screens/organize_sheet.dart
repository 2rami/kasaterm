import 'package:flutter/material.dart';

import '../nacho.dart';
import '../organize.dart';
import '../server.dart';
import '../status_style.dart';
import '../workboard.dart';

/// 작업판에서 고른 일 하나의 정리 — 현재 작업·변경·다음 일·막힘·검증. PC 작업 탭 정리 칸과 같은 말.
/// 장부 상세는 [load] 가 가져오고(없으면 판만으로), 못 가져오면 그 까닭을 칸에 적는다.
class OrganizeSheet extends StatelessWidget {
  const OrganizeSheet({
    super.key,
    required this.item,
    this.load,
    this.ledgerProblem,
    this.onOpenTask,
    this.onOpenPane,
  });

  final WorkItem item;
  final Future<NachoTaskDetail?>? load;

  /// 작업판이 장부를 못 읽은 까닭(키 없는 허브 등).
  final String? ledgerProblem;
  final VoidCallback? onOpenTask;
  final VoidCallback? onOpenPane;

  @override
  Widget build(BuildContext context) => DraggableScrollableSheet(
    expand: false,
    initialChildSize: 0.8,
    minChildSize: 0.4,
    maxChildSize: 0.95,
    builder: (context, controller) => FutureBuilder<NachoTaskDetail?>(
      future: load ?? Future<NachoTaskDetail?>.value(null),
      builder: (context, snap) {
        final loading =
            load != null && snap.connectionState != ConnectionState.done;
        final error = snap.error;
        final view = organize(
          item: item,
          task: snap.data,
          ledgerProblem: error == null
              ? ledgerProblem
              : error is NachoError
              ? error.message
              : error is ServerException
              ? error.message
              : '$error',
        );
        return ListView(
          controller: controller,
          padding: const EdgeInsets.fromLTRB(20, 0, 20, 24),
          children: [
            _Head(view: view),
            const SizedBox(height: 12),
            if (loading)
              const Padding(
                padding: EdgeInsets.symmetric(vertical: 32),
                child: Center(child: CircularProgressIndicator.adaptive()),
              )
            else ...[
              for (final s in view.slots) _Slot(slot: s),
              const SizedBox(height: 4),
              for (final s in view.sources) _Source(text: s),
            ],
            if (onOpenTask != null || onOpenPane != null) ...[
              const SizedBox(height: 14),
              Row(
                children: [
                  if (onOpenTask != null)
                    Expanded(
                      child: OutlinedButton(
                        onPressed: onOpenTask,
                        style: OutlinedButton.styleFrom(
                          minimumSize: const Size.fromHeight(48),
                        ),
                        child: const Text('작업 상세'),
                      ),
                    ),
                  if (onOpenTask != null && onOpenPane != null)
                    const SizedBox(width: 8),
                  if (onOpenPane != null)
                    Expanded(
                      child: OutlinedButton(
                        onPressed: onOpenPane,
                        style: OutlinedButton.styleFrom(
                          minimumSize: const Size.fromHeight(48),
                        ),
                        child: const Text('학생 화면'),
                      ),
                    ),
                ],
              ),
            ],
          ],
        );
      },
    ),
  );
}

class _Head extends StatelessWidget {
  const _Head({required this.view});

  final OrganizeView view;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final dim = TextStyle(
      fontSize: 13,
      color: theme.colorScheme.onSurfaceVariant,
    );
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            Text('정리', style: dim.copyWith(fontWeight: FontWeight.w600)),
            if (view.demo) ...[
              const SizedBox(width: 8),
              Text('예시', style: dim),
            ],
          ],
        ),
        const SizedBox(height: 2),
        Text(view.title, style: theme.textTheme.titleLarge),
      ],
    );
  }
}

class _Slot extends StatelessWidget {
  const _Slot({required this.slot});

  final OrganizeSlot slot;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final (bar, ink) = switch (slot.tone) {
      SlotTone.alert => (StatusStyle.attentionInk, StatusStyle.attentionInk),
      SlotTone.ok => (StatusStyle.successInk, scheme.onSurface),
      SlotTone.quiet => (scheme.outlineVariant, scheme.onSurfaceVariant),
      SlotTone.plain => (scheme.outlineVariant, scheme.onSurface),
    };
    return Container(
      margin: const EdgeInsets.only(bottom: 10),
      padding: const EdgeInsets.only(left: 12),
      decoration: BoxDecoration(
        border: Border(left: BorderSide(color: bar, width: 3)),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            slot.label,
            style: TextStyle(fontSize: 13, color: scheme.onSurfaceVariant),
          ),
          const SizedBox(height: 2),
          SelectableText(
            slot.value,
            style: TextStyle(fontSize: 15, height: 1.45, color: ink),
          ),
        ],
      ),
    );
  }
}

class _Source extends StatelessWidget {
  const _Source({required this.text});

  final String text;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.only(top: 2),
    child: Text(
      text,
      style: TextStyle(
        fontSize: 12,
        color: Theme.of(context).colorScheme.onSurfaceVariant,
      ),
    ),
  );
}
