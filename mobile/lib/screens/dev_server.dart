import 'package:flutter/material.dart';
import 'package:url_launcher/url_launcher.dart';
import 'package:webview_flutter/webview_flutter.dart';

import '../net_tcp.dart';
import '../server.dart';

/// 데스크톱 개발 서버를 앱 안에서 본다 — 데스크톱 `127.0.0.1:port` 를 이 폰 localhost 로 끌어와 웹뷰로 연다.
/// 길은 데스크톱 직통(카사넷)이면 그쪽, 아니면 관문. 머리 아래 한 줄이 지금 길이다.
class DevServerScreen extends StatefulWidget {
  const DevServerScreen({
    super.key,
    required this.server,
    required this.port,
    this.machine,
    this.path = '/',
  });

  final Server server;
  final int port;
  final String? machine;
  final String path;

  @override
  State<DevServerScreen> createState() => _DevServerScreenState();
}

class _DevServerScreenState extends State<DevServerScreen> {
  NetTcpBridge? _bridge;
  WebViewController? _web;
  String? _error;
  int _progress = 0;

  @override
  void initState() {
    super.initState();
    widget.server.routeChanges?.addListener(_route);
    _open();
  }

  void _route() {
    if (mounted) setState(() {});
  }

  Future<void> _open() async {
    try {
      final bridge = await NetTcpBridge.start(
        widget.server,
        port: widget.port,
        machine: widget.machine,
      );
      if (!mounted) {
        await bridge.close();
        return;
      }
      final path = widget.path.startsWith('/')
          ? widget.path
          : '/${widget.path}';
      final web = WebViewController()
        ..setJavaScriptMode(JavaScriptMode.unrestricted)
        ..setNavigationDelegate(
          NavigationDelegate(
            onProgress: (p) {
              if (mounted) setState(() => _progress = p);
            },
            onWebResourceError: (e) {
              if (mounted && e.isForMainFrame != false) {
                setState(
                  () => _error =
                      '데스크톱 localhost:${widget.port} 를 못 열었어요 — ${e.description}',
                );
              }
            },
          ),
        )
        ..loadRequest(Uri.parse('http://localhost:${bridge.localPort}$path'));
      setState(() {
        _bridge = bridge;
        _web = web;
      });
    } catch (e) {
      if (mounted) setState(() => _error = '이 폰에 localhost 를 열지 못했어요 — $e');
    }
  }

  @override
  void dispose() {
    widget.server.routeChanges?.removeListener(_route);
    _bridge?.close();
    super.dispose();
  }

  String get _pathLine {
    final p = widget.server.pathOf(widget.machine);
    return switch (p) {
      (true, final int ms) => '데스크톱 직통 · ${ms}ms',
      (true, null) => '데스크톱 직통',
      _ => '관문 경유',
    };
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final web = _web;
    final local = _bridge?.localPort;
    return Scaffold(
      appBar: AppBar(
        title: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('localhost:${widget.port}'),
            Text(
              local == null || local == widget.port
                  ? _pathLine
                  : '$_pathLine · 이 폰 $local',
              style: theme.textTheme.bodySmall,
            ),
          ],
        ),
        actions: [
          IconButton(
            tooltip: '다시 읽기',
            icon: const Icon(Icons.refresh),
            onPressed: web == null
                ? null
                : () {
                    setState(() => _error = null);
                    web.reload();
                  },
          ),
        ],
        bottom: _progress > 0 && _progress < 100
            ? PreferredSize(
                preferredSize: const Size.fromHeight(2),
                child: LinearProgressIndicator(
                  value: _progress / 100,
                  minHeight: 2,
                ),
              )
            : null,
      ),
      body: _error != null
          ? Center(
              child: Padding(
                padding: const EdgeInsets.all(24),
                child: Text(_error!, textAlign: TextAlign.center),
              ),
            )
          : web == null
          ? const Center(child: CircularProgressIndicator())
          : WebViewWidget(controller: web),
    );
  }
}

/// 데스크톱이 보여 주기로 넘긴 그 기계의 localhost 주소면 (포트, 경로·쿼리) — 앱 안 웹뷰로 연다. 새 판 폰 앱이
/// 등록한 데스크톱만 이런 주소를 쪽지에 넣는다(옛 판에는 임시 터널 주소). 폰에는 제 localhost 서버가 없으니
/// 쪽지의 localhost 는 늘 그 쪽지를 낸 데스크톱이다.
({int port, String path})? desktopLocal(Uri u) {
  const hosts = {'localhost', '127.0.0.1', '::1', '[::1]', '0.0.0.0'};
  if (!(u.scheme == 'http' || u.scheme == 'https') || !hosts.contains(u.host)) {
    return null;
  }
  final port = u.hasPort ? u.port : (u.scheme == 'https' ? 443 : 80);
  final path =
      (u.path.isEmpty ? '/' : u.path) + (u.hasQuery ? '?${u.query}' : '');
  return (port: port, path: path);
}

/// 보여 주기 링크를 연다 — 데스크톱 localhost 면 앱 안 웹뷰, 아니면 밖(사파리).
Future<void> openShownLink(
  NavigatorState nav,
  Server server,
  Uri u, {
  String? machine,
}) async {
  final local = desktopLocal(u);
  if (local == null) {
    await launchUrl(u, mode: LaunchMode.externalApplication);
    return;
  }
  await nav.push(
    MaterialPageRoute<void>(
      builder: (_) => DevServerScreen(
        server: server,
        port: local.port,
        machine: machine,
        path: local.path,
      ),
    ),
  );
}

/// 포트(와 경로)를 물어 개발 서버 화면을 연다.
Future<void> openDevServer(
  BuildContext context, {
  required Server server,
  String? machine,
}) async {
  final port = TextEditingController();
  final path = TextEditingController(text: '/');
  final ok = await showDialog<bool>(
    context: context,
    builder: (context) => AlertDialog(
      title: const Text('데스크톱 개발 서버'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          TextField(
            controller: port,
            autofocus: true,
            keyboardType: TextInputType.number,
            decoration: const InputDecoration(
              labelText: '포트',
              hintText: '3000',
            ),
          ),
          TextField(
            controller: path,
            keyboardType: TextInputType.url,
            decoration: const InputDecoration(labelText: '경로'),
          ),
        ],
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context, false),
          child: const Text('취소'),
        ),
        FilledButton(
          onPressed: () => Navigator.pop(context, true),
          child: const Text('열기'),
        ),
      ],
    ),
  );
  final n = int.tryParse(port.text.trim());
  if (ok != true || n == null || n <= 0 || n > 65535 || !context.mounted) {
    return;
  }
  await Navigator.of(context).push(
    MaterialPageRoute<void>(
      builder: (_) => DevServerScreen(
        server: server,
        port: n,
        machine: machine,
        path: path.text.trim().isEmpty ? '/' : path.text.trim(),
      ),
    ),
  );
}
