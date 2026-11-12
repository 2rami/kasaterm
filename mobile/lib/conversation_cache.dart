import 'dart:async';

import 'package:flutter/foundation.dart';

import 'archive_model.dart';
import 'archive_repository.dart';
import 'background_grace.dart';
import 'network_watch.dart';
import 'server.dart';

/// The account owns the worker so leaving a conversation does not cancel its save.
class ConversationCache {
  ConversationCache(this.server, {ArchiveRepository? repository})
      : repository = repository ?? ArchiveRepository(server);
  static final _instances = Expando<ConversationCache>();
  static ConversationCache of(Server server) =>
      _instances[server] ??= ConversationCache(server);

  static void bind(Server server, ConversationCache cache) {
    if (server.resumeScope != cache.server.resumeScope || server.account?.token != cache.server.account?.token) {
      throw StateError('conversation cache account mismatch');
    }
    final old = _instances[server];
    if (old != null && !identical(old, cache)) old.dispose();
    _instances[server] = cache;
  }

  final Server server;
  final ArchiveRepository repository;
  final _panes = <String, Pane>{};
  final _last = <String, DateTime>{};
  Timer? _timer;
  bool _running = false, _disposed = false, _started = false;
  DateTime? _catalogAt;
  String? _priority;

  void start() {
    if (_started || _disposed || server.isClosed) return;
    _started = true;
    server.addCloseListener(dispose);
    BackgroundGrace.instance.addListener(_reconnect);
    NetworkWatch.instance.addListener(_reconnect);
    _timer = Timer.periodic(const Duration(seconds: 15), (_) => unawaited(refresh()));
    unawaited(refresh());
  }

  void warm(Iterable<Pane> panes, {bool priority = false}) {
    if (_disposed) return;
    for (final pane in panes) {
      if (pane.closed || pane.isShell || pane.surfaceKey == null) continue;
      _panes.remove(pane.viewKey);
      _panes[pane.viewKey] = pane;
      if (priority) _priority = pane.viewKey;
    }
    while (_panes.length > 20) { _panes.remove(_panes.keys.firstWhere((key) => key != _priority)); }
    // A foreground conversation takes the next slot; in-flight downloads remain atomic.
    if (priority) _catalogAt = DateTime.now();
    unawaited(refresh());
  }

  void _reconnect() {
    if (!BackgroundGrace.instance.live) { repository.cancel(pause: false); return; }
    server.freshConnections();
    repository.freshConnections();
    _last.clear();
    _catalogAt = null;
    unawaited(refresh());
  }

  @visibleForTesting
  Future<void> refresh() async {
    if (_running || _disposed || server.isClosed || !BackgroundGrace.instance.live) return;
    _running = true;
    try {
      final keys = [if (_panes.containsKey(_priority)) _priority!,
        ..._panes.keys.toList().reversed.where((key) => key != _priority)];
      for (final key in keys) {
        if (_disposed || server.isClosed || !BackgroundGrace.instance.live) break;
        final pane = _panes[key];
        if (pane == null) continue;
        final now = DateTime.now();
        if (_last[key] case final at? when now.difference(at) < const Duration(seconds: 30)) continue;
        _last[key] = now;
        try {
          final previous = await repository.openPane(pane);
          if (previous != null && !await repository.autoAllowed(previous.entry.id)) continue;
          if (repository.downloading == null) await repository.savePane(pane, direct: true);
        } on ArchiveException {
          // A missing or disconnected source must not hide its previous snapshot.
        } catch (_) {
          // Platform storage can be unavailable; live conversation polling still works.
        }
      }
      if (!_disposed && !server.isClosed && BackgroundGrace.instance.live &&
          (_catalogAt == null || DateTime.now().difference(_catalogAt!) >= const Duration(minutes: 1))) {
        _catalogAt = DateTime.now();
        await repository.refreshRecent();
      }
    } catch (_) {
      // The next foreground tick retries without an unhandled background future.
    } finally { _running = false; }
  }

  void dispose() {
    if (_disposed) return;
    _disposed = true;
    _timer?.cancel();
    server.removeCloseListener(dispose);
    BackgroundGrace.instance.removeListener(_reconnect);
    NetworkWatch.instance.removeListener(_reconnect);
    repository.dispose();
    _panes.clear();
    _last.clear();
  }
}
