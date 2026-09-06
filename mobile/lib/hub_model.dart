import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

import 'hub_prefs.dart';
import 'server.dart';

class HubRoom {
  const HubRoom({
    required this.title,
    required this.panes,
    this.rects = const [],
    this.aspect,
  });
  final String title;
  final List<Pane> panes;

  /// 데스크톱에서의 자리 — 비어 있으면 미니맵 없이 목록만.
  final List<PaneRect> rects;

  /// 데스크톱 창의 가로÷세로 — 미니맵을 그 모양대로 그린다.
  final double? aspect;

  Pane? paneOf(String surface) {
    for (final p in panes) {
      if (p.id == surface) return p;
    }
    return null;
  }
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
  HubModel(this.server, {HubPrefs? prefs}) : _prefs = prefs;

  final Server server;
  final HubPrefs? _prefs;
  static const pollEvery = Duration(seconds: 5);

  List<HubSection> sections = const [];

  /// 어느 기기를 따라갈지·어떤 모양으로 볼지. 배지는 보기와 무관하게 전부 센다 —
  /// 맥미니만 보고 있어도 맥북 학생이 기다리면 알아야 한다.
  HubView view = const HubView();

  /// 주소가 가리키는 기계의 이름(`mobile/me`). 못 받으면 「이 기계」.
  String? rootName;

  /// 지금 보기로 거른 목록. 고른 기기가 목록에서 사라졌으면(이름이 바뀌거나 꺼짐)
  /// 빈 화면 대신 전부를 보인다.
  List<HubSection> get visible => filterSections(sections, view.machine);

  @visibleForTesting
  static List<HubSection> filterSections(
    List<HubSection> all,
    String? machine,
  ) {
    if (machine == null) return all;
    final picked = [
      for (final s in all)
        if ((s.machine ?? '') == machine) s,
    ];
    return picked.isEmpty ? all : picked;
  }

  Future<void> setView(HubView next) async {
    view = next;
    notifyListeners();
    await _prefs?.save(next);
  }

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
    _loadPrefs();
    _loadRootName();
  }

  Future<void> _loadPrefs() async {
    final p = _prefs;
    if (p == null) return;
    view = await p.load();
    notifyListeners();
  }

  Future<void> _loadRootName() async {
    try {
      final me = await server.me();
      final m = me.machine;
      if (m != null && m.isNotEmpty) {
        rootName = m;
        notifyListeners();
      }
    } catch (_) {
      // 이름은 꾸밈이다 — 못 받으면 「이 기계」로 둔다.
    }
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
      final layouts = await Future.wait([
        _layoutsOf(null),
        for (final m in machines)
          m.online ? _layoutsOf(m.label) : Future.value(const <WindowLayout>[]),
      ]);
      final next = <HubSection>[
        HubSection(
          machine: null,
          online: true,
          rooms: rooms(panes, labels, layouts[0]),
        ),
        for (var i = 0; i < machines.length; i++)
          HubSection(
            machine: machines[i].label,
            online: machines[i].online,
            rooms: rooms(machines[i].panes, const [], layouts[i + 1]),
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

  /// 배치는 곁들이다 — 못 받아도 학생 목록은 그대로 뜬다.
  Future<List<WindowLayout>> _layoutsOf(String? machine) async {
    try {
      return await server.windows(machine: machine);
    } catch (_) {
      return const [];
    }
  }

  static int _rank(Pane p) => p.isWaiting ? 0 : (p.isBusy ? 1 : 2);

  @visibleForTesting
  static List<HubRoom> rooms(
    List<Pane> panes,
    List<String> labels, [
    List<WindowLayout> layouts = const [],
  ]) {
    final byWindow = <int, List<Pane>>{};
    for (final p in panes) {
      byWindow.putIfAbsent(p.window, () => []).add(p);
    }
    final layoutOf = {for (final l in layouts) l.idx: l};
    final windows = byWindow.keys.toList()..sort();
    return [
      for (final w in windows)
        HubRoom(
          title: w < labels.length && labels[w].isNotEmpty
              ? labels[w]
              : '방 ${w + 1}',
          rects: layoutOf[w]?.rects ?? const [],
          aspect: layoutOf[w]?.aspect,
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
