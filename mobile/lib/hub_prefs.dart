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
  });

  final String? machine;
  final HubShape shape;

  /// 머리글을 눌러 접어 둔 기계들(주소 기계는 빈 문자열). 기계가 셋이면 방 목록이
  /// 화면 몇 장을 넘어가, 지금 안 보는 기계는 이름만 남긴다(2026-09-08 지시).
  final Set<String> folded;

  bool get allMachines => machine == null;

  bool isFolded(String? machine) => folded.contains(machine ?? '');

  HubView toggleFolded(String? machine) {
    final key = machine ?? '';
    final next = {...folded};
    if (!next.remove(key)) next.add(key);
    return copyWith(folded: next);
  }

  HubView copyWith({
    String? machine,
    bool clearMachine = false,
    HubShape? shape,
    Set<String>? folded,
  }) => HubView(
    machine: clearMachine ? null : (machine ?? this.machine),
    shape: shape ?? this.shape,
    folded: folded ?? this.folded,
  );
}

class HubPrefs {
  const HubPrefs();

  static const _machineKey = 'hub.machine';
  static const _shapeKey = 'hub.shape';
  static const _foldedKey = 'hub.folded';
  static const _all = '*';
  static const _storage = FlutterSecureStorage();

  Future<HubView> load() async {
    try {
      final m = await _storage.read(key: _machineKey);
      final s = await _storage.read(key: _shapeKey);
      final f = await _storage.read(key: _foldedKey);
      return HubView(
        machine: m == null || m == _all ? null : m,
        shape: HubShape.values.firstWhere(
          (v) => v.name == s,
          orElse: () => HubShape.both,
        ),
        folded: decodeFolded(f),
      );
    } catch (_) {
      return const HubView();
    }
  }

  Future<void> save(HubView v) async {
    try {
      await _storage.write(key: _machineKey, value: v.machine ?? _all);
      await _storage.write(key: _shapeKey, value: v.shape.name);
      await _storage.write(key: _foldedKey, value: encodeFolded(v.folded));
    } catch (_) {
      // 저장소가 막혀도 이번 화면은 고른 대로 보인다 — 다음에 다시 고르면 된다.
    }
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
