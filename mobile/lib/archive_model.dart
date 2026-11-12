import 'dart:convert';

const maxArchiveBytes = 32 << 20;
const maxOfflineBytes = 256 << 20;

class ArchiveException implements Exception {
  const ArchiveException(this.message, {this.code});
  final String message;
  final String? code;
  @override
  String toString() => message;
}

String archiveError(String? code, int status) => switch (code) {
  'archive_changed' => '대화가 갱신됐어요. 다시 저장해 주세요.',
  'archive_missing' => '계정에 보관된 대화를 찾지 못했어요.',
  'archive_limit' => '대화 보관 한도를 넘었어요. 필요 없는 기록을 정리해 주세요.',
  'archive_disabled' => '계정의 대화 동기화를 먼저 켜 주세요.',
  'archive_busy' => '대화 저장소가 바빠요. 잠시 뒤 다시 시도해 주세요.',
  _ when status == 401 || status == 403 => '대화를 읽으려면 다시 로그인해 주세요.',
  _ when status == 404 => '서버를 업데이트해야 대화 보관함을 사용할 수 있어요.',
  _ => '대화를 받지 못했어요. 저장한 대화는 계속 읽을 수 있어요.',
};

class ArchiveEntry {
  const ArchiveEntry({
    required this.id,
    required this.machineId,
    required this.machineName,
    required this.source,
    required this.surfaceKey,
    required this.title,
    required this.name,
    required this.harness,
    required this.cwd,
    required this.generation,
    required this.revision,
    required this.cursor,
    required this.bytes,
    required this.updatedAt,
    required this.ready,
    this.deleted = false,
  });

  final String id,
      machineId,
      machineName,
      source,
      surfaceKey,
      title,
      name,
      harness,
      cwd,
      generation;
  final int revision, cursor, bytes;
  final DateTime updatedAt;
  final bool ready, deleted;
  String get displayTitle => title.isNotEmpty
      ? title
      : name.isNotEmpty
      ? '$name 대화'
      : harness == 'codex'
      ? 'Codex 대화'
      : 'Claude 대화';

  factory ArchiveEntry.fromJson(Object? value) {
    if (value is! Map) throw const ArchiveException('대화 정보를 확인하지 못했어요.');
    String text(String key, int max, {bool empty = true}) {
      final v = value[key];
      if (v is! String ||
          (!empty && v.isEmpty) ||
          utf8.encode(v).length > max ||
          RegExp(r'[\x00-\x1f\x7f]').hasMatch(v)) {
        throw const ArchiveException('대화 정보를 확인하지 못했어요.');
      }
      return v;
    }

    int number(String key, int max) {
      final v = value[key];
      if (v is! int || v < 0 || v > max) {
        throw const ArchiveException('대화 크기를 확인하지 못했어요.');
      }
      return v;
    }

    final id = text('id', 64), generation = text('generation', 32);
    if (!RegExp(r'^[0-9a-f]{64}$').hasMatch(id) ||
        !RegExp(r'^[0-9a-f]{32}$').hasMatch(generation)) {
      throw const ArchiveException('대화 식별자를 확인하지 못했어요.');
    }
    final harness = text('harness', 16);
    if (!const ['claude', 'codex'].contains(harness) ||
        value['ready'] is! bool ||
        (value['deleted'] != null && value['deleted'] is! bool)) {
      throw const ArchiveException('대화 형식을 확인하지 못했어요.');
    }
    return ArchiveEntry(
      id: id,
      machineId: text('machine_id', 128, empty: false),
      machineName: text('machine_name', 256),
      source: text('source', 512, empty: false),
      surfaceKey: text('surface_key', 128),
      title: text('title', 512),
      name: text('name', 128),
      harness: harness,
      cwd: text('cwd', 4096),
      generation: generation,
      revision: number('revision', 1 << 53),
      cursor: number('cursor', 1 << 53),
      bytes: number('bytes', maxArchiveBytes),
      updatedAt: DateTime.fromMillisecondsSinceEpoch(
        number('updated_at', 8640000000000) * 1000,
        isUtc: true,
      ),
      ready: value['ready'] as bool,
      deleted: value['deleted'] == true,
    );
  }

  Map<String, Object> toJson() => {
    'id': id,
    'machine_id': machineId,
    'machine_name': machineName,
    'source': source,
    'surface_key': surfaceKey,
    'title': title,
    'name': name,
    'harness': harness,
    'cwd': cwd,
    'generation': generation,
    'revision': revision,
    'cursor': cursor,
    'bytes': bytes,
    'updated_at': updatedAt.millisecondsSinceEpoch ~/ 1000,
    'ready': ready,
    'deleted': deleted,
  };

  bool sameSnapshot(ArchiveEntry other) =>
      id == other.id &&
      generation == other.generation &&
      bytes == other.bytes &&
      revision == other.revision;

  bool newerThan(ArchiveEntry cached) {
    if (id != cached.id || !ready || deleted || sameSnapshot(cached)) {
      return false;
    }
    if (cached.revision == 0) {
      return cursor > cached.cursor ||
          (cursor == cached.cursor && updatedAt.isAfter(cached.updatedAt));
    }
    return revision > cached.revision;
  }
}

class ArchiveCatalog {
  const ArchiveCatalog({required this.enabled, required this.entries});
  final bool enabled;
  final List<ArchiveEntry> entries;
  factory ArchiveCatalog.fromJson(Map<String, dynamic> json) {
    final rows = json['conversations'];
    if (json['enabled'] is! bool || rows is! List || rows.length > 2048) {
      throw const ArchiveException('대화 목록을 확인하지 못했어요.');
    }
    final entries = rows.map(ArchiveEntry.fromJson).toList();
    if (entries.map((e) => e.id).toSet().length != entries.length) {
      throw const ArchiveException('대화 목록에 중복이 있어요.');
    }
    return ArchiveCatalog(enabled: json['enabled'] as bool, entries: entries);
  }
}

class ArchiveChunk {
  const ArchiveChunk({
    required this.raw,
    required this.next,
    required this.end,
    required this.generation,
  });
  final String raw, generation;
  final int next, end;
  factory ArchiveChunk.fromJson(Object? value) {
    if (value is! Map ||
        value['raw'] is! String ||
        value['next'] is! int ||
        value['end'] is! int ||
        value['generation'] is! String) {
      throw const ArchiveException('대화 조각을 확인하지 못했어요.');
    }
    final result = ArchiveChunk(
      raw: value['raw'] as String,
      next: value['next'] as int,
      end: value['end'] as int,
      generation: value['generation'] as String,
    );
    if (result.next < 0 ||
        result.end < result.next ||
        result.end > maxArchiveBytes ||
        utf8.encode(result.raw).length > 256 << 10) {
      throw const ArchiveException('대화 조각의 크기가 맞지 않아요.');
    }
    return result;
  }
}

class SavedConversation {
  const SavedConversation({
    required this.entry,
    required this.raw,
    required this.savedAt,
  });
  final ArchiveEntry entry;
  final String raw;
  final DateTime savedAt;
}

abstract class ArchiveStore {
  bool get persistent;
  Future<List<ArchiveEntry>> list(String scope);
  Future<SavedConversation?> read(String scope, String id);
  Future<void> save(
    String scope,
    SavedConversation conversation, {
    required bool Function() current,
  });
  Future<void> remove(String scope, String id);
  Future<bool> excluded(String scope, String id);
}

abstract class ArchiveApi {
  Future<ArchiveCatalog> catalog();
  Future<ArchiveChunk> read(ArchiveEntry entry, int offset);
  Future<void> setEnabled(bool enabled);
  Future<void> delete(String id);
  Future<ArchiveEntry> restore(String id);
  void close();
}
