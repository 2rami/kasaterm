import 'dart:async';

import 'package:flutter/foundation.dart';

import 'server.dart';
import 'shared_rooms_model.dart';
import 'shared_rooms_repository.dart';

/// 목적지 데스크톱이 공유방 창구를 모르는 옛 판 — 임의 터미널 입력으로 돌아가지 않고 업데이트를 알린다.
class ForkHostOutdated implements Exception {
  const ForkHostOutdated();
  @override
  String toString() => '이 PC의 카사텀이 공유방 포크를 아직 몰라요. PC 카사텀을 업데이트해 주세요.';
}

/// 내 데스크톱의 공유방 창구(`/shared-rooms/*`). 실제 native 포크·파일 설치는 데스크톱이 한다.
abstract class ForkHost {
  Future<ForkDestinationStatus> status(String? machine);
  Future<String> start(String? machine, {required String forkId, required String requestId, required bool includeProject, String? cwd});
  Future<ForkOperation> operation(String? machine, String operationId);

  /// 게시할 수 있는 이 PC 의 대화 — 고를 때마다 새로 받는다.
  Future<List<PublishSource>> sources(String? machine);

  /// 원본과 파일은 PC 가 읽는다 — 폰은 완전한 원본 주소만 넘긴다.
  Future<String> publish(String? machine, {required String roomId, required List<Map<String, Object?>> addresses, required bool includeProject});
}

class ServerForkHost implements ForkHost {
  ServerForkHost(this.server);
  final Server server;

  Future<T> _call<T>(Future<T> Function() action) async {
    try {
      return await action();
    } on ServerException catch (e) {
      if (e.status == 404) throw const ForkHostOutdated();
      rethrow;
    }
  }

  static String _operationId(Map<String, dynamic> value) {
    final id = value['operation_id'];
    if (id is! String || !RegExp(r'^[A-Za-z0-9._~-]{1,128}$').hasMatch(id)) {
      throw const ServerException('PC 응답을 확인하지 못했어요.');
    }
    return id;
  }

  @override
  Future<ForkDestinationStatus> status(String? machine) =>
      _call(() async => ForkDestinationStatus.fromJson(await server.desktopValues('shared-rooms/status', machine: machine)));

  @override
  Future<String> start(String? machine, {required String forkId, required String requestId, required bool includeProject, String? cwd}) => _call(
    () async => _operationId(
      await server.desktopAction(
        'shared-rooms/fork',
        machine: machine,
        body: {
          'fork_id': forkId,
          'destination': {'cwd': ?cwd, 'include_project': includeProject, 'tab': true},
          'request_id': requestId,
        },
      ),
    ),
  );

  @override
  Future<ForkOperation> operation(String? machine, String operationId) => _call(
    () async => ForkOperation.fromJson(
      await server.desktopValues('shared-rooms/forks/${Uri.encodeComponent(operationId)}', machine: machine),
    ),
  );

  @override
  Future<List<PublishSource>> sources(String? machine) =>
      _call(() async => publishSourcesFrom(await server.desktopValues('shared-rooms/sources', machine: machine)));

  @override
  Future<String> publish(
    String? machine, {
    required String roomId,
    required List<Map<String, Object?>> addresses,
    required bool includeProject,
  }) => _call(
    () async => _operationId(
      await server.desktopAction(
        'shared-rooms/publish',
        machine: machine,
        body: {'room_id': roomId, 'addresses': addresses, 'include_project': includeProject},
      ),
    ),
  );
}

/// 이 계정의 데스크톱 후보 — (이름, route). route 가 null 이면 기준 기기다. 공유 창구가 있는지는 부르는 쪽이 묻는다.
Future<List<(String, String?)>> myDesktops(Server server) async {
  final out = <(String, String?)>[];
  try {
    final me = await server.me().timeout(const Duration(seconds: 15));
    out.add((me.machine?.isNotEmpty == true ? me.machine! : '기준 PC', null));
  } catch (_) {
    out.add(('기준 PC', null));
  }
  try {
    for (final m in await server.machines().timeout(const Duration(seconds: 15))) {
      if (m.online) out.add((m.label, m.route));
    }
  } catch (_) {
    // 기준 기기만으로도 고를 수 있다.
  }
  return out;
}

/// 포크를 받을 내 데스크톱 하나. [route] 가 null 이면 계정의 기준 기기다.
class ForkDestination {
  const ForkDestination({required this.label, required this.route, required this.status});
  final String label;
  final String? route;
  final ForkDestinationStatus status;
  bool get ready => status.available && status.authenticated;
}

enum ForkPhase { idle, searching, choosing, fixing, starting, waiting, done, failed }

/// 공유 대화 하나를 내 계정·내 데스크톱의 새 대화로 포크하는 한 번. 원본 게시 기기는 목적지가 될 수 없다.
class SharedForkFlow extends ChangeNotifier {
  SharedForkFlow(
    this.repository, {
    required this.roomId,
    required this.sessionId,
    ForkHost Function(Server)? host,
    this.pollEvery = const Duration(milliseconds: 1500),
    this.patience = const Duration(minutes: 3),
  }) : _host = host ?? ServerForkHost.new;

  final SharedRoomsRepository repository;
  final String roomId, sessionId;
  final ForkHost Function(Server) _host;
  final Duration pollEvery, patience;

  ForkPhase phase = ForkPhase.idle;
  List<ForkDestination> destinations = const [];

  /// 공유방 창구를 모르는 옛 판 PC 이름.
  List<String> outdated = const [];
  String? message;
  ForkAddress? result;
  ForkDestination? chosen;
  Server? _server;
  bool _disposed = false;
  int _run = 0;

  bool get busy => const [ForkPhase.searching, ForkPhase.fixing, ForkPhase.starting, ForkPhase.waiting].contains(phase);
  Server? get server => _server;

  void _set(ForkPhase next, [String? text]) {
    phase = next;
    message = text;
    if (!_disposed) notifyListeners();
  }

  bool _current(int run, Server server) =>
      !_disposed && run == _run && !repository.locked && !server.isClosed && identical(repository.desktop, server);

  SharedSession? get _session => repository.room(roomId)?.session(sessionId);

  /// 이 계정의 데스크톱 가운데 공유방 창구가 있는 것을 찾는다.
  Future<void> search() async {
    final server = repository.desktop;
    final run = ++_run;
    _server = server;
    destinations = const [];
    outdated = const [];
    if (server == null) {
      _set(ForkPhase.choosing, '포크하려면 내 PC 연결이 필요해요. PC 카사텀을 켜고 같은 계정으로 로그인해 주세요. 읽기는 계속할 수 있어요.');
      return;
    }
    _set(ForkPhase.searching);
    final host = _host(server);
    final candidates = await myDesktops(server);
    if (!_current(run, server)) return;
    final publisher = _session?.publisherMachineId ?? '';
    final found = <String, ForkDestination>{};
    final old = <String>[];
    await Future.wait([
      for (final (label, route) in candidates)
        host.status(route).then((status) {
          if (status.machineId == publisher) return;
          found.putIfAbsent(status.machineId, () => ForkDestination(label: label, route: route, status: status));
        }).catchError((Object e) {
          if (e is ForkHostOutdated) old.add(label);
        }),
    ]);
    if (!_current(run, server)) return;
    destinations = found.values.toList();
    outdated = old;
    _set(
      ForkPhase.choosing,
      destinations.isNotEmpty
          ? null
          : old.isNotEmpty
          ? const ForkHostOutdated().toString()
          : '공유방 포크를 받을 수 있는 내 PC를 찾지 못했어요. PC가 켜져 있는지 확인해 주세요.',
    );
  }

  /// 고른 [destination] 에서 새 대화를 만든다. 같은 사본·같은 목적지의 재시도는 같은 request_id 로 간다.
  /// [includeProject] 가 거짓이면 대화만 가져온다 — 사람이 그렇게 고른 때만.
  Future<void> start(ForkDestination destination, {String cwd = '', bool includeProject = true}) async {
    final server = _server;
    if (server == null || busy) return;
    final run = ++_run;
    final room = repository.room(roomId);
    final session = _session;
    chosen = destination;
    result = null;
    if (room == null || session == null || !room.canFork(session)) {
      _set(ForkPhase.failed, '이 대화는 지금 포크할 수 없어요.');
      return;
    }
    if (!destination.ready) {
      _set(ForkPhase.failed, '그 PC에서 KASA 계정 로그인이 필요해요.');
      return;
    }
    if (destination.status.machineId == session.publisherMachineId) {
      _set(ForkPhase.failed, '원본을 게시한 기기로는 포크할 수 없어요.');
      return;
    }
    final host = _host(server);
    try {
      _set(ForkPhase.fixing, '내 사본을 고정하는 중');
      final info = await repository.fixFork(room, session);
      if (!_current(run, server)) return;
      _set(ForkPhase.starting, '${destination.label}에서 새 대화를 준비하는 중');
      final withProject = includeProject && session.checkpoint!.hasProject;
      final requestId = repository.stableRequestId(
        roomId,
        'host\u0000${info.id}\u0000${destination.status.machineId}\u0000$withProject\u0000$cwd',
      );
      final operation = await host.start(
        destination.route,
        forkId: info.id,
        requestId: requestId,
        includeProject: withProject,
        cwd: cwd.isEmpty ? null : cwd,
      );
      if (!_current(run, server)) return;
      _set(ForkPhase.waiting, '${destination.label}에서 새 대화를 준비하는 중');
      final deadline = DateTime.now().add(patience);
      while (true) {
        final op = await host.operation(destination.route, operation);
        if (!_current(run, server)) return;
        switch (op.stage) {
          case ForkStage.ready:
            final address = op.address!;
            if (address.machineId != destination.status.machineId || address.machineId == session.publisherMachineId) {
              _set(ForkPhase.failed, '새 대화가 고른 PC가 아닌 곳에 생겼다고 답했어요. 열지 않았어요.');
              return;
            }
            result = address;
            _set(ForkPhase.done, '${destination.label}에 내 새 대화를 만들었어요.');
            return;
          case ForkStage.failed:
            _set(ForkPhase.failed, op.error.isNotEmpty ? op.error : '포크를 만들지 못했어요.');
            return;
          case ForkStage.needsInput:
            _set(ForkPhase.waiting, op.error.isNotEmpty ? op.error : '${destination.label} 화면에서 확인이 필요해요.');
          case ForkStage.preparing || ForkStage.starting:
            if (phase != ForkPhase.waiting || message == null) _set(ForkPhase.waiting, '${destination.label}에서 새 대화를 준비하는 중');
        }
        if (DateTime.now().isAfter(deadline)) {
          _set(ForkPhase.failed, '준비가 오래 걸리고 있어요. 다시 누르면 같은 작업을 이어서 확인해요.');
          return;
        }
        await Future<void>.delayed(pollEvery);
        if (!_current(run, server)) return;
      }
    } on ForkHostOutdated catch (e) {
      if (_current(run, server)) _set(ForkPhase.failed, e.toString());
    } on SharedRoomsException catch (e) {
      if (_current(run, server)) _set(ForkPhase.failed, e.message);
    } on ServerException catch (e) {
      if (_current(run, server)) _set(ForkPhase.failed, e.message);
    } catch (_) {
      if (_current(run, server)) _set(ForkPhase.failed, '포크 응답을 확인하지 못했어요.');
    }
  }

  @override
  void dispose() {
    _disposed = true;
    _run++;
    super.dispose();
  }
}
