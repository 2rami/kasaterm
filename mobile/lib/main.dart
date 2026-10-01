import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart' show SystemUiOverlayStyle;

import 'connection.dart';
import 'app_link.dart';
import 'desktop_palette.dart';
import 'hub_model.dart';
import 'hub_prefs.dart';
import 'kasanet.dart';
import 'look.dart';
import 'push.dart';
import 'screens/controls.dart';
import 'screens/connect.dart';
import 'screens/dev_server.dart';
import 'screens/conversation_view.dart';
import 'screens/hub.dart';
import 'screens/terminal.dart';
import 'server.dart';
import 'theme_prefs.dart';
import 'twins_loading.dart';
import 'weather/store.dart';

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
  // 모든 토큰을 채운다 — 비워 두면 Material 이 글자색·원색으로 떨어뜨려 방 상자 테가 글자색 60%,
  // 경고 띠가 원색 빨강으로 그려졌다(2026-09-29).
  final scheme = ColorScheme(
    brightness: brightness,
    primary: primary,
    onPrimary: onPrimary,
    secondary: primary,
    onSecondary: onPrimary,
    error: error,
    onError: Colors.white,
    errorContainer: Color.alphaBlend(error.withValues(alpha: Look.dangerTint), background),
    onErrorContainer: error,
    surface: surface,
    onSurface: onSurface,
    surfaceContainerLowest: background,
    surfaceContainerLow: Color.alphaBlend(surface.withValues(alpha: 0.5), background),
    surfaceContainer: surface,
    surfaceContainerHigh: surfaceHigh,
    surfaceContainerHighest: surfaceHigh,
    onSurfaceVariant: onSurfaceVariant,
    outline: outline,
    outlineVariant: outline,
  );
  final base = ThemeData(brightness: brightness, fontFamily: 'Pretendard', useMaterial3: true).textTheme;
  TextStyle? t(TextStyle? s, double size, FontWeight w, [double? height]) =>
      s?.copyWith(fontSize: size, fontWeight: w, height: height, color: onSurface, letterSpacing: 0);
  final text = base.copyWith(
    titleLarge: t(base.titleLarge, Look.title, FontWeight.w600),
    titleMedium: t(base.titleMedium, Look.group, FontWeight.w600),
    titleSmall: t(base.titleSmall, Look.body, FontWeight.w600),
    bodyLarge: t(base.bodyLarge, Look.body, FontWeight.w400, 1.4),
    bodyMedium: t(base.bodyMedium, Look.body, FontWeight.w400, 1.4),
    bodySmall: t(base.bodySmall, Look.sub, FontWeight.w400),
    labelLarge: t(base.labelLarge, Look.body, FontWeight.w600),
    labelMedium: t(base.labelMedium, Look.sub, FontWeight.w600),
    labelSmall: t(base.labelSmall, Look.chip, FontWeight.w600),
  );
  final line = BorderSide(color: outline);
  final corners = RoundedRectangleBorder(borderRadius: Look.corners);
  const pill = StadiumBorder();
  final press = WidgetStatePropertyAll(onSurface.withValues(alpha: Look.pressTint));
  final tone = Color.alphaBlend(surfaceHigh, background);
  final twin = TwinTone.on(brightness: brightness, surfaceHigh: surfaceHigh, background: background, outline: outline);
  // 쌍둥이 결 — 테 없이 채운다. 주 동작은 강조 채움, 일반은 옅은 톤 채움(design.md 「쌍둥이 결」).
  ButtonStyle filled(Color fill, Color ink) => ButtonStyle(
    backgroundColor: WidgetStateProperty.resolveWith(
      (s) => s.contains(WidgetState.disabled) ? tone.withValues(alpha: 0.6) : fill,
    ),
    foregroundColor: WidgetStateProperty.resolveWith(
      (s) => s.contains(WidgetState.disabled) ? onSurfaceVariant : ink,
    ),
    side: const WidgetStatePropertyAll(BorderSide.none),
    overlayColor: press,
    shape: WidgetStatePropertyAll(corners),
    minimumSize: const WidgetStatePropertyAll(Size(Look.tap, Look.buttonH)),
    padding: const WidgetStatePropertyAll(EdgeInsets.symmetric(horizontal: Look.buttonPadX)),
    textStyle: WidgetStatePropertyAll(text.labelLarge),
    elevation: const WidgetStatePropertyAll(0),
  );
  return ThemeData(
    fontFamily: 'Pretendard',
    useMaterial3: true,
    colorScheme: scheme,
    textTheme: text,
    extensions: [twin],
    scaffoldBackgroundColor: background,
    canvasColor: background,
    // 앱바 아래 선은 없다 — 바탕 머리 빛이 앱바 뒤까지 이어진다. 투명한 앱바는 상태줄 글자색을 바탕으로 못 잰다 —
    // 밝은 테마에서 시계·배터리가 흰 글자로 사라졌다.
    appBarTheme: AppBarTheme(
      systemOverlayStyle: brightness == Brightness.dark ? SystemUiOverlayStyle.light : SystemUiOverlayStyle.dark,
      backgroundColor: background,
      foregroundColor: onSurface,
      elevation: 0,
      scrolledUnderElevation: 0,
      centerTitle: false,
      toolbarHeight: Look.appBarH,
      titleTextStyle: text.titleLarge,
    ),
    tabBarTheme: TabBarThemeData(
      labelColor: primary,
      unselectedLabelColor: onSurfaceVariant,
      labelStyle: text.labelLarge,
      unselectedLabelStyle: text.labelLarge?.copyWith(fontWeight: FontWeight.w400),
      dividerColor: outline,
      indicatorSize: TabBarIndicatorSize.label,
      indicator: UnderlineTabIndicator(
        borderSide: BorderSide(color: primary, width: 3),
        borderRadius: const BorderRadius.vertical(top: Radius.circular(3)),
      ),
      splashFactory: NoSplash.splashFactory,
      overlayColor: press,
    ),
    dividerTheme: DividerThemeData(color: outline, space: 1, thickness: 1),
    cardTheme: CardThemeData(
      elevation: 0,
      color: twin.card,
      shape: RoundedRectangleBorder(
        borderRadius: Look.cardCorners,
        side: twin.cardEdge.a == 0 ? BorderSide.none : BorderSide(color: twin.cardEdge),
      ),
      margin: EdgeInsets.zero,
    ),
    listTileTheme: ListTileThemeData(
      minTileHeight: Look.row1,
      minVerticalPadding: 8,
      contentPadding: const EdgeInsets.symmetric(horizontal: Look.pagePad),
      iconColor: onSurfaceVariant,
      titleTextStyle: text.bodyLarge,
      subtitleTextStyle: text.bodySmall?.copyWith(color: onSurfaceVariant),
      shape: corners,
    ),
    inputDecorationTheme: InputDecorationTheme(
      filled: true,
      fillColor: tone,
      hintStyle: TextStyle(color: onSurfaceVariant),
      floatingLabelBehavior: FloatingLabelBehavior.never,
      border: OutlineInputBorder(borderRadius: Look.corners, borderSide: BorderSide.none),
      enabledBorder: OutlineInputBorder(borderRadius: Look.corners, borderSide: BorderSide.none),
      focusedBorder: OutlineInputBorder(
        borderRadius: Look.corners,
        borderSide: BorderSide(color: primary, width: 1.5),
      ),
      constraints: const BoxConstraints(minHeight: Look.tap),
      contentPadding: const EdgeInsets.symmetric(horizontal: 14, vertical: 12),
    ),
    filledButtonTheme: FilledButtonThemeData(style: filled(primary, onPrimary)),
    outlinedButtonTheme: OutlinedButtonThemeData(style: filled(tone, onSurface)),
    elevatedButtonTheme: ElevatedButtonThemeData(style: filled(tone, onSurface)),
    textButtonTheme: TextButtonThemeData(
      style: ButtonStyle(
        foregroundColor: WidgetStatePropertyAll(primary),
        overlayColor: press,
        shape: WidgetStatePropertyAll(corners),
        minimumSize: const WidgetStatePropertyAll(Size(Look.tap, Look.buttonH)),
        textStyle: WidgetStatePropertyAll(text.labelLarge),
      ),
    ),
    iconButtonTheme: IconButtonThemeData(
      style: IconButton.styleFrom(
        foregroundColor: onSurface,
        minimumSize: const Size(Look.tap, Look.tap),
        iconSize: Look.iconSize,
        shape: corners,
      ),
    ),
    // 알약 분할 — 고른 칸만 강조 물을 깐다. 테는 바깥 한 줄.
    segmentedButtonTheme: SegmentedButtonThemeData(
      style: ButtonStyle(
        backgroundColor: WidgetStateProperty.resolveWith(
          (s) => s.contains(WidgetState.selected) ? primary.withValues(alpha: 0.16) : Colors.transparent,
        ),
        foregroundColor: WidgetStateProperty.resolveWith(
          (s) => s.contains(WidgetState.selected) ? primary : onSurfaceVariant,
        ),
        side: WidgetStatePropertyAll(line),
        overlayColor: press,
        shape: const WidgetStatePropertyAll(pill),
        minimumSize: const WidgetStatePropertyAll(Size(Look.tap, Look.buttonH)),
        textStyle: WidgetStatePropertyAll(text.labelLarge),
      ),
    ),
    chipTheme: ChipThemeData(
      // M3 는 고른 칩을 color 로 채운다 — 고른 것만 강조 물, 나머지는 톤.
      color: WidgetStateProperty.resolveWith(
        (s) => s.contains(WidgetState.selected) ? primary.withValues(alpha: 0.16) : tone,
      ),
      backgroundColor: tone,
      selectedColor: primary.withValues(alpha: 0.16),
      disabledColor: tone,
      checkmarkColor: primary,
      side: WidgetStateBorderSide.resolveWith(
        (s) => s.contains(WidgetState.selected) ? BorderSide(color: primary) : BorderSide.none,
      ),
      labelStyle: text.labelMedium?.copyWith(color: onSurface),
      secondaryLabelStyle: text.labelMedium?.copyWith(color: primary),
      shape: pill,
      padding: const EdgeInsets.symmetric(horizontal: Look.chipPadX),
    ),
    checkboxTheme: CheckboxThemeData(
      fillColor: WidgetStateProperty.resolveWith(
        (s) => s.contains(WidgetState.selected) ? primary : Colors.transparent,
      ),
      checkColor: WidgetStatePropertyAll(onPrimary),
      side: WidgetStateBorderSide.resolveWith(
        (s) => BorderSide(color: s.contains(WidgetState.selected) ? primary : outline, width: 1.5),
      ),
    ),
    switchTheme: SwitchThemeData(
      trackColor: WidgetStateProperty.resolveWith(
        (s) => s.contains(WidgetState.selected) ? primary : tone,
      ),
      trackOutlineColor: const WidgetStatePropertyAll(Colors.transparent),
      thumbColor: WidgetStateProperty.resolveWith(
        (s) => s.contains(WidgetState.selected) ? onPrimary : onSurfaceVariant,
      ),
    ),
    // 대화상자·시트는 둥근 판이다 — 안의 주 동작 채움은 ModalLook 이 맡는다.
    dialogTheme: DialogThemeData(
      backgroundColor: Color.alphaBlend(surfaceHigh, background),
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(Look.dialogRadius)),
      titleTextStyle: text.titleLarge,
      contentTextStyle: text.bodyMedium,
    ),
    bottomSheetTheme: BottomSheetThemeData(
      backgroundColor: Color.alphaBlend(surface, background),
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.vertical(top: Radius.circular(Look.sheetRadius)),
      ),
    ),
    popupMenuTheme: PopupMenuThemeData(
      color: Color.alphaBlend(surfaceHigh, background),
      shape: RoundedRectangleBorder(borderRadius: Look.corners, side: line),
      textStyle: text.bodyMedium,
    ),
    progressIndicatorTheme: ProgressIndicatorThemeData(color: primary, linearTrackColor: Colors.transparent),
    snackBarTheme: SnackBarThemeData(
      behavior: SnackBarBehavior.floating,
      shape: RoundedRectangleBorder(borderRadius: Look.corners),
    ),
    badgeTheme: const BadgeThemeData(largeSize: 18, padding: EdgeInsets.symmetric(horizontal: 5)),
  );
}

/// 「데스크톱 따라감」이면 데스크톱 밝기 그대로, 밝게·어둡게를 골랐으면 같은 팔레트를 그 밝기로
/// 뒤집어 입는다 — 어느 쪽이든 데스크톱과 같은 테마다. 색을 못 받았을 때만 폰 기본 얼굴.
({ThemeMode mode, ThemeData light, ThemeData dark}) appThemes(
  ThemeMode mode,
  DesignTokens? tokens,
) => (
  mode: mode == ThemeMode.system && tokens != null
      ? (tokens.looksDark ? ThemeMode.dark : ThemeMode.light)
      : mode,
  light: tokens == null
      ? buildTheme(Brightness.light)
      : themeFromTokens(tokens.inBrightness(dark: false)),
  dark: tokens == null
      ? buildTheme(Brightness.dark)
      : themeFromTokens(tokens.inBrightness(dark: true)),
);

class KasatermApp extends StatelessWidget {
  const KasatermApp({super.key});

  @override
  Widget build(BuildContext context) => ValueListenableBuilder<ThemeMode>(
    valueListenable: phoneThemeMode,
    builder: (context, mode, _) => ValueListenableBuilder<DesignTokens?>(
      valueListenable: designTokens,
      builder: (context, tokens, _) {
        final themes = appThemes(mode, tokens);
        return MaterialApp(
          navigatorKey: navigatorKey,
          debugShowCheckedModeBanner: false,
          title: 'KASA Mobile',
          themeMode: themes.mode,
          theme: themes.light,
          darkTheme: themes.dark,
          home: const RootScreen(),
        );
      },
    ),
  );
}

/// 저장된 주소가 있으면 학생 허브, 없으면 연결 화면. 주소를 바꾸거나 지우면 다시 여기로.
class RootScreen extends StatefulWidget {
  const RootScreen({super.key});

  @override
  State<RootScreen> createState() => _RootScreenState();
}

class _RootScreenState extends State<RootScreen> with WidgetsBindingObserver {
  final _connection = ConnectionController();
  Server? _boundServer;
  AppLink? _pendingLink;

  /// 기다림 화면을 한 번 봤나 — 그 뒤의 「다시 확인」은 그 화면의 단추가 진행을 보이고,
  /// 쌍둥이는 켜고 처음 확인할 때만 나온다.
  bool _waited = false;

  /// 검증용: 빌드 때 `KASA_OPEN_PANE`(과 `KASA_OPEN_MACHINE`)을 주면 켜자마자 그 학생
  /// 화면을 연다 — 시뮬레이터는 탭을 못 보내니 링크와 같은 길로 화면을 꺼내 본다.
  static const _openPane = String.fromEnvironment('KASA_OPEN_PANE');
  static const _openMachine = String.fromEnvironment('KASA_OPEN_MACHINE');

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    phoneThemeSync.bind(null);
    weather.bind(null);
    _connection.addListener(_changed);
    _connection.beforeDisconnect = PushBridge.instance.unbind;
    unawaited(PushBridge.instance.unbind());
    _connection.restore(bakedRoot: _baked, preferBaked: _preferBaked);
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
    weather.unbind();
    WidgetsBinding.instance.removeObserver(this);
    PushBridge.instance.unbind();
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) {
      unawaited(PushBridge.instance.retryCleanup());
      unawaited(phoneThemeSync.refresh());
      unawaited(weather.refresh());
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
    final url = Uri.tryParse(link.url ?? '');
    final nav = navigatorKey.currentState;
    if (url != null && url.hasScheme && nav != null) {
      await openShownLink(nav, server, url, machine: link.machine);
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
  static const _preferBaked = bool.fromEnvironment('KASA_ROOT_REPLACE');

  void _loadTokens(Server server) {
    // 서버가 정해지는 자리가 여기 하나라 푸시 등록도 같이 건다.
    PushBridge.instance.bind(server, _openLink);
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
      weather.bind(_connection.account);
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
    if (_connection.phase == ConnectionPhase.signedOut) {
      _pendingLink = null;
      _waited = false;
    }
    if (_connection.phase == ConnectionPhase.waiting) _waited = true;
    setState(() {});
  }

  @override
  Widget build(BuildContext context) {
    final server = _connection.server;
    final phase = _connection.phase;
    // 로그인 중(계정이 아직 없음)에는 연결 화면을 남긴다 — 갈아 끼우면 친 아이디가 지워진다.
    if (phase == ConnectionPhase.restoring ||
        (phase == ConnectionPhase.checking &&
            _connection.account != null &&
            server == null &&
            !_waited)) {
      return const Scaffold(body: SafeArea(child: TwinsLoading()));
    }
    if (server == null && _connection.account == null) {
      return ConnectScreen(
        onConnected: _connection.connectLegacy,
        onLogin: _connection.login,
        message: _connection.message,
        relay: (origin) => _connection.relay(origin, null),
        installId: _connection.installId,
        onSession: _connection.adopt,
      );
    }
    if (server == null) return AccountWaitingScreen(connection: _connection);
    final account = server.account;
    return HubScreen(
      key: ObjectKey(server),
      server: server,
      onChangeAddress: _connection.logout,
      prefs: HubPrefs(
        scope: account == null ? '' : '${account.origin}|${account.account}',
      ),
    );
  }
}

class AccountWaitingScreen extends StatelessWidget {
  const AccountWaitingScreen({super.key, required this.connection});
  final ConnectionController connection;

  @override
  Widget build(BuildContext context) {
    final checking = connection.phase == ConnectionPhase.checking;
    final devices = [
      for (final d in connection.devices)
        if (d['kind'] != 'phone') d,
    ];
    return TwinBackdrop(
      child: Scaffold(
      backgroundColor: Colors.transparent,
      appBar: AppBar(backgroundColor: Colors.transparent, title: const Text('기기 연결')),
      body: SafeArea(
        child: ListView(
          padding: const EdgeInsets.all(24),
          children: [
            Center(child: TwinsMark(hopping: checking, face: Look.pullFace)),
            const SizedBox(height: Look.fieldGap),
            Text(
              '${connection.account!.label} 계정',
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
            // Google·GitHub 로 처음 들어오면 관문이 새 계정을 만든다 — 쓰던 데스크톱 계정과 다르다는 것을 여기서 알린다.
            if (connection.account!.account.startsWith('oauth_') &&
                !connection.devices.any((d) => d['kind'] != 'phone')) ...[
              const SizedBox(height: 12),
              const Text(
                '이 계정에는 아직 데스크톱이 없어요. 쓰던 데스크톱 계정이 있다면 로그아웃한 뒤 아이디로 로그인하고, 설정에서 Google·GitHub 을 연결해 주세요.',
              ),
            ],
            if (devices.isNotEmpty)
              SettingsGroup(
                title: '이 계정의 데스크톱',
                children: [
                  for (final (i, device) in devices.indexed)
                    SettingsRow(
                      tone: i,
                      icon: Icons.computer_outlined,
                      title: '${device['label'] ?? device['device_id']}',
                      subtitle: device['online'] == true ? '온라인 · 연결 준비 확인 중' : '오프라인',
                    ),
                ],
              ),
            const SizedBox(height: 20),
            FilledButton(
              onPressed: checking ? null : connection.retry,
              child: Text(checking ? '확인 중' : '연결 다시 확인'),
            ),
            const SizedBox(height: 8),
            OutlinedButton(
              style: OutlinedButton.styleFrom(minimumSize: const Size(Look.tap, Look.buttonH)),
              onPressed: connection.logout,
              child: const Text('로그아웃'),
            ),
          ],
        ),
      ),
      ),
    );
  }
}
