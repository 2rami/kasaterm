import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/painting.dart';
import 'package:flutter/services.dart';

/// 학생 도트 프레임(`assets/students/<motion>/<slug>-N.png`)과 상태줄 로고를 한 번만
/// 디코딩해 든다. 없는 학생·모션은 한 번 실패하면 기억해 두고 다시 묻지 않는다 —
/// 데스크톱이 `student_has_sprite` 로 도트 없는 학생의 스피너 글자를 남겨 두는 것과
/// 같은 판단이다.
class SpriteCache extends ChangeNotifier {
  final _images = <String, ui.Image>{};
  final _loading = <String>{};
  final _missing = <String>{};

  /// 로드에 실패한 적이 없으면 true — 첫 프레임은 아직 안 왔어도 자리를 잡아 둔다.
  bool available(String slug, String motion) =>
      !_missing.contains('$motion/$slug');

  ui.Image? frame(String slug, String motion, int i) => _get(
    '$motion/$slug-$i',
    '$motion/$slug',
    'assets/students/$motion/$slug-$i.png',
  );

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
