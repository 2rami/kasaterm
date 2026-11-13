import 'dart:async';
import 'dart:convert';

import 'package:crypto/crypto.dart';

import 'hub_snapshot_disk.dart';
import 'server.dart';

/// 허브가 마지막으로 본 목록 — 기기 절·방 이름·배치·학생. 데스크톱에 못 닿아도, 앱을 껐다 켜도 이것부터 그린다.
class HubSnapshot {
  HubSnapshot({
    this.rootPanes,
    this.rootLabels = const [],
    this.rootLayouts = const [],
    this.machines = const [],
    this.remotePanes = const {},
    this.remoteLayouts = const {},
    this.notes = const {},
    this.rootName,
    this.seenAt = const {},
  });

  final List<Pane>? rootPanes;
  final List<String> rootLabels;
  final List<WindowLayout> rootLayouts;
  final List<Machine> machines;
  final Map<String, List<Pane>> remotePanes;
  final Map<String, List<WindowLayout>> remoteLayouts;

  /// 쪽지는 살아 있는 기기에서만 보이므로 메모리에만 둔다.
  final Map<String, List<Note>> notes;
  final String? rootName;

  /// 기기마다 마지막으로 새 목록을 받은 때. 주소 기기는 빈 문자열, 나머지는 route.
  final Map<String, DateTime> seenAt;

  static const version = 1;

  bool get isEmpty => rootPanes == null && machines.isEmpty;

  Map<String, Object?> toJson() => {
    'version': version,
    'root_panes': ?rootPanes?.map((p) => p.toJson()).toList(),
    'root_labels': rootLabels,
    'root_layouts': [for (final l in rootLayouts) l.toJson()],
    'machines': [
      for (final m in machines)
        {
          'label': m.label,
          'route': m.route,
          'online': m.online,
          'panes': [for (final p in m.panes) p.toJson()],
          'ago_secs': ?m.agoSecs,
        },
    ],
    'remote_panes': {
      for (final e in remotePanes.entries) e.key: [for (final p in e.value) p.toJson()],
    },
    'remote_layouts': {
      for (final e in remoteLayouts.entries) e.key: [for (final l in e.value) l.toJson()],
    },
    'root_name': ?rootName,
    'seen_at': {for (final e in seenAt.entries) e.key: e.value.toUtc().toIso8601String()},
  };

  /// 모르는 판·깨진 파일은 없는 것으로 — 지난 화면은 꾸밈이라 앱을 막지 않는다.
  static HubSnapshot? fromJson(Object? json) {
    if (json is! Map || json['version'] != version) return null;
    List<Pane> panes(Object? list) => [
      if (list is List)
        for (final p in list)
          if (p is Map) Pane.fromJson(p.cast<String, Object?>(), machine: p['machine'] as String?),
    ];
    List<WindowLayout> layouts(Object? list) => [
      if (list is List)
        for (final l in list)
          if (l is Map) WindowLayout.fromJson(l.cast<String, Object?>()),
    ];
    Map<String, T> byKey<T>(Object? map, T Function(Object?) read) => {
      if (map is Map)
        for (final e in map.entries)
          if (e.key is String) e.key as String: read(e.value),
    };
    final root = json['root_panes'];
    return HubSnapshot(
      rootPanes: root is List ? panes(root) : null,
      rootLabels: [
        for (final l in (json['root_labels'] as List?) ?? const []) l is String ? l : '',
      ],
      rootLayouts: layouts(json['root_layouts']),
      machines: [
        for (final m in (json['machines'] as List?) ?? const [])
          if (m is Map && m['route'] is String)
            Machine(
              label: m['label'] as String? ?? '',
              route: m['route'] as String,
              online: m['online'] == true,
              panes: panes(m['panes']),
              agoSecs: (m['ago_secs'] as num?)?.toInt(),
            ),
      ],
      remotePanes: byKey(json['remote_panes'], panes),
      remoteLayouts: byKey(json['remote_layouts'], layouts),
      rootName: json['root_name'] as String?,
      seenAt: {
        for (final e in byKey(json['seen_at'], (v) => v is String ? DateTime.tryParse(v) : null).entries)
          if (e.value case final at?) e.key: at.toLocal(),
      },
    );
  }
}

/// 연결(계정·주소)마다 하나의 허브 스냅숏. 메모리엔 곧바로 두고 파일은 한 번에 하나씩 쓴다 — 쓰는 동안 몇 번
/// 바뀌었어도 끝난 뒤 마지막 것만 한 번 더 쓴다. 로그아웃하면 [forget] 이 메모리·파일을 함께 지운다.
class HubSnapshots {
  HubSnapshots({HubSnapshotDisk? disk}) : _disk = disk ?? openHubSnapshotDisk();

  HubSnapshots.memory() : _disk = null;

  static HubSnapshots shared = HubSnapshots();

  final HubSnapshotDisk? _disk;
  final Map<String, HubSnapshot> _memory = {};
  final Map<String, String> _written = {};
  final Set<String> _dirty = {};
  bool _writing = false;

  /// 지운 뒤 늦게 끝난 읽기가 지난 계정의 화면을 되살리지 못하게 범위마다 세대를 올린다.
  final Map<String, int> _epoch = {};
  Future<void> _queue = Future.value();

  static String fileKey(String scope) => sha256.convert(utf8.encode(scope)).toString();

  HubSnapshot? peek(String scope) => _memory[scope];

  Future<HubSnapshot?> load(String scope) async {
    if (_memory[scope] case final hit?) return hit;
    final disk = _disk;
    if (disk == null) return null;
    final epoch = _epoch[scope] ?? 0;
    String? text;
    try {
      text = await disk.read(fileKey(scope));
    } catch (_) {
      return null;
    }
    if (text == null || (_epoch[scope] ?? 0) != epoch) return _memory[scope];
    HubSnapshot? snap;
    try {
      snap = HubSnapshot.fromJson(jsonDecode(text));
    } catch (_) {
      snap = null;
    }
    if (snap == null || snap.isEmpty) return _memory[scope];
    _written[scope] = text;
    return _memory[scope] ??= snap;
  }

  void put(String scope, HubSnapshot snap) {
    _memory[scope] = snap;
    if (_disk == null) return;
    _dirty.add(scope);
    unawaited(_drain());
  }

  /// 밀린 쓰기를 끝까지 — 앱이 뒤로 갈 때와 시험.
  Future<void> flush() async {
    await _drain();
    await _queue;
  }

  Future<void> _drain() {
    if (_writing) return _queue;
    _writing = true;
    final run = _queue.catchError((Object _) {}).then((_) async {
      try {
        while (_dirty.isNotEmpty) {
          final scope = _dirty.first;
          _dirty.remove(scope);
          await _write(scope);
        }
      } finally {
        _writing = false;
      }
    });
    _queue = run;
    return run;
  }

  Future<void> _write(String scope) async {
    final snap = _memory[scope];
    if (snap == null) return;
    final text = jsonEncode(snap.toJson());
    if (_written[scope] == text) return;
    try {
      await _disk!.write(fileKey(scope), text);
      _written[scope] = text;
    } catch (_) {
      // 못 쓰면 다음 변화 때 다시 쓴다 — 지난 화면은 꾸밈이다.
    }
  }

  Future<void> forget(String scope) {
    _epoch[scope] = (_epoch[scope] ?? 0) + 1;
    _dirty.remove(scope);
    _memory.remove(scope);
    _written.remove(scope);
    final disk = _disk;
    if (disk == null) return Future.value();
    final next = _queue.catchError((Object _) {}).then((_) async {
      try {
        await disk.delete(fileKey(scope));
      } catch (_) {}
      // 지우는 사이 같은 계정으로 다시 들어와 새로 본 목록은 다시 쓴다.
      _written.remove(scope);
      if (_memory.containsKey(scope)) {
        _dirty.add(scope);
        unawaited(_drain());
      }
    });
    _queue = next;
    return next;
  }

  /// 메모리만 비운다 — 파일은 [forget] 이 지운다.
  void clearMemory() {
    _dirty.clear();
    _memory.clear();
    _written.clear();
  }
}
