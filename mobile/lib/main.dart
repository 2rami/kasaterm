import 'package:flutter/material.dart';

import 'address_store.dart';
import 'app_link.dart';
import 'screens/connect.dart';
import 'screens/hub.dart';
import 'screens/terminal.dart';
import 'server.dart';

final navigatorKey = GlobalKey<NavigatorState>();

/// 붙은 기계의 색. MaterialApp 의 테마로 들어가야 한다 — RootScreen 안에서 Theme 으로
/// 감싸면 Navigator 가 위에 올리는 학생 화면(다른 라우트)에는 안 닿아, 허브만 데스크톱
/// 색이고 상단 바·키 줄·입력창은 기본 흰색으로 남았다.
final designTokens = ValueNotifier<DesignTokens?>(null);

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  AppLinkObserver.instance.install();
  runApp(const KasatermApp());
}

/// DESIGN.md 의 「SCHALE 작업대」— 흰색·연하늘 표면 위 네이비 잉크, 강조는 하늘색
/// 하나. 다크는 같은 역할을 깊은 네이비 층으로 뒤집는다. 그림자 없이 톤과 한 줄
/// 경계로만 층을 만든다. 데스크톱 색을 못 받았을 때의 기본 얼굴이다.
ThemeData buildTheme(Brightness brightness) {
  final dark = brightness == Brightness.dark;
  return buildThemeFrom(
    brightness: brightness,
    primary: dark ? const Color(0xff7ab8ff) : const Color(0xff4a90e2),
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
  Widget build(BuildContext context) => ValueListenableBuilder<DesignTokens?>(
    valueListenable: designTokens,
    // 데스크톱 색을 받았으면 폰의 라이트·다크 설정과 상관없이 그 얼굴이다.
    builder: (context, tokens, _) => MaterialApp(
      navigatorKey: navigatorKey,
      debugShowCheckedModeBanner: false,
      title: 'kasaterm',
      theme: tokens == null
          ? buildTheme(Brightness.light)
          : themeFromTokens(tokens),
      darkTheme: tokens == null
          ? buildTheme(Brightness.dark)
          : themeFromTokens(tokens),
      home: const RootScreen(),
    ),
  );
}

/// 저장된 주소가 있으면 허브, 없으면 연결 화면. 주소를 바꾸거나 지우면 다시 여기로.
class RootScreen extends StatefulWidget {
  const RootScreen({super.key});

  @override
  State<RootScreen> createState() => _RootScreenState();
}

class _RootScreenState extends State<RootScreen> {
  static const _store = AddressStore();

  /// 검증용: 빌드 때 `KASA_OPEN_PANE`(과 `KASA_OPEN_MACHINE`)을 주면 켜자마자 그 학생
  /// 화면을 연다 — 시뮬레이터는 탭을 못 보내니 링크와 같은 길로 화면을 꺼내 본다.
  static const _openPane = String.fromEnvironment('KASA_OPEN_PANE');
  static const _openMachine = String.fromEnvironment('KASA_OPEN_MACHINE');

  @override
  void initState() {
    super.initState();
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
    super.dispose();
  }

  /// 웹에서 건너뛴 링크. root 는 **주소가 하나도 없을 때만** 받는다 — 링크 한 줄로
  /// 저장된 주소를 갈아치우게 두면 남이 보낸 링크가 자격을 바꾸는 문이 된다.
  Future<void> _openLink(AppLink link) async {
    var server = _server ?? await _initial;
    if (server == null) {
      final root = Server.parse(link.root ?? '');
      if (root == null) return;
      final candidate = Server(root);
      try {
        await candidate.me();
      } on ServerException {
        return;
      }
      await _connected(candidate);
      server = candidate;
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
    if (found == null || nav == null || !mounted) return;
    final s = server;
    nav.popUntil((r) => r.isFirst);
    nav.push(
      MaterialPageRoute<void>(
        builder: (_) =>
            TerminalScreen(server: s, pane: found, initialScroll: link.scroll),
      ),
    );
  }

  late Future<Server?> _initial = _load();
  Server? _server;

  /// 빌드 때 `KASA_ROOT` 로 구워 넣은 주소(tool/phone.sh). 자기 맥에서 만들어 자기
  /// 폰에 넣는 판은 주소를 처음부터 알고 있어 연결 화면을 안 거친다. 저장된 주소가
  /// 있으면 그쪽이 우선이고, 「주소 지우기」는 그 자리에서 연결 화면을 보이되 다음
  /// 실행에는 다시 이 값으로 돌아온다.
  static const _baked = String.fromEnvironment('KASA_ROOT');

  Future<Server?> _load() async {
    final root = await _store.load() ?? Uri.tryParse(_baked);
    if (root == null || !root.hasScheme) return null;
    final server = Server(root);
    _loadTokens(server);
    return server;
  }

  void _loadTokens(Server server) {
    server.designTokens().then((t) {
      if (t == null || !mounted || (_server != null && _server != server)) {
        return;
      }
      designTokens.value = t;
    });
  }

  Future<void> _connected(Server server) async {
    await _store.save(server.root);
    if (!mounted) return;
    setState(() {
      _server = server;
      _initial = Future.value(server);
    });
    designTokens.value = null;
    _loadTokens(server);
  }

  Future<void> _disconnected() async {
    await _store.clear();
    if (!mounted) return;
    setState(() {
      _server = null;
      _initial = Future.value(null);
    });
    designTokens.value = null;
  }

  @override
  Widget build(BuildContext context) => FutureBuilder<Server?>(
    future: _initial,
    builder: (context, snap) {
      if (snap.connectionState != ConnectionState.done) {
        return const Scaffold(body: Center(child: CircularProgressIndicator()));
      }
      final server = _server ?? snap.data;
      if (server == null) return ConnectScreen(onConnected: _connected);
      return HubScreen(
        key: ValueKey(server.root),
        server: server,
        onChangeAddress: _disconnected,
      );
    },
  );
}
