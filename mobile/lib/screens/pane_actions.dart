import 'package:flutter/material.dart';

import '../hub_model.dart';
import '../server.dart';
import '../student_art.dart';
import '../weather/sheet.dart';
import 'controls.dart';
import 'desktop_controls.dart';

/// 허브에서 pane·방을 다루는 판들 — 닫기·옆에 추가·자리 바꾸기·방 만들기/이름/닫기.
/// 데스크톱 없이 폰에서 배치를 만질 수 있게(2026-09-07 지시). 명령은 전부 서버의
/// `cmd` 창구로 가고, 끝나면 `onChanged` 로 목록을 다시 받는다 — 푸시가 없어서다.
enum _PaneAct { open, split, swap, move, ratio, focus, launch, close }

enum _RoomAct { add, rename, close }

Future<void> showPaneSheet(
  BuildContext context, {
  required Server server,
  required HubRoom room,
  List<HubRoom>? rooms,
  required Pane pane,
  required String? machine,
  required void Function(Pane) onOpen,
  required Future<void> Function() onChanged,
}) async {
  final act = await showModalBottomSheet<_PaneAct>(
    context: context,
    showDragHandle: true,
    isScrollControlled: true,
    builder: (ctx) => SafeArea(
      child: ConstrainedBox(
        constraints: BoxConstraints(
          maxHeight: MediaQuery.sizeOf(ctx).height * .8,
        ),
        child: ListView(
          shrinkWrap: true,
          children: [
            ListTile(
              leading: StudentFace(
                server: server,
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
            SettingsRow(
              icon: Icons.open_in_new,
              title: '화면 보기',
              onTap: () => Navigator.pop(ctx, _PaneAct.open),
            ),
            SettingsRow(
              icon: Icons.add_box_outlined,
              title: '옆에 pane 추가',
              subtitle: '방향을 골라 새 셸을 만들어요',
              onTap: () => Navigator.pop(ctx, _PaneAct.split),
            ),
            if (room.panes.length > 1)
              SettingsRow(
                icon: Icons.swap_horiz,
                title: '자리 바꾸기',
                subtitle: '같은 방의 다른 pane 과 자리를 맞바꾼다',
                onTap: () => Navigator.pop(ctx, _PaneAct.swap),
              ),
            SettingsRow(
              icon: Icons.drive_file_move_outline,
              title: '다른 자리로 이동',
              subtitle: '같은 기기의 다른 방으로도 옮겨요',
              onTap: () => Navigator.pop(ctx, _PaneAct.move),
            ),
            if (room.panes.length > 1)
              SettingsRow(
                icon: Icons.aspect_ratio,
                title: '분할 크기 조절',
                onTap: () => Navigator.pop(ctx, _PaneAct.ratio),
              ),
            SettingsRow(
              icon: Icons.desktop_windows_outlined,
              title: 'PC에서 이 칸 보기',
              onTap: () => Navigator.pop(ctx, _PaneAct.focus),
            ),
            if (pane.isShell && !pane.isWebShell)
              SettingsRow(
                icon: Icons.play_arrow_outlined,
                title: '에이전트 시작',
                subtitle: '이 셸에서 Claude 또는 Codex를 시작해요',
                onTap: () => Navigator.pop(ctx, _PaneAct.launch),
              ),
            SettingsRow(
              danger: true,
              icon: Icons.close,
              title: '닫기',
              subtitle: '데스크톱의 × 와 같다 — 되살리기로 되돌릴 수 있다',
              onTap: () => Navigator.pop(ctx, _PaneAct.close),
            ),
            const CardWeatherRow(),
            const SizedBox(height: 8),
          ],
        ),
      ),
    ),
  );
  if (act == null || !context.mounted) return;
  switch (act) {
    case _PaneAct.open:
      onOpen(pane);
    case _PaneAct.split:
      final direction = await desktopChoice(context, '새 칸을 만들 방향', _directions);
      if (direction == null || !context.mounted) return;
      await _run(context, onChanged, () async {
        await server.cmd('surface.split', {
          'from': pane.id,
          'direction': direction,
        }, machine: machine);
      });
    case _PaneAct.swap:
      final other = await _pickPane(context, server, room, pane);
      if (other == null || !context.mounted) return;
      await _run(
        context,
        onChanged,
        () => server.swapPanes(pane.id, other.id, machine: machine),
      );
    case _PaneAct.move:
      final targets = <(Pane, String)>[
        for (final r in rooms ?? [room])
          for (final p in r.panes)
            if (p.id != pane.id && !p.closed && !p.isWebShell)
              (p, '${r.title} · ${p.rowTitle}'),
      ];
      if (targets.isEmpty) {
        ScaffoldMessenger.of(context).showSnackBar(
          const SnackBar(content: Text('옮길 다른 칸이 없어요. 먼저 방이나 칸을 추가해 주세요.')),
        );
        return;
      }
      final target = await desktopChoice(context, '어느 칸 옆으로 옮길까요?', targets);
      if (target == null || !context.mounted) return;
      final direction = await desktopChoice(context, '옮길 방향', _directions);
      if (direction == null || !context.mounted) return;
      await _run(context, onChanged, () async {
        await server.cmd('surface.move', {
          'surface_id': pane.id,
          'target': target.id,
          'direction': direction,
        }, machine: machine);
      });
    case _PaneAct.ratio:
      final ratio = await desktopChoice(context, '분할선 비율', const [
        (0.25, '1 : 3'),
        (1 / 3, '1 : 2'),
        (0.5, '1 : 1'),
        (2 / 3, '2 : 1'),
        (0.75, '3 : 1'),
      ]);
      if (ratio == null || !context.mounted) return;
      await _run(context, onChanged, () async {
        await server.cmd('surface.set_ratio', {
          'surface_id': pane.id,
          'ratio': ratio,
        }, machine: machine);
      });
    case _PaneAct.focus:
      await _run(context, onChanged, () async {
        await server.cmd('surface.focus', {
          'surface_id': pane.id,
        }, machine: machine);
      });
    case _PaneAct.launch:
      final provider = await desktopChoice(context, '시작할 에이전트', const [
        ('claude', 'Claude'),
        ('codex', 'Codex'),
      ]);
      if (provider == null || !context.mounted) return;
      await _run(context, onChanged, () async {
        await server.cmd('surface.launch', {
          'surface_id': pane.id,
          'command': provider,
        }, machine: machine);
      });
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

const _directions = [
  ('left', '왼쪽'),
  ('right', '오른쪽'),
  ('up', '위'),
  ('down', '아래'),
];

Future<void> showRoomSheet(
  BuildContext context, {
  required Server server,
  required HubRoom room,
  required String? machine,
  required Future<void> Function() onChanged,
}) async {
  final first = room.panes.isEmpty ? null : room.panes.first;
  final act = await showModalBottomSheet<_RoomAct>(
    context: context,
    showDragHandle: true,
    isScrollControlled: true,
    builder: (ctx) => SafeArea(
      child: ConstrainedBox(
        constraints: BoxConstraints(
          maxHeight: MediaQuery.sizeOf(ctx).height * .8,
        ),
        child: ListView(
          shrinkWrap: true,
          children: [
            ListTile(
              leading: const Icon(Icons.meeting_room_outlined),
              title: Text(room.title),
              subtitle: Text('pane ${room.panes.length}개'),
            ),
            const Divider(height: 1),
            if (first != null)
              SettingsRow(
                icon: Icons.add_box_outlined,
                title: 'pane 추가',
                subtitle: '이 방에 셸 하나를 쪼갠다',
                onTap: () => Navigator.pop(ctx, _RoomAct.add),
              ),
            if (first != null)
              SettingsRow(
                icon: Icons.drive_file_rename_outline,
                title: '방 이름 바꾸기',
                onTap: () => Navigator.pop(ctx, _RoomAct.rename),
              ),
            if (first != null)
              SettingsRow(
                danger: true,
                icon: Icons.close,
                title: '방 닫기',
                subtitle: '이 방의 pane 을 전부 닫는다',
                onTap: () => Navigator.pop(ctx, _RoomAct.close),
              ),
            const CardWeatherRow(),
            const SizedBox(height: 8),
          ],
        ),
      ),
    ),
  );
  if (act == null || first == null || !context.mounted) return;
  switch (act) {
    case _RoomAct.add:
      await _run(
        context,
        onChanged,
        () => server.splitPane(first.id, machine: machine),
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
}) => _run(context, onChanged, () => server.newWindow(machine: machine));

Future<Pane?> _pickPane(
  BuildContext context,
  Server server,
  HubRoom room,
  Pane me,
) => showModalBottomSheet<Pane>(
  context: context,
  showDragHandle: true,
  isScrollControlled: true,
  builder: (ctx) => SafeArea(
    child: ConstrainedBox(
      constraints: BoxConstraints(
        maxHeight: MediaQuery.sizeOf(ctx).height * .8,
      ),
      child: ListView(
        shrinkWrap: true,
        children: [
          ListTile(title: Text('${me.displayName} 과(와) 자리를 바꿀 pane')),
          const Divider(height: 1),
          for (final p in room.panes)
            if (p.id != me.id)
              ListTile(
                leading: StudentFace(
                  server: server,
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
  ),
);

Future<bool> _confirm(BuildContext context, String text) =>
    desktopConfirm(context, text, action: '닫기');

Future<String?> _askText(BuildContext context, String label, String initial) =>
    desktopText(context, label, initial, submit: '바꾸기');

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
