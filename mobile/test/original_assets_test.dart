import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/original_assets.dart';
import 'package:kasaterm_mobile/sprite_cache.dart';
import 'package:kasaterm_mobile/student_art.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test('application manifest ships original art without historical student folders', () async {
    final manifest = await AssetManifest.loadFromAssetBundle(rootBundle);
    final assets = manifest.listAssets();
    expect(assets.where((path) => path.startsWith('assets/students/')), isEmpty);
    expect(assets, containsAll(originalCharacterAssets.values));
    expect(assets, contains('assets/original/twins.png'));
  });

  test('original art decodes with transparent corners', () async {
    for (final path in [...originalCharacterAssets.values, 'assets/original/twins.png']) {
      final data = await rootBundle.load(path);
      final codec = await ui.instantiateImageCodec(data.buffer.asUint8List());
      final image = (await codec.getNextFrame()).image;
      expect(image.width, 1024);
      expect(image.height, 1024);
      final pixels = (await image.toByteData(format: ui.ImageByteFormat.rawRgba))!;
      for (final offset in [3, (image.width - 1) * 4 + 3,
        (image.width * (image.height - 1)) * 4 + 3, pixels.lengthInBytes - 1]) {
        expect(pixels.getUint8(offset), 0, reason: path);
      }
      image.dispose();
      codec.dispose();
    }
  });

  test('unknown legacy sprites keep terminal text instead of requesting removed files', () {
    final cache = SpriteCache();
    expect(cache.available('shiroko', 'idle'), isFalse);
    expect(cache.frame('shiroko', 'idle', 0), isNull);
    expect(cache.available('kasa_sky', 'idle'), isTrue);
    cache.dispose();
  });

  testWidgets('unknown portraits show a neutral fallback', (tester) async {
    await tester.pumpWidget(const MaterialApp(home: StudentFace(slug: 'shiroko')));
    await tester.pumpAndSettle();
    expect(find.byIcon(Icons.person_outline), findsOneWidget);
    expect(tester.takeException(), isNull);
  });
}
