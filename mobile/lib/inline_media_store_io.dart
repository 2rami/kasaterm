import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:path_provider/path_provider.dart';
import 'package:video_player/video_player.dart';

import 'inline_media.dart';

export 'inline_media_bridge_io.dart' show openMediaBridge;

const mediaPlayable = true;

Future<MediaStore?> openMediaStore() async {
  final base = await getApplicationCacheDirectory();
  final store = IoMediaStore(Directory('${base.path}/inline-media'));
  store.sweep().ignore();
  return store;
}

VideoPlayerController mediaController(Uri uri) => uri.scheme == 'file'
    ? VideoPlayerController.file(File.fromUri(uri))
    : VideoPlayerController.networkUrl(uri);

/// 전 판이 다 받아 대조한 파일(`<sha256>.<확장자>`)과 카드 정보·포스터(`meta/`). 새 파일은 더 쌓지 않는다 —
/// 재생은 루프백 창구가 필요한 구간만 받는다. 정보·포스터는 원본 기기가 꺼졌거나 지운 뒤에도 옛 대화의 카드가
/// 서게 하고, 남은 완성본은 그 카드를 원본 없이 재생한다.
class IoMediaStore implements MediaStore {
  IoMediaStore(this.dir, {this.keep = const Duration(days: 30)});
  final Directory dir;

  /// 완성본을 이만큼 안 열면 지운다 — 더 받지 않으니 남은 것은 이렇게만 줄어든다.
  final Duration keep;

  File _done(InlineMedia m) => File('${dir.path}/${m.sha256}.${m.extension}');
  File _meta(String key) => File('${dir.path}/meta/$key.json');
  File _poster(String sha) => File('${dir.path}/meta/$sha.poster');

  @override
  Future<String?> finished(InlineMedia m) async {
    final f = _done(m);
    if (!await f.exists() || await f.length() != m.size) return null;
    try {
      await f.setLastModified(DateTime.now());
    } catch (_) {}
    return f.path;
  }

  @override
  Future<void> remember(String key, InlineMedia m, Uint8List? poster) async {
    final meta = _meta(key);
    await meta.parent.create(recursive: true);
    final tmp = File('${meta.path}.tmp');
    await tmp.writeAsString(jsonEncode(m.toJson()), flush: true);
    await tmp.rename(meta.path);
    if (poster != null) {
      final p = _poster(m.sha256);
      if (!await p.exists()) {
        final ptmp = File('${p.path}.tmp');
        await ptmp.writeAsBytes(poster, flush: true);
        await ptmp.rename(p.path);
      }
    }
  }

  @override
  Future<(InlineMedia, Uint8List?)?> recall(String key, String id) async {
    final meta = _meta(key);
    if (!await meta.exists()) return null;
    final InlineMedia? m;
    try {
      m = InlineMedia.fromJson(jsonDecode(await meta.readAsString()), id: id);
    } catch (_) {
      return null;
    }
    if (m == null) return null;
    final p = _poster(m.sha256);
    return (m, await p.exists() ? await p.readAsBytes() : null);
  }

  /// 전 판이 받다 만 `.part` 는 이을 길이 없어 지우고, [keep] 동안 안 연 완성본도 지운다.
  Future<void> sweep() async {
    if (!await dir.exists()) return;
    final now = DateTime.now();
    await for (final e in dir.list()) {
      if (e is! File) continue;
      if (e.path.endsWith('.part') ||
          now.difference((await e.stat()).modified) >= keep) {
        await e.delete();
      }
    }
  }
}
