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

  /// 배치도 어느 칸에도 안 앉은 학생 — 탭 안에 숨어 있는데 서버가 탭 목록을 안 주는
  /// 경우(옛 판)다. 지도가 그를 빠뜨리면 「답 기다림」도 같이 사라지므로 따로 줄 세운다.
  /// 서버가 탭을 실어 주면 자연히 빈다.
  List<Pane> get unplaced {
    if (rects.isEmpty) return const [];
    final seated = <String>{
      for (final r in rects) ...[r.surface, ...r.tabs],
    };
    return [
      for (final p in panes)
        if (!seated.contains(p.id) && !p.undocked) p,
    ];
  }

  /// 별도 OS 창으로 뗀 학생 — 배치도 칸엔 없고 지도 밑 「별도창」 줄에 선다.
  List<Pane> get undocked => [
    for (final p in panes)
      if (p.undocked) p,
  ];
}

class HubSection {
  const HubSection({
    required this.machine,
    this.route,
    required this.online,
    required this.rooms,
  });

  /// null 이면 주소가 가리키는 그 기계.
  final String? machine;

  /// 요청에 쓰는 안정 route. `machine`은 사람에게 보여 줄 이름으로만 남긴다.
  final String? route;
  final bool online;
  final List<HubRoom> rooms;

  int get paneCount => rooms.fold(0, (n, r) => n + r.panes.length);
}

/// 앱이 살아 있는 동안 기억하는 마지막 목록 — 허브를 다시 열면 이것부터 그린다.
/// 관문 너머 요청 하나가 1초 안팎이라 빈 화면으로 기다리게 하지 않는다.
class _Snapshot {
  List<Pane>? rootPanes;
  List<String> rootLabels = const [];
  List<WindowLayout> rootLayouts = const [];
  List<Machine> machines = const [];
  Map<String, List<Pane>> remotePanes = const {};
  Map<String, List<WindowLayout>> remoteLayouts = const {};
  Map<String, List<Note>> notes = const {};
  String? rootName;
}

/// 허브 한 화면의 상태. 기계마다 따로 받아 받은 것부터 그리고, 서버가 알려 주는
/// 변경 번호(`term/changes` 롱폴)에 매달려 바뀐 기계만 곧바로 다시 읽는다. 폴링은
/// 그 알림이 못 덮는 것(작업 중 도구 이름·옛 판 서버)을 위한 바닥이다.
class HubModel extends ChangeNotifier {
  HubModel(this.server, {HubPrefs? prefs}) : _prefs = prefs {
    _seed();
  }

  final Server server;
  final HubPrefs? _prefs;

  /// 알림을 못 받는 서버에서의 폴링 박자 — 학생 화면도 같은 박자로 제 줄을 다시 읽는다.
  static const pollEvery = Duration(seconds: 5);

  /// 상태 전이를 알려 주는 서버에서는 도구 이름·컨텍스트 % 만 폴링으로 따라간다 —
  /// [pollEvery] 몇 바퀴에 한 번인가(15초).
  static const quietPollTicks = 3;

  /// 목록 요청 하나의 상한. 관문 왕복이 1초 안팎이라 넉넉하고, 넘으면 그 기계만 직전 것을 둔다.
  static const listTimeout = Duration(seconds: 10);

  static final Map<String, _Snapshot> _cache = {};

  @visibleForTesting
  static void clearCache() => _cache.clear();

  /// 첫 화면에서 미리 한 바퀴 받아 둔다 — 학생 목록을 여는 순간 이미 그려져 있게.
  static Future<void> warm(Server server) async {
    final model = HubModel(server);
    try {
      await model.refresh();
    } finally {
      model.dispose();
    }
  }

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

  Future<void> toggleFold(HubSection s) =>
      setView(view.toggleFolded(s.machine));

  String? error;
  DateTime? updatedAt;

  /// 주소 기계의 줄이 아직 지난번 기억이다 — 화면이 얇은 진행 막대로 「확인 중」을 말한다.
  bool get showingCached => _seeded.contains(_rootKey) && error == null;

  /// 지금 기다리는 학생 수 — 상단 배지. 「마지막으로 본 뒤 새로 기다리게 된 수」를
  /// 누적했더니 학생 화면에 머무는 동안 상태가 오락가락한 것까지 쌓여 52 같은 수가
  /// 떴다. 서버가 말하는 지금 수가 늘 맞고, 목록이 길 때 위에서 한눈에 보인다.
  int waiting = 0;

  /// 나쵸가 남긴 학생 쪽지 — 모든 기계 것을 합쳐 최근 것부터. 종 아이콘 목록.
  List<Note> notes = const [];
  int get unread => notes.where((n) => !n.read).length;

  // 조각마다 따로 받고 따로 고친다 — 느린 기계 하나가 나머지를 붙잡지 않게.
  static const _rootKey = '';
  List<Pane>? _rootPanes;
  List<String> _rootLabels = const [];
  List<WindowLayout> _rootLayouts = const [];
  List<Machine> _machines = const [];
  final Map<String, List<Pane>> _remotePanes = {};
  final Map<String, List<WindowLayout>> _remoteLayouts = {};
  final Map<String, List<Note>> _notesBy = {};

  /// 기억에서 꺼낸 채 아직 새로 못 받은 기계. 전이 진동은 새로 받은 것끼리만 비교한다 —
  /// 허브를 닫아 둔 사이 바뀐 것까지 울리면 「방금 바뀐 것」이라는 뜻이 사라진다.
  final Set<String> _seeded = {};
  final Set<String> _seenNotes = {};
  final Set<String> _notesPrimed = {};
  Map<String, String> _lastStatus = const {};

  bool _running = false;
  bool _disposed = false;
  bool _prefsLoaded = false;
  Timer? _timer;

  final Map<String, Future<void>> _inflight = {};
  final Set<String> _again = {};

  /// 기계마다 마지막으로 읽기 시작한 차례와, 그 뒤로 지난 폴링 바퀴 수. 벽시계 대신
  /// 바퀴를 센다 — 타이머와 요청 시작이 어긋나 박자가 두 배로 늘어지는 일이 없다.
  final Map<String, int> _startSeq = {};
  int _seq = 0;
  final Map<String, int> _ticksSince = {};

  /// 기계마다 롱폴 한 줄. 값은 그 줄의 번호표 — 멈추거나 새로 걸면 옛 줄은 제 번호가
  /// 사라진 것을 보고 스스로 끝난다(날아가는 요청은 취소할 수 없다).
  final Map<String, int> _watch = {};
  int _watchSeq = 0;
  final Set<String> _watchOk = {};
  final Set<String> _statusAware = {};
  final Set<String> _noChanges = {};
  final Map<Timer, Completer<void>> _sleeps = {};

  void _seed() {
    final c = _cache[server.root.toString()];
    if (c == null) return;
    _rootPanes = c.rootPanes;
    _rootLabels = c.rootLabels;
    _rootLayouts = c.rootLayouts;
    _machines = c.machines;
    _remotePanes.addAll(c.remotePanes);
    _remoteLayouts.addAll(c.remoteLayouts);
    _notesBy.addAll(c.notes);
    rootName = c.rootName;
    if (_rootPanes != null) _seeded.add(_rootKey);
    _seeded.addAll(_machines.map((m) => m.route));
    for (final list in _notesBy.values) {
      _seenNotes.addAll(list.map((n) => n.key));
    }
    _compose();
  }

  void start() {
    if (_disposed) return;
    _running = true;
    // 앱이 쉬는 사이 서버가 새 판으로 바뀌었을 수 있다 — 알림 길을 다시 물어본다.
    _noChanges.clear();
    unawaited(refresh());
    _timer ??= Timer.periodic(pollEvery, (_) => _tick());
    _syncWatchers();
    if (!_prefsLoaded) {
      _prefsLoaded = true;
      _loadPrefs();
      _loadRootName();
    }
  }

  Future<void> _loadPrefs() async {
    final p = _prefs;
    if (p == null) return;
    view = await p.load();
    if (!_disposed) notifyListeners();
  }

  Future<void> _loadRootName() async {
    try {
      final me = await server.me();
      final m = me.machine;
      if (m != null && m.isNotEmpty) {
        rootName = m;
        _save();
        if (!_disposed) notifyListeners();
      }
    } catch (_) {
      // 이름은 꾸밈이다 — 못 받으면 「이 기계」로 둔다.
    }
  }

  void stop() {
    _running = false;
    _timer?.cancel();
    _timer = null;
    _watch.clear();
    _watchOk.clear();
    for (final e in _sleeps.entries) {
      e.key.cancel();
      if (!e.value.isCompleted) e.value.complete();
    }
    _sleeps.clear();
  }

  /// 전부 한 번 — 당겨서 새로 고침·첫 화면. 명부에 새로 뜬 기계까지 받은 뒤 돌아온다.
  Future<void> refresh() async {
    final mark = _seq;
    await Future.wait([
      _refreshSource(_rootKey),
      for (final m in _machines)
        if (m.online) _refreshSource(m.route),
    ]);
    await Future.wait([
      for (final m in _machines)
        if (m.online)
          _inflight[m.route] ??
              ((_startSeq[m.route] ?? -1) <= mark
                  ? _refreshSource(m.route)
                  : Future<void>.value()),
    ]);
  }

  /// 한 기계를 다시 읽는다. 이미 읽는 중이면 끝난 뒤 한 번 더 — 그 사이 바뀐 것을
  /// 놓치지 않으면서 같은 요청을 겹쳐 쏘지 않는다.
  Future<void> _refreshSource(String key) {
    final running = _inflight[key];
    if (running != null) {
      _again.add(key);
      return running;
    }
    final future = () async {
      do {
        _again.remove(key);
        _startSeq[key] = ++_seq;
        _ticksSince[key] = 0;
        try {
          await (key == _rootKey ? _loadRoot() : _loadRemote(key));
        } catch (_) {
          // 조각마다 제 실패를 삼킨다 — 여기까지 오면 다음 바퀴에 맡긴다.
        }
      } while (_again.contains(key) && !_disposed);
    }();
    _inflight[key] = future;
    return future.whenComplete(() => _inflight.remove(key));
  }

  void _tick() {
    for (final key in [
      _rootKey,
      for (final m in _machines)
        if (m.online) m.route,
    ]) {
      final ticks = (_ticksSince[key] ?? quietPollTicks) + 1;
      _ticksSince[key] = ticks;
      final pushed = _watchOk.contains(key) && _statusAware.contains(key);
      if (ticks >= (pushed ? quietPollTicks : 1)) {
        unawaited(_refreshSource(key));
      }
    }
  }

  Future<void> _loadRoot() =>
      Future.wait([_loadRootPanes(), _loadMachines(), _loadNotes(null)]);

  Future<void> _loadRootPanes() async {
    try {
      final results = await Future.wait<Object?>([
        server.panes().timeout(listTimeout),
        _orNull(server.sessions()),
        _layoutsOf(null),
      ]);
      if (_disposed) return;
      _rootPanes = results[0] as List<Pane>;
      _rootLabels = results[1] as List<String>? ?? _rootLabels;
      _rootLayouts = results[2] as List<WindowLayout>? ?? _rootLayouts;
      _seeded.remove(_rootKey);
      error = null;
      updatedAt = DateTime.now();
    } on ServerException catch (e) {
      error = e.message;
    } on TimeoutException {
      error = '${server.describe()} 응답이 늦다';
    } catch (_) {
      error = '${server.describe()} 에 닿지 못했다';
    }
    _compose();
  }

  /// 명부를 받으면 새로 켜졌거나 처음 보는 기계를 바로 읽기 시작한다 — 주소 기계의
  /// 목록을 기다리지 않는다.
  Future<void> _loadMachines() async {
    final List<Machine> list;
    try {
      list = await server.machines().timeout(listTimeout);
    } catch (_) {
      return; // 명부를 못 받으면 직전 것을 둔다.
    }
    if (_disposed) return;
    final before = {for (final m in _machines) m.route: m.online};
    final routes = {for (final m in list) m.route};
    _remotePanes.removeWhere((r, _) => !routes.contains(r));
    _remoteLayouts.removeWhere((r, _) => !routes.contains(r));
    for (final m in list) {
      // 명부가 실어 온 행도 방금 받은 것이다. 비었으면(관문으로만 붙은 기계) 기억을 둔다.
      if (_seeded.contains(m.route) && m.panes.isNotEmpty) {
        _remotePanes.remove(m.route);
        _seeded.remove(m.route);
      }
      if (!m.online) _seeded.remove(m.route);
    }
    _machines = list;
    _compose();
    _syncWatchers();
    for (final m in list) {
      if (m.online &&
          (before[m.route] != true || !_startSeq.containsKey(m.route))) {
        unawaited(_refreshSource(m.route));
      }
    }
  }

  /// 원격 pane 조회만 실패하면 직전 유효 목록을 유지한다. `/machines`가 online이라
  /// 답한 사실까지 이 한 요청의 실패로 뒤집으면 화면이 5초마다 깜빡인다.
  Future<void> _loadRemote(String route) async {
    final results = await Future.wait<Object?>([
      _orNull(server.panes(machine: route)),
      _layoutsOf(route),
      _loadNotes(route),
    ]);
    if (_disposed) return;
    final panes = results[0] as List<Pane>?;
    if (panes != null) {
      _remotePanes[route] = panes;
      _seeded.remove(route);
    }
    final layouts = results[1] as List<WindowLayout>?;
    if (layouts != null) _remoteLayouts[route] = layouts;
    _compose();
  }

  /// 배치는 곁들이다 — 못 받아도 학생 목록은 그대로 뜨고, 직전 배치를 둔다.
  Future<List<WindowLayout>?> _layoutsOf(String? machine) =>
      _orNull(server.windows(machine: machine));

  Future<T?> _orNull<T>(Future<T> f) async {
    try {
      return await f.timeout(listTimeout);
    } catch (_) {
      return null;
    }
  }

  /// 쪽지도 곁들이다 — 못 받으면 직전 목록을 둔다. 새로 온 쪽지엔 진동 한 번.
  Future<void> _loadNotes(String? machine) async {
    final key = machine ?? _rootKey;
    final List<Note> list;
    try {
      list = await server.notes(machine: machine).timeout(listTimeout);
    } catch (_) {
      return;
    }
    if (_disposed) return;
    final fresh = list.where((n) => !n.read && !_seenNotes.contains(n.key));
    if (_notesPrimed.contains(key) && fresh.isNotEmpty) {
      HapticFeedback.lightImpact();
    }
    _seenNotes.addAll(list.map((n) => n.key));
    _notesPrimed.add(key);
    _notesBy[key] = list;
    _compose();
  }

  void _syncWatchers() {
    if (!_running || _disposed) return;
    final want = <String>{
      _rootKey,
      for (final m in _machines)
        if (m.online) m.route,
    };
    _watch.removeWhere((k, _) => !want.contains(k));
    _watchOk.removeWhere((k) => !want.contains(k));
    for (final key in want) {
      if (_watch.containsKey(key) || _noChanges.contains(key)) continue;
      final token = ++_watchSeq;
      _watch[key] = token;
      unawaited(_watchLoop(key, token));
    }
  }

  Future<void> _watchLoop(String key, int token) async {
    bool alive() => !_disposed && _watch[key] == token;
    int? since;
    var backoff = 0;
    while (alive()) {
      try {
        final c = await server.changes(
          machine: key == _rootKey ? null : key,
          since: since ?? 0,
        );
        if (!alive()) return;
        backoff = 0;
        _watchOk.add(key);
        if (c.status) {
          _statusAware.add(key);
        } else {
          _statusAware.remove(key);
        }
        // 첫 답은 기준 번호다. 서버가 다시 켜져 번호가 줄어도 「다르다」로 받는다.
        if (since != null && c.epoch != since) unawaited(_refreshSource(key));
        since = c.epoch;
      } on ServerException catch (e) {
        if (!alive()) return;
        _watchOk.remove(key);
        if (e.status == 404) {
          // 알림 길이 없는 옛 판 — 폴링만으로 간다. 앱이 돌아오면(start) 다시 물어본다.
          _noChanges.add(key);
          _watch.remove(key);
          return;
        }
        backoff = backoff == 0 ? 2 : (backoff * 2).clamp(2, 30);
        await _sleep(Duration(seconds: backoff));
      }
    }
  }

  /// 멈출 때 깨울 수 있는 잠 — `Future.delayed` 는 못 걷어서 화면을 닫아도 타이머가 남는다.
  Future<void> _sleep(Duration d) {
    final done = Completer<void>();
    late final Timer timer;
    timer = Timer(d, () {
      _sleeps.remove(timer);
      if (!done.isCompleted) done.complete();
    });
    _sleeps[timer] = done;
    return done.future;
  }

  void _compose() {
    final root = _rootPanes;
    final next = <HubSection>[
      if (root != null)
        HubSection(
          machine: null,
          online: true,
          rooms: rooms(root, _rootLabels, _rootLayouts),
        ),
      for (final m in _machines)
        HubSection(
          machine: m.label,
          route: m.route,
          online: m.online,
          rooms: m.online
              ? rooms(
                  _remotePanes[m.route] ?? m.panes,
                  const [],
                  _remoteLayouts[m.route] ?? const [],
                )
              : rooms(m.panes, const []),
        ),
    ];
    _noteWaiting(next);
    sections = next;
    final live = {
      _rootKey,
      for (final m in _machines)
        if (m.online) m.route,
    };
    notes = [
      for (final e in _notesBy.entries)
        if (live.contains(e.key)) ...e.value,
    ]..sort((a, b) => b.when.compareTo(a.when));
    _save();
    if (!_disposed) notifyListeners();
  }

  void _save() {
    _cache[server.root.toString()] = _Snapshot()
      ..rootPanes = _rootPanes
      ..rootLabels = _rootLabels
      ..rootLayouts = _rootLayouts
      ..machines = _machines
      ..remotePanes = Map.of(_remotePanes)
      ..remoteLayouts = Map.of(_remoteLayouts)
      ..notes = Map.of(_notesBy)
      ..rootName = rootName;
  }

  /// 처음 본 학생은 세지 않는다 — 앱을 켠 순간 이미 기다리던 것은 목록 맨 위에
  /// 보이는 것으로 충분하고, 진동은 「방금 바뀐 것」에만 의미가 있다.
  void _noteWaiting(List<HubSection> next) {
    final status = <String, String>{};
    var fresh = 0;
    var now = 0;
    for (final s in next) {
      final seeded = _seeded.contains(s.route ?? _rootKey);
      for (final r in s.rooms) {
        for (final p in r.panes) {
          if (p.isWaiting) now++;
          if (seeded) continue;
          final key = '${s.machine ?? ''}|${p.id}';
          status[key] = p.status;
          final before = _lastStatus[key];
          final wasWaiting = before == 'waiting' || before == 'blocked';
          if (p.isWaiting && before != null && !wasWaiting) fresh++;
        }
      }
    }
    _lastStatus = status;
    waiting = now;
    if (fresh > 0 && !_disposed) HapticFeedback.mediumImpact();
  }

  void _editNotes(List<Note> Function(List<Note>) edit) {
    notes = edit(notes);
    for (final k in _notesBy.keys.toList()) {
      _notesBy[k] = edit(_notesBy[k]!);
    }
    _save();
    if (!_disposed) notifyListeners();
  }

  /// 종 목록을 열어 봤으면 전부 읽음 — 기계마다 따로 표시한다.
  Future<void> markAllRead() async {
    final machines = {
      for (final n in notes)
        if (!n.read) n.machine,
    };
    for (final m in machines) {
      try {
        await server.markNotesRead(all: true, machine: m);
      } catch (_) {}
    }
    _editNotes((l) => [for (final n in l) n.copyWith(read: true)]);
  }

  /// 하나만 읽음 — 눌러서 그 학생 화면으로 갔거나 페이지를 열었을 때.
  Future<void> markRead(Note n) async {
    _editNotes(
      (l) => [for (final x in l) x.key == n.key ? x.copyWith(read: true) : x],
    );
    try {
      await server.markNotesRead(ids: [n.id], machine: n.machine);
    } catch (_) {}
  }

  /// 옆으로 밀어 지움. 목록에서 먼저 빼야 Dismissible 이 빈 자리를 안 찾는다 —
  /// 서버가 늦거나 실패하면 다음 폴링에 되돌아온다.
  Future<void> deleteNote(Note n) async {
    _editNotes(
      (l) => [
        for (final x in l)
          if (x.key != n.key) x,
      ],
    );
    try {
      await server.deleteNotes(ids: [n.id], machine: n.machine);
    } catch (_) {}
  }

  /// 쪽지의 pane 을 지금 목록에서 찾는다 — 닫혔으면 null.
  Pane? paneOfNote(Note n) {
    for (final s in sections) {
      if (s.route != n.machine) continue;
      for (final r in s.rooms) {
        for (final p in r.panes) {
          if (p.id == n.pane) return p;
        }
      }
    }
    return null;
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
      // 되살리기 목록의 pane 은 어느 방에도 없다 — 1번방에 끼워 넣지 않는다.
      if (p.closed) continue;
      // 거울(다른 기기 방의 보기 창)은 몸통이 저쪽이라 저쪽 기기 절에 따로 온다 —
      // 여기 실으면 맥북 학생이 맥미니 절에, 맥미니 학생이 맥북 절에 겹쳐 뜬다(09-17).
      if (p.mirrorOf != null && p.mirrorOf!.isNotEmpty) continue;
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
    _disposed = true;
    stop();
    super.dispose();
  }
}
