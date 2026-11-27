import 'dart:convert';

import 'package:flutter_secure_storage/flutter_secure_storage.dart';

/// 허브를 어떤 모양으로 볼지 — 지도(미니맵)와 목록을 함께, 목록만, 지도만.
enum HubShape { both, list, map }

/// 허브 보기 설정. 기기는 null 이 전체, 빈 문자열이 주소가 가리키는 기계, 그 외는
/// 다른 기계의 이름. 주소와 같은 저장소를 쓴다 — 의존성을 하나 더 들이지 않으려고.
class HubView {
  const HubView({
    this.machine,
    this.shape = HubShape.both,
    this.folded = const {},
    this.hidden = const {},
    this.foldedRooms = const {},
    this.deviceOrder = const [],
    this.roomOrders = const {},
  });

  final String? machine;
  final HubShape shape;

  /// 머리글을 눌러 접어 둔 기계들(주소 기계는 빈 문자열). 기계가 셋이면 방 목록이
  /// 화면 몇 장을 넘어가, 지금 안 보는 기계는 이름만 남긴다(2026-09-08 지시).
  final Set<String> folded;
  final Set<String> hidden;
  final Set<String> foldedRooms;
  final List<String> deviceOrder;
  final Map<String, List<String>> roomOrders;

  bool get allMachines => machine == null;

  bool isFolded(String? machine) => folded.contains(machine ?? '');

  HubView toggleFolded(String? machine) {
    final key = machine ?? '';
    final next = {...folded};
    if (!next.remove(key)) next.add(key);
    return copyWith(folded: next);
  }

  String _roomKey(String device, String room) => jsonEncode([device, room]);

  bool isRoomFolded(String device, String room) =>
      foldedRooms.contains(_roomKey(device, room));

  HubView toggleRoomFolded(String device, String room) {
    final key = _roomKey(device, room);
    final next = {...foldedRooms};
    if (!next.remove(key)) next.add(key);
    return copyWith(foldedRooms: next);
  }

  HubView copyWith({
    String? machine,
    bool clearMachine = false,
    HubShape? shape,
    Set<String>? folded,
    Set<String>? hidden,
    Set<String>? foldedRooms,
    List<String>? deviceOrder,
    Map<String, List<String>>? roomOrders,
  }) => HubView(
    machine: clearMachine ? null : (machine ?? this.machine),
    shape: shape ?? this.shape,
    folded: folded ?? this.folded,
    hidden: hidden ?? this.hidden,
    foldedRooms: foldedRooms ?? this.foldedRooms,
    deviceOrder: deviceOrder ?? this.deviceOrder,
    roomOrders: roomOrders ?? this.roomOrders,
  );
}

class HubPrefs {
  const HubPrefs({this.scope = ''});

  final String scope;
  String _key(String key) => scope.isEmpty ? key : '$key/$scope';

  static const _machineKey = 'hub.machine';
  static const _shapeKey = 'hub.shape';
  static const _foldedKey = 'hub.folded';
  static const _hiddenKey = 'hub.hidden';
  static const _arrangementKey = 'hub.arrangement';
  static const _all = '*';
  static const _storage = FlutterSecureStorage();

  Future<HubView> load() async {
    try {
      final m = await _storage.read(key: _key(_machineKey));
      final s = await _storage.read(key: _shapeKey);
      final f = await _storage.read(key: _key(_foldedKey));
      final h = await _storage.read(key: _key(_hiddenKey));
      final arrangement = _arrangement(
        await _storage.read(key: _key(_arrangementKey)),
      );
      final roomOrders = arrangement['room_orders'];
      return HubView(
        machine: m == null || m == _all ? null : m,
        shape: HubShape.values.firstWhere(
          (v) => v.name == s,
          orElse: () => HubShape.both,
        ),
        folded: decodeFolded(f),
        hidden: decodeHidden(h),
        foldedRooms: _strings(arrangement['folded_rooms']).toSet(),
        deviceOrder: _strings(arrangement['device_order']),
        roomOrders: {
          if (roomOrders is Map)
            for (final entry in roomOrders.entries)
              if (entry.key is String)
                entry.key as String: _strings(entry.value),
        },
      );
    } catch (_) {
      return const HubView();
    }
  }

  Future<void> save(HubView v) async {
    try {
      await _storage.write(key: _key(_machineKey), value: v.machine ?? _all);
      await _storage.write(key: _shapeKey, value: v.shape.name);
      await _storage.write(
        key: _key(_foldedKey),
        value: encodeFolded(v.folded),
      );
      await _storage.write(
        key: _key(_hiddenKey),
        value: encodeHidden(v.hidden),
      );
      await _storage.write(
        key: _key(_arrangementKey),
        value: jsonEncode({
          'folded_rooms': v.foldedRooms.toList(),
          'device_order': v.deviceOrder,
          'room_orders': v.roomOrders,
        }),
      );
    } catch (_) {
      // 저장소가 막혀도 이번 화면은 고른 대로 보인다 — 다음에 다시 고르면 된다.
    }
  }

  static Map<String, dynamic> _arrangement(String? text) {
    try {
      final value = jsonDecode(text ?? '{}');
      return value is Map<String, dynamic> ? value : const {};
    } on FormatException {
      return const {};
    }
  }

  static List<String> _strings(Object? value) =>
      value is List ? value.whereType<String>().toSet().toList() : const [];
}

String encodeHidden(Set<String> keys) =>
    jsonEncode(keys.where((key) => key.startsWith('s:')).take(2000).toList());

Set<String> decodeHidden(String? text) {
  try {
    final value = jsonDecode(text ?? '[]');
    return value is List
        ? value
              .whereType<String>()
              .where((key) => key.startsWith('s:'))
              .take(2000)
              .toSet()
        : {};
  } catch (_) {
    return {};
  }
}

/// 접힌 기계 목록을 한 값으로 — 기계 이름에 줄바꿈은 없고, 주소 기계(빈 이름)도
/// 한 줄로 남아야 하므로 이름마다 줄 하나로 세운다.
String encodeFolded(Set<String> folded) =>
    [for (final m in folded) '$m\n'].join();

Set<String> decodeFolded(String? raw) {
  if (raw == null || raw.isEmpty) return const {};
  final lines = raw.split('\n');
  return {...lines.sublist(0, lines.length - 1)};
}
