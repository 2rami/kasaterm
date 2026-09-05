import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/painting.dart';
import 'package:flutter/services.dart';

/// 학생 도트 프레임(`assets/students/<motion>/<slug>-N.png`) 을 한 번만 디코딩해 든다.
/// 없는 학생·모션은 한 번 실패하면 기억해 두고 다시 묻지 않는다 — 데스크톱이
/// `student_has_sprite` 로 도트 없는 학생의 스피너 글자를 남겨 두는 것과 같은 판단이다.
class SpriteCache extends ChangeNotifier {
  final _frames = <String, ui.Image>{};
  final _loading = <String>{};
  final _missing = <String>{};

  static String _key(String slug, String motion, int i) => '$motion/$slug-$i';

  /// 로드에 실패한 적이 없으면 true — 첫 프레임은 아직 안 왔어도 자리를 잡아 둔다.
  bool available(String slug, String motion) =>
      !_missing.contains('$motion/$slug');

  ui.Image? frame(String slug, String motion, int i) {
    final k = _key(slug, motion, i);
    final img = _frames[k];
    if (img != null) return img;
    if (!_loading.contains(k) && !_missing.contains('$motion/$slug')) {
      _loading.add(k);
      _load(slug, motion, i, k);
    }
    return null;
  }

  Future<void> _load(String slug, String motion, int i, String k) async {
    try {
      final bytes = await rootBundle.load(
        'assets/students/$motion/$slug-$i.png',
      );
      final img = await decodeImageFromList(bytes.buffer.asUint8List());
      _frames[k] = img;
    } catch (_) {
      _missing.add('$motion/$slug');
    } finally {
      _loading.remove(k);
    }
    notifyListeners();
  }
}

final spriteCache = SpriteCache();
