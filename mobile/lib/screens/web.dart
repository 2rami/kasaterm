import 'package:flutter/foundation.dart' show kIsWeb;
import 'package:flutter/material.dart';
import 'package:url_launcher/url_launcher.dart';
import 'package:webview_flutter/webview_flutter.dart';

import '../control_session.dart';

/// 제어 채널의 `open-url` 을 연다 — web 이면 앱 안 웹 화면, 아니면 이 폰의 브라우저.
/// 크롬 개발 루프(kIsWeb)에는 webview 가 없어 둘 다 새 탭이다. 실패하면 던진다 —
/// [ControlSession] 이 그 문구로 `ok:false` 를 답한다.
Future<void> openControlUrl(NavigatorState nav, OpenUrlFrame frame) async {
  if (frame.web && !kIsWeb) {
    nav.push(
      MaterialPageRoute<void>(builder: (_) => WebScreen(url: frame.url)),
    );
    return;
  }
  await openExternally(frame.url);
}

/// 이 폰의 브라우저(사파리)로. 못 열면 던진다.
Future<void> openExternally(Uri url) async {
  final ok = await launchUrl(url, mode: LaunchMode.externalApplication);
  if (!ok) throw Exception('브라우저가 열리지 않았다');
}

/// 앱 안 웹 화면 — 주소 표시·뒤로·새로고침·사파리로 열기. 학생이 「폰에서 보라」고
/// 띄운 페이지를 앱을 떠나지 않고 본다.
class WebScreen extends StatefulWidget {
  const WebScreen({super.key, required this.url});

  final Uri url;

  @override
  State<WebScreen> createState() => _WebScreenState();
}

class _WebScreenState extends State<WebScreen> {
  late final WebViewController _controller;
  Uri _current = Uri();
  bool _loading = true;
  bool _canGoBack = false;

  @override
  void initState() {
    super.initState();
    _current = widget.url;
    _controller = WebViewController()
      ..setJavaScriptMode(JavaScriptMode.unrestricted)
      ..setNavigationDelegate(
        NavigationDelegate(
          onPageStarted: (u) => _track(u, loading: true),
          onPageFinished: (u) => _track(u, loading: false),
          onUrlChange: (c) {
            final u = c.url;
            if (u != null) _track(u, loading: _loading);
          },
        ),
      )
      ..loadRequest(widget.url);
  }

  Future<void> _track(String url, {required bool loading}) async {
    final u = Uri.tryParse(url);
    final back = await _controller.canGoBack();
    if (!mounted) return;
    setState(() {
      if (u != null) _current = u;
      _loading = loading;
      _canGoBack = back;
    });
  }

  Future<void> _back() async {
    if (await _controller.canGoBack()) {
      await _controller.goBack();
      return;
    }
    if (mounted) Navigator.of(context).pop();
  }

  Future<void> _safari() async {
    try {
      await openExternally(_current);
    } catch (_) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(content: Text('브라우저가 열리지 않았다')),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Scaffold(
      appBar: AppBar(
        leading: IconButton(
          tooltip: '뒤로',
          icon: const Icon(Icons.arrow_back),
          onPressed: _back,
        ),
        title: Text(
          addressLabel(_current),
          style: theme.textTheme.titleSmall,
          maxLines: 1,
          overflow: TextOverflow.ellipsis,
        ),
        actions: [
          IconButton(
            tooltip: '새로고침',
            icon: const Icon(Icons.refresh),
            onPressed: () => _controller.reload(),
          ),
          IconButton(
            tooltip: '사파리로 열기',
            icon: const Icon(Icons.open_in_browser),
            onPressed: _safari,
          ),
          IconButton(
            tooltip: '닫기',
            icon: const Icon(Icons.close),
            onPressed: () => Navigator.of(context).pop(),
          ),
        ],
        bottom: PreferredSize(
          preferredSize: const Size.fromHeight(2),
          child: _loading
              ? const LinearProgressIndicator(minHeight: 2)
              : const SizedBox(height: 2),
        ),
      ),
      body: PopScope(
        canPop: !_canGoBack,
        onPopInvokedWithResult: (didPop, _) {
          if (!didPop) _controller.goBack();
        },
        child: WebViewWidget(controller: _controller),
      ),
    );
  }
}

/// 상단 바에 보일 주소 — 스킴은 빼고 host 와 path 만. 쿼리는 길어 자른다.
String addressLabel(Uri u) {
  final port = u.hasPort ? ':${u.port}' : '';
  final path = u.path == '/' ? '' : u.path;
  return '${u.host}$port$path';
}
