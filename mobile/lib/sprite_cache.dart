import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/painting.dart';
import 'package:flutter/services.dart';
import 'original_assets.dart';

/// 원본 정지 그림을 공유해 같은 캐릭터의 상태별 프레임마다 디코딩하지 않는다.
class SpriteCache extends ChangeNotifier {
  final _images = <String, ui.Image>{};
  final _loading = <String>{};
  final _missing = <String>{};

  /// 로드에 실패한 적이 없으면 true — 첫 프레임은 아직 안 왔어도 자리를 잡아 둔다.
  bool available(String slug, String motion) =>
      originalCharacterAssets.containsKey(slug) && !_missing.contains('original/$slug');

  ui.Image? frame(String slug, String motion, int i) {
    final asset = originalCharacterAssets[slug];
    return asset == null ? null : _get('original/$slug', 'original/$slug', asset);
  }

  /// 상태줄 로고(`assets/icons/<name>.png`, 흰 형상 + 알파) — 그릴 때 색을 입힌다.
  ui.Image? icon(String name) =>
      _get('icon/$name', 'icon/$name', 'assets/icons/$name.png');

  ui.Image? _get(String key, String missKey, String path) {
    final img = _images[key];
    if (img != null) return img;
    if (!_loading.contains(key) && !_missing.contains(missKey)) {
      _loading.add(key);
      _load(key, missKey, path);
    }
    return null;
  }

  Future<void> _load(String key, String missKey, String path) async {
    try {
      final bytes = await rootBundle.load(path);
      _images[key] = await decodeImageFromList(bytes.buffer.asUint8List());
    } catch (_) {
      _missing.add(missKey);
    } finally {
      _loading.remove(key);
    }
    notifyListeners();
  }
}

final spriteCache = SpriteCache();
