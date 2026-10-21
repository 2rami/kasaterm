import 'dart:typed_data';

import 'package:kasaterm_mobile/inline_media.dart';

/// 위젯 시험용 — 가짜 시계 안에서 파일 입출력을 기다리지 않게 메모리에 둔다.
class MemoryMediaStore implements MediaStore {
  final done = <String, Uint8List>{};
  final meta = <String, (InlineMedia, Uint8List?)>{};

  @override
  Future<String?> finished(InlineMedia m) async =>
      done.containsKey(m.sha256) ? '/cache/${m.sha256}.${m.extension}' : null;

  @override
  Future<void> remember(String key, InlineMedia m, Uint8List? poster) async =>
      meta[key] = (m, poster);

  @override
  Future<(InlineMedia, Uint8List?)?> recall(String key, String id) async =>
      meta[key];
}
