import 'dart:io';

import 'package:integration_test/integration_test_driver_extended.dart';

/// `flutter drive` 가 기기에서 찍은 화면을 KASA_SHOT_DIR(기본 build/integration_shots)에 남긴다.
Future<void> main() => integrationDriver(
  onScreenshot: (name, bytes, [args]) async {
    final dir =
        Platform.environment['KASA_SHOT_DIR'] ?? 'build/integration_shots';
    File('$dir/$name.png')
      ..createSync(recursive: true)
      ..writeAsBytesSync(bytes);
    return true;
  },
);
