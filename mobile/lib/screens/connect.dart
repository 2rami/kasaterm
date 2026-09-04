import 'package:flutter/material.dart';

import '../server.dart';

/// 카사텀 허브의 「폰 주소」를 붙여 넣는 화면. 주소가 곧 자격이라 다른 로그인은 없다.
class ConnectScreen extends StatefulWidget {
  const ConnectScreen({super.key, required this.onConnected});

  final Future<void> Function(Server server) onConnected;

  @override
  State<ConnectScreen> createState() => _ConnectScreenState();
}

class _ConnectScreenState extends State<ConnectScreen> {
  final _controller = TextEditingController();
  String? _error;
  bool _busy = false;

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  Future<void> _connect() async {
    final root = Server.parse(_controller.text);
    if (root == null) {
      setState(() => _error = '주소 모양이 아니다 — https://… 로 시작하는 폰 주소를 붙여 넣어라');
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

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Scaffold(
      body: SafeArea(
        child: Center(
          child: SingleChildScrollView(
            padding: const EdgeInsets.all(24),
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 420),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  Text('kasaterm', style: theme.textTheme.headlineMedium),
                  const SizedBox(height: 8),
                  Text(
                    '데스크톱 카사텀 허브에서 「폰 주소」를 복사해 여기 붙여 넣어라. 그 주소가 곧 열쇠다.',
                    style: theme.textTheme.bodyMedium?.copyWith(
                      color: theme.colorScheme.onSurfaceVariant,
                    ),
                  ),
                  const SizedBox(height: 24),
                  TextField(
                    controller: _controller,
                    autocorrect: false,
                    enableSuggestions: false,
                    keyboardType: TextInputType.url,
                    textInputAction: TextInputAction.go,
                    onSubmitted: (_) => _busy ? null : _connect(),
                    decoration: InputDecoration(
                      hintText: 'https://kasaterm.debimarlene.com/u/…/',
                      errorText: _error,
                      errorMaxLines: 3,
                    ),
                  ),
                  const SizedBox(height: 12),
                  FilledButton(
                    onPressed: _busy ? null : _connect,
                    child: _busy
                        ? const SizedBox(
                            width: 18,
                            height: 18,
                            child: CircularProgressIndicator(strokeWidth: 2),
                          )
                        : const Text('연결'),
                  ),
                  const SizedBox(height: 8),
                  TextButton(
                    onPressed: _busy
                        ? null
                        : () => _controller.text = 'http://127.0.0.1:8765/',
                    child: const Text('이 컴퓨터에서 개발 중이면 127.0.0.1:8765'),
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}
