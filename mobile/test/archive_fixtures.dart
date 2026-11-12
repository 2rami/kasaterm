import 'dart:async';
import 'dart:convert';
import 'dart:math';

import 'package:kasaterm_mobile/archive_model.dart';
import 'package:kasaterm_mobile/relay_account.dart';

String repeated(String s, int n) => List.filled(n, s).join();
String message(String type, String text) =>
    '${jsonEncode({
      'type': type,
      'sessionId': 'fixture-session',
      'message': {'role': type, 'content': text},
    })}\n';

ArchiveEntry entryFor(
  String raw, {
  int revision = 1,
  String? generation,
  String source = 'fixture-session.jsonl',
  String key = 'fixture-key',
  String title = '연결이 끊겨도 읽는 대화',
}) => ArchiveEntry(
  id: repeated('a', 64),
  machineId: 'machine-a',
  machineName: '작업 기기',
  source: source,
  surfaceKey: key,
  title: title,
  name: 'Assistant',
  harness: 'claude',
  cwd: '/workspace/project',
  generation: generation ?? repeated('b', 32),
  revision: revision,
  cursor: revision * 1000,
  bytes: utf8.encode(raw).length,
  updatedAt: DateTime.utc(2026, 11, 4, 2, revision),
  ready: true,
);

AccountSession archiveSession([String name = 'alice']) => AccountSession(
  origin: Uri.parse('https://archive.invalid'),
  account: name,
  deviceId: 'phone-fixture',
  token: 'fixture-device-token',
);

class FakeArchiveApi implements ArchiveApi {
  FakeArchiveApi(this.entry, this.raw);
  ArchiveEntry entry;
  String raw;
  bool failCatalog = false, failRead = false, closed = false, enabled = true;
  int chunkSize = 1 << 20;
  Completer<void>? readGate;
  final requested = <int>[];
  final started = Completer<void>();
  ArchiveChunk Function(ArchiveChunk)? alter;
  @override
  Future<ArchiveCatalog> catalog() async {
    if (failCatalog || closed) throw const ArchiveException('연결이 끊겼어요.');
    return ArchiveCatalog(enabled: enabled, entries: [entry]);
  }

  @override
  Future<ArchiveChunk> read(ArchiveEntry snapshot, int offset) async {
    requested.add(offset);
    if (!started.isCompleted) started.complete();
    if (readGate != null) await readGate!.future;
    if (failRead || closed) throw const ArchiveException('연결이 끊겼어요.');
    final bytes = utf8.encode(raw);
    var end = min(offset + chunkSize, snapshot.bytes);
    while (end < bytes.length && end > offset && (bytes[end] & 0xc0) == 0x80) {
      end--;
    }
    final chunk = ArchiveChunk(
      raw: utf8.decode(bytes.sublist(offset, end)),
      next: end,
      end: snapshot.bytes,
      generation: snapshot.generation,
    );
    return alter?.call(chunk) ?? chunk;
  }

  @override
  Future<void> setEnabled(bool value) async {
    enabled = value;
  }

  @override
  Future<void> delete(String id) async {}
  @override
  Future<ArchiveEntry> restore(String id) async => entry;
  @override
  void close() {
    closed = true;
  }
}

class MemoryArchiveStore implements ArchiveStore {
  final exclusions = <String>{};
  @override
  Future<bool> excluded(String scope, String id) async => exclusions.contains('$scope|$id');
  final values = <String, Map<String, SavedConversation>>{};
  @override
  bool get persistent => true;
  @override
  Future<List<ArchiveEntry>> list(String scope) async =>
      values[scope]?.values.map((s) => s.entry).toList() ?? [];
  @override
  Future<SavedConversation?> read(String scope, String id) async =>
      values[scope]?[id];
  @override
  Future<void> save(
    String scope,
    SavedConversation conversation, {
    required bool Function() current,
  }) async {
    if (!current()) throw const ArchiveException('저장을 취소했어요.');
    values.putIfAbsent(scope, () => {})[conversation.entry.id] = conversation;
    exclusions.remove('$scope|${conversation.entry.id}');
  }

  @override
  Future<void> remove(String scope, String id) async {
    values[scope]?.remove(id);
    exclusions.add('$scope|$id');
  }
}
