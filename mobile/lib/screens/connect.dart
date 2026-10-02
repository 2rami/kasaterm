import 'dart:async';

import 'package:flutter/material.dart';

import '../server.dart';
import '../twins_loading.dart';
import '../relay_account.dart';
import '../look.dart';
import 'controls.dart';
import 'oauth_sheet.dart';

class ConnectScreen extends StatefulWidget {
  const ConnectScreen({
    super.key,
    required this.onConnected,
    required this.onLogin,
    this.message,
    this.relay,
    this.installId,
    this.onSession,
  });

  final Future<void> Function(Server server) onConnected;
  final Future<void> Function(Uri origin, String account, String password)
  onLogin;
  final String? message;

  /// Google·GitHub 로그인 — 셋이 다 있어야 단추가 선다(`ConnectionController.relay`·`installId`·`adopt`).
  final RelayAccountApi Function(Uri origin)? relay;
  final Future<String> Function()? installId;
  final Future<void> Function(AccountSession session)? onSession;

  @override
  State<ConnectScreen> createState() => _ConnectScreenState();
}

class _ConnectScreenState extends State<ConnectScreen> {
  final _controller = TextEditingController();
  final _account = TextEditingController();
  final _password = TextEditingController();
  final _gateway = TextEditingController(text: defaultGateway);
  String? _error;
  bool _busy = false;
  OAuthProviders _providers = const OAuthProviders([]);
  Uri? _providersFor;

  /// Google·GitHub 이 켜진 서버에서는 아이디·비밀번호 칸을 「다른 방법」 아래 접어 둔다.
  bool _passwordOpen = false;

  @override
  void initState() {
    super.initState();
    unawaited(_loadProviders());
  }

  /// 서버 주소를 바꾸면 그 서버가 켜 둔 것으로 다시 묻는다.
  Future<void> _loadProviders() async {
    final origin = parseGateway(_gateway.text);
    final relay = widget.relay;
    if (relay == null || widget.onSession == null || widget.installId == null || origin == null) return;
    if (origin == _providersFor) return;
    _providersFor = origin;
    final api = relay(origin);
    try {
      final providers = await api.oauthProviders();
      if (mounted && _providersFor == origin) setState(() => _providers = providers);
    } finally {
      api.close();
    }
  }

  Future<void> _oauth(OAuthProvider provider) async {
    final origin = parseGateway(_gateway.text);
    if (_busy || origin == null) return;
    setState(() {
      _busy = true;
      _error = null;
    });
    final api = widget.relay!(origin);
    try {
      final id = await widget.installId!();
      if (!mounted) return;
      final result = await showOAuthSheet(context, api: api, provider: provider, machineId: id);
      final session = result?.session;
      if (session != null) await widget.onSession!(session);
    } on AccountException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } catch (_) {
      if (mounted) setState(() => _error = '이 폰의 고유 id 를 저장하지 못했어요. 다시 시도해 주세요.');
    } finally {
      api.close();
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  void dispose() {
    _controller.dispose();
    _account.dispose();
    _password.dispose();
    _gateway.dispose();
    super.dispose();
  }

  Future<void> _connect() async {
    final root = Server.parse(_controller.text);
    if (root == null) {
      setState(() => _error = 'https://로 시작하는 폰 주소를 입력해 주세요.');
      return;
    }
    setState(() {
      _busy = true;
      _error = null;
    });
    final server = Server(root);
    try {
      await server.me();
      await widget.onConnected(server);
    } on ServerException catch (e) {
      server.close();
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _login() async {
    if (_busy) return;
    final origin = parseGateway(_gateway.text);
    if (origin == null ||
        _account.text.trim().isEmpty ||
        _password.text.isEmpty) {
      setState(
        () => _error = origin == null
            ? 'https://로 시작하는 서버 주소를 확인해 주세요.'
            : '아이디와 비밀번호를 입력해 주세요.',
      );
      return;
    }
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await widget.onLogin(origin, _account.text, _password.text);
      if (mounted) _password.clear();
    } on AccountException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  /// 처음 보는 Google·GitHub 을 어떻게 받는지. 옛 서버는 묻지 않고 새 계정을 만든다.
  String get _oauthHint => switch ((_providers.choose, _providers.signup)) {
    (true, true) => '처음 쓰는 Google·GitHub 이면 새 계정을 만들지, 이미 있는 KASA 계정에 연결할지 물어봐요.',
    (true, false) => '처음 쓰는 Google·GitHub 은 이미 있는 KASA 계정에 한 번 연결하면 그다음부터 바로 들어가요.',
    (false, true) => '처음 쓰는 Google·GitHub 은 새 계정이 돼요. 데스크톱과 같은 계정을 쓰려면 아이디로 로그인한 뒤 설정에서 연결해 주세요.',
    (false, false) => '데스크톱 설정 → 계정에서 연결해 둔 Google·GitHub 으로 들어가요.',
  };

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final oauth = _providers.enabled.isNotEmpty;
    return TwinBackdrop(
      child: Scaffold(
      backgroundColor: Colors.transparent,
      body: SafeArea(
        child: Align(
          alignment: Alignment.topCenter,
          child: SingleChildScrollView(
            padding: const EdgeInsets.all(24),
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 420),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  const SizedBox(height: Look.groupGap),
                  const ExcludeSemantics(child: Center(child: TwinsStage(t: 0, size: Look.twinsSmall))),
                  const SizedBox(height: Look.fieldGap),
                  Text('KASA Mobile', textAlign: TextAlign.center, style: theme.textTheme.titleLarge),
                  const SizedBox(height: 8),
                  Text(
                    '데스크톱과 같은 계정으로 로그인하세요.',
                    textAlign: TextAlign.center,
                    style: theme.textTheme.bodyMedium?.copyWith(
                      color: theme.colorScheme.onSurfaceVariant,
                    ),
                  ),
                  const SizedBox(height: 24),
                  if (oauth) ...[
                    for (final provider in _providers.enabled) ...[
                      FilledButton(
                        key: Key('oauth-${provider.id}'),
                        style: FilledButton.styleFrom(
                          minimumSize: const Size(Look.tap, Look.buttonH),
                        ),
                        onPressed: _busy ? null : () => _oauth(provider),
                        child: Text('${provider.label}로 로그인'),
                      ),
                      const SizedBox(height: 8),
                    ],
                    Text(
                      _oauthHint,
                      style: theme.textTheme.bodySmall?.copyWith(
                        color: theme.colorScheme.onSurfaceVariant,
                      ),
                    ),
                    const SizedBox(height: Look.groupGap),
                    Text(
                      '다른 방법',
                      style: theme.textTheme.bodySmall?.copyWith(
                        color: theme.colorScheme.onSurfaceVariant,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                    const SizedBox(height: Look.groupTitleGap),
                  ],
                  if (oauth && !_passwordOpen)
                    OutlinedButton(
                      key: const Key('account-open'),
                      style: OutlinedButton.styleFrom(
                        minimumSize: const Size(Look.tap, Look.buttonH),
                      ),
                      onPressed: _busy ? null : () => setState(() => _passwordOpen = true),
                      child: const Text('아이디로 로그인'),
                    )
                  else ...[
                    LabeledField(
                      label: '아이디',
                      child: TextField(
                        key: const Key('account-input'),
                        controller: _account,
                        enabled: !_busy,
                        autofocus: oauth,
                        style: const TextStyle(fontSize: 16),
                        autofillHints: const [AutofillHints.username],
                        autocorrect: false,
                        enableSuggestions: false,
                        textInputAction: TextInputAction.next,
                        decoration: const InputDecoration(),
                      ),
                    ),
                    const SizedBox(height: 12),
                    LabeledField(
                      label: '비밀번호',
                      child: TextField(
                        key: const Key('password-input'),
                        controller: _password,
                        enabled: !_busy,
                        style: const TextStyle(fontSize: 16),
                        obscureText: true,
                        autocorrect: false,
                        enableSuggestions: false,
                        autofillHints: const [AutofillHints.password],
                        textInputAction: TextInputAction.go,
                        onSubmitted: (_) => _login(),
                        decoration: const InputDecoration(),
                      ),
                    ),
                  ],
                  if (_error ?? widget.message case final message?) ...[
                    const SizedBox(height: 12),
                    Semantics(
                      liveRegion: true,
                      child: Text(
                        message,
                        style: TextStyle(color: theme.colorScheme.error),
                      ),
                    ),
                  ],
                  if (!oauth || _passwordOpen) ...[
                    const SizedBox(height: 12),
                    (oauth ? OutlinedButton.new : FilledButton.new)(
                      key: const Key('account-login'),
                      style: (oauth ? OutlinedButton.styleFrom : FilledButton.styleFrom)(
                        minimumSize: const Size(Look.tap, Look.buttonH),
                      ),
                      onPressed: _busy ? null : _login,
                      child: _busy
                          ? const SizedBox(
                              width: 18,
                              height: 18,
                              child: CircularProgressIndicator(strokeWidth: 2),
                            )
                          : const Text('로그인'),
                    ),
                  ],
                  const SizedBox(height: 8),
                  ExpansionTile(
                    title: const Text('고급 설정'),
                    tilePadding: EdgeInsets.zero,
                    children: [
                      LabeledField(
                        label: '계정 서버 주소',
                        child: TextField(
                        controller: _gateway,
                        enabled: !_busy,
                        style: const TextStyle(fontSize: 16),
                        autocorrect: false,
                        keyboardType: TextInputType.url,
                        onSubmitted: (_) => unawaited(_loadProviders()),
                        decoration: const InputDecoration(),
                        ),
                      ),
                      const SizedBox(height: Look.fieldGap),
                      LabeledField(
                        label: '기존 폰 주소',
                        child: TextField(
                        controller: _controller,
                        enabled: !_busy,
                        style: const TextStyle(fontSize: 16),
                        autocorrect: false,
                        enableSuggestions: false,
                        keyboardType: TextInputType.url,
                        decoration: const InputDecoration(hintText: 'https://…/u/…/'),
                        ),
                      ),
                      const SizedBox(height: 12),
                      SizedBox(
                        width: double.infinity,
                        child: OutlinedButton(
                          style: OutlinedButton.styleFrom(
                            minimumSize: const Size(Look.tap, Look.buttonH),
                          ),
                          onPressed: _busy ? null : _connect,
                          child: const Text('폰 주소로 연결'),
                        ),
                      ),
                      const SizedBox(height: 8),
                    ],
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
      ),
    );
  }
}
