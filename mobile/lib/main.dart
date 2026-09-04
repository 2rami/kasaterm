import 'package:flutter/material.dart';

import 'address_store.dart';
import 'screens/connect.dart';
import 'screens/hub.dart';
import 'server.dart';

void main() => runApp(const KasatermApp());

/// DESIGN.md 의 「SCHALE 작업대」— 흰색·연하늘 표면 위 네이비 잉크, 강조는 하늘색
/// 하나. 다크는 같은 역할을 깊은 네이비 층으로 뒤집는다. 그림자 없이 톤과 한 줄
/// 경계로만 층을 만든다.
ThemeData buildTheme(Brightness brightness) {
  final dark = brightness == Brightness.dark;
  final scheme = ColorScheme(
    brightness: brightness,
    primary: dark ? const Color(0xff7ab8ff) : const Color(0xff4a90e2),
    onPrimary: dark ? const Color(0xff0f1b2d) : Colors.white,
    secondary: dark ? const Color(0xff9fc5f0) : const Color(0xff2f63c4),
    onSecondary: dark ? const Color(0xff0f1b2d) : Colors.white,
    error: dark ? const Color(0xffff7a93) : const Color(0xffc4304f),
    onError: Colors.white,
    surface: dark ? const Color(0xff16243a) : Colors.white,
    onSurface: dark ? const Color(0xffe6eef8) : const Color(0xff15294a),
    surfaceContainerHighest: dark ? const Color(0xff213247) : const Color(0xffe3eefb),
    onSurfaceVariant: dark ? const Color(0xffa9b8cf) : const Color(0xff5b6b8a),
    outline: dark ? const Color(0xff2a3b55) : const Color(0xffd6e0ee),
  );
  final background = dark ? const Color(0xff0f1b2d) : const Color(0xfff5f9ff);
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
    dividerTheme: DividerThemeData(color: scheme.outline, space: 1, thickness: 1),
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
  Widget build(BuildContext context) => MaterialApp(
        title: 'kasaterm',
        theme: buildTheme(Brightness.light),
        darkTheme: buildTheme(Brightness.dark),
        home: const RootScreen(),
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
  late Future<Server?> _initial = _load();
  Server? _server;

  /// 빌드 때 `KASA_ROOT` 로 구워 넣은 주소(tool/phone.sh). 자기 맥에서 만들어 자기
  /// 폰에 넣는 판은 주소를 처음부터 알고 있어 연결 화면을 안 거친다. 저장된 주소가
  /// 있으면 그쪽이 우선이고, 「주소 지우기」는 그 자리에서 연결 화면을 보이되 다음
  /// 실행에는 다시 이 값으로 돌아온다.
  static const _baked = String.fromEnvironment('KASA_ROOT');

  Future<Server?> _load() async {
    final root = await _store.load() ?? Uri.tryParse(_baked);
    return root == null || !root.hasScheme ? null : Server(root);
  }

  Future<void> _connected(Server server) async {
    await _store.save(server.root);
    if (!mounted) return;
    setState(() {
      _server = server;
      _initial = Future.value(server);
    });
  }

  Future<void> _disconnected() async {
    await _store.clear();
    if (!mounted) return;
    setState(() {
      _server = null;
      _initial = Future.value(null);
    });
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
