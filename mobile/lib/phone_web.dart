import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:webview_flutter/webview_flutter.dart';

import 'server.dart';

/// 폰 브라우저 — 에이전트가 데스크톱에서 다룰 수 있는 앱 안 웹뷰(WKWebView).
///
/// 데스크톱은 폰에 먼저 닿지 못해 명령을 줄 세워 두고(`kasa_mcp::phone_web`), 앱이 앞에 있는 동안 이 쪽이 길게
/// 기다리며 가져간다. 사람이 보는 링크는 그대로 Safari 보기로 열린다(로그인·자동 채우기가 거기 있다) — 이 화면은
/// 스크립트로 읽고 찍을 수 있는 쪽이다.
class PhoneWebAgent {
  PhoneWebAgent._();
  static final instance = PhoneWebAgent._();

  /// 화면을 띄울 길 — 앱이 넘겨준다(main.dart 의 navigatorKey).
  NavigatorState? Function()? navigator;

  static const _snapshots = MethodChannel('kasaterm/phone_web');

  Server? _server;
  bool _foreground = true;
  int _gen = 0;
  WebViewController? _controller;
  final ValueNotifier<String> address = ValueNotifier('');
  final ValueNotifier<bool> loading = ValueNotifier(false);
  bool _shown = false;
  Completer<void>? _loaded;

  void bind(Server? server) {
    if (identical(server, _server)) return;
    _server = server;
    _restart();
  }

  void setForeground(bool foreground) {
    if (_foreground == foreground) return;
    _foreground = foreground;
    _restart();
  }

  void _restart() {
    final gen = ++_gen;
    if (_server == null || !_foreground) return;
    unawaited(_loop(gen));
  }

  Future<void> _loop(int gen) async {
    final server = _server;
    if (server == null) return;
    var backoff = 2;
    while (gen == _gen) {
      try {
        final command = await server.phoneWebNext();
        if (gen != _gen) return;
        backoff = 2;
        if (command == null) continue;
        await _answer(server, command);
      } on ServerException catch (e) {
        if (gen != _gen) return;
        // 이 기능이 없는 데스크톱 판이면 묻기를 그만둔다 — 판이 바뀌면 다시 연결될 때 새로 묶인다.
        if (e.status == 404 || e.status == 403) return;
        await Future<void>.delayed(Duration(seconds: backoff));
        backoff = (backoff * 2).clamp(2, 30);
      }
    }
  }

  Future<void> _answer(Server server, PhoneWebCommand command) async {
    try {
      if (command.op == 'shot') {
        final png = await _shot();
        await server.phoneWebResult(command.id, png: png);
        return;
      }
      final value = await _run(command);
      await server.phoneWebResult(command.id, json: value);
    } catch (e) {
      await server.phoneWebResult(command.id, json: {'error': '$e'});
    }
  }

  WebViewController get controller => _controller ??= _make();

  WebViewController _make() {
    final c = WebViewController()
      ..setJavaScriptMode(JavaScriptMode.unrestricted)
      ..setNavigationDelegate(NavigationDelegate(
        onPageStarted: (url) {
          address.value = url;
          loading.value = true;
        },
        onPageFinished: (url) {
          address.value = url;
          loading.value = false;
          _loaded?.complete();
          _loaded = null;
        },
        onWebResourceError: (error) {
          if (error.isForMainFrame ?? true) {
            loading.value = false;
            _loaded?.complete();
            _loaded = null;
          }
        },
      ));
    return c;
  }

  /// 화면이 없으면 띄운다 — 찍기는 화면에 보이는 웹뷰만 된다.
  Future<void> _show() async {
    if (_shown) return;
    final nav = navigator?.call();
    if (nav == null) throw StateError('앱 화면이 아직 준비되지 않았다');
    unawaited(nav.push(MaterialPageRoute<void>(builder: (_) => const PhoneWebScreen())));
    // 웹뷰가 창에 붙을 때까지 한 박자.
    await Future<void>.delayed(const Duration(milliseconds: 600));
  }

  Future<Map<String, Object?>> _page() async {
    final title = await controller.getTitle() ?? '';
    return {'url': await controller.currentUrl() ?? '', 'title': title, 'loading': loading.value};
  }

  Future<void> _waitLoaded(Duration within) async {
    final loaded = _loaded ??= Completer<void>();
    await loaded.future.timeout(within, onTimeout: () {});
  }

  Future<Map<String, Object?>> _run(PhoneWebCommand command) async {
    switch (command.op) {
      case 'open':
        final uri = Uri.tryParse(command.arg.trim());
        if (uri == null || !(uri.isScheme('http') || uri.isScheme('https'))) {
          throw ArgumentError('http 나 https 주소만 연다');
        }
        await _show();
        _loaded = Completer<void>();
        await controller.loadRequest(uri);
        await _waitLoaded(const Duration(seconds: 20));
        return _page();
      case 'url':
        return _page();
      case 'text':
        final text = await controller.runJavaScriptReturningResult(
            'document.body ? document.body.innerText.slice(0, 200000) : ""');
        return {...await _page(), 'text': _plain(text)};
      case 'eval':
        final value = await controller.runJavaScriptReturningResult(command.arg);
        return {'value': _plain(value)};
      case 'back':
        if (await controller.canGoBack()) await controller.goBack();
        return _page();
      case 'forward':
        if (await controller.canGoForward()) await controller.goForward();
        return _page();
      case 'reload':
        _loaded = Completer<void>();
        await controller.reload();
        await _waitLoaded(const Duration(seconds: 20));
        return _page();
    }
    throw ArgumentError('모르는 명령 ${command.op}');
  }

  /// WKWebView 는 문자열을 JSON 따옴표째 돌려줄 때가 있다 — 사람이 읽는 값으로 푼다.
  static Object? _plain(Object? value) {
    if (value is String && value.length >= 2 && value.startsWith('"') && value.endsWith('"')) {
      try {
        return jsonDecode(value);
      } catch (_) {}
    }
    return value;
  }

  Future<List<int>> _shot() async {
    if (_controller == null) throw StateError('폰 브라우저에 연 페이지가 없다 — open 을 먼저');
    await _show();
    final png = await _snapshots.invokeMethod<Uint8List>('snapshot');
    if (png == null || png.isEmpty) throw StateError('웹뷰를 찍지 못했다');
    return png;
  }
}

/// 폰 브라우저 화면. 에이전트가 연 페이지를 사람이 보고, 직접 이동·뒤로·새로고침도 한다.
class PhoneWebScreen extends StatefulWidget {
  const PhoneWebScreen({super.key});

  @override
  State<PhoneWebScreen> createState() => _PhoneWebScreenState();
}

class _PhoneWebScreenState extends State<PhoneWebScreen> {
  final _agent = PhoneWebAgent.instance;
  late final TextEditingController _field = TextEditingController(text: _agent.address.value);
  final _focus = FocusNode();

  @override
  void initState() {
    super.initState();
    _agent._shown = true;
    _agent.address.addListener(_follow);
  }

  @override
  void dispose() {
    _agent._shown = false;
    _agent.address.removeListener(_follow);
    _field.dispose();
    _focus.dispose();
    super.dispose();
  }

  void _follow() {
    if (!_focus.hasFocus) _field.text = _agent.address.value;
  }

  void _go(String text) {
    var t = text.trim();
    if (t.isEmpty) return;
    if (!t.contains('://')) t = t.contains('.') && !t.contains(' ') ? 'https://$t' : 'https://www.google.com/search?q=${Uri.encodeQueryComponent(t)}';
    final uri = Uri.tryParse(t);
    if (uri != null) unawaited(_agent.controller.loadRequest(uri));
  }

  @override
  Widget build(BuildContext context) {
    final c = _agent.controller;
    return Scaffold(
      appBar: AppBar(
        titleSpacing: 0,
        title: TextField(
          controller: _field,
          focusNode: _focus,
          keyboardType: TextInputType.url,
          textInputAction: TextInputAction.go,
          autocorrect: false,
          decoration: const InputDecoration(hintText: '주소 또는 검색', border: InputBorder.none, isDense: true),
          onSubmitted: _go,
        ),
        actions: [
          IconButton(tooltip: '뒤로', icon: const Icon(Icons.arrow_back_ios_new, size: 18), onPressed: () => c.goBack()),
          IconButton(tooltip: '앞으로', icon: const Icon(Icons.arrow_forward_ios, size: 18), onPressed: () => c.goForward()),
          ValueListenableBuilder<bool>(
            valueListenable: _agent.loading,
            builder: (_, busy, _) => IconButton(
              tooltip: busy ? '멈춤' : '새로고침',
              icon: Icon(busy ? Icons.close : Icons.refresh, size: 20),
              onPressed: () => busy ? c.loadRequest(Uri.parse('about:blank')) : c.reload(),
            ),
          ),
        ],
        bottom: PreferredSize(
          preferredSize: const Size.fromHeight(2),
          child: ValueListenableBuilder<bool>(
            valueListenable: _agent.loading,
            builder: (_, busy, _) => busy ? const LinearProgressIndicator(minHeight: 2) : const SizedBox(height: 2),
          ),
        ),
      ),
      body: WebViewWidget(controller: c),
    );
  }
}
