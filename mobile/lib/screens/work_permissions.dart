import 'dart:async';
import 'dart:convert';
import 'dart:math';

import 'package:flutter/material.dart';
import 'package:url_launcher/url_launcher.dart';

import '../look.dart';
import '../relay_account.dart';
import '../twins_loading.dart';
import 'controls.dart';

/// 이 앱 실행 동안만 메모리에 있는 승인 열쇠. 키체인에도 디스크에도 두지 않는다 — 같은 계정의 다른 프로그램이
/// 정해진 길로는 승인을 흉내 내지 못하게(docs/account-connections.md 「쓰기는 사람 확인을 거친다」).
final String _approver = () {
  final rng = Random.secure();
  return base64Url
      .encode(List.generate(32, (_) => rng.nextInt(256)))
      .replaceAll('=', '');
}();

/// 계정에 붙인 GitHub 과, 학생·나쵸가 낸 PR 이 사람 승인을 기다리는 목록.
class WorkPermissionsScreen extends StatefulWidget {
  const WorkPermissionsScreen({super.key, required this.api});

  final RelayAccountApi Function() api;

  @override
  State<WorkPermissionsScreen> createState() => _WorkPermissionsScreenState();
}

class _WorkPermissionsScreenState extends State<WorkPermissionsScreen> {
  WorkList? _list;
  String? _error;
  bool _busy = false;

  @override
  void initState() {
    super.initState();
    unawaited(_reload());
  }

  Future<T> _with<T>(Future<T> Function(RelayAccountApi api) work) async {
    final api = widget.api();
    try {
      return await work(api);
    } finally {
      api.close();
    }
  }

  Future<void> _reload() async {
    try {
      final list = await _with((api) => api.work());
      if (mounted) {
        setState(() {
          _list = list;
          _error = null;
        });
      }
    } on AccountException catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
  }

  void _say(String text) {
    if (!mounted) return;
    ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(text)));
  }

  Future<void> _disconnect(Map<String, dynamic> connection) async {
    final name = 'GitHub · ${connection['display'] ?? ''}';
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => ModalLook(
        child: AlertDialog(
          title: const Text('연결 끊기'),
          content: Text('$name 연결을 끊어요. 기다리던 그 연결의 요청도 버려져요.'),
          actions: [
            TextButton(
              onPressed: () => Navigator.of(context).pop(false),
              child: const Text('취소'),
            ),
            FilledButton(
              onPressed: () => Navigator.of(context).pop(true),
              child: const Text('끊기'),
            ),
          ],
        ),
      ),
    );
    if (ok != true || !mounted) return;
    setState(() => _busy = true);
    try {
      final revoked = await _with(
        (api) => api.disconnect('${connection['id']}'),
      );
      _say(
        revoked
            ? '연결을 끊고 권한도 돌려줬어요.'
            : '연결을 끊었어요. 깃허브 보안 설정에서도 앱 권한을 지울 수 있어요.',
      );
    } on AccountException catch (e) {
      _say(e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
      await _reload();
    }
  }

  Future<void> _open(Map<String, dynamic> pending) async {
    final result = await showModalBottomSheet<String>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (_) => ModalLook(
        child: PendingWriteSheet(
          pending: pending,
          approve: () => _with(
            (api) => api.approve(
              '${pending['id']}',
              '${pending['digest']}',
              _approver,
            ),
          ),
          reject: () => _with((api) => api.reject('${pending['id']}')),
        ),
      ),
    );
    if (result != null) _say(result);
    await _reload();
  }

  @override
  Widget build(BuildContext context) {
    final list = _list;
    return TwinBackdrop(
      child: Scaffold(
        backgroundColor: Colors.transparent,
        appBar: AppBar(
          backgroundColor: Colors.transparent,
          title: const Text('일 권한'),
        ),
        body: RefreshIndicator(
          onRefresh: _reload,
          child: ListView(
            padding: const EdgeInsets.fromLTRB(
              Look.pagePad,
              8,
              Look.pagePad,
              Look.groupGap * 2,
            ),
            children: [
              if (list == null && _error == null)
                const Padding(
                  padding: EdgeInsets.all(Look.groupGap),
                  child: Center(child: CircularProgressIndicator()),
                )
              else if (list == null)
                SettingsGroup(
                  children: [
                    SettingsRow(
                      icon: Icons.cloud_off_outlined,
                      title: '목록을 못 받았어요',
                      subtitle: _error,
                      trailing: TextButton(
                        onPressed: _reload,
                        child: const Text('다시 시도'),
                      ),
                    ),
                  ],
                )
              else ...[
                if (list.pending.isNotEmpty)
                  SettingsGroup(
                    title: '승인을 기다리는 일',
                    children: [
                      for (final p in list.pending)
                        SettingsRow(
                          key: Key('pending-${p['id']}'),
                          tone: 1,
                          icon: Icons.merge_type_rounded,
                          title: pendingSummary(p),
                          subtitle: '요청: ${p['device_label'] ?? ''}',
                          chevron: true,
                          onTap: _busy ? null : () => unawaited(_open(p)),
                        ),
                    ],
                  ),
                if (list.connections.isNotEmpty)
                  SettingsGroup(
                    title: '연결',
                    children: [
                      for (final c in list.connections)
                        SettingsRow(
                          key: Key('connection-${c['id']}'),
                          icon: Icons.code_rounded,
                          title: 'GitHub · ${c['display'] ?? ''}',
                          subtitle: c['state'] == 'reconnect_required'
                              ? '다시 연결 필요'
                              : featureLabel(c['features']),
                          trailing: TextButton(
                            onPressed: _busy
                                ? null
                                : () => unawaited(_disconnect(c)),
                            child: const Text('끊기'),
                          ),
                        ),
                      if (list.installUrl case final url?
                          when list.connections.any(
                            (c) => c['provider'] == 'github',
                          ))
                        SettingsRow(
                          icon: Icons.open_in_new_rounded,
                          title: 'PR 올릴 레포 고르기',
                          subtitle: 'GitHub 앱을 둔 레포를 더하거나 빼요',
                          chevron: true,
                          onTap: () => unawaited(
                            launchUrl(url, mode: LaunchMode.externalApplication),
                          ),
                        ),
                    ],
                  ),
                Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 4),
                  child: Text(
                    list.connections.isEmpty
                        ? '설정 「계정」의 GitHub 연결 한 번이면 로그인과 함께 PR 권한이 붙어요.'
                        : list.connections.any((c) => c['state'] == 'reconnect_required')
                        ? '「다시 연결 필요」는 설정 「계정」의 GitHub 연결을 한 번 더 누르면 돼요.'
                        : '학생은 kasaterm-cli pr, 나쵸는 kasa-device work 로 써요. PR 만들기는 여기나 데스크톱 설정에서 승인해야 나가요.',
                    style: Theme.of(context).textTheme.bodySmall?.copyWith(
                      color: Theme.of(context).colorScheme.onSurfaceVariant,
                    ),
                  ),
                ),
              ],
            ],
          ),
        ),
      ),
    );
  }
}

String featureLabel(Object? features) =>
    features is List && features.contains('github.pr') ? 'PR 요청' : '권한 없음';

String pendingSummary(Map<String, dynamic> pending) {
  final write = pending['write'];
  if (write is! Map || write['kind'] != 'pr') return '알 수 없는 쓰기';
  return 'PR · ${write['repo'] ?? ''} · ${write['title'] ?? ''}';
}

/// 승인할 PR 전부 — 레포·브랜치·제목·본문 전체. 이 시트가 보인 내용(digest)만 실행된다.
class PendingWriteSheet extends StatefulWidget {
  const PendingWriteSheet({
    super.key,
    required this.pending,
    required this.approve,
    required this.reject,
  });

  final Map<String, dynamic> pending;
  final Future<Map<String, dynamic>> Function() approve;
  final Future<void> Function() reject;

  @override
  State<PendingWriteSheet> createState() => _PendingWriteSheetState();
}

class _PendingWriteSheetState extends State<PendingWriteSheet> {
  bool _busy = false;
  String? _error;

  Future<void> _run(bool approve) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      String done;
      if (approve) {
        final r = await widget.approve();
        done = r['status'] == 'created'
            ? 'PR #${r['number']} 을 만들었어요.'
            : '처리했어요.';
      } else {
        await widget.reject();
        done = '요청을 버렸어요.';
      }
      if (mounted) Navigator.of(context).pop(done);
    } on AccountException catch (e) {
      if (mounted) {
        setState(() {
          _busy = false;
          _error = e.message;
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final p = widget.pending;
    final write = p['write'] is Map ? p['write'] as Map : const {};
    final rows = <(String, String)>[
      ('보낼 계정', 'GitHub · ${p['display'] ?? ''}'),
      ('레포', '${write['repo'] ?? ''}'),
      (
        '브랜치',
        '${write['head'] ?? ''} → ${write['base'] ?? ''}${write['draft'] == true ? ' (초안)' : ''}',
      ),
      ('제목', '${write['title'] ?? ''}'),
      ('본문', '${write['body'] ?? ''}'),
      ('요청한 곳', '${p['device_label'] ?? ''}'),
    ];
    final dim = theme.textTheme.bodySmall?.copyWith(
      color: theme.colorScheme.onSurfaceVariant,
    );
    return SafeArea(
      child: SingleChildScrollView(
        padding: const EdgeInsets.fromLTRB(
          Look.pagePad,
          0,
          Look.pagePad,
          Look.pagePad,
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text('PR 만들기 승인', style: theme.textTheme.titleLarge),
            const SizedBox(height: Look.fieldGap),
            for (final (name, value) in rows) ...[
              Text(name, style: dim),
              const SizedBox(height: 2),
              SelectableText(
                value.isEmpty ? '—' : value,
                style: theme.textTheme.bodyMedium,
              ),
              const SizedBox(height: Look.fieldGap),
            ],
            if (_error case final error?) ...[
              Semantics(
                liveRegion: true,
                child: Text(
                  error,
                  style: TextStyle(color: theme.colorScheme.error),
                ),
              ),
              const SizedBox(height: Look.fieldGap),
            ],
            FilledButton(
              key: const Key('approve-write'),
              onPressed: _busy ? null : () => unawaited(_run(true)),
              child: _busy
                  ? const SizedBox.square(
                      dimension: 18,
                      child: CircularProgressIndicator(strokeWidth: 2),
                    )
                  : const Text('PR 만들기'),
            ),
            const SizedBox(height: 8),
            OutlinedButton(
              key: const Key('reject-write'),
              style: OutlinedButton.styleFrom(
                minimumSize: const Size(Look.tap, Look.buttonH),
              ),
              onPressed: _busy ? null : () => unawaited(_run(false)),
              child: const Text('버리기'),
            ),
          ],
        ),
      ),
    );
  }
}
