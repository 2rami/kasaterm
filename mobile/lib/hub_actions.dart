import 'server.dart';

class HubTarget {
  const HubTarget(this.pane, this.address);
  final Pane pane;
  final Map<String, dynamic> address;
}

class HubRestore {
  const HubRestore(this.machine, this.receipt);
  final String? machine;
  final Map<String, dynamic> receipt;
}

class HubCloseResult {
  const HubCloseResult({
    this.needsConfirmation = false,
    this.restores = const [],
  });
  final bool needsConfirmation;
  final List<HubRestore> restores;
}

class HubCloseException extends ServerException {
  const HubCloseException(super.message, this.restores);
  final List<HubRestore> restores;
}

class HubActions {
  const HubActions(this.server);
  final Server server;

  Future<List<HubTarget>> prepare(Iterable<Pane> panes) async {
    final grouped = <String?, List<Pane>>{};
    for (final pane in panes) {
      if (pane.surfaceKey == null ||
          pane.surfaceKey!.isEmpty ||
          pane.isWebShell) {
        throw const ServerException(
          '칸의 신원을 확인하지 못했어요. PC를 업데이트하고 목록을 새로 받아 주세요.',
        );
      }
      grouped.putIfAbsent(pane.machine, () => []).add(pane);
    }
    final targets = <HubTarget>[];
    for (final group in grouped.entries) {
      final board = await server.desktopValues(
        'collab/board',
        machine: group.key,
        query: const {'scope': 'local'},
      );
      final rows = board['panes'];
      if (rows is! List) throw const ServerException('PC에서 칸 목록을 받지 못했어요.');
      for (final pane in group.value) {
        final matches = rows
            .whereType<Map>()
            .map((row) => row['address'])
            .whereType<Map>()
            .where(
              (address) =>
                  address['surface_id'] == pane.id &&
                  address['surface_key'] == pane.surfaceKey,
            );
        if (matches.length != 1) {
          throw const ServerException('칸이 바뀌었어요. 새 목록에서 다시 골라 주세요.');
        }
        final address = Map<String, dynamic>.from(matches.single);
        if (['machine_id', 'surface_id', 'surface_key', 'instance_id'].any(
              (key) =>
                  address[key] is! String || (address[key] as String).isEmpty,
            ) ||
            (pane.machine?.startsWith('~') == true &&
                pane.machine != '~${address['machine_id']}')) {
          throw const ServerException('기기나 세션이 바뀌었어요. 다시 골라 주세요.');
        }
        targets.add(HubTarget(pane, address));
      }
    }
    return targets;
  }

  Future<HubCloseResult> close(
    List<HubTarget> targets, {
    required bool idleOnly,
  }) async {
    if (targets.isEmpty) return const HubCloseResult();
    final machine = targets.first.pane.machine;
    if (targets.any((target) => target.pane.machine != machine)) {
      throw const ServerException('한 번에 같은 기기의 칸을 골라 주세요.');
    }
    final reply = await server.cmd(
      idleOnly ? 'surface.close_idle_checked' : 'surface.close_checked',
      {
        'kind': 'tabs',
        'addresses': [for (final target in targets) target.address],
      },
      machine: machine,
    );
    final result = reply['result'];
    if (result is Map && result['requires_confirmation'] == true) {
      return const HubCloseResult(needsConfirmation: true);
    }
    final restores = <HubRestore>[
      for (final record
          in (result is Map ? result['records'] as List? : null) ?? const [])
        if (record is Map)
          HubRestore(machine, Map<String, dynamic>.from(record)),
    ];
    if (result is! Map || result['closed'] != true || result['error'] != null) {
      throw HubCloseException(
        result is Map && result['error'] is String
            ? result['error'] as String
            : '일부 칸을 닫지 못했어요. 새 목록을 확인해 주세요.',
        restores,
      );
    }
    return HubCloseResult(restores: restores);
  }

  Future<void> restore(List<HubRestore> records) async {
    var failed = 0;
    for (final record in records) {
      try {
        await server.cmd(
          'surface.reopen_checked',
          record.receipt,
          machine: record.machine,
        );
      } catch (_) {
        failed++;
      }
    }
    if (failed > 0) {
      throw ServerException('$failed개 칸은 되살리지 못했어요. PC의 닫힌 칸 목록을 확인해 주세요.');
    }
  }

  Future<void> move(List<Pane> panes, Pane target, String direction) async {
    if (panes.any(
      (pane) =>
          pane.machine != target.machine || pane.viewKey == target.viewKey,
    )) {
      throw const ServerException('같은 기기의 다른 칸 옆으로 옮겨 주세요.');
    }
    await prepare([...panes, target]);
    for (final pane in panes) {
      await server.cmd('surface.move', {
        'surface_id': pane.id,
        'target': target.id,
        'direction': direction,
      }, machine: target.machine);
    }
  }
}
