import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:url_launcher/url_launcher.dart';

import '../look.dart';
import '../relay_account.dart';
import 'controls.dart';

/// Google·GitHub 로그인(또는 [link] 면 지금 계정에 연결). 관문 확인 화면을 Safari 로 열고, 이 앱에만 보이는 확인
/// 코드를 거기 넣게 한 뒤, 끝날 때까지 관문에 묻는다. 데스크톱(`native_device_account.rs`)과 같은 흐름이다.
///
/// 로그인이면 받은 세션을, 연결이면 [OAuthResult.linked] 를 돌려준다. 취소·실패면 null(실패 글은 시트가 보인다).
Future<OAuthResult?> showOAuthSheet(
  BuildContext context, {
  required RelayAccountApi api,
  required OAuthProvider provider,
  required String machineId,
  bool link = false,
  Future<bool> Function(Uri url)? open,
  Duration every = const Duration(seconds: 2),
}) => showModalBottomSheet<OAuthResult>(
  context: context,
  isScrollControlled: true,
  showDragHandle: true,
  isDismissible: false,
  builder: (_) => ModalLook(
    child: _OAuthSheet(
      api: api,
      provider: provider,
      machineId: machineId,
      link: link,
      open: open ?? (url) => launchUrl(url, mode: LaunchMode.externalApplication),
      every: every,
    ),
  ),
);

class _OAuthSheet extends StatefulWidget {
  const _OAuthSheet({
    required this.api,
    required this.provider,
    required this.machineId,
    required this.link,
    required this.open,
    required this.every,
  });

  final RelayAccountApi api;
  final OAuthProvider provider;
  final String machineId;
  final bool link;
  final Future<bool> Function(Uri url) open;
  final Duration every;

  @override
  State<_OAuthSheet> createState() => _OAuthSheetState();
}

class _OAuthSheetState extends State<_OAuthSheet> with WidgetsBindingObserver {
  OAuthFlow? _flow;
  String? _error;
  Timer? _timer;
  bool _polling = false;
  bool _done = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    unawaited(_start());
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _timer?.cancel();
    final flow = _flow;
    if (flow != null && !_done) unawaited(widget.api.oauthCancel(flow));
    super.dispose();
  }

  /// Safari 에서 돌아오면 바로 묻는다 — 뒤에 있던 동안 시계가 멈춰 있었다.
  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) unawaited(_poll());
  }

  Future<void> _start() async {
    try {
      final flow = await widget.api.oauthStart(widget.provider, widget.machineId, link: widget.link);
      if (!mounted) {
        unawaited(widget.api.oauthCancel(flow));
        return;
      }
      setState(() => _flow = flow);
      _timer = Timer.periodic(widget.every, (_) => unawaited(_poll()));
      await _openBrowser();
    } on AccountException catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
  }

  /// 코드를 클립보드에 두고 연다 — Safari 확인 칸에 붙여 넣기만 하면 된다.
  Future<void> _openBrowser() async {
    final flow = _flow;
    if (flow == null) return;
    await Clipboard.setData(ClipboardData(text: flow.userCode));
    var opened = false;
    try {
      opened = await widget.open(flow.authorization);
    } catch (_) {}
    if (!opened && mounted) {
      ScaffoldMessenger.maybeOf(context)?.showSnackBar(
        const SnackBar(content: Text('브라우저를 열지 못했어요. 「브라우저 다시 열기」를 눌러 주세요.')),
      );
    }
  }

  Future<void> _poll() async {
    final flow = _flow;
    if (flow == null || _polling || _done || !mounted) return;
    if (DateTime.now().isAfter(flow.expires)) {
      _stop(oauthError('expired', 400));
      return;
    }
    _polling = true;
    try {
      final r = await widget.api.oauthPoll(flow);
      if (!mounted || r.pending) return;
      _done = true;
      _timer?.cancel();
      Navigator.of(context).pop(r);
    } on AccountException catch (e) {
      _stop(e.message);
    } finally {
      _polling = false;
    }
  }

  void _stop(String message) {
    _done = true;
    _timer?.cancel();
    if (mounted) setState(() => _error = message);
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final flow = _flow;
    final name = widget.provider.label;
    return SafeArea(
      child: Padding(
        padding: const EdgeInsets.fromLTRB(Look.pagePad, 0, Look.pagePad, Look.pagePad),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text(widget.link ? '$name 연결' : '$name 로그인', style: theme.textTheme.titleLarge),
            const SizedBox(height: Look.fieldGap),
            if (_error case final error?) ...[
              Semantics(liveRegion: true, child: Text(error, style: TextStyle(color: scheme.error))),
              const SizedBox(height: Look.groupGap),
              FilledButton(onPressed: () => Navigator.of(context).pop(), child: const Text('닫기')),
            ] else if (flow == null) ...[
              const Center(child: Padding(padding: EdgeInsets.all(Look.groupGap), child: CircularProgressIndicator())),
            ] else ...[
              Text(
                'Safari 의 확인 화면에 이 코드를 넣고 $name 로그인을 마친 뒤 이 앱으로 돌아오세요. 코드는 복사해 두었어요.',
                style: theme.textTheme.bodyMedium?.copyWith(color: scheme.onSurfaceVariant),
              ),
              const SizedBox(height: Look.groupGap),
              SelectableText(
                flow.userCode,
                key: const Key('oauth-code'),
                textAlign: TextAlign.center,
                style: theme.textTheme.headlineMedium?.copyWith(
                  fontFeatures: const [FontFeature.tabularFigures()],
                  letterSpacing: 2,
                ),
              ),
              const SizedBox(height: Look.groupGap),
              Row(
                children: [
                  const SizedBox.square(dimension: 18, child: CircularProgressIndicator(strokeWidth: 2)),
                  const SizedBox(width: Look.fieldGap),
                  Expanded(
                    child: Text('로그인을 기다리는 중', style: theme.textTheme.bodyMedium),
                  ),
                ],
              ),
              const SizedBox(height: Look.groupGap),
              FilledButton(onPressed: () => unawaited(_openBrowser()), child: const Text('브라우저 다시 열기')),
              const SizedBox(height: 8),
              OutlinedButton(
                style: OutlinedButton.styleFrom(minimumSize: const Size(Look.tap, Look.buttonH)),
                onPressed: () => Navigator.of(context).pop(),
                child: const Text('취소'),
              ),
            ],
          ],
        ),
      ),
    );
  }
}
