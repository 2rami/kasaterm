import 'dart:async';

import 'package:flutter/material.dart';

import '../connection.dart';
import '../conversation_cache.dart';
import '../look.dart';
import '../server.dart';
import '../shared_rooms_repository.dart';
import 'controls.dart';
import 'saved_conversations.dart';
import 'shared_rooms.dart';

/// 끊긴 기기 머리의 시각 — 데스크톱 기기 절과 같은 꼴. 하루 안이면 `15:32`, 그 전이면 `2일 전 15:32`.
String lastSeenLabel(DateTime? at, {DateTime? now}) {
  if (at == null) return '본 적 없음';
  final t = at.toLocal();
  final today = now ?? DateTime.now();
  String two(int n) => n.toString().padLeft(2, '0');
  final clock = '${two(t.hour)}:${two(t.minute)}';
  final days = DateTime(
    today.year,
    today.month,
    today.day,
  ).difference(DateTime(t.year, t.month, t.day)).inDays;
  return days <= 0 ? clock : '$days일 전 $clock';
}

/// 끊긴 기기에서 막힌 조작 — 까닭을 짧게 알린다.
void explainOffline(BuildContext context, String what) {
  ScaffoldMessenger.of(context)
    ..hideCurrentSnackBar()
    ..showSnackBar(SnackBar(content: Text('기기가 끊겨 $what 수 없어요. 다시 붙으면 돼요.')));
}

/// 끊긴 학생은 폰에 저장한 대화를 읽기 전용으로 연다 — 닿지 않는 기기에 화면을 열지 않는다.
Future<void> openFrozenPane(
  BuildContext context,
  Server server,
  Pane pane,
) async {
  final messenger = ScaffoldMessenger.of(context);
  void tell(String text) => messenger
    ..hideCurrentSnackBar()
    ..showSnackBar(SnackBar(content: Text(text)));
  if (pane.isShell || pane.surfaceKey == null) {
    tell('셸은 저장한 대화가 없어요. 기기가 다시 붙으면 열 수 있어요.');
    return;
  }
  final repository = ConversationCache.of(server).repository;
  try {
    final saved = await repository.openPane(pane);
    if (!context.mounted) return;
    if (saved == null) {
      tell('이 학생의 저장한 대화가 아직 없어요. 기기가 다시 붙으면 열 수 있어요.');
      return;
    }
    await Navigator.of(context).push(
      MaterialPageRoute<void>(
        settings: const RouteSettings(name: 'saved-conversation'),
        builder: (_) => SavedConversationScreen(
          repository: repository,
          conversation: saved,
          offline: true,
        ),
      ),
    );
  } catch (e) {
    tell('저장한 대화를 열지 못했어요. $e');
  }
}

/// 닿는 데스크톱 없이 지난 허브를 보일 때 맨 위 띠 — 무엇이 낡았는지와 다음 행동. 누르면 계정·기기 시트.
class OfflineBand extends StatelessWidget {
  const OfflineBand({
    super.key,
    required this.connection,
    required this.onOpen,
  });

  final ConnectionController connection;
  final VoidCallback onOpen;

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: connection,
    builder: (context, _) {
      final theme = Theme.of(context);
      final scheme = theme.colorScheme;
      final checking = connection.phase == ConnectionPhase.checking;
      final head = checking ? '데스크톱 연결을 확인하는 중' : '데스크톱에 닿지 않아요';
      return Padding(
        padding: const EdgeInsets.only(top: Look.cardGap),
        child: Material(
          key: const Key('hub-offline-band'),
          color: scheme.surfaceContainerHigh,
          borderRadius: Look.corners,
          child: InkWell(
            onTap: onOpen,
            borderRadius: Look.corners,
            child: Container(
              constraints: const BoxConstraints(minHeight: Look.row2),
              padding: const EdgeInsets.only(left: Look.cardPad),
              child: Row(
                children: [
                  Icon(
                    Icons.cloud_off_outlined,
                    size: Look.iconSize,
                    color: scheme.onSurfaceVariant,
                  ),
                  const SizedBox(width: 12),
                  Expanded(
                    child: Padding(
                      padding: const EdgeInsets.symmetric(vertical: 8),
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          Text(head, style: theme.textTheme.titleSmall),
                          const SizedBox(height: Look.rowGap),
                          Text(
                            '마지막으로 본 화면이에요 · 학생을 누르면 저장한 대화',
                            style: theme.textTheme.bodySmall?.copyWith(
                              color: scheme.onSurfaceVariant,
                            ),
                          ),
                        ],
                      ),
                    ),
                  ),
                  TextButton(
                    onPressed: checking ? null : connection.retry,
                    child: Text(checking ? '확인 중' : '다시 확인'),
                  ),
                ],
              ),
            ),
          ),
        ),
      );
    },
  );
}

/// 허브 안에서 여는 계정·기기 시트 — 대기 화면이 하던 일(로그인 상태·기기 목록·저장한 대화·로그아웃)을 그대로.
Future<void> showAccountSheet(
  BuildContext context, {
  required ConnectionController connection,
  SharedRoomsRepository? shared,
}) => showModalBottomSheet<void>(
  context: context,
  showDragHandle: true,
  isScrollControlled: true,
  builder: (sheet) => ModalLook(
    child: SafeArea(
      child: ConstrainedBox(
        constraints: BoxConstraints(
          maxHeight: MediaQuery.sizeOf(sheet).height * 0.85,
        ),
        child: ListView(
          shrinkWrap: true,
          padding: const EdgeInsets.fromLTRB(24, 0, 24, 24),
          children: [
            AccountStatus(
              connection: connection,
              shared: shared,
              inSheet: true,
            ),
          ],
        ),
      ),
    ),
  ),
);

/// 계정 연결의 지금 사정 — 대기 화면과 허브 시트가 같은 글·같은 단추를 쓴다.
class AccountStatus extends StatelessWidget {
  const AccountStatus({
    super.key,
    required this.connection,
    this.shared,
    this.inSheet = false,
  });

  final ConnectionController connection;
  final SharedRoomsRepository? shared;
  final bool inSheet;

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: connection,
    builder: (context, _) {
      final account = connection.account;
      if (account == null) return const SizedBox.shrink();
      final checking = connection.phase == ConnectionPhase.checking;
      final devices = [
        for (final d in connection.devices)
          if (d['kind'] != 'phone') d,
      ];
      void leaveSheet(void Function(BuildContext) next) {
        if (!inSheet) return next(context);
        final nav = Navigator.of(context);
        final root = nav.context;
        nav.pop();
        next(root);
      }

      return Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(
            '${account.label} 계정',
            style: Theme.of(context).textTheme.titleLarge,
          ),
          const SizedBox(height: 12),
          Text(
            checking
                ? '로그인과 기기 상태를 확인하고 있어요.'
                : connection.message ?? '연결할 데스크톱을 기다리고 있어요.',
          ),
          const SizedBox(height: 12),
          const Text(
            '데스크톱 카사텀을 켜고 같은 계정으로 로그인해 주세요. 이미 켜져 있다면 최신 버전인지 확인해 주세요.',
          ),
          // Google·GitHub 로 처음 들어오면 관문이 새 계정을 만든다 — 쓰던 데스크톱 계정과 다르다는 것을 여기서 알린다.
          if (account.account.startsWith('oauth_') && devices.isEmpty) ...[
            const SizedBox(height: 12),
            const Text(
              '이 계정에는 아직 데스크톱이 없어요. 쓰던 데스크톱 계정이 있다면 로그아웃한 뒤 아이디로 로그인하고, 설정에서 Google·GitHub 을 연결해 주세요.',
            ),
          ],
          if (devices.isNotEmpty)
            SettingsGroup(
              title: '이 계정의 데스크톱',
              children: [
                for (final (i, device) in devices.indexed)
                  SettingsRow(
                    tone: i,
                    icon: Icons.computer_outlined,
                    title: '${device['label'] ?? device['device_id']}',
                    subtitle: device['online'] == true
                        ? '온라인 · 연결 준비 확인 중'
                        : '오프라인',
                  ),
              ],
            ),
          const SizedBox(height: 20),
          // 공유방은 계정 기능이라 데스크톱 없이도 목록·참여·읽기가 된다.
          if (shared case final shared?) ...[
            FilledButton.tonalIcon(
              onPressed: () => leaveSheet((c) => openSharedRooms(c, shared)),
              icon: const Icon(Icons.groups_outlined),
              label: const Text('공유방'),
            ),
            const SizedBox(height: 8),
          ],
          if (connection.archiveServer case final archiveServer?) ...[
            FilledButton.tonalIcon(
              onPressed: () =>
                  leaveSheet((c) => openSavedConversations(c, archiveServer)),
              icon: const Icon(Icons.offline_pin_outlined),
              label: const Text('저장한 대화'),
            ),
            const SizedBox(height: 8),
          ],
          FilledButton(
            onPressed: checking ? null : () => unawaited(connection.retry()),
            child: Text(checking ? '확인 중' : '연결 다시 확인'),
          ),
          const SizedBox(height: 8),
          OutlinedButton(
            style: OutlinedButton.styleFrom(
              minimumSize: const Size(Look.tap, Look.buttonH),
            ),
            onPressed: () => leaveSheet((_) => unawaited(connection.logout())),
            child: const Text('로그아웃'),
          ),
        ],
      );
    },
  );
}
