import 'dart:async';

import 'package:flutter/material.dart';

import '../hub_model.dart';
import '../look.dart';
import '../machine_look.dart';
import 'controls.dart';

typedef _OrderRow = ({String id, String label});
typedef _OrderGroup = ({String? device, String label, List<_OrderRow> rows});

class HubOrderSheet extends StatefulWidget {
  const HubOrderSheet({super.key, required this.model});

  final HubModel model;

  static Future<void> show(BuildContext context, HubModel model) =>
      showModalBottomSheet<void>(
        context: context,
        isScrollControlled: true,
        enableDrag: false,
        showDragHandle: true,
        builder: (_) => ModalLook(child: HubOrderSheet(model: model)),
      );

  @override
  State<HubOrderSheet> createState() => _HubOrderSheetState();
}

class _HubOrderSheetState extends State<HubOrderSheet> {
  late final List<_OrderGroup> _groups;
  int _selected = 0;

  @override
  void initState() {
    super.initState();
    final model = widget.model;
    final named = [
      for (final section in model.orderedSections)
        (
          section,
          machineName(
            section.machine ?? model.rootName ?? '이 기계',
            route: section.route,
            local: section.machine == null,
          ),
        ),
    ];
    _groups = [
      if (named.length > 1)
        (
          device: null,
          label: '기기 순서',
          rows: [
            for (final (section, name) in named)
              (id: section.layoutKey, label: name),
          ],
        ),
      for (final (section, name) in named)
        if (section.rooms.length > 1)
          (
            device: section.layoutKey,
            label: '$name · 방 순서',
            rows: [
              for (final room in model.orderedRooms(section))
                (id: room.layoutKey, label: room.title),
            ],
          ),
    ];
  }

  void _reorder(int from, int to) {
    if (from < to) to--;
    if (from == to) return;
    final group = _groups[_selected];
    setState(() => group.rows.insert(to, group.rows.removeAt(from)));
    final ids = group.rows.map((row) => row.id).toList();
    final device = group.device;
    unawaited(
      device == null
          ? widget.model.orderDevices(ids)
          : widget.model.orderRooms(device, ids),
    );
  }

  @override
  Widget build(BuildContext context) {
    if (_groups.isEmpty) return const SizedBox.shrink();
    final group = _groups[_selected];
    return SafeArea(
      top: false,
      child: SizedBox(
        height: MediaQuery.sizeOf(context).height * .58,
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: Look.pagePad),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              if (_groups.length > 1)
                DropdownButton<int>(
                  isExpanded: true,
                  value: _selected,
                  items: [
                    for (final (index, group) in _groups.indexed)
                      DropdownMenuItem(
                        value: index,
                        child: Text(
                          group.label,
                          overflow: TextOverflow.ellipsis,
                        ),
                      ),
                  ],
                  onChanged: (index) {
                    if (index != null) setState(() => _selected = index);
                  },
                )
              else
                Text(
                  group.label,
                  style: Theme.of(context).textTheme.titleMedium,
                ),
              Expanded(
                child: ReorderableListView.builder(
                  key: ValueKey(_selected),
                  buildDefaultDragHandles: false,
                  itemCount: group.rows.length,
                  onReorder: _reorder,
                  itemBuilder: (context, index) {
                    final row = group.rows[index];
                    return ListTile(
                      key: ValueKey(row.id),
                      title: Text(
                        row.label,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                      ),
                      trailing: ReorderableDragStartListener(
                        index: index,
                        child: Tooltip(
                          message: '${row.label} 순서 이동',
                          child: const SizedBox.square(
                            dimension: Look.tap,
                            child: Icon(Icons.drag_handle),
                          ),
                        ),
                      ),
                    );
                  },
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
