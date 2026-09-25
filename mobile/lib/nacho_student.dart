/// 나쵸 작업 장부의 「맡은 학생」(`student: {surface, host, machine_id}`)을 카사텀의 실제 pane 으로
/// 잇는다 — 이름·얼굴·지금 상태를 보이고 그 학생 화면을 앱 안에서 열기 위해.
///
/// 짐작하지 않는다. 기계는 장부가 적은 machine_id 가 명부(`/machines` 의 `~<id>` route)에 있으면
/// 그 기계, 나쵸가 사는 기계(`host` 「미니」)면 이 주소의 기계다. 둘 다 아니면 못 찾았다고 한다 —
/// pane 번호는 기계마다 따로 매겨져 엉뚱한 기계에서 같은 번호의 다른 학생을 집을 수 있다.
library;

import 'package:flutter/foundation.dart';

import 'server.dart';

/// 나쵸가 사는 기계를 장부가 부르는 이름(`panewatch.Host.name`).
const nachoHost = '미니';

class StudentSeat {
  const StudentSeat({
    required this.surface,
    required this.machine,
    this.route,
    this.pane,
    this.placed = true,
    this.read = false,
    this.unreachable = false,
  });

  final String surface;

  /// 사람에게 보일 기계 이름.
  final String machine;

  /// 요청에 쓰는 route. null 이면 이 주소의 기계.
  final String? route;

  /// 지금 목록의 그 pane. 없으면 닫혔거나 아직 못 읽었다.
  final Pane? pane;

  /// 어느 기계인지 정했나 — 못 정했으면 pane 을 찾지 않는다.
  final bool placed;

  /// 그 기계의 목록을 한 번이라도 읽었나 — 읽기 전의 「없음」은 닫힘이 아니다.
  final bool read;

  /// 마지막 읽기가 실패했다 — 직전 목록으로 답하는 중.
  final bool unreachable;
}

class StudentLookup extends ChangeNotifier {
  StudentLookup(this.server, {this.ttl = const Duration(seconds: 15)});

  final Server server;
  final Duration ttl;

  List<Machine>? _machines;
  String? _here;
  DateTime? _rosterAt;
  final Map<String, ({DateTime at, List<Pane> list, bool ok})> _panes = {};
  final Set<String> _inflight = {};
  bool _disposed = false;

  /// 캐시로 곧장 답한다. 없거나 낡았으면 뒤에서 다시 읽고 알린다 — 같은 기계를 겹쳐 묻지 않는다.
  StudentSeat? seat(Map<String, Object?> student) {
    final surface = student['surface'] as String? ?? '';
    if (surface.isEmpty) return null;
    if (_stale(_rosterAt)) _fetchRoster();
    final roster = _machines;
    if (roster == null) return null;
    final id = student['machine_id'] as String? ?? '';
    final host = student['host'] as String? ?? '';
    Machine? remote;
    for (final m in roster) {
      if (id.isNotEmpty && m.route == '~$id') remote = m;
    }
    if (remote == null && id.isEmpty && host.isNotEmpty && host != nachoHost) {
      for (final m in roster) {
        if (m.label == host) remote = m;
      }
    }
    if (remote == null && host != nachoHost) {
      return StudentSeat(surface: surface, machine: host.isEmpty ? '기기 모름' : host, placed: false);
    }
    final route = remote?.route;
    final key = route ?? '';
    final cached = _panes[key];
    if (cached == null || _stale(cached.at)) _fetchPanes(route);
    Pane? pane;
    for (final p in cached?.list ?? const <Pane>[]) {
      if (p.id == surface) pane = p;
    }
    return StudentSeat(
      surface: surface,
      machine: remote?.label ?? _here ?? '이 기계',
      route: route,
      pane: pane,
      read: cached != null,
      unreachable: cached != null && !cached.ok,
    );
  }

  /// 그 기계의 pane 목록을 지금 다시 읽는다 — 링크를 눌러 열 때.
  Future<Pane?> fresh(String? route, String surface) async {
    final list = await server.panes(machine: route);
    _panes[route ?? ''] = (at: DateTime.now(), list: list, ok: true);
    _notify();
    for (final p in list) {
      if (p.id == surface) return p;
    }
    return null;
  }

  bool _stale(DateTime? at) => at == null || DateTime.now().difference(at) > ttl;

  void _fetchRoster() {
    if (!_inflight.add('#roster')) return;
    () async {
      try {
        final list = await server.machines();
        String? here;
        try {
          here = (await server.me()).machine;
        } on ServerException {
          here = _here;
        }
        _machines = list;
        _here = here;
      } on ServerException {
        _machines ??= const [];
      } finally {
        _rosterAt = DateTime.now();
        _inflight.remove('#roster');
        _notify();
      }
    }();
  }

  void _fetchPanes(String? route) {
    final key = route ?? '';
    if (!_inflight.add(key)) return;
    () async {
      try {
        _panes[key] = (at: DateTime.now(), list: await server.panes(machine: route), ok: true);
      } on ServerException {
        // 못 읽었으면 직전 목록을 두고 다음 기회에 — 한 번 실패로 학생을 「닫힘」이라 하지 않는다.
        _panes[key] = (at: DateTime.now(), list: _panes[key]?.list ?? const [], ok: false);
      } finally {
        _inflight.remove(key);
        _notify();
      }
    }();
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    super.dispose();
  }
}
