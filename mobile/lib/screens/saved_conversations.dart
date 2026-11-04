import 'dart:async';
import 'dart:convert';

import 'package:crypto/crypto.dart';
import 'package:flutter/material.dart';

import '../archive_model.dart';
import '../archive_repository.dart';
import '../background_grace.dart';
import '../conversation.dart';
import '../look.dart';
import '../network_watch.dart';
import '../server.dart';
import '../term_session.dart';
import '../twins_loading.dart';
import 'controls.dart';
import 'conversation_view.dart';

final Map<String, int> _openScopes = {};
String _scopeKey(String scope) => sha256.convert(utf8.encode(scope)).toString();
bool archiveRouteOpenFor(String scope) =>
    (_openScopes[_scopeKey(scope)] ?? 0) > 0;
void _entered(String scope) =>
    _openScopes.update(_scopeKey(scope), (n) => n + 1, ifAbsent: () => 1);
void _left(String scope) {
  final key = _scopeKey(scope),
      count = (_openScopes[_scopeKey(scope)] ?? 1) - 1;
  if (count == 0) {
    _openScopes.remove(key);
  } else {
    _openScopes[key] = count;
  }
}

String archiveTime(DateTime at) {
  final t = at.toLocal();
  String two(int n) => n.toString().padLeft(2, '0');
  return '${t.year}.${two(t.month)}.${two(t.day)} ${two(t.hour)}:${two(t.minute)}';
}

String archiveSize(int bytes) => bytes < 1 << 20
    ? '${(bytes / 1024).ceil()} KB'
    : '${(bytes / (1 << 20)).toStringAsFixed(1)} MB';

void openSavedConversations(BuildContext context, Server server) {
  Navigator.of(context).push(
    MaterialPageRoute<void>(
      settings: const RouteSettings(name: 'saved-conversations'),
      builder: (_) => SavedConversationsScreen(server: server),
    ),
  );
}

class ArchiveSaveButton extends StatefulWidget {
  const ArchiveSaveButton({
    super.key,
    required this.server,
    required this.pane,
    required this.source,
  });
  final Server server;
  final Pane pane;
  final String Function() source;
  @override
  State<ArchiveSaveButton> createState() => _ArchiveSaveButtonState();
}

class _ArchiveSaveButtonState extends State<ArchiveSaveButton> {
  late final _repository = ArchiveRepository(widget.server);
  @override
  void dispose() {
    _repository.dispose();
    super.dispose();
  }

  Future<void> _save() async {
    try {
      await _repository.savePane(widget.pane, source: widget.source());
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          content: const Text('대화 텍스트를 오프라인에 저장했어요'),
          action: SnackBarAction(
            label: '보기',
            onPressed: () => openSavedConversations(context, widget.server),
          ),
        ),
      );
    } catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(
          context,
        ).showSnackBar(SnackBar(content: Text('$e')));
      }
    }
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: _repository,
    builder: (context, _) {
      final busy = _repository.downloading != null;
      return Tooltip(
        message: '대화 텍스트·코드·도구 요약을 저장해요. 외부 첨부 파일은 포함되지 않아요.',
        child: TextButton.icon(
          onPressed: _repository.locked
              ? null
              : busy
              ? _repository.cancel
              : _save,
          icon: busy
              ? SizedBox.square(
                  dimension: Look.iconSize,
                  child: CircularProgressIndicator(
                    value: _repository.progress,
                    strokeWidth: 2,
                  ),
                )
              : const Icon(Icons.download_outlined),
          label: Text(busy ? '저장 취소' : '오프라인 저장'),
        ),
      );
    },
  );
}

class SavedConversationsScreen extends StatefulWidget {
  const SavedConversationsScreen({
    super.key,
    required this.server,
    this.repository,
  });
  final Server server;
  final ArchiveRepository? repository;
  @override
  State<SavedConversationsScreen> createState() =>
      _SavedConversationsScreenState();
}

class _SavedConversationsScreenState extends State<SavedConversationsScreen>
    with WidgetsBindingObserver {
  late final _repository =
      widget.repository ?? ArchiveRepository(widget.server);
  bool _savedOnly = true;
  Timer? _timer;
  @override
  void initState() {
    super.initState();
    _entered(widget.server.resumeScope);
    WidgetsBinding.instance.addObserver(this);
    BackgroundGrace.instance.addListener(_connected);
    NetworkWatch.instance.addListener(_connected);
    unawaited(_repository.load(updateSaved: true));
    _timer = Timer.periodic(const Duration(seconds: 15), (_) {
      if (BackgroundGrace.instance.live &&
          !_repository.locked &&
          _repository.downloading == null) {
        unawaited(_repository.load(updateSaved: true));
      }
    });
  }

  void _connected() {
    if (BackgroundGrace.instance.live && !_repository.locked) {
      _repository.reconnect();
    }
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) _connected();
  }

  @override
  void dispose() {
    _timer?.cancel();
    _left(widget.server.resumeScope);
    WidgetsBinding.instance.removeObserver(this);
    BackgroundGrace.instance.removeListener(_connected);
    NetworkWatch.instance.removeListener(_connected);
    if (widget.repository == null) _repository.dispose();
    super.dispose();
  }

  Future<void> _open(ArchiveEntry entry) async {
    try {
      final saved =
          await _repository.open(entry.id) ?? await _repository.save(entry);
      if (!mounted || _repository.locked) return;
      await Navigator.of(context).push(
        MaterialPageRoute<void>(
          settings: const RouteSettings(name: 'saved-conversation'),
          builder: (_) => SavedConversationScreen(
            repository: _repository,
            conversation: saved,
          ),
        ),
      );
    } catch (e) {
      _show(e);
    }
  }

  void _show(Object e) {
    if (mounted) {
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('$e')));
    }
  }

  Future<void> _download(ArchiveEntry entry) async {
    try {
      await _repository.save(entry);
    } catch (e) {
      _show(e);
    }
  }

  Future<void> _remove(ArchiveEntry entry, bool account) async {
    if (account) {
      final confirmed = await showDialog<bool>(
        context: context,
        builder: (context) => ModalLook(
          child: AlertDialog(
            title: const Text('계정 기록을 삭제할까요?'),
            content: const Text(
              '계정 기록과 이 폰의 저장본을 지워요. 다른 기기의 오프라인 사본은 남을 수 있어요.',
            ),
            actions: [
              TextButton(
                autofocus: true,
                onPressed: () => Navigator.pop(context, false),
                child: const Text('취소'),
              ),
              FilledButton(
                style: FilledButton.styleFrom(
                  backgroundColor: Theme.of(context).colorScheme.error,
                  foregroundColor: Theme.of(context).colorScheme.onError,
                ),
                onPressed: () => Navigator.pop(context, true),
                child: const Text('삭제'),
              ),
            ],
          ),
        ),
      );
      if (confirmed != true) return;
    }
    try {
      await _repository.remove(entry.id, fromAccount: account);
    } catch (e) {
      _show(e);
    }
  }

  Future<void> _restore(ArchiveEntry entry) async {
    try {
      await _repository.restore(entry.id);
    } catch (e) {
      _show(e);
    }
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: _repository,
    builder: (context, _) {
      final r = _repository;
      final local = {for (final entry in r.saved) entry.id: entry};
      final remote = {
        for (final entry in r.remote.where((e) => !e.deleted)) entry.id: entry,
      };
      final rows = _savedOnly ? r.saved : remote.values.toList();
      final used = r.saved.fold<int>(0, (size, entry) => size + entry.bytes);
      return Scaffold(
        appBar: AppBar(
          title: const Text('저장한 대화'),
          actions: [
            IconButton(
              tooltip: '새로고침',
              onPressed: r.locked || r.refreshing
                  ? null
                  : () => r.load(updateSaved: true),
              icon: const Icon(Icons.refresh),
            ),
          ],
        ),
        body: r.locked
            ? const Center(child: Text('현재 계정의 연결이 종료됐어요.'))
            : SafeArea(
                child: Column(
                  children: [
                    if (r.hasAccount)
                      Padding(
                        padding: const EdgeInsets.symmetric(
                          horizontal: Look.pagePad,
                          vertical: Look.groupTitleGap,
                        ),
                        child: SegmentedButton<bool>(
                          segments: const [
                            ButtonSegment(
                              value: true,
                              label: Text('이 기기에 저장됨'),
                            ),
                            ButtonSegment(value: false, label: Text('계정 기록')),
                          ],
                          selected: {_savedOnly},
                          onSelectionChanged: (values) =>
                              setState(() => _savedOnly = values.single),
                        ),
                      ),
                    if (!_savedOnly && r.hasAccount)
                      SwitchListTile(
                        title: const Text('기기 대화 동기화'),
                        subtitle: const Text('연결된 기기의 대화 텍스트를 계정에 보관해요.'),
                        value: r.enabled,
                        onChanged: r.loading || r.refreshing || r.configuring
                            ? null
                            : r.setEnabled,
                      ),
                    Padding(
                      padding: const EdgeInsets.symmetric(
                        horizontal: Look.pagePad,
                        vertical: Look.groupTitleGap,
                      ),
                      child: Text(
                        '텍스트·코드·도구 요약을 읽을 수 있어요. 외부 첨부 파일은 포함되지 않아요.\n이 기기 저장 공간 ${archiveSize(used)} / ${archiveSize(maxOfflineBytes)}',
                        style: Theme.of(context).textTheme.bodySmall,
                      ),
                    ),
                    if (r.refreshing)
                      const LinearProgressIndicator(minHeight: 2),
                    if (r.error != null)
                      Padding(
                        padding: const EdgeInsets.all(Look.pagePad),
                        child: Text(
                          r.error!,
                          style: Theme.of(context).textTheme.bodySmall,
                        ),
                      ),
                    Expanded(
                      child: r.loading
                          ? const TwinsLoading(label: '저장한 대화를 읽는 중')
                          : rows.isEmpty
                          ? Center(
                              child: Padding(
                                padding: const EdgeInsets.all(Look.pagePad),
                                child: Text(
                                  _savedOnly
                                      ? '저장한 대화가 없어요. 대화 화면에서 오프라인 저장을 눌러 주세요.'
                                      : r.enabled
                                      ? '기기가 연결되면 대화 기록이 여기에 모여요.'
                                      : '기기 대화 동기화를 켜면 원본 기기가 꺼져도 기록을 받을 수 있어요.',
                                ),
                              ),
                            )
                          : ListView.builder(
                              itemCount: rows.length,
                              itemBuilder: (context, index) {
                                final entry = rows[index],
                                    cached = local[rows[index].id],
                                    cloud = remote[rows[index].id];
                                final downloading = r.downloading == entry.id;
                                final newer =
                                    cached != null &&
                                    cloud != null &&
                                    cloud.newerThan(cached);
                                final status = cached != null
                                    ? newer
                                          ? '저장됨 · 새 내용 있음'
                                          : '오프라인 저장됨'
                                    : entry.ready
                                    ? '저장할 수 있음'
                                    : '기기에서 동기화 중';
                                return SettingsRow(
                                  tone: index,
                                  icon: cached != null
                                      ? Icons.offline_pin_outlined
                                      : Icons.forum_outlined,
                                  title: entry.displayTitle,
                                  subtitle:
                                      '${entry.machineName} · ${archiveTime(entry.updatedAt)}\n$status · ${archiveSize(entry.bytes)}',
                                  onTap: cached != null || entry.ready
                                      ? () => _open(entry)
                                      : null,
                                  trailing: Row(
                                    mainAxisSize: MainAxisSize.min,
                                    children: [
                                      if (downloading)
                                        IconButton(
                                          tooltip: '저장 취소',
                                          onPressed: r.cancel,
                                          icon: SizedBox.square(
                                            dimension: Look.iconSize,
                                            child: CircularProgressIndicator(
                                              value: r.progress,
                                              strokeWidth: 2,
                                            ),
                                          ),
                                        )
                                      else if ((cached == null || newer) &&
                                          entry.ready)
                                        IconButton(
                                          tooltip: newer
                                              ? '새 내용 저장'
                                              : '오프라인 저장',
                                          onPressed:
                                              r.downloading != null ||
                                                  !r.available
                                              ? null
                                              : () => _download(cloud ?? entry),
                                          icon: const Icon(
                                            Icons.download_outlined,
                                          ),
                                        ),
                                      PopupMenuButton<String>(
                                        tooltip: '대화 관리',
                                        onSelected: (action) =>
                                            action == 'restore'
                                            ? _restore(entry)
                                            : _remove(
                                                entry,
                                                action == 'account',
                                              ),
                                        itemBuilder: (_) => [
                                          if (cached != null)
                                            const PopupMenuItem(
                                              value: 'local',
                                              child: Text('이 기기 저장본 삭제'),
                                            ),
                                          if (cloud != null)
                                            const PopupMenuItem(
                                              value: 'account',
                                              child: Text('계정 기록도 삭제'),
                                            ),
                                          if (r.remote.any(
                                            (e) =>
                                                e.id == entry.id && e.deleted,
                                          ))
                                            const PopupMenuItem(
                                              value: 'restore',
                                              child: Text('계정에 다시 동기화'),
                                            ),
                                        ],
                                      ),
                                    ],
                                  ),
                                );
                              },
                            ),
                    ),
                  ],
                ),
              ),
      );
    },
  );
}

class SavedConversationScreen extends StatefulWidget {
  const SavedConversationScreen({
    super.key,
    required this.repository,
    required this.conversation,
  });
  final ArchiveRepository repository;
  final SavedConversation conversation;
  @override
  State<SavedConversationScreen> createState() =>
      _SavedConversationScreenState();
}

class _SavedConversationScreenState extends State<SavedConversationScreen> {
  late SavedConversation _saved = widget.conversation;
  final _conversation = Conversation(archived: true);
  late final Pane _pane;
  late final TermSession _session;
  int _reading = 0;
  @override
  void initState() {
    super.initState();
    _entered(widget.repository.scope);
    final e = _saved.entry;
    _pane = Pane(
      id: 'saved-${e.id}',
      name: e.name,
      title: e.title,
      status: 'idle',
      window: 0,
      cwd: e.cwd,
      machine: '~${e.machineId}',
      harness: e.harness,
    );
    _session = TermSession(widget.repository.server, _pane)..pause();
    _conversation.apply(
      _saved.raw,
      reset: true,
      next: e.cursor,
      source: e.source,
    );
    widget.repository.addListener(_changed);
  }

  void _changed() {
    if (!mounted) return;
    if (widget.repository.locked) {
      setState(() {});
      return;
    }
    final entry = widget.repository.saved
        .where((e) => e.id == _saved.entry.id)
        .firstOrNull;
    if (entry == null || entry.sameSnapshot(_saved.entry)) return;
    final read = ++_reading;
    unawaited(() async {
      try {
        final fresh = await widget.repository.open(entry.id);
        if (!mounted ||
            read != _reading ||
            fresh == null ||
            widget.repository.locked) {
          return;
        }
        final append =
            fresh.entry.generation == _saved.entry.generation &&
            fresh.raw.startsWith(_saved.raw);
        final delta = append
            ? fresh.raw.substring(_saved.raw.length)
            : fresh.raw;
        if (!append) _conversation.clear();
        _conversation.apply(
          delta,
          reset: !append,
          next: fresh.entry.cursor,
          source: fresh.entry.source,
        );
        setState(() => _saved = fresh);
      } catch (_) {}
    }());
  }

  @override
  void dispose() {
    widget.repository.removeListener(_changed);
    _left(widget.repository.scope);
    _session.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(
      title: Text(
        _saved.entry.displayTitle,
        maxLines: 1,
        overflow: TextOverflow.ellipsis,
      ),
    ),
    body: widget.repository.locked
        ? const Center(child: Text('현재 계정의 연결이 종료됐어요.'))
        : SafeArea(
            child: Column(
              children: [
                Padding(
                  padding: const EdgeInsets.symmetric(
                    horizontal: Look.pagePad,
                    vertical: Look.groupTitleGap,
                  ),
                  child: Text(
                    '저장한 대화 · 읽기 전용\n마지막 동기화 ${archiveTime(_saved.entry.updatedAt)}',
                    style: Theme.of(context).textTheme.bodySmall,
                  ),
                ),
                Expanded(
                  child: SelectionArea(
                    child: ConversationView(
                      server: widget.repository.server,
                      pane: _pane,
                      session: _session,
                      accent: Theme.of(context).colorScheme.primary,
                      onTerminal: () {},
                      conversation: _conversation,
                      readOnly: true,
                      active: false,
                    ),
                  ),
                ),
              ],
            ),
          ),
  );
}
