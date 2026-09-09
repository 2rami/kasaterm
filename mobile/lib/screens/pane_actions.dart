import 'package:flutter/material.dart';

import '../hub_model.dart';
import '../server.dart';
import '../student_art.dart';

/// 허브에서 pane·방을 다루는 판들 — 닫기·옆에 추가·자리 바꾸기·방 만들기/이름/닫기.
/// 데스크톱 없이 폰에서 배치를 만질 수 있게(2026-09-07 지시). 명령은 전부 서버의
/// `cmd` 창구로 가고, 끝나면 `onChanged` 로 목록을 다시 받는다 — 푸시가 없어서다.
enum _PaneAct { open, split, swap, close }

enum _RoomAct { add, rename, close }

Future<void> showPaneSheet(
  BuildContext context, {
  required Server server,
  required HubRoom room,
  required Pane pane,
  required String? machine,
  required void Function(Pane) onOpen,
  required Future<void> Function() onChanged,
}) async {
  final scheme = Theme.of(context).colorScheme;
  final act = await showModalBottomSheet<_PaneAct>(
    context: context,
    showDragHandle: true,
    builder: (ctx) => SafeArea(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          ListTile(
            leading: StudentFace(
              slug: pane.slug,
              url: pane.slug == null
                  ? null
                  : server.avatar(pane.slug!, machine: pane.machine),
              shell: pane.isShell,
            ),
            title: Text(pane.displayName),
            subtitle: pane.subtitle.isEmpty
                ? null
                : Text(
                    pane.subtitle,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                  ),
          ),
          const Divider(height: 1),
          ListTile(
            leading: const Icon(Icons.open_in_new),
            title: const Text('화면 보기'),
            onTap: () => Navigator.pop(ctx, _PaneAct.open),
          ),
          ListTile(
            leading: const Icon(Icons.add_box_outlined),
            title: const Text('옆에 pane 추가'),
            subtitle: const Text('셸 하나를 이 pane 옆에 쪼갠다'),
            onTap: () => Navigator.pop(ctx, _PaneAct.split),
          ),
          if (room.panes.length > 1)
            ListTile(
              leading: const Icon(Icons.swap_horiz),
              title: const Text('자리 바꾸기'),
              subtitle: const Text('같은 방의 다른 pane 과 자리를 맞바꾼다'),
              onTap: () => Navigator.pop(ctx, _PaneAct.swap),
            ),
          ListTile(
            leading: Icon(Icons.close, color: scheme.error),
            title: Text('닫기', style: TextStyle(color: scheme.error)),
            subtitle: const Text('데스크톱의 × 와 같다 — 되살리기로 되돌릴 수 있다'),
            onTap: () => Navigator.pop(ctx, _PaneAct.close),
          ),
          const SizedBox(height: 8),
        ],
      ),
    ),
  );
  if (act == null || !context.mounted) return;
  switch (act) {
    case _PaneAct.open:
      onOpen(pane);
    case _PaneAct.split:
      await addPane(
        context,
        server: server,
        machine: machine,
        onChanged: onChanged,
        onOpen: onOpen,
        splitFrom: pane,
      );
    case _PaneAct.swap:
      final other = await _pickPane(context, server, room, pane);
      if (other == null || !context.mounted) return;
      await _run(
        context,
        onChanged,
        () => server.swapPanes(pane.id, other.id, machine: machine),
      );
    case _PaneAct.close:
      final ok = await _confirm(context, '${pane.displayName} 을(를) 닫을까?');
      if (!ok || !context.mounted) return;
      await _run(
        context,
        onChanged,
        () => server.closePane(pane.id, machine: machine),
      );
  }
}

Future<void> showRoomSheet(
  BuildContext context, {
  required Server server,
  required HubRoom room,
  required String? machine,
  required Future<void> Function() onChanged,
  void Function(Pane)? onOpen,
}) async {
  final scheme = Theme.of(context).colorScheme;
  final first = room.panes.isEmpty ? null : room.panes.first;
  final act = await showModalBottomSheet<_RoomAct>(
    context: context,
    showDragHandle: true,
    builder: (ctx) => SafeArea(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          ListTile(
            leading: const Icon(Icons.meeting_room_outlined),
            title: Text(room.title),
            subtitle: Text('pane ${room.panes.length}개'),
          ),
          const Divider(height: 1),
          if (first != null)
            ListTile(
              leading: const Icon(Icons.add_box_outlined),
              title: const Text('pane 추가'),
              subtitle: const Text('이 방에 셸 하나를 쪼갠다'),
              onTap: () => Navigator.pop(ctx, _RoomAct.add),
            ),
          if (first != null)
            ListTile(
              leading: const Icon(Icons.drive_file_rename_outline),
              title: const Text('방 이름 바꾸기'),
              onTap: () => Navigator.pop(ctx, _RoomAct.rename),
            ),
          if (first != null)
            ListTile(
              leading: Icon(Icons.close, color: scheme.error),
              title: Text('방 닫기', style: TextStyle(color: scheme.error)),
              subtitle: const Text('이 방의 pane 을 전부 닫는다'),
              onTap: () => Navigator.pop(ctx, _RoomAct.close),
            ),
          const SizedBox(height: 8),
        ],
      ),
    ),
  );
  if (act == null || first == null || !context.mounted) return;
  switch (act) {
    case _RoomAct.add:
      await addPane(
        context,
        server: server,
        machine: machine,
        onChanged: onChanged,
        onOpen: onOpen,
        splitFrom: first,
      );
    case _RoomAct.rename:
      final name = await _askText(context, '방 이름', room.title);
      if (name == null || name.isEmpty || !context.mounted) return;
      await _run(
        context,
        onChanged,
        () => server.renameWindow(first.id, name, machine: machine),
      );
    case _RoomAct.close:
      final ok = await _confirm(
        context,
        '${room.title} 방을 닫을까? pane ${room.panes.length}개가 함께 닫힌다.',
      );
      if (!ok || !context.mounted) return;
      await _run(
        context,
        onChanged,
        () => server.closeWindow(first.window, machine: machine),
      );
  }
}

Future<void> newRoom(
  BuildContext context, {
  required Server server,
  required String? machine,
  required Future<void> Function() onChanged,
  void Function(Pane)? onOpen,
  HubModel? model,
}) => addPane(
  context,
  server: server,
  machine: machine,
  onChanged: onChanged,
  onOpen: onOpen,
  model: model,
);

/// 폰 허브의 「+」 — 기계·방을 고르면 그 방에 pane 하나(셸). 「새 방」이면 방부터
/// 만든다(데스크톱의 새 window 는 셸 pane 하나를 품고 뜬다). 데스크톱 pane 머리의
/// 쪼개기·+ 와 같은 명령이라 claude 는 안 켠다 — 화면에 들어가 사람이 켠다.
Future<void> showAddPaneSheet(
  BuildContext context, {
  required Server server,
  required HubModel model,
  required void Function(Pane) onOpen,
}) async {
  final sections = [
    for (final s in model.sections)
      if (s.online) s,
  ];
  if (sections.isEmpty) {
    ScaffoldMessenger.maybeOf(
      context,
    )?.showSnackBar(const SnackBar(content: Text('닿는 기계가 없다')));
    return;
  }
  // 기계가 하나면 고를 것이 없다 — 바로 방 고르기.
  HubSection? section = sections.length == 1 ? sections.single : null;
  section ??= await _pickSection(context, sections, model.rootName);
  if (section == null || !context.mounted) return;
  final choice = await _pickRoom(context, section, model.rootName);
  if (choice == null || !context.mounted) return;
  await addPane(
    context,
    server: server,
    machine: section.route,
    onChanged: model.refresh,
    onOpen: onOpen,
    model: model,
    splitFrom: choice.panes.isEmpty ? null : choice.panes.first,
  );
}

/// 새 방을 뜻하는 자리표시자 — pane 이 없다.
const _newRoom = HubRoom(title: '새 방', panes: []);

Future<HubSection?> _pickSection(
  BuildContext context,
  List<HubSection> sections,
  String? rootName,
) => showModalBottomSheet<HubSection>(
  context: context,
  showDragHandle: true,
  builder: (ctx) => SafeArea(
    child: Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        const ListTile(title: Text('어느 기계에 pane 을 추가할까')),
        const Divider(height: 1),
        for (final s in sections)
          ListTile(
            leading: const Icon(Icons.computer_outlined),
            title: Text(s.machine ?? rootName ?? '이 기계'),
            subtitle: Text('방 ${s.rooms.length}개 · ${s.paneCount}명'),
            onTap: () => Navigator.pop(ctx, s),
          ),
        const SizedBox(height: 8),
      ],
    ),
  ),
);

Future<HubRoom?> _pickRoom(
  BuildContext context,
  HubSection section,
  String? rootName,
) => showModalBottomSheet<HubRoom>(
  context: context,
  showDragHandle: true,
  builder: (ctx) => SafeArea(
    child: Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        ListTile(
          title: Text(
            '${section.machine ?? rootName ?? '이 기계'} — 어느 방에 추가할까',
          ),
        ),
        const Divider(height: 1),
        Flexible(
          child: ListView(
            shrinkWrap: true,
            children: [
              for (final r in section.rooms)
                ListTile(
                  leading: const Icon(Icons.meeting_room_outlined),
                  title: Text(r.title),
                  subtitle: Text('pane ${r.panes.length}개'),
                  onTap: () => Navigator.pop(ctx, r),
                ),
              ListTile(
                leading: const Icon(Icons.add_home_outlined),
                title: const Text('새 방'),
                subtitle: const Text('방을 만들고 그 안에 pane 하나'),
                onTap: () => Navigator.pop(ctx, _newRoom),
              ),
            ],
          ),
        ),
        const SizedBox(height: 8),
      ],
    ),
  ),
);

/// pane 하나를 만들고, 목록을 지금 다시 받고, 만든 자리로 옮겨 간다.
/// `splitFrom` 이 있으면 그 옆에 쪼개고(`surface.split`), 없으면 새 방(`window.new`).
/// 새 pane 을 목록에서 못 찾으면(옛 서버·느린 기계) 목록 갱신까지만 — 다음 폴링에 뜬다.
Future<void> addPane(
  BuildContext context, {
  required Server server,
  required String? machine,
  required Future<void> Function() onChanged,
  void Function(Pane)? onOpen,
  HubModel? model,
  Pane? splitFrom,
}) async {
  final messenger = ScaffoldMessenger.maybeOf(context);
  final before = model?.windowsOf(machine) ?? const <int>{};
  String? id;
  try {
    if (splitFrom != null) {
      id = await server.splitPane(splitFrom.id, machine: machine);
    } else {
      await server.newWindow(machine: machine);
    }
  } on ServerException catch (e) {
    messenger?.showSnackBar(SnackBar(content: Text(e.message)));
    return;
  }
  Pane? made;
  if (model != null) {
    made = await model.locateNew(machine, id: id, before: before);
  } else {
    await onChanged();
  }
  if (made == null && id != null) {
    // 목록엔 아직 없어도 자리는 안다 — 셸로 들어간다. 목록은 다음 폴링이 채운다.
    made = Pane(
      id: id,
      name: '',
      title: '',
      status: '',
      window: splitFrom?.window ?? 0,
      cwd: splitFrom?.cwd ?? '',
      machine: machine,
    );
  }
  if (made == null || onOpen == null || !context.mounted) return;
  onOpen(made);
}

Future<Pane?> _pickPane(
  BuildContext context,
  Server server,
  HubRoom room,
  Pane me,
) => showModalBottomSheet<Pane>(
  context: context,
  showDragHandle: true,
  builder: (ctx) => SafeArea(
    child: Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        ListTile(title: Text('${me.displayName} 과(와) 자리를 바꿀 pane')),
        const Divider(height: 1),
        for (final p in room.panes)
          if (p.id != me.id)
            ListTile(
              leading: StudentFace(
                slug: p.slug,
                url: p.slug == null
                    ? null
                    : server.avatar(p.slug!, machine: p.machine),
                shell: p.isShell,
              ),
              title: Text(p.displayName),
              subtitle: p.subtitle.isEmpty
                  ? null
                  : Text(
                      p.subtitle,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                    ),
              onTap: () => Navigator.pop(ctx, p),
            ),
        const SizedBox(height: 8),
      ],
    ),
  ),
);

Future<bool> _confirm(BuildContext context, String text) async {
  final r = await showDialog<bool>(
    context: context,
    builder: (ctx) => AlertDialog(
      content: Text(text),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(ctx, false),
          child: const Text('아니'),
        ),
        FilledButton(
          onPressed: () => Navigator.pop(ctx, true),
          child: const Text('닫기'),
        ),
      ],
    ),
  );
  return r ?? false;
}

Future<String?> _askText(BuildContext context, String label, String initial) {
  final ctl = TextEditingController(text: initial);
  return showDialog<String>(
    context: context,
    builder: (ctx) => AlertDialog(
      title: Text(label),
      content: TextField(
        controller: ctl,
        autofocus: true,
        onSubmitted: (v) => Navigator.pop(ctx, v.trim()),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(ctx),
          child: const Text('그만'),
        ),
        FilledButton(
          onPressed: () => Navigator.pop(ctx, ctl.text.trim()),
          child: const Text('바꾸기'),
        ),
      ],
    ),
  ).whenComplete(ctl.dispose);
}

/// 명령 하나를 돌리고 목록을 다시 받는다. 실패는 스낵바 한 줄 — 주소(slug)가 새지
/// 않게 서버 쪽 문구만 쓴다.
Future<void> _run(
  BuildContext context,
  Future<void> Function() onChanged,
  Future<void> Function() job,
) async {
  final messenger = ScaffoldMessenger.maybeOf(context);
  try {
    await job();
  } on ServerException catch (e) {
    messenger?.showSnackBar(SnackBar(content: Text(e.message)));
    return;
  }
  await onChanged();
}
