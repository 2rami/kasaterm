import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../look.dart';
import '../server.dart';
import '../student_art.dart';

class HubDragData {
  HubDragData(Iterable<Pane> panes) : panes = List.unmodifiable(panes);
  final List<Pane> panes;
  bool canMoveTo(Pane target) =>
      panes.isNotEmpty &&
      panes.every(
        (pane) =>
            pane.machine == target.machine && pane.viewKey != target.viewKey,
      );
}

enum HubDropAction { select, hide, trash, more }

class HubDragItem extends StatefulWidget {
  const HubDragItem({
    super.key,
    required this.server,
    required this.pane,
    required this.data,
    required this.child,
    required this.onStart,
    required this.onUpdate,
    required this.onEnd,
    required this.onMenu,
    this.selected = false,
    this.enabled = true,
  });
  final Server server;
  final Pane pane;
  final HubDragData data;
  final Widget child;
  final bool selected;
  final bool enabled;
  final ValueChanged<HubDragData> onStart;
  final ValueChanged<Offset> onUpdate;
  final VoidCallback onEnd;
  final VoidCallback onMenu;
  @override
  State<HubDragItem> createState() => _HubDragItemState();
}

class _HubDragItemState extends State<HubDragItem> {
  bool _moved = false;
  final _pointer = ValueNotifier(Offset.zero);
  @override
  void dispose() {
    _pointer.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final data = widget.data;
    final media = MediaQuery.of(context);
    final feedbackHeight = (media.textScaler.scale(16) * 3 + 24).clamp(
      80.0,
      180.0,
    );
    return Listener(
      onPointerDown: (event) => _pointer.value = event.position,
      child: LongPressDraggable<HubDragData>(
        data: data,
        maxSimultaneousDrags: widget.enabled ? 1 : 0,
        dragAnchorStrategy: pointerDragAnchorStrategy,
        onDragStarted: () {
          _moved = false;
          HapticFeedback.mediumImpact();
          widget.onStart(data);
        },
        onDragUpdate: (details) {
          _moved = true;
          _pointer.value = details.globalPosition;
          widget.onUpdate(details.globalPosition);
        },
        onDragEnd: (details) {
          widget.onEnd();
          if (!details.wasAccepted && !_moved) widget.onMenu();
        },
        feedback: ValueListenableBuilder<Offset>(
          valueListenable: _pointer,
          builder: (context, point, child) {
            final left = (point.dx - 92).clamp(8.0, media.size.width - 192);
            final above = point.dy - feedbackHeight - 16;
            final top = (above < media.padding.top + 8 ? point.dy + 16 : above)
                .clamp(
                  media.padding.top + 8,
                  media.size.height - feedbackHeight - media.padding.bottom - 8,
                );
            return Transform.translate(
              offset: Offset(left - point.dx, top - point.dy),
              child: child,
            );
          },
          child: Material(
            elevation: 8,
            color: scheme.surfaceContainerHigh,
            borderRadius: Look.corners,
            child: SizedBox(
              width: 184,
              height: feedbackHeight,
              child: Padding(
                padding: const EdgeInsets.all(12),
                child: Row(
                  children: [
                    StudentFace(
                      server: widget.server,
                      slug: widget.pane.slug,
                      shell: widget.pane.isShell,
                      url: widget.pane.slug == null
                          ? null
                          : widget.server.avatar(
                              widget.pane.slug!,
                              machine: widget.pane.machine,
                            ),
                    ),
                    const SizedBox(width: 8),
                    Expanded(
                      child: Text(
                        data.panes.length > 1
                            ? '${data.panes.length}개 선택'
                            : widget.pane.rowTitle,
                        maxLines: 2,
                        overflow: TextOverflow.ellipsis,
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ),
        ),
        childWhenDragging: Opacity(opacity: .3, child: widget.child),
        child: DecoratedBox(
          position: DecorationPosition.foreground,
          decoration: BoxDecoration(
            borderRadius: Look.smallCorners,
            border: widget.selected
                ? Border.all(color: scheme.primary, width: 3)
                : null,
          ),
          child: Semantics(selected: widget.selected, child: widget.child),
        ),
      ),
    );
  }
}

String hubDropDirection(Offset point, Size size) {
  final x = (point.dx / size.width).clamp(0.0, 1.0);
  final y = (point.dy / size.height).clamp(0.0, 1.0);
  final edges = [(x, 'left'), (1 - x, 'right'), (y, 'up'), (1 - y, 'down')]
    ..sort((a, b) => a.$1.compareTo(b.$1));
  return edges.first.$2;
}

class HubDropDestination extends StatefulWidget {
  const HubDropDestination({
    super.key,
    required this.target,
    required this.child,
    required this.onDrop,
    this.enabled = true,
  });
  final Pane target;
  final Widget child;
  final bool enabled;
  final void Function(HubDragData, Pane, String) onDrop;
  @override
  State<HubDropDestination> createState() => _HubDropDestinationState();
}

class _HubDropDestinationState extends State<HubDropDestination> {
  String _direction = 'right';
  String _at(Offset point) {
    final box = context.findRenderObject() as RenderBox;
    return hubDropDirection(box.globalToLocal(point), box.size);
  }

  @override
  Widget build(BuildContext context) => DragTarget<HubDragData>(
    onWillAcceptWithDetails: (details) =>
        widget.enabled && details.data.canMoveTo(widget.target),
    onMove: (details) {
      final direction = _at(details.offset);
      if (direction != _direction) setState(() => _direction = direction);
    },
    onAcceptWithDetails: (details) =>
        widget.onDrop(details.data, widget.target, _at(details.offset)),
    builder: (context, accepted, rejected) {
      final color = Theme.of(context).colorScheme.primary;
      final side = BorderSide(color: color, width: 5);
      return DecoratedBox(
        position: DecorationPosition.foreground,
        decoration: BoxDecoration(
          border: accepted.isEmpty
              ? null
              : Border(
                  left: _direction == 'left' ? side : BorderSide.none,
                  right: _direction == 'right' ? side : BorderSide.none,
                  top: _direction == 'up' ? side : BorderSide.none,
                  bottom: _direction == 'down' ? side : BorderSide.none,
                ),
        ),
        child: widget.child,
      );
    },
  );
}

class HubActionDock extends StatelessWidget {
  const HubActionDock({
    super.key,
    required this.count,
    required this.onAction,
    required this.onCancel,
    required this.busy,
    this.drag,
  });
  final int count;
  final HubDragData? drag;
  final bool busy;
  final void Function(HubDropAction, HubDragData?) onAction;
  final VoidCallback onCancel;
  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Material(
      elevation: 8,
      color: scheme.surfaceContainerHigh,
      borderRadius: Look.corners,
      child: Padding(
        padding: const EdgeInsets.all(8),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Row(
              children: [
                Expanded(
                  child: Text(
                    drag != null ? '같은 기기의 칸 가장자리로 이동' : '$count개 선택',
                    style: Theme.of(context).textTheme.bodySmall,
                  ),
                ),
                IconButton(
                  tooltip: '선택 끝내기',
                  onPressed: busy ? null : onCancel,
                  icon: const Icon(Icons.close),
                ),
              ],
            ),
            if (busy) const LinearProgressIndicator(),
            Row(
              children: [
                for (final (action, icon, label) in [
                  (HubDropAction.select, Icons.check_circle_outline, '선택'),
                  (HubDropAction.hide, Icons.visibility_off_outlined, '숨기기'),
                  (HubDropAction.trash, Icons.delete_outline, '휴지통'),
                  (HubDropAction.more, Icons.more_horiz, '더보기'),
                ])
                  Expanded(
                    child: DragTarget<HubDragData>(
                      onWillAcceptWithDetails: (_) => !busy,
                      onAcceptWithDetails: (details) =>
                          onAction(action, details.data),
                      builder: (context, accepted, rejected) {
                        final hot = accepted.isNotEmpty;
                        final ink = action == HubDropAction.trash
                            ? scheme.error
                            : scheme.primary;
                        return Semantics(
                          button: true,
                          label: label,
                          child: InkWell(
                            key: ValueKey('hub-drop-${action.name}'),
                            onTap: busy ? null : () => onAction(action, drag),
                            borderRadius: Look.corners,
                            child: Container(
                              constraints: const BoxConstraints(minHeight: 64),
                              decoration: BoxDecoration(
                                color: hot ? ink.withValues(alpha: .2) : null,
                                borderRadius: Look.corners,
                                border: hot
                                    ? Border.all(color: ink, width: 2)
                                    : null,
                              ),
                              padding: const EdgeInsets.symmetric(vertical: 8),
                              child: Column(
                                mainAxisSize: MainAxisSize.min,
                                children: [
                                  Icon(icon, color: ink),
                                  const SizedBox(height: 4),
                                  Text(
                                    label,
                                    maxLines: 1,
                                    overflow: TextOverflow.ellipsis,
                                    style: Theme.of(context)
                                        .textTheme
                                        .labelMedium
                                        ?.copyWith(color: ink),
                                  ),
                                ],
                              ),
                            ),
                          ),
                        );
                      },
                    ),
                  ),
              ],
            ),
          ],
        ),
      ),
    );
  }
}
