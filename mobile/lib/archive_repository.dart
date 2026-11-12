import 'dart:async';
import 'dart:convert';

import 'package:crypto/crypto.dart';
import 'package:flutter/foundation.dart';

import 'archive_model.dart';
import 'archive_store.dart';
import 'relay_account.dart';
import 'server.dart';

String _metadata(String value, int budget) {
  final text = StringBuffer();
  for (final rune in value.runes) {
    if (rune < 32 || rune == 127) continue;
    final bytes = rune < 0x80
        ? 1
        : rune < 0x800
        ? 2
        : rune < 0x10000
        ? 3
        : 4;
    if (bytes > budget) break;
    text.writeCharCode(rune);
    budget -= bytes;
  }
  return text.toString();
}

class RelayArchiveApi implements ArchiveApi {
  RelayArchiveApi(AccountSession session)
    : _api = RelayAccountApi(session.origin, session: session);
  final RelayAccountApi _api;
  void freshConnections() => _api.freshConnections();
  @override
  Future<ArchiveCatalog> catalog() => _api.conversations();
  @override
  Future<ArchiveChunk> read(ArchiveEntry entry, int offset) =>
      _api.conversationChunk(entry, offset);
  @override
  Future<void> setEnabled(bool enabled) => _api.setConversationSync(enabled);
  @override
  Future<void> delete(String id) => _api.deleteConversation(id);
  @override
  Future<ArchiveEntry> restore(String id) => _api.restoreConversation(id);
  @override
  void close() => _api.close();
}

class ArchiveRepository extends ChangeNotifier {
  ArchiveRepository(this.server, {ArchiveStore? store, ArchiveApi? api})
    : _store = store,
      _api =
          api ??
          (server.account == null ? null : RelayArchiveApi(server.account!)) {
    server.addCloseListener(_closed);
  }
  final Server server;
  ArchiveStore? _store;
  final ArchiveApi? _api;
  bool _disposed = false;
  bool configuring = false;
  int _settingsEpoch = 0;
  bool _loadingTask = false, _again = false, _againUpdate = false;
  int _job = 0;
  static int _deleteSerial = 0;
  static final Map<String, int> _deleted = {};
  static final Set<String> _autoExcluded = {};
  final Set<String> _paused = {};
  List<ArchiveEntry> saved = const [];
  List<ArchiveEntry> remote = const [];
  bool loading = true, refreshing = false, enabled = false;
  bool available = true;
  String? error, downloading;
  double? progress;
  bool get locked => _disposed || server.isClosed;
  bool get hasAccount => _api != null;
  String get scope => server.resumeScope;
  String _key(String id) => '$scope\u0000$id';
  Future<ArchiveStore> _storage() async => _store ??= await openArchiveStore();
  void _notify() {
    if (!_disposed) notifyListeners();
  }

  void _closed() {
    _job++;
    _api?.close();
    saved = const [];
    remote = const [];
    downloading = null;
    _notify();
  }

  String _message(Object e) {
    if (e is AccountException) {
      if (e.status == 401) {
        server.onUnauthorized?.call();
        server.close();
      }
      return e.message;
    }
    if (e is ArchiveException) return e.message;
    if (e is ServerException) return e.message;
    return '대화를 받지 못했어요. 저장한 대화는 계속 읽을 수 있어요.';
  }

  void freshConnections() {
    if (_api case final RelayArchiveApi api) api.freshConnections();
  }

  void reconnect() {
    freshConnections();
    unawaited(load(updateSaved: true));
  }

  Future<void> load({bool updateSaved = false}) async {
    if (locked) return;
    if (_loadingTask) {
      _again = true;
      _againUpdate |= updateSaved;
      return;
    }
    _loadingTask = true;
    try {
      await _loadOnce(updateSaved: updateSaved);
    } finally {
      _loadingTask = false;
      if (_again && !locked) {
        final update = _againUpdate;
        _again = false;
        _againUpdate = false;
        unawaited(load(updateSaved: update));
      }
    }
  }

  Future<void> _loadOnce({required bool updateSaved}) async {
    try {
      final store = await _storage();
      final rows = await store.list(scope);
      if (locked) return;
      available = store.persistent;
      saved = rows;
      error = null;
    } catch (e) {
      if (!locked) error = _message(e);
    }
    loading = false;
    _notify();
    if (_api == null || locked) return;
    refreshing = true;
    _notify();
    try {
      final settingsEpoch = _settingsEpoch;
      final catalog = await _api.catalog();
      if (locked) return;
      if (settingsEpoch == _settingsEpoch && !configuring) {
        enabled = catalog.enabled;
      }
      remote = catalog.entries;
      error = null;
      if (updateSaved && downloading == null) {
        for (final local in List<ArchiveEntry>.of(saved)) {
          final newer = remote
              .where((e) => e.id == local.id && e.ready && !e.deleted)
              .firstOrNull;
          if (newer != null && newer.newerThan(local) && !_paused.contains(local.id) && !locked) {
            await save(newer);
          }
        }
      }
    } catch (e) {
      if (!locked) error = _message(e);
    } finally {
      refreshing = false;
      _notify();
    }
  }

  Future<void> setEnabled(bool value) async {
    if (locked || _api == null || configuring) return;
    configuring = true;
    _settingsEpoch++;
    _notify();
    try {
      await _api.setEnabled(value);
      if (!locked) {
        enabled = value;
        error = null;
        _notify();
      }
    } catch (e) {
      if (!locked) {
        error = _message(e);
        _notify();
      }
    } finally {
      configuring = false;
      _settingsEpoch++;
      _notify();
    }
  }

  Future<SavedConversation?> open(String id) async {
    if (locked) throw const ArchiveException('현재 계정의 연결이 종료됐어요.');
    final result = await (await _storage()).read(scope, id);
    if (locked) throw const ArchiveException('현재 계정의 연결이 종료됐어요.');
    return result;
  }

  Future<SavedConversation?> openPane(Pane pane, {String source = ''}) async {
    if (locked || pane.surfaceKey == null) return null;
    final rows = await (await _storage()).list(scope);
    final matches = rows.where((e) => e.surfaceKey == pane.surfaceKey &&
        (source.isEmpty || e.source == source) &&
        (!(pane.machine?.startsWith('~') ?? false) ||
            (pane.mirrorOf?.isNotEmpty ?? false) || e.machineId == pane.machine!.substring(1))).toList()
      ..sort((a, b) => b.updatedAt.compareTo(a.updatedAt));
    return matches.isEmpty ? null : open(matches.first.id);
  }

  Future<bool> autoAllowed(String id) async => !_autoExcluded.contains(_key(id)) && !_paused.contains(id)
      && !await (await _storage()).excluded(scope, id);

  Future<void> refreshRecent() async {
    await load(updateSaved: true);
    if (locked || downloading != null) return;
    final recent = remote.where((e) => e.ready && !e.deleted).toList()
      ..sort((a, b) => b.updatedAt.compareTo(a.updatedAt));
    var bytes = 0;
    for (final entry in recent.take(20)) {
      if (!await autoAllowed(entry.id)) continue;
      bytes += entry.bytes;
      if (bytes > 128 << 20 || locked) break;
      final local = saved.where((e) => e.id == entry.id).firstOrNull;
      if (local == null || entry.newerThan(local)) {
        try { await save(entry); } on ArchiveException { /* Keep completed snapshots on network failure. */ }
      }
    }
  }

  void cancel({bool pause = true}) {
    if (downloading case final String id when pause && RegExp(r'^[0-9a-f]{64}$').hasMatch(id)) _paused.add(id);
    _job++;
    downloading = null;
    progress = null;
    _notify();
  }

  Future<void> remove(String id, {bool fromAccount = false}) async {
    if (locked) return;
    _deleted[_key(id)] = ++_deleteSerial;
    _autoExcluded.add(_key(id));
    if (downloading == id) cancel();
    if (fromAccount && _api != null) await _api.delete(id);
    await (await _storage()).remove(scope, id);
    if (!locked) {
      saved = await (await _storage()).list(scope);
      _notify();
      unawaited(load());
    }
  }

  Future<void> restore(String id) async {
    if (locked || _api == null) return;
    await _api.restore(id);
    await load();
  }

  Future<SavedConversation> save(ArchiveEntry entry) {
    _paused.remove(entry.id);
    _autoExcluded.remove(_key(entry.id));
    return _saving(entry.id, (current) => _download(entry, current));
  }

  Future<SavedConversation> savePane(
    Pane pane, {
    String source = '',
    bool direct = false,
  }) => _saving('pane:${pane.id}', (current) async {
    if (_api != null && !direct) {
      try {
        final catalog = await _api.catalog();
        final matches =
            catalog.entries
                .where(
                  (e) =>
                      !e.deleted &&
                      e.ready &&
                      (source.isNotEmpty
                          ? e.source == source
                          : pane.surfaceKey != null &&
                                e.surfaceKey == pane.surfaceKey) &&
                      ((pane.mirrorOf?.isNotEmpty ?? false)
                          ? pane.surfaceKey != null &&
                                e.surfaceKey == pane.surfaceKey
                          : pane.machine?.startsWith('~') == true
                          ? e.machineId == pane.machine!.substring(1)
                          : pane.surfaceKey != null &&
                                e.surfaceKey == pane.surfaceKey),
                )
                .toList()
              ..sort((a, b) => b.updatedAt.compareTo(a.updatedAt));
        if (matches.length == 1 ||
            (matches.isNotEmpty &&
                pane.surfaceKey != null &&
                matches.first.surfaceKey == pane.surfaceKey)) {
          return _download(matches.first, current);
        }
      } on AccountException catch (e) {
        if (e.status == 401) rethrow;
      }
    }
    if (source.isEmpty && pane.surfaceKey == null) {
      throw const ArchiveException('대화를 먼저 불러온 뒤 저장해 주세요.');
    }
    final previous = await openPane(pane, source: source);
    var cursor = previous?.entry.cursor ?? 0;
    var held = source.isNotEmpty ? source : previous?.entry.source ?? '';
    String? machine = previous?.entry.machineId, key = previous?.entry.surfaceKey, id;
    var machineName = pane.machine?.startsWith('~') == true
        ? '연결된 기기'
        : pane.machine ?? '이 기기';
    var size = previous?.entry.bytes ?? 0;
    final raw = StringBuffer(previous?.raw ?? '');
    var restarted = false;
    final deadline = DateTime.now().add(const Duration(minutes: 5));
    while (true) {
      if (!current(id)) throw const ArchiveException('저장을 취소했어요.');
      if (DateTime.now().isAfter(deadline)) {
        throw const ArchiveException('받기가 오래 걸리고 있어요. 연결이 안정되면 다시 저장해 주세요.');
      }
      final chunk = await server.conversationArchive(pane, cursor, held);
      final address = chunk['address'];
      final text = chunk['raw'],
          next = chunk['offset'],
          origin = chunk['source'];
      if (cursor != 0 && chunk['reset'] == true && !restarted) {
        restarted = true;
        cursor = 0;
        held = source;
        size = 0;
        raw.clear();
        continue;
      }
      if (address is! Map ||
          text is! String ||
          next is! int ||
          next < cursor ||
          origin is! String ||
          origin.isEmpty ||
          address['machine_id'] is! String ||
          address['surface_key'] is! String ||
          address['surface_id'] != pane.id ||
          (cursor != 0 && chunk['reset'] == true)) {
        throw const ArchiveException('저장 도중 대화가 바뀌었어요. 다시 저장해 주세요.');
      }
      if ((held.isNotEmpty && held != origin) ||
          (key != null && key != address['surface_key']) ||
          (pane.surfaceKey != null &&
              pane.surfaceKey != address['surface_key']) ||
          (machine != null && machine != address['machine_id'])) {
        throw const ArchiveException('저장 도중 대화가 바뀌었어요. 다시 저장해 주세요.');
      }
      held = origin;
      machine = address['machine_id'] as String;
      if (chunk['machine_name'] case final String label when label.isNotEmpty) {
        machineName = label;
      }
      key = address['surface_key'] as String;
      id = sha256.convert(utf8.encode('$machine\u0000$held')).toString();
      if (direct && !await autoAllowed(id)) throw const ArchiveException('자동 저장에서 제외한 대화예요.');
      final bytes = utf8.encode(text).length;
      if (bytes > 4 << 20 ||
          size + bytes > maxArchiveBytes ||
          (next == cursor && text.isNotEmpty)) {
        throw const ArchiveException('대화가 오프라인 저장 한도를 넘었어요.');
      }
      raw.write(text);
      size += bytes;
      if (next == cursor) {
        if (!restarted && previous != null && previous.entry.id == id &&
            previous.entry.cursor == cursor && previous.entry.bytes == size) {
          return previous;
        }
        break;
      }
      cursor = next;
    }
    final entry = ArchiveEntry(
      id: id,
      machineId: machine,
      machineName: _metadata(machineName, 256),
      source: held,
      surfaceKey: key,
      title: _metadata(
        pane.title.isNotEmpty ? pane.title : pane.session ?? '',
        512,
      ),
      name: _metadata(pane.name, 128),
      harness: pane.harness == 'codex' ? 'codex' : 'claude',
      cwd: _metadata(pane.cwd, 4096),
      generation: sha256
          .convert(utf8.encode('direct\u0000$held\u0000$key'))
          .toString()
          .substring(0, 32),
      revision: 0,
      cursor: cursor,
      bytes: size,
      updatedAt: DateTime.now().toUtc(),
      ready: true,
    );
    return _commit(entry, raw.toString(), current);
  });

  Future<SavedConversation> _saving(
    String id,
    Future<SavedConversation> Function(bool Function(String?) current) action,
  ) async {
    if (locked) throw const ArchiveException('현재 계정의 연결이 종료됐어요.');
    if (downloading != null) throw const ArchiveException('다른 대화를 저장 중이에요.');
    final epoch = ++_job, deletion = _deleteSerial;
    bool current(String? key) =>
        !locked &&
        _job == epoch &&
        (key == null || (_deleted[_key(key)] ?? 0) <= deletion);
    downloading = id;
    progress = null;
    error = null;
    _notify();
    try {
      final result = await action(current);
      if (!current(result.entry.id)) throw const ArchiveException('저장을 취소했어요.');
      saved = await (await _storage()).list(scope);
      return result;
    } catch (e) {
      final message = _message(e);
      if (current(null)) error = message;
      throw ArchiveException(message);
    } finally {
      if (_job == epoch) {
        downloading = null;
        progress = null;
      }
      _notify();
    }
  }

  Future<SavedConversation> _download(
    ArchiveEntry entry,
    bool Function(String?) current,
  ) async {
    if (_api == null || !entry.ready || entry.deleted) {
      throw const ArchiveException('대화 동기화가 끝나면 저장할 수 있어요.');
    }
    SavedConversation? previous;
    try {
      previous = await (await _storage()).read(scope, entry.id);
    } on ArchiveException {
      previous = null;
    }
    final raw = StringBuffer();
    var offset = 0;
    if (previous != null &&
        previous.entry.generation == entry.generation &&
        previous.entry.bytes <= entry.bytes) {
      raw.write(previous.raw);
      offset = previous.entry.bytes;
    }
    while (offset < entry.bytes) {
      if (!current(entry.id)) throw const ArchiveException('저장을 취소했어요.');
      final chunk = await _api.read(entry, offset);
      if (chunk.generation != entry.generation ||
          chunk.end != entry.bytes ||
          chunk.next <= offset ||
          chunk.next - offset != utf8.encode(chunk.raw).length) {
        throw const ArchiveException('대화 조각이 달라졌어요. 다시 저장해 주세요.');
      }
      raw.write(chunk.raw);
      offset = chunk.next;
      if (current(entry.id)) {
        progress = offset / entry.bytes;
        _notify();
      }
    }
    return _commit(entry, raw.toString(), current);
  }

  Future<SavedConversation> _commit(
    ArchiveEntry entry,
    String raw,
    bool Function(String?) current,
  ) async {
    if (!current(entry.id)) throw const ArchiveException('저장을 취소했어요.');
    try {
      if (raw.isNotEmpty && !raw.endsWith('\n')) throw const FormatException();
      for (final line in const LineSplitter().convert(raw)) {
        if (line.trim().isNotEmpty && jsonDecode(line) is! Map) {
          throw const FormatException();
        }
      }
    } catch (_) {
      throw const ArchiveException('대화 기록의 일부가 빠졌어요. 다시 받아 주세요.');
    }
    final saved = SavedConversation(
      entry: entry,
      raw: raw,
      savedAt: DateTime.now().toUtc(),
    );
    await (await _storage()).save(
      scope,
      saved,
      current: () => current(entry.id),
    );
    return saved;
  }

  @override
  void dispose() {
    server.removeCloseListener(_closed);
    _disposed = true;
    _job++;
    _api?.close();
    super.dispose();
  }
}
