import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

import 'server.dart';

class HubRoom {
  const HubRoom({required this.title, required this.panes});
  final String title;
  final List<Pane> panes;
}

class HubSection {
  const HubSection({
    required this.machine,
    required this.online,
    required this.rooms,
  });

  /// null 이면 주소가 가리키는 그 기계.
  final String? machine;
  final bool online;
  final List<HubRoom> rooms;

  int get paneCount => rooms.fold(0, (n, r) => n + r.panes.length);
}

/// 허브 한 화면의 상태. 서버 푸시가 없어 앞에 있을 때만 5초마다 묻는다.
class HubModel extends ChangeNotifier {
  HubModel(this.server);

  final Server server;
  static const pollEvery = Duration(seconds: 5);

  List<HubSection> sections = const [];
  String? error;
  bool loading = false;
  DateTime? updatedAt;

  /// 지금 기다리는 학생 수 — 상단 배지. 「마지막으로 본 뒤 새로 기다리게 된 수」를
  /// 누적했더니 학생 화면에 머무는 동안 상태가 오락가락한 것까지 쌓여 52 같은 수가
  /// 떴다. 서버가 말하는 지금 수가 늘 맞고, 목록이 길 때 위에서 한눈에 보인다.
  int waiting = 0;

  Map<String, String> _lastStatus = const {};
  Timer? _timer;

  void start() {
    refresh();
    _timer ??= Timer.periodic(pollEvery, (_) => refresh());
  }

  void stop() {
    _timer?.cancel();
    _timer = null;
  }

  Future<void> refresh() async {
    if (loading) return;
    loading = true;
    try {
      final results = await Future.wait<Object>([
        server.panes(),
        server.sessions(),
        server.machines(),
      ]);
      final panes = results[0] as List<Pane>;
      final labels = results[1] as List<String>;
      final machines = results[2] as List<Machine>;
      final next = <HubSection>[
        HubSection(machine: null, online: true, rooms: _rooms(panes, labels)),
        for (final m in machines)
          HubSection(
            machine: m.label,
            online: m.online,
            rooms: _rooms(m.panes, const []),
          ),
      ];
      _noteWaiting(next);
      sections = next;
      error = null;
      updatedAt = DateTime.now();
    } on ServerException catch (e) {
      error = e.message;
    } catch (_) {
      error = '${server.describe()} 에 닿지 못했다';
    }
    loading = false;
    notifyListeners();
  }

  /// 처음 본 학생은 세지 않는다 — 앱을 켠 순간 이미 기다리던 것은 목록 맨 위에
  /// 보이는 것으로 충분하고, 진동은 「방금 바뀐 것」에만 의미가 있다.
  void _noteWaiting(List<HubSection> next) {
    final status = <String, String>{};
    var fresh = 0;
    var now = 0;
    for (final s in next) {
      for (final r in s.rooms) {
        for (final p in r.panes) {
          final key = '${s.machine ?? ''}|${p.id}';
          status[key] = p.status;
          if (p.isWaiting) now++;
          final before = _lastStatus[key];
          final wasWaiting = before == 'waiting' || before == 'blocked';
          if (p.isWaiting && before != null && !wasWaiting) fresh++;
        }
      }
    }
    _lastStatus = status;
    waiting = now;
    if (fresh > 0) HapticFeedback.mediumImpact();
  }

  static int _rank(Pane p) => p.isWaiting ? 0 : (p.isBusy ? 1 : 2);

  static List<HubRoom> _rooms(List<Pane> panes, List<String> labels) {
    final byWindow = <int, List<Pane>>{};
    for (final p in panes) {
      byWindow.putIfAbsent(p.window, () => []).add(p);
    }
    final windows = byWindow.keys.toList()..sort();
    return [
      for (final w in windows)
        HubRoom(
          title: w < labels.length && labels[w].isNotEmpty
              ? labels[w]
              : '방 ${w + 1}',
          panes: byWindow[w]!
            ..sort((a, b) {
              final r = _rank(a).compareTo(_rank(b));
              return r != 0 ? r : a.name.compareTo(b.name);
            }),
        ),
    ];
  }

  @override
  void dispose() {
    stop();
    super.dispose();
  }
}
