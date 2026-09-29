import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:url_launcher/url_launcher.dart';

import '../net_tcp.dart';
import '../server.dart';
import '../look.dart';
import 'controls.dart';

/// 데스크톱 개발 서버를 앱 안 Safari 화면(안드로이드는 크롬 커스텀 탭)으로 연다 — 데스크톱 `127.0.0.1:port` 를
/// 이 폰 localhost 로 끌어와 그 주소를 연다. 임베디드 웹뷰가 아니라서 로그인이 앱별로 남고 비밀번호 자동 채우기·
/// 구글 OAuth 가 된다.
///
/// 입구 수명은 이 화면에 건다 — Safari 화면은 닫혀도 알려 주지 않아서(url_launcher 는 첫 로드까지만 답한다), 이 화면이
/// 스택에 있는 동안 입구가 산다. 길(직통·관문)은 Safari 화면에 못 넣으니 여기서 열기 전에 잠깐, 닫고 돌아와서도 보인다.
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
  String? _error;
  // 첫 열기까지 단추 대신 도는 표시. launchUrl 은 첫 로드가 끝나야(또는 그 전에 닫아야) 답한다.
  bool _launching = true;

  /// 열기 전에 길 한 줄을 읽을 틈.
  static const _showPath = Duration(milliseconds: 700);

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
      setState(() => _bridge = bridge);
    } catch (e) {
      if (mounted) {
        setState(() {
          _error = '이 폰에 localhost 를 열지 못했어요 — $e';
          _launching = false;
        });
      }
      return;
    }
    await Future<void>.delayed(_showPath);
    if (mounted) await _launch();
  }

  Uri get _uri {
    final path = widget.path.startsWith('/') ? widget.path : '/${widget.path}';
    return Uri.parse('http://localhost:${_bridge!.localPort}$path');
  }

  Future<void> _launch() async {
    setState(() {
      _error = null;
      _launching = true;
    });
    String? error;
    try {
      // false 는 첫 로드 전에 닫은 것 — 사람이 닫았으니 알릴 것이 없다.
      await launchUrl(_uri, mode: LaunchMode.inAppBrowserView);
    } on PlatformException {
      error = '데스크톱 localhost:${widget.port} 첫 화면을 못 받았어요 — 개발 서버가 떠 있는지 봐 주세요';
    }
    if (mounted) {
      setState(() {
        _error = error;
        _launching = false;
      });
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
    final scheme = theme.colorScheme;
    final local = _bridge?.localPort;
    final direct = widget.server.pathOf(widget.machine)?.$1 ?? false;
    return Scaffold(
      appBar: AppBar(title: Text('localhost:${widget.port}')),
      body: Center(
        child: Padding(
          padding: const EdgeInsets.all(Look.groupGap),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(
                direct ? Icons.bolt_rounded : Icons.cloud_outlined,
                size: 40,
                color: direct ? scheme.primary : scheme.onSurfaceVariant,
              ),
              const SizedBox(height: 12),
              Text(_pathLine, style: theme.textTheme.titleMedium),
              if (local != null && local != widget.port) ...[
                const SizedBox(height: 4),
                Text(
                  '이 폰 localhost:$local',
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: scheme.onSurfaceVariant,
                  ),
                ),
              ],
              if (_error != null) ...[
                const SizedBox(height: 16),
                Text(
                  _error!,
                  textAlign: TextAlign.center,
                  style: TextStyle(color: scheme.error),
                ),
              ],
              const SizedBox(height: 24),
              if (_launching)
                const CircularProgressIndicator()
              else if (local != null)
                FilledButton.icon(
                  onPressed: _launch,
                  icon: const Icon(Icons.open_in_new_rounded),
                  label: const Text('다시 열기'),
                )
              // 다리를 못 세운 실패도 문장만 두지 않는다 — 다음 행동 단추를 단다.
              else if (_error != null)
                FilledButton.icon(
                  onPressed: () {
                    setState(() {
                      _error = null;
                      _launching = true;
                    });
                    _open();
                  },
                  icon: const Icon(Icons.refresh_rounded),
                  label: const Text('다시 시도'),
                ),
            ],
          ),
        ),
      ),
    );
  }
}

/// 데스크톱이 보여 주기로 넘긴 그 기계의 localhost 주소면 (포트, 경로·쿼리) — 입구를 세워 앱 안 Safari 화면으로 연다. 새 판 폰 앱이
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

/// 보여 주기 링크를 앱 안 Safari 화면으로 연다 — 데스크톱 localhost 면 입구를 세워서.
Future<void> openShownLink(
  NavigatorState nav,
  Server server,
  Uri u, {
  String? machine,
}) async {
  final local = desktopLocal(u);
  if (local == null) {
    await launchUrl(u, mode: LaunchMode.inAppBrowserView);
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
    builder: (context) => ModalLook(
      child: AlertDialog(
        title: const Text('데스크톱 개발 서버'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            LabeledField(
              label: '포트',
              child: TextField(
                controller: port,
                autofocus: true,
                keyboardType: TextInputType.number,
                decoration: const InputDecoration(hintText: '3000'),
              ),
            ),
            const SizedBox(height: Look.fieldGap),
            LabeledField(
              label: '경로',
              child: TextField(
                controller: path,
                keyboardType: TextInputType.url,
                decoration: const InputDecoration(),
              ),
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
