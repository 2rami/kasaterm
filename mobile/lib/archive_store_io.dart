import 'dart:convert';
import 'dart:io';
import 'dart:math';

import 'package:crypto/crypto.dart';
import 'package:path_provider/path_provider.dart';

import 'archive_model.dart';

Future<ArchiveStore>? _opened;
Future<ArchiveStore> openArchiveStore() => _opened ??= _open();
Future<ArchiveStore> _open() async {
  try {
    final store = IoArchiveStore(
      Directory(
        '${(await getApplicationSupportDirectory()).path}/offline-conversations',
      ),
    );
    await store.recover();
    return store;
  } catch (_) {
    _opened = null;
    rethrow;
  }
}

class IoArchiveStore implements ArchiveStore {
  IoArchiveStore(this.root);
  final Directory root;
  Future<void> _writes = Future.value();
  @override
  bool get persistent => true;

  Future<void> recover() async {
    final type = await FileSystemEntity.type(root.path, followLinks: false);
    if (type == FileSystemEntityType.notFound) return;
    if (type != FileSystemEntityType.directory) {
      throw const ArchiveException('대화 저장 폴더를 열지 못했어요.');
    }
    await for (final entity in root.list(followLinks: false)) {
      if (entity is! Directory ||
          !RegExp(
            r'^[0-9a-f]{64}$',
          ).hasMatch(entity.uri.pathSegments.where((s) => s.isNotEmpty).last)) {
        continue;
      }
      final live = <String>{}, uncertain = <String>{};
      final files = await entity.list(followLinks: false).toList();
      for (final file in files) {
        final name = file.uri.pathSegments.last;
        if (!RegExp(r'^[0-9a-f]{64}\.json$').hasMatch(name)) continue;
        final id = name.substring(0, 64);
        try {
          final meta = await _manifest(entity, id);
          if (meta != null) live.add('$id-${meta['sha256']}.jsonl');
        } catch (_) {
          uncertain.add(id);
        }
      }
      for (final file in files.whereType<File>()) {
        final name = file.uri.pathSegments.last;
        if (RegExp(r'^[0-9a-f]{64}.*\.part$').hasMatch(name) ||
            (RegExp(r'^[0-9a-f]{64}-[0-9a-f]{64}\.jsonl$').hasMatch(name) &&
                !live.contains(name) &&
                !uncertain.contains(name.substring(0, 64)))) {
          await file.delete();
        }
      }
    }
  }

  String _id(String id) {
    if (!RegExp(r'^[0-9a-f]{64}$').hasMatch(id)) {
      throw const ArchiveException('대화 식별자가 맞지 않아요.');
    }
    return id;
  }

  Future<Directory> _dir(String scope) async {
    if (scope.isEmpty) throw const ArchiveException('대화의 계정을 확인하지 못했어요.');
    final dir = Directory('${root.path}/${sha256.convert(utf8.encode(scope))}');
    for (final path in [root.path, dir.path]) {
      final type = await FileSystemEntity.type(path, followLinks: false);
      if (type != FileSystemEntityType.notFound &&
          type != FileSystemEntityType.directory) {
        throw const ArchiveException('대화 저장 폴더를 열지 못했어요.');
      }
    }
    await dir.create(recursive: true);
    return dir;
  }

  Future<Map<String, dynamic>?> _manifest(Directory dir, String id) async {
    final path = '${dir.path}/${_id(id)}.json';
    final type = await FileSystemEntity.type(path, followLinks: false);
    if (type == FileSystemEntityType.notFound) return null;
    final file = File(path);
    if (type != FileSystemEntityType.file || await file.length() > 32 << 10) {
      throw const ArchiveException('저장한 대화 정보를 읽지 못했어요.');
    }
    try {
      final value = jsonDecode(await file.readAsString());
      if (value is! Map<String, dynamic> ||
          value['version'] != 1 ||
          value['sha256'] is! String ||
          !RegExp(r'^[0-9a-f]{64}$').hasMatch(value['sha256'] as String)) {
        throw const FormatException();
      }
      final entry = ArchiveEntry.fromJson(value['entry']);
      if (entry.id != id ||
          !entry.ready ||
          entry.deleted ||
          value['saved_at'] is! int) {
        throw const FormatException();
      }
      return value;
    } catch (_) {
      throw const ArchiveException('저장한 대화 정보가 손상됐어요. 다시 받아 주세요.');
    }
  }

  @override
  Future<List<ArchiveEntry>> list(String scope) async {
    final dir = await _dir(scope);
    final entries = <ArchiveEntry>[];
    await for (final file in dir.list(followLinks: false)) {
      final name = file.uri.pathSegments.last;
      if (!RegExp(r'^[0-9a-f]{64}\.json$').hasMatch(name)) continue;
      try {
        final value = await _manifest(dir, name.substring(0, 64));
        if (value != null) entries.add(ArchiveEntry.fromJson(value['entry']));
      } on ArchiveException {
        continue;
      }
      if (entries.length > 2048) {
        throw const ArchiveException('저장한 대화가 너무 많아요.');
      }
    }
    entries.sort((a, b) => b.updatedAt.compareTo(a.updatedAt));
    return entries;
  }

  @override
  Future<SavedConversation?> read(String scope, String id) async {
    _id(id);
    final dir = await _dir(scope);
    for (var attempt = 0; attempt < 2; attempt++) {
      final meta = await _manifest(dir, id);
      if (meta == null) return null;
      final entry = ArchiveEntry.fromJson(meta['entry']);
      final file = File('${dir.path}/$id-${meta['sha256']}.jsonl');
      try {
        if (await FileSystemEntity.type(file.path, followLinks: false) !=
                FileSystemEntityType.file ||
            await file.length() != entry.bytes) {
          throw const FormatException();
        }
        if ((await sha256.bind(file.openRead()).first).toString() !=
            meta['sha256']) {
          throw const FormatException();
        }
        final raw = await file.readAsString();
        return SavedConversation(
          entry: entry,
          raw: raw,
          savedAt: DateTime.fromMillisecondsSinceEpoch(
            meta['saved_at'] as int,
            isUtc: true,
          ),
        );
      } catch (_) {
        if (attempt == 1) {
          throw const ArchiveException('저장한 대화가 손상됐어요. 연결 후 다시 받아 주세요.');
        }
      }
    }
    return null;
  }

  Future<void> _write(Future<void> Function() operation) {
    final next = _writes.catchError((Object _) {}).then((_) => operation());
    _writes = next;
    return next;
  }

  @override
  Future<void> save(
    String scope,
    SavedConversation conversation, {
    required bool Function() current,
  }) => _write(() async {
    final entry = conversation.entry;
    ArchiveEntry.fromJson(entry.toJson());
    if (!entry.ready || entry.deleted || !current()) {
      throw const ArchiveException('저장을 취소했어요.');
    }
    final bytes = utf8.encode(conversation.raw);
    if (bytes.length != entry.bytes || bytes.length > maxArchiveBytes) {
      throw const ArchiveException('대화를 끝까지 받지 못했어요.');
    }
    final dir = await _dir(scope);
    Map<String, dynamic>? old;
    try {
      old = await _manifest(dir, entry.id);
    } on ArchiveException {
      old = null;
    }
    final used = (await list(
      scope,
    )).where((e) => e.id != entry.id).fold<int>(0, (sum, e) => sum + e.bytes);
    if (used + bytes.length > maxOfflineBytes) {
      throw const ArchiveException('오프라인 저장 공간이 가득 찼어요. 필요 없는 대화를 지워 주세요.');
    }
    final hash = sha256.convert(bytes).toString();
    final target = File('${dir.path}/${entry.id}-$hash.jsonl');
    final hadTarget = await target.exists();
    final salt =
        '${DateTime.now().microsecondsSinceEpoch}-${Random.secure().nextInt(1 << 32)}';
    final part = File('${target.path}.$salt.part');
    final manifest = File('${dir.path}/${entry.id}.json');
    final pending = File('${manifest.path}.$salt.part');
    try {
      await part.writeAsBytes(bytes, flush: true);
      if (!current()) throw const ArchiveException('저장을 취소했어요.');
      await part.rename(target.path);
      await pending.writeAsString(
        jsonEncode({
          'version': 1,
          'entry': entry.toJson(),
          'sha256': hash,
          'saved_at': conversation.savedAt.millisecondsSinceEpoch,
        }),
        flush: true,
      );
      if (!current()) throw const ArchiveException('저장을 취소했어요.');
      await pending.rename(manifest.path);
      final marker = File('${dir.path}/${entry.id}.excluded');
      if (await FileSystemEntity.type(marker.path, followLinks:false) == FileSystemEntityType.file) await marker.delete();
      if (old != null && old['sha256'] != hash) {
        final previous = File('${dir.path}/${entry.id}-${old['sha256']}.jsonl');
        if (await previous.exists()) await previous.delete();
      }
    } finally {
      for (final file in [part, pending]) {
        if (await file.exists()) await file.delete();
      }
      try {
        final committed = await _manifest(dir, entry.id);
        if (!hadTarget && committed?['sha256'] != hash && await target.exists()) {
          await target.delete();
        }
      } on ArchiveException {
        /* Keep bytes when the previous manifest needs repair. */
      }
    }
  });

  @override
  Future<bool> excluded(String scope, String id) async =>
      await FileSystemEntity.type('${(await _dir(scope)).path}/${_id(id)}.excluded', followLinks: false)
          != FileSystemEntityType.notFound;

  @override
  Future<void> remove(String scope, String id) => _write(() async {
    _id(id);
    final dir = await _dir(scope);
    final marker = File('${dir.path}/$id.${DateTime.now().microsecondsSinceEpoch}.part');
    await marker.writeAsString('', flush:true);
    await marker.rename('${dir.path}/$id.excluded');
    final manifest = File('${dir.path}/${_id(id)}.json');
    if (await manifest.exists()) await manifest.delete();
    await for (final body in dir.list(followLinks: false)) {
      final name = body.uri.pathSegments.last;
      if (body is File &&
          name.startsWith('$id-') &&
          RegExp(r'^[0-9a-f]{64}-[0-9a-f]{64}\.jsonl$').hasMatch(name)) {
        await body.delete();
      }
    }
  });
}
