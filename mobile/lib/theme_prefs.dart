import 'package:flutter/material.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';

/// 폰 자체 밝기. 「데스크톱 따라감」이 기본 — 붙은 기계의 색을 그대로 입는다.
/// 밝게·어둡게를 고르면 데스크톱 색을 접고 폰의 기본 얼굴을 그 밝기로 입는다
/// (데스크톱 팔레트는 한 벌뿐이라 밝기를 뒤집을 수가 없다).
final phoneThemeMode = ValueNotifier<ThemeMode>(ThemeMode.system);

class ThemePrefs {
  const ThemePrefs();

  static const _key = 'theme.mode';
  static const _storage = FlutterSecureStorage();

  Future<ThemeMode> load() async {
    try {
      final v = await _storage.read(key: _key);
      return ThemeMode.values.firstWhere(
        (m) => m.name == v,
        orElse: () => ThemeMode.system,
      );
    } catch (_) {
      return ThemeMode.system;
    }
  }

  Future<void> save(ThemeMode m) async {
    try {
      await _storage.write(key: _key, value: m.name);
    } catch (_) {
      // 저장소가 막혀도 이번 실행은 고른 대로 보인다.
    }
  }
}
