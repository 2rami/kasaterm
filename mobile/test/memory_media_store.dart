import 'dart:typed_data';

import 'package:crypto/crypto.dart';
import 'package:kasaterm_mobile/inline_media.dart';

/// 위젯 시험용 — 가짜 시계 안에서 파일 입출력을 기다리지 않게 메모리에 둔다. 대조 규칙은 앱 저장소와 같다.
class MemoryMediaStore implements MediaStore {
  final done = <String, Uint8List>{};
  final parts = <String, BytesBuilder>{};
  final meta = <String, (InlineMedia, Uint8List?)>{};

  @override
  Future<String?> finished(InlineMedia m) async =>
      done.containsKey(m.sha256) ? '/cache/${m.sha256}.${m.extension}' : null;

  @override
  Future<MediaPart> part(InlineMedia m) async =>
      _Part(this, m, parts.putIfAbsent(m.sha256, BytesBuilder.new));

  @override
  Future<void> remember(String key, InlineMedia m, Uint8List? poster) async =>
      meta[key] = (m, poster);

  @override
  Future<(InlineMedia, Uint8List?)?> recall(String key, String id) async =>
      meta[key];
}

class _Part implements MediaPart {
  _Part(this.store, this.media, this.bytes);
  final MemoryMediaStore store;
  final InlineMedia media;
  final BytesBuilder bytes;

  @override
  int get length => bytes.length;

  @override
  Future<void> add(Uint8List data) async => bytes.add(data);

  @override
  Future<String> finish() async {
    final all = bytes.takeBytes();
    store.parts.remove(media.sha256);
    if (all.length != media.size ||
        sha256.convert(all).toString() != media.sha256) {
      throw const MediaIntegrityException();
    }
    store.done[media.sha256] = all;
    return '/cache/${media.sha256}.${media.extension}';
  }

  @override
  Future<void> discard() async => store.parts.remove(media.sha256);

  @override
  Future<void> close() async {}
}
