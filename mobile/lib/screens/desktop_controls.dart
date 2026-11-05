import 'package:flutter/material.dart';

import '../look.dart';
import '../server.dart';
import 'controls.dart';

Future<String?> desktopText(
  BuildContext context,
  String title,
  String initial, {
  String submit = '저장',
  int lines = 1,
  bool allowEmpty = false,
}) async {
  final controller = TextEditingController(text: initial);
  try {
    return await showDialog<String>(
      context: context,
      builder: (ctx) => ModalLook(
        child: AlertDialog(
          title: Text(title),
          content: TextField(
            controller: controller,
            autofocus: true,
            minLines: lines,
            maxLines: lines + 3,
            style: const TextStyle(fontSize: 16),
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(ctx),
              child: const Text('취소'),
            ),
            ValueListenableBuilder(
              valueListenable: controller,
              builder: (_, value, _) => FilledButton(
                onPressed: !allowEmpty && value.text.trim().isEmpty
                    ? null
                    : () => Navigator.pop(ctx, value.text.trim()),
                child: Text(submit),
              ),
            ),
          ],
        ),
      ),
    );
  } finally {
    // The outgoing dialog still owns its EditableText during the reverse animation.
    await Future<void>.delayed(const Duration(milliseconds: 300));
    controller.dispose();
  }
}

Future<T?> desktopChoice<T>(
  BuildContext context,
  String title,
  List<(T, String)> choices, {
  T? selected,
}) => showModalBottomSheet<T>(
  context: context,
  showDragHandle: true,
  isScrollControlled: true,
  builder: (ctx) => SafeArea(
    child: ConstrainedBox(
      constraints: BoxConstraints(
        maxHeight: MediaQuery.sizeOf(ctx).height * .75,
      ),
      child: ListView(
        shrinkWrap: true,
        padding: const EdgeInsets.only(bottom: Look.pagePad),
        children: [
          ListTile(
            title: Text(title, style: Theme.of(ctx).textTheme.titleMedium),
          ),
          for (final (value, label) in choices)
            ListTile(
              title: Text(label),
              selected: value == selected,
              trailing: value == selected ? const Icon(Icons.check) : null,
              onTap: () => Navigator.pop(ctx, value),
            ),
        ],
      ),
    ),
  ),
);

Future<bool> desktopConfirm(
  BuildContext context,
  String text, {
  String action = '계속',
}) async =>
    await showDialog<bool>(
      context: context,
      builder: (ctx) => ModalLook(
        child: AlertDialog(
          content: Text(text),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(ctx, false),
              child: const Text('취소'),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(ctx, true),
              child: Text(action),
            ),
          ],
        ),
      ),
    ) ??
    false;

String desktopError(Object error) =>
    error is ServerException ? error.message : '처리하지 못했어요. 다시 시도해 주세요.';

Map<String, dynamic> desktopMap(Object? value) =>
    value is Map ? Map<String, dynamic>.from(value) : {};

List<Map<String, dynamic>> desktopRows(Object? value) => value is List
    ? [
        for (final row in value)
          if (row is Map) Map<String, dynamic>.from(row),
      ]
    : [];
