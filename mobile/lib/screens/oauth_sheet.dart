import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:url_launcher/url_launcher.dart';

import '../look.dart';
import '../twins_loading.dart';
import '../relay_account.dart';
import 'controls.dart';

/// 시스템 로그인 창으로 [url] 을 열고, [scheme] 주소로 돌아온 순간 그 주소를 준다. 사람이 닫으면 null.
typedef WebAuthenticate = Future<Uri?> Function(Uri url, String scheme);

const _webAuth = MethodChannel('kasaterm/web_auth');

/// iOS ASWebAuthenticationSession(`AppDelegate.swift`). Safari 의 Google 로그인을 그대로 쓰고 결과는 이 앱에만 온다.
Future<Uri?> systemWebAuthenticate(Uri url, String scheme) async {
  final back = await _webAuth.invokeMethod<String>('authenticate', {'url': '$url', 'scheme': scheme});
  return back == null ? null : Uri.tryParse(back);
}

/// Google·GitHub 로그인(또는 [link] 면 지금 계정에 연결). 관문이 앱 리다이렉트를 알고 이 기기가 시스템 로그인 창을
/// 띄울 수 있으면([authenticate]) 확인 코드 없이 그 창에서 끝낸다. 아니면 옛 길 — 관문 확인 화면을 Safari 로 열고,
/// 이 앱에만 보이는 확인 코드를 거기 넣게 한 뒤, 끝날 때까지 관문에 묻는다. 데스크톱(`device_oauth.rs`)과 같은 흐름이다.
///
/// 로그인이면 받은 세션을, 연결이면 [OAuthResult.linked] 를 돌려준다. 취소·실패면 null(실패 글은 시트가 보인다).
/// 어느 계정에도 연결 안 된 로그인이면 시트 안에서 새 계정을 만들지, 기존 계정에 연결할지(비밀번호 한 번) 고르게 한다.
Future<OAuthResult?> showOAuthSheet(
  BuildContext context, {
  required RelayAccountApi api,
  required OAuthProvider provider,
  required String machineId,
  bool link = false,
  List<String> connect = const [],
  String? title,
  Future<bool> Function(Uri url)? open,
  WebAuthenticate? authenticate,
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
      connect: connect,
      title: title,
      open: open ?? (url) => launchUrl(url, mode: LaunchMode.externalApplication),
      authenticate:
          authenticate ?? (!kIsWeb && defaultTargetPlatform == TargetPlatform.iOS ? systemWebAuthenticate : null),
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
    required this.connect,
    required this.title,
    required this.open,
    required this.authenticate,
    required this.every,
  });

  final RelayAccountApi api;
  final OAuthProvider provider;
  final String machineId;
  final bool link;
  final List<String> connect;
  final String? title;
  final Future<bool> Function(Uri url) open;
  final WebAuthenticate? authenticate;
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
  OAuthChoice? _choice;
  bool _claim = false;
  bool _busy = false;
  String? _claimError;
  final _account = TextEditingController();
  final _password = TextEditingController();

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
    _account.dispose();
    _password.dispose();
    super.dispose();
  }

  /// Safari 에서 돌아오면 바로 묻는다 — 뒤에 있던 동안 시계가 멈춰 있었다.
  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) unawaited(_poll());
  }

  Future<void> _start() async {
    try {
      final flow = await widget.api.oauthStart(
        widget.provider,
        widget.machineId,
        link: widget.link,
        redirect: widget.authenticate != null,
        connect: widget.connect,
      );
      if (!mounted) {
        unawaited(widget.api.oauthCancel(flow));
        return;
      }
      setState(() => _flow = flow);
      if (flow.redirect != null) {
        await _authenticate(flow);
        return;
      }
      _timer = Timer.periodic(widget.every, (_) => unawaited(_poll()));
      await _openBrowser();
    } on AccountException catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
  }

  /// 확인 코드 없는 길 — 시스템 창이 돌아온 주소의 code 를 이 앱의 verifier 로 바꾼다. 창을 닫았으면 시트도 닫는다.
  Future<void> _authenticate(OAuthFlow flow) async {
    Uri? back;
    try {
      back = await widget.authenticate!(flow.authorization, oauthRedirectScheme);
    } on PlatformException {
      if (mounted) _stop('로그인 창을 열지 못했어요. 다시 시도해 주세요.');
      return;
    }
    if (!mounted || _done) return;
    if (back == null) {
      _done = true;
      Navigator.of(context).pop();
      return;
    }
    try {
      final r = await widget.api.oauthRedeem(flow, back);
      if (!mounted) return;
      _finish(r);
    } on AccountException catch (e) {
      _stop(e.message);
    }
  }

  /// 코드를 클립보드에 두고 연다 — Safari 확인 칸에 붙여 넣기만 하면 된다.
  Future<void> _openBrowser() async {
    final flow = _flow;
    final code = flow?.userCode;
    if (flow == null || code == null) return;
    await Clipboard.setData(ClipboardData(text: code));
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
    if (flow == null || flow.pollToken == null || _polling || _done || !mounted) return;
    if (DateTime.now().isAfter(flow.expires)) {
      _stop(oauthError('expired', 400));
      return;
    }
    _polling = true;
    try {
      final r = await widget.api.oauthPoll(flow);
      if (!mounted || r.pending) return;
      _finish(r);
    } on AccountException catch (e) {
      _stop(e.message);
    } finally {
      _polling = false;
    }
  }

  void _finish(OAuthResult r) {
    _done = true;
    _timer?.cancel();
    if (r.choice case final choice?) {
      setState(() => _choice = choice);
      return;
    }
    Navigator.of(context).pop(r);
  }

  Future<void> _answer({required bool claim}) async {
    final choice = _choice;
    if (choice == null || _busy) return;
    if (claim && (_account.text.trim().isEmpty || _password.text.isEmpty)) {
      setState(() => _claimError = '아이디와 비밀번호를 입력해 주세요.');
      return;
    }
    setState(() {
      _busy = true;
      _claimError = null;
    });
    try {
      final session = claim
          ? await widget.api.oauthClaim(choice, _account.text, _password.text)
          : await widget.api.oauthSignup(choice);
      if (mounted) Navigator.of(context).pop(OAuthResult(session: session));
    } on AccountException catch (e) {
      if (!mounted) return;
      // 틀린 비밀번호·잠깐 잠김은 같은 고르기로 다시 해 볼 수 있다. 나머지는 처음부터 다시.
      if (e.code == 'bad_credentials' || e.code == 'rate_limited') {
        _password.clear();
        setState(() => _claimError = e.message);
      } else {
        _stop(e.message);
      }
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  List<Widget> _chooseAccount(ThemeData theme, OAuthChoice choice) {
    final name = choice.provider.label;
    final dim = theme.textTheme.bodyMedium?.copyWith(color: theme.colorScheme.onSurfaceVariant);
    return [
      Text(
        choice.display.isEmpty ? name : '$name · ${choice.display}',
        key: const Key('oauth-choice-who'),
        style: theme.textTheme.bodyLarge?.copyWith(fontWeight: FontWeight.w600),
      ),
      const SizedBox(height: 2),
      Text('아직 KASA 계정에 연결되지 않은 로그인이에요.', style: theme.textTheme.bodyMedium),
      const SizedBox(height: Look.groupGap),
      if (!_claim) ...[
        FilledButton(
          key: const Key('oauth-choice-claim'),
          onPressed: _busy ? null : () => setState(() => _claim = true),
          child: const Text('기존 계정에 연결'),
        ),
        if (choice.signup) ...[
          const SizedBox(height: 8),
          OutlinedButton(
            key: const Key('oauth-choice-signup'),
            style: OutlinedButton.styleFrom(minimumSize: const Size(Look.tap, Look.buttonH)),
            onPressed: _busy ? null : () => unawaited(_answer(claim: false)),
            child: const Text('새 계정 만들기'),
          ),
        ],
        const SizedBox(height: 8),
        OutlinedButton(
          style: OutlinedButton.styleFrom(minimumSize: const Size(Look.tap, Look.buttonH)),
          onPressed: _busy ? null : () => Navigator.of(context).pop(),
          child: const Text('취소'),
        ),
        const SizedBox(height: Look.fieldGap),
        Text(
          choice.signup
              ? '이미 쓰던 KASA 계정이 있으면 연결하세요. 새 계정을 만들면 기존 계정의 기기·설정과 따로 움직여요.'
              : '이 서버는 새 계정을 받지 않아요. 기존 KASA 계정에 연결해 주세요.',
          style: dim,
        ),
      ] else ...[
        LabeledField(
          label: '아이디',
          child: TextField(
            key: const Key('claim-account'),
            controller: _account,
            enabled: !_busy,
            style: const TextStyle(fontSize: 16),
            autofillHints: const [AutofillHints.username],
            autocorrect: false,
            enableSuggestions: false,
            textInputAction: TextInputAction.next,
          ),
        ),
        const SizedBox(height: 12),
        LabeledField(
          label: '비밀번호',
          child: TextField(
            key: const Key('claim-password'),
            controller: _password,
            enabled: !_busy,
            style: const TextStyle(fontSize: 16),
            obscureText: true,
            autocorrect: false,
            enableSuggestions: false,
            autofillHints: const [AutofillHints.password],
            textInputAction: TextInputAction.go,
            onSubmitted: (_) => unawaited(_answer(claim: true)),
          ),
        ),
        if (_claimError case final error?) ...[
          const SizedBox(height: 12),
          Semantics(liveRegion: true, child: Text(error, style: TextStyle(color: theme.colorScheme.error))),
        ],
        const SizedBox(height: Look.groupGap),
        FilledButton(
          key: const Key('claim-submit'),
          onPressed: _busy ? null : () => unawaited(_answer(claim: true)),
          child: _busy
              ? const SizedBox.square(dimension: 18, child: CircularProgressIndicator(strokeWidth: 2))
              : const Text('연결하고 로그인'),
        ),
        const SizedBox(height: 8),
        OutlinedButton(
          style: OutlinedButton.styleFrom(minimumSize: const Size(Look.tap, Look.buttonH)),
          onPressed: _busy
              ? null
              : () => setState(() {
                  _claim = false;
                  _claimError = null;
                }),
          child: const Text('뒤로'),
        ),
        const SizedBox(height: Look.fieldGap),
        Text('기존 KASA 계정의 비밀번호를 이번 한 번만 확인해요. 다음부터는 $name 로그인만으로 들어와요.', style: dim),
      ],
    ];
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
      child: SingleChildScrollView(
        padding: EdgeInsets.fromLTRB(
          Look.pagePad,
          0,
          Look.pagePad,
          Look.pagePad + MediaQuery.viewInsetsOf(context).bottom,
        ),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text(widget.title ?? (widget.link ? '$name 연결' : '$name 로그인'), style: theme.textTheme.titleLarge),
            const SizedBox(height: Look.fieldGap),
            if (_error case final error?) ...[
              Semantics(liveRegion: true, child: Text(error, style: TextStyle(color: scheme.error))),
              const SizedBox(height: Look.groupGap),
              FilledButton(onPressed: () => Navigator.of(context).pop(), child: const Text('닫기')),
            ] else if (_choice case final choice?) ...[
              ..._chooseAccount(theme, choice),
            ] else if (flow == null) ...[
              const Center(child: Padding(padding: EdgeInsets.all(Look.groupGap), child: TwinsMark(hopping: true, face: Look.pullFace))),
            ] else if (flow.userCode == null) ...[
              Row(
                children: [
                  const SizedBox.square(dimension: 18, child: CircularProgressIndicator(strokeWidth: 2)),
                  const SizedBox(width: Look.fieldGap),
                  Expanded(child: Text('$name 로그인 창에서 마쳐 주세요', style: theme.textTheme.bodyMedium)),
                ],
              ),
            ] else ...[
              Text(
                'Safari 의 확인 화면에 이 코드를 넣고 $name 로그인을 마친 뒤 이 앱으로 돌아오세요. 코드는 복사해 두었어요.',
                style: theme.textTheme.bodyMedium?.copyWith(color: scheme.onSurfaceVariant),
              ),
              const SizedBox(height: Look.groupGap),
              SelectableText(
                flow.userCode!,
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
