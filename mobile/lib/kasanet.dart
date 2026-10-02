import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';

/// 입구 하나의 지금 길. [direct] 는 입구로 실어도 되는가 — 직통이거나 국내 자체 중계([relayed]).
class KasanetPath {
  const KasanetPath({required this.direct, this.relayed = false, this.rttMs, this.error});
  final bool direct;
  final bool relayed;
  final int? rttMs;
  final String? error;
}

/// 앱에 링크된 카사넷(`crates/kasa-net-ffi`). 시험은 가짜를 끼운다.
abstract interface class KasanetNative {
  String? get id;

  /// 데스크톱 `/version` 의 `kasanet` 칸으로 입구를 연다. 입구 로컬 포트, 실패면 -1.
  int open(String peerJson);
  KasanetPath? state(int port);
  void networkChanged();
  String? get lastError;
}

/// 관문으로만 가는 두 요청 — 데스크톱 주소 받기와 폰 id 등록.
abstract interface class KasanetGateway {
  Future<Map<String, Object?>?> version(String? machine);

  /// 등록 수명. 데스크톱이 거절하면(옛 판·주인 아님·카사넷 꺼짐) null.
  Future<Duration?> register(String? machine, String phoneId);
}

class _Desk {
  _Desk(this.port);
  final int port;
  bool direct = false;
  bool relayed = false;
  int? rttMs;
  Timer? renew;
  DateTime? reRegisteredAt;
}

/// 카사넷 — 폰이 데스크톱으로 가는 길 고르기. 설계 `docs/kasanet.md` 「P5」.
///
/// 데스크톱마다 앱 안 입구(`127.0.0.1:L`) 하나. 요청마다 [base] 가 직통이면 입구를, 아니면 null(관문)을 준다.
/// 데스크톱 주소는 관문으로 받은 `/version` 의 `kasanet` 칸에서, 폰 id 등록도 관문으로만 한다 — 데스크톱은 관문이
/// 주인 폰 자격을 확인한 등록만 받는다. 등록은 수명이 있어 그 3분의 1마다 다시 한다.
/// 입구는 직통을 잃으면 실던 연결을 끊는다 — 부르는 쪽은 끊긴 연결을 다시 붙이며 관문으로 간다.
class KasanetRouter extends ChangeNotifier {
  KasanetRouter(
    this._native,
    this._gateway, {
    Duration poll = const Duration(seconds: 1),
  }) {
    _live.add(this);
    _poll = Timer.periodic(poll, (_) => _refresh());
  }

  static final Set<KasanetRouter> _live = {};

  /// 앱이 깨어났다 — 망이 바뀌었을 수 있으니 경로를 다시 찾고 등록을 곧바로 새로 한다.
  static void resumed() {
    for (final r in List.of(_live)) {
      r._native.networkChanged();
      r._renewAll();
    }
  }

  final KasanetNative _native;
  final KasanetGateway _gateway;
  late final Timer _poll;
  final Map<String, _Desk> _desks = {};
  final Map<String, DateTime> _retryAfter = {};
  final Set<String> _learning = {};
  bool _disposed = false;

  static const _retryMissing = Duration(minutes: 10);
  static const _retryFailed = Duration(minutes: 1);
  static const _reRegisterGap = Duration(seconds: 20);
  static const _retryGateway = Duration(seconds: 90);

  /// `m/<route>` 의 route. 기본 기계(관문이 고르는 쪽)는 빈 문자열. 표시 이름뿐인 옛 route 는 다루지 않는다.
  static String? _key(String? machine) {
    if (machine == null || machine.isEmpty) return '';
    return machine.startsWith('~') ? machine : null;
  }

  /// 이 기계로 갈 입구 — 직통일 때만. 처음 묻는 기계면 뒤에서 배우기 시작하고 이번에는 관문으로 보낸다.
  Uri? base(String? machine) {
    final key = _key(machine);
    if (key == null || _disposed) return null;
    final desk = _desks[key];
    if (desk == null) {
      _learn(key);
      return null;
    }
    return desk.direct ? Uri.parse('http://127.0.0.1:${desk.port}/') : null;
  }

  /// 이 주소가 입구로 가는가.
  bool owns(Uri u) =>
      u.host == '127.0.0.1' && _desks.values.any((d) => d.port == u.port);

  /// 입구 주소의 기계 route — 관문으로 되돌려 보낼 때 쓴다. 기본 기계면 빈 문자열.
  String? machineOf(Uri u) {
    if (!owns(u)) return null;
    for (final e in _desks.entries) {
      if (e.value.port == u.port) return e.key;
    }
    return null;
  }

  /// 기계 route → (직통인가, 왕복 ms). 모르는 기계면 null — 설정 화면·개발 서버 화면의 「직통/관문」 표시.
  (bool, int?)? pathOf(String? machine) {
    final key = _key(machine);
    final desk = key == null ? null : _desks[key];
    return desk == null ? null : (desk.direct, desk.rttMs);
  }

  /// 입구로 가되 직통이 아니라 국내 자체 중계를 거치는가 — 「직통/중계」 표시.
  bool relayedOf(String? machine) {
    final key = _key(machine);
    final desk = key == null ? null : _desks[key];
    return desk != null && desk.direct && desk.relayed;
  }

  /// `/version` → 등록 → 입구. 처음 배울 때도, 수명 3분의 1마다 다시 등록할 때도 같은 길이다 — 데스크톱 앱이 다른
  /// 포트로 다시 떴으면 입구가 새 포트로 잇게 된다(같은 데스크톱이면 입구 로컬 포트는 그대로).
  Future<void> _learn(String key, {bool renew = false}) async {
    final wait = _retryAfter[key];
    if (_learning.contains(key) ||
        (!renew && wait != null && DateTime.now().isBefore(wait))) {
      return;
    }
    final id = _native.id;
    if (id == null) return;
    _learning.add(key);
    final machine = key.isEmpty ? null : key;
    try {
      final v = await _gateway.version(machine);
      final kasanet = v?['kasanet'];
      if (_disposed) return;
      // 옛 판·카사넷을 끈 데스크톱, 거절(주인 아님) — 한동안 묻지 않는다.
      final ttl = kasanet is Map ? await _gateway.register(machine, id) : null;
      if (_disposed) return;
      if (ttl == null) {
        _forget(key);
        _retryAfter[key] = DateTime.now().add(_retryMissing);
        return;
      }
      final port = _native.open(jsonEncode(kasanet));
      if (port <= 0) {
        _retryAfter[key] = DateTime.now().add(_retryFailed);
        return;
      }
      final old = _desks[key];
      final desk = old?.port == port
          ? old!
          : _desks.values.where((d) => d.port == port).firstOrNull ??
                _Desk(port);
      if (old != null && !identical(old, desk)) _forget(key);
      _desks[key] = desk;
      final machineId = v?['machine_id'];
      if (key.isEmpty && machineId is String && machineId.isNotEmpty) {
        _desks['~$machineId'] = desk;
      }
      _schedule(desk, ttl);
      _refresh();
    } catch (_) {
      // 관문에 못 닿았다 — 이미 배운 입구는 데스크톱 등록이 살아 있을 수 있어 두고 조금 뒤 다시.
      final desk = _desks[key];
      if (desk != null) {
        _schedule(desk, _retryGateway);
      } else {
        _retryAfter[key] = DateTime.now().add(_retryFailed);
      }
    } finally {
      _learning.remove(key);
    }
  }

  String? _keyOfDesk(_Desk desk) {
    for (final e in _desks.entries) {
      if (identical(e.value, desk)) return e.key;
    }
    return null;
  }

  /// 수명의 3분의 1 뒤 다시 등록한다. 등록은 기본 기계 쪽 route 하나로 — 같은 입구의 별칭은 같이 산다.
  void _schedule(_Desk desk, Duration ttl) {
    desk.renew?.cancel();
    final every = ttl ~/ 3;
    desk.renew = Timer(
      every < _retryGateway ~/ 3 ? _retryGateway ~/ 3 : every,
      () => _reRegister(desk),
    );
  }

  void _reRegister(_Desk desk) {
    final key = _keyOfDesk(desk);
    if (key == null || _disposed) return;
    desk.reRegisteredAt = DateTime.now();
    _learn(key, renew: true);
  }

  void _renewAll() {
    for (final desk in _desks.values.toSet()) {
      _reRegister(desk);
    }
  }

  /// 이 route 가 가리키던 입구와 그 별칭을 잊는다. 입구 자체는 닫지 않는다 — 앱 안 입구는 데스크톱마다 하나라
  /// 다른 연결(알림 정리용 서버)이 같이 쓴다.
  void _forget(String key) {
    final desk = _desks[key];
    if (desk == null) return;
    desk.renew?.cancel();
    _desks.removeWhere((_, d) => identical(d, desk));
    notifyListeners();
  }

  void _refresh() {
    if (_disposed) return;
    var changed = false;
    for (final desk in _desks.values.toSet()) {
      final s = _native.state(desk.port);
      final direct = s?.direct ?? false;
      final relayed = s?.relayed ?? false;
      desk.rttMs = s?.rttMs;
      if (direct != desk.direct || relayed != desk.relayed) {
        desk.direct = direct;
        desk.relayed = relayed;
        changed = true;
      }
      // 데스크톱이 다시 떠 이 폰을 잊었으면(403) 수명을 기다리지 않고 다시 등록한다.
      final at = desk.reRegisteredAt;
      if (!direct &&
          (s?.error ?? '').contains('403') &&
          (at == null || DateTime.now().difference(at) > _reRegisterGap)) {
        _reRegister(desk);
      }
    }
    if (changed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _live.remove(this);
    _poll.cancel();
    for (final desk in _desks.values.toSet()) {
      desk.renew?.cancel();
    }
    _desks.clear();
    super.dispose();
  }
}
