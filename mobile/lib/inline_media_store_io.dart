import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:crypto/crypto.dart';
import 'package:path_provider/path_provider.dart';
import 'package:video_player/video_player.dart';

import 'inline_media.dart';

Future<MediaStore?> openMediaStore() async {
  final base = await getApplicationCacheDirectory();
  return IoMediaStore(Directory('${base.path}/inline-media'));
}

VideoPlayerController mediaController(String path) =>
    VideoPlayerController.file(File(path));

/// 받은 파일은 내용 해시 이름(`<sha256>.<확장자>`)으로 둔다 — 같은 파일을 두 대화가 걸어도 한 번만 받는다.
/// 확장자는 AVPlayer 가 컨테이너를 고르는 단서라 붙인다. 정보·포스터는 `meta/` 에 둬서 원본 기기가
/// 꺼졌거나 지운 뒤에도 옛 대화의 카드가 서고 받아 둔 파일이 재생된다.
class IoMediaStore implements MediaStore {
  IoMediaStore(this.dir, {this.budget = 2 << 30});
  final Directory dir;

  /// 다 받은 파일의 합 상한. 넘으면 오래 안 연 것부터 지운다.
  final int budget;

  File _done(InlineMedia m) => File('${dir.path}/${m.sha256}.${m.extension}');
  File _part(InlineMedia m) => File('${dir.path}/${m.sha256}.part');
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
  Future<MediaPart> part(InlineMedia m) async {
    await dir.create(recursive: true);
    final file = _part(m);
    final digest = _DigestSink();
    final hash = sha256.startChunkedConversion(digest);
    var length = 0;
    if (await file.exists()) {
      length = await file.length();
      if (length > m.size) {
        await file.delete();
        length = 0;
      } else {
        // 이어 받기 — 받아 둔 앞부분도 해시에 넣어야 끝에서 전체를 대조할 수 있다.
        await for (final chunk in file.openRead()) {
          hash.add(chunk);
        }
      }
    }
    final raf = await file.open(mode: FileMode.append);
    return _IoPart(this, m, file, raf, hash, digest, length);
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

  Future<void> _prune(String keep) async {
    final done = <File, FileStat>{};
    final now = DateTime.now();
    await for (final e in dir.list()) {
      if (e is! File) continue;
      final stat = await e.stat();
      if (e.path.endsWith('.part')) {
        // 끝내 안 이어 받은 조각은 이레 뒤에 걷는다.
        if (now.difference(stat.modified).inDays >= 7) await e.delete();
        continue;
      }
      done[e] = stat;
    }
    var total = done.values.fold<int>(0, (a, s) => a + s.size);
    final old = done.keys.toList()
      ..sort((a, b) => done[a]!.modified.compareTo(done[b]!.modified));
    for (final f in old) {
      if (total <= budget) break;
      if (f.path == keep) continue;
      total -= done[f]!.size;
      await f.delete();
    }
  }
}

class _IoPart implements MediaPart {
  _IoPart(
    this._store,
    this._media,
    this._file,
    this._raf,
    this._hash,
    this._digest,
    this.length,
  );
  final IoMediaStore _store;
  final InlineMedia _media;
  final File _file;
  final RandomAccessFile _raf;
  final ByteConversionSink _hash;
  final _DigestSink _digest;
  bool _closed = false;

  @override
  int length;

  @override
  Future<void> add(Uint8List bytes) async {
    await _raf.writeFrom(bytes);
    _hash.add(bytes);
    length += bytes.length;
  }

  @override
  Future<String> finish() async {
    await _close();
    _hash.close();
    if (length != _media.size || _digest.value.toString() != _media.sha256) {
      await _file.delete();
      throw const MediaIntegrityException();
    }
    final target = _store._done(_media);
    await _file.rename(target.path);
    try {
      await _store._prune(target.path);
    } catch (_) {}
    return target.path;
  }

  @override
  Future<void> discard() async {
    await _close();
    if (await _file.exists()) await _file.delete();
  }

  @override
  Future<void> close() => _close();

  Future<void> _close() async {
    if (_closed) return;
    _closed = true;
    await _raf.flush();
    await _raf.close();
  }
}

class _DigestSink implements Sink<Digest> {
  Digest? value;
  @override
  void add(Digest data) => value = data;
  @override
  void close() {}
}
