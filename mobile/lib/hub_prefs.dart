import 'package:flutter_secure_storage/flutter_secure_storage.dart';

/// 허브를 어떤 모양으로 볼지 — 지도(미니맵)와 목록을 함께, 목록만, 지도만.
enum HubShape { both, list, map }

/// 허브 보기 설정. 기기는 null 이 전체, 빈 문자열이 주소가 가리키는 기계, 그 외는
/// 다른 기계의 이름. 주소와 같은 저장소를 쓴다 — 의존성을 하나 더 들이지 않으려고.
class HubView {
  const HubView({this.machine, this.shape = HubShape.both});

  final String? machine;
  final HubShape shape;

  bool get allMachines => machine == null;

  HubView copyWith({
    String? machine,
    bool clearMachine = false,
    HubShape? shape,
  }) => HubView(
    machine: clearMachine ? null : (machine ?? this.machine),
    shape: shape ?? this.shape,
  );
}

class HubPrefs {
  const HubPrefs();

  static const _machineKey = 'hub.machine';
  static const _shapeKey = 'hub.shape';
  static const _all = '*';
  static const _storage = FlutterSecureStorage();

  Future<HubView> load() async {
    try {
      final m = await _storage.read(key: _machineKey);
      final s = await _storage.read(key: _shapeKey);
      return HubView(
        machine: m == null || m == _all ? null : m,
        shape: HubShape.values.firstWhere(
          (v) => v.name == s,
          orElse: () => HubShape.both,
        ),
      );
    } catch (_) {
      return const HubView();
    }
  }

  Future<void> save(HubView v) async {
    try {
      await _storage.write(key: _machineKey, value: v.machine ?? _all);
      await _storage.write(key: _shapeKey, value: v.shape.name);
    } catch (_) {
      // 저장소가 막혀도 이번 화면은 고른 대로 보인다 — 다음에 다시 고르면 된다.
    }
  }
}
