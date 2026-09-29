import 'dart:async';

import 'package:flutter/material.dart';

import 'connection.dart';
import 'app_link.dart';
import 'hub_model.dart';
import 'kasanet.dart';
import 'push.dart';
import 'screens/connect.dart';
import 'screens/conversation_view.dart';
import 'screens/nacho_home.dart';
import 'screens/terminal.dart';
import 'server.dart';
import 'theme_prefs.dart';

final navigatorKey = GlobalKey<NavigatorState>();

/// 붙은 기계의 색. MaterialApp 의 테마로 들어가야 한다 — RootScreen 안에서 Theme 으로
/// 감싸면 Navigator 가 위에 올리는 학생 화면(다른 라우트)에는 안 닿아, 허브만 데스크톱
/// 색이고 상단 바·키 줄·입력창은 기본 흰색으로 남았다.
final designTokens = ValueNotifier<DesignTokens?>(null);

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  AppLinkObserver.instance.install();
  const PaneViewPrefs().load().then((v) => paneView.value = v);
  runApp(const KasatermApp());
}

ThemeData buildTheme(Brightness brightness) {
  final dark = brightness == Brightness.dark;
  return buildThemeFrom(
    brightness: brightness,
    primary: dark ? const Color(0xff7ab8ff) : const Color(0xff326fb8),
    onPrimary: dark ? const Color(0xff0f1b2d) : Colors.white,
    error: dark ? const Color(0xffff7a93) : const Color(0xffc4304f),
    surface: dark ? const Color(0xff16243a) : Colors.white,
    onSurface: dark ? const Color(0xffe6eef8) : const Color(0xff15294a),
    surfaceHigh: dark ? const Color(0xff213247) : const Color(0xffe3eefb),
    onSurfaceVariant: dark ? const Color(0xffa9b8cf) : const Color(0xff5b6b8a),
    outline: dark ? const Color(0xff2a3b55) : const Color(0xffd6e0ee),
    background: dark ? const Color(0xff0f1b2d) : const Color(0xfff5f9ff),
  );
}

/// 데스크톱이 지금 쓰는 색 그대로 — 허브·상단 바·입력창까지 같은 얼굴이 된다.
ThemeData themeFromTokens(DesignTokens t) => buildThemeFrom(
  brightness: t.dark ? Brightness.dark : Brightness.light,
  primary: Color(t.accent),
  onPrimary: Color(t.onAccent),
  error: Color(t.danger),
  surface: Color(t.surface),
  onSurface: Color(t.text),
  surfaceHigh: Color(t.surfaceHover),
  onSurfaceVariant: Color(t.textDim),
  outline: Color(t.border),
  background: Color(t.bg),
);

ThemeData buildThemeFrom({
  required Brightness brightness,
  required Color primary,
  required Color onPrimary,
  required Color error,
  required Color surface,
  required Color onSurface,
  required Color surfaceHigh,
  required Color onSurfaceVariant,
  required Color outline,
  required Color background,
}) {
  final scheme = ColorScheme(
    brightness: brightness,
    primary: primary,
    onPrimary: onPrimary,
    secondary: primary,
    onSecondary: onPrimary,
    error: error,
    onError: Colors.white,
    surface: surface,
    onSurface: onSurface,
    surfaceContainerHighest: surfaceHigh,
    onSurfaceVariant: onSurfaceVariant,
    outline: outline,
  );
  return ThemeData(
    fontFamily: 'Pretendard',
    useMaterial3: true,
    colorScheme: scheme,
    scaffoldBackgroundColor: background,
    appBarTheme: AppBarTheme(
      backgroundColor: background,
      foregroundColor: scheme.onSurface,
      elevation: 0,
      scrolledUnderElevation: 0,
      centerTitle: false,
    ),
    dividerTheme: DividerThemeData(
      color: scheme.outline,
      space: 1,
      thickness: 1,
    ),
    cardTheme: CardThemeData(
      elevation: 0,
      color: scheme.surface,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(10),
        side: BorderSide(color: scheme.outline),
      ),
      margin: EdgeInsets.zero,
    ),
    inputDecorationTheme: InputDecorationTheme(
      filled: true,
      fillColor: scheme.surface,
      border: OutlineInputBorder(
        borderRadius: BorderRadius.circular(8),
        borderSide: BorderSide(color: scheme.outline),
      ),
      enabledBorder: OutlineInputBorder(
        borderRadius: BorderRadius.circular(8),
        borderSide: BorderSide(color: scheme.outline),
      ),
      focusedBorder: OutlineInputBorder(
        borderRadius: BorderRadius.circular(8),
        borderSide: BorderSide(color: scheme.primary, width: 1.5),
      ),
      contentPadding: const EdgeInsets.symmetric(horizontal: 12, vertical: 12),
    ),
    filledButtonTheme: FilledButtonThemeData(
      style: FilledButton.styleFrom(
        minimumSize: const Size(44, 48),
        shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(8)),
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
      ),
    ),
    snackBarTheme: const SnackBarThemeData(behavior: SnackBarBehavior.floating),
  );
}

class KasatermApp extends StatelessWidget {
  const KasatermApp({super.key});

  @override
  Widget build(BuildContext context) => ValueListenableBuilder<ThemeMode>(
    valueListenable: phoneThemeMode,
    builder: (context, mode, _) => ValueListenableBuilder<DesignTokens?>(
      valueListenable: designTokens,
      // 「데스크톱 따라감」이면 받은 색이 밝기와 상관없이 그 얼굴이다. 폰에서 밝게·
      // 어둡게를 골랐으면 데스크톱 색을 접고 폰 기본 얼굴을 그 밝기로.
      builder: (context, tokens, _) {
        final desktop = mode == ThemeMode.system ? tokens : null;
        return MaterialApp(
          navigatorKey: navigatorKey,
          debugShowCheckedModeBanner: false,
          title: 'KASA Mobile',
          themeMode: mode,
          theme: desktop == null
              ? buildTheme(Brightness.light)
              : themeFromTokens(desktop),
          darkTheme: desktop == null
              ? buildTheme(Brightness.dark)
              : themeFromTokens(desktop),
          home: const RootScreen(),
        );
      },
    ),
  );
}

/// 저장된 주소가 있으면 나쵸 창구(대화·작업), 없으면 연결 화면. 학생 허브는 나쵸 창구의
/// 오른쪽 위에서 들어간다. 주소를 바꾸거나 지우면 다시 여기로.
class RootScreen extends StatefulWidget {
  const RootScreen({super.key});

  @override
  State<RootScreen> createState() => _RootScreenState();
}

class _RootScreenState extends State<RootScreen> with WidgetsBindingObserver {
  final _connection = ConnectionController();
  Server? _boundServer;
  AppLink? _pendingLink;

  /// 검증용: 빌드 때 `KASA_OPEN_PANE`(과 `KASA_OPEN_MACHINE`)을 주면 켜자마자 그 학생
  /// 화면을 연다 — 시뮬레이터는 탭을 못 보내니 링크와 같은 길로 화면을 꺼내 본다.
  static const _openPane = String.fromEnvironment('KASA_OPEN_PANE');
  static const _openMachine = String.fromEnvironment('KASA_OPEN_MACHINE');

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    phoneThemeSync.bind(null);
    _connection.addListener(_changed);
    _connection.beforeDisconnect = PushBridge.instance.unbind;
    unawaited(PushBridge.instance.unbind());
    _connection.restore(bakedRoot: _baked);
    AppLinkObserver.instance.attach(_openLink);
    if (_openPane.isNotEmpty) {
      _openLink(
        AppLink(
          pane: _openPane,
          machine: _openMachine.isEmpty ? null : _openMachine,
        ),
      );
    }
  }

  @override
  void dispose() {
    AppLinkObserver.instance.detach();
    _connection.removeListener(_changed);
    _connection.dispose();
    phoneThemeSync.unbind();
    WidgetsBinding.instance.removeObserver(this);
    PushBridge.instance.unbind();
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) {
      unawaited(PushBridge.instance.retryCleanup());
      unawaited(phoneThemeSync.refresh());
      KasanetRouter.resumed();
      if (_connection.server == null) unawaited(_connection.retry());
    }
  }

  Future<void> _openLink(AppLink link) async {
    final server = _connection.server;
    // Links navigate the current session; they cannot install credentials.
    if (server == null || server.isClosed) {
      if (_connection.phase == ConnectionPhase.restoring) _pendingLink = link;
      return;
    }
    final pane = link.pane;
    if (pane == null) return;
    final List<Pane> panes;
    try {
      panes = await server.panes(machine: link.machine);
    } on ServerException {
      return;
    }
    final found = panes.where((p) => p.id == pane).firstOrNull;
    final nav = navigatorKey.currentState;
    if (found == null ||
        nav == null ||
        !mounted ||
        server.isClosed ||
        _connection.server != server) {
      return;
    }
    final s = server;
    nav.popUntil((r) => r.isFirst);
    nav.push(
      MaterialPageRoute<void>(
        builder: (_) =>
            TerminalScreen(server: s, pane: found, initialScroll: link.scroll),
      ),
    );
  }

  // An explicit logout record takes precedence over development launch defaults.
  static const _baked = String.fromEnvironment('KASA_ROOT');

  void _loadTokens(Server server) {
    // 서버가 정해지는 자리가 여기 하나라 푸시 등록도 같이 건다.
    PushBridge.instance.bind(server, _openLink);
    // 첫 화면은 나쵸 창구다 — 그동안 학생 목록을 받아 두면 허브가 빈 채로 안 열린다.
    unawaited(HubModel.warm(server));
    server.designTokens().then((t) {
      if (t == null ||
          !mounted ||
          server.isClosed ||
          _connection.server != server) {
        return;
      }
      designTokens.value = t;
    });
  }

  void _changed() {
    if (!mounted) return;
    final server = _connection.server;
    if (phoneThemeSync.account != _connection.account) {
      if (_connection.account != null) unawaited(PushBridge.instance.unbind());
      phoneThemeSync.bind(_connection.account);
      final account = _connection.account;
      phoneThemeSync.onUnauthorized = () {
        if (_connection.account == account) {
          unawaited(_connection.logout(revoke: false));
        }
      };
    }
    if (_boundServer != server) {
      _boundServer = server;
      designTokens.value = null;
      HubModel.clearCache();
      PaintingBinding.instance.imageCache.clear();
      PaintingBinding.instance.imageCache.clearLiveImages();
      PushBridge.instance.unbind();
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted && _connection.server == server) {
          navigatorKey.currentState?.popUntil((route) => route.isFirst);
          final pending = _pendingLink;
          _pendingLink = null;
          if (server != null && pending != null) unawaited(_openLink(pending));
        }
      });
      if (server != null) _loadTokens(server);
    }
    if (_connection.phase == ConnectionPhase.signedOut) _pendingLink = null;
    setState(() {});
  }

  @override
  Widget build(BuildContext context) {
    if (_connection.phase == ConnectionPhase.restoring) {
      return const Scaffold(body: Center(child: CircularProgressIndicator()));
    }
    final server = _connection.server;
    if (server == null && _connection.account == null) {
      return ConnectScreen(
        onConnected: _connection.connectLegacy,
        onLogin: _connection.login,
        message: _connection.message,
      );
    }
    if (server == null) return AccountWaitingScreen(connection: _connection);
    return NachoHome(
      key: ObjectKey(server),
      server: server,
      onChangeAddress: _connection.logout,
    );
  }
}

class AccountWaitingScreen extends StatelessWidget {
  const AccountWaitingScreen({super.key, required this.connection});
  final ConnectionController connection;

  @override
  Widget build(BuildContext context) {
    final checking = connection.phase == ConnectionPhase.checking;
    return Scaffold(
      appBar: AppBar(title: const Text('기기 연결')),
      body: SafeArea(
        child: ListView(
          padding: const EdgeInsets.all(24),
          children: [
            Text(
              '${connection.account!.account} 계정',
              style: Theme.of(context).textTheme.titleLarge,
            ),
            const SizedBox(height: 12),
            Text(
              checking
                  ? '로그인과 기기 상태를 확인하고 있어요.'
                  : connection.message ?? '연결할 데스크톱을 기다리고 있어요.',
            ),
            const SizedBox(height: 12),
            const Text(
              '데스크톱 카사텀을 켜고 같은 계정으로 로그인해 주세요. 이미 켜져 있다면 최신 버전인지 확인해 주세요.',
            ),
            for (final device in connection.devices.where(
              (d) => d['kind'] != 'phone',
            ))
              ListTile(
                contentPadding: EdgeInsets.zero,
                leading: const Icon(Icons.computer_outlined),
                title: Text('${device['label'] ?? device['device_id']}'),
                subtitle: Text(
                  device['online'] == true ? '온라인 · 연결 준비 확인 중' : '오프라인',
                ),
              ),
            const SizedBox(height: 20),
            FilledButton(
              onPressed: checking ? null : connection.retry,
              child: Text(checking ? '확인 중' : '연결 다시 확인'),
            ),
            const SizedBox(height: 8),
            OutlinedButton(
              style: OutlinedButton.styleFrom(minimumSize: const Size(44, 48)),
              onPressed: connection.logout,
              child: const Text('로그아웃'),
            ),
          ],
        ),
      ),
    );
  }
}
