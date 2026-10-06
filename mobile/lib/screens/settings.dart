import 'dart:async';

import 'package:flutter/material.dart';
import 'package:url_launcher/url_launcher.dart';

import '../app_release.dart';
import '../connection_store.dart';
import '../main.dart' show designTokens;
import '../relay_account.dart';
import '../server.dart';
import 'browser_device.dart';
import 'character_picks.dart';
import 'device_names.dart';
import 'dev_server.dart';
import '../theme_prefs.dart';
import '../weather/sheet.dart';
import '../weather/store.dart';
import '../look.dart';
import '../twins_loading.dart';
import 'controls.dart';
import 'hub.dart' show parseHexColor;
import 'oauth_sheet.dart';
import 'approval_key.dart';
import 'work_permissions.dart';

class SettingsScreen extends StatefulWidget {
  const SettingsScreen({
    super.key,
    required this.server,
    required this.onChangeAddress,
    this.installId,
  });

  final Server server;
  final Future<void> Function() onChangeAddress;

  /// 검사용 — 이 설치의 고정 id 를 키체인 대신 준다.
  final Future<String> Function()? installId;

  @override
  State<SettingsScreen> createState() => _SettingsScreenState();
}

class _SettingsScreenState extends State<SettingsScreen> {
  Server get server => widget.server;

  /// 데스크톱 「외형」 값 — 못 받으면 null 이고 그 칸은 안 그린다.
  Map<String, Object?>? _appearance;
  bool _loading = true;
  String? _pending;
  late final Future<AppRelease?> _release = server.latestRelease();
  late final Future<OAuthProviders> _providers = _loadProviders();

  Future<OAuthProviders> _loadProviders() async {
    final api = _relay();
    if (api == null) return const OAuthProviders([]);
    try {
      return await api.oauthProviders();
    } finally {
      api.close();
    }
  }
  bool _linking = false;

  RelayAccountApi? _relay() {
    final session = server.account;
    return session == null ? null : RelayAccountApi(session.origin, session: session);
  }

  /// 지금 계정에 Google·GitHub 로그인을 더한다 — 다음부터 그 단추로 이 계정에 들어온다.
  Future<void> _link(OAuthProvider provider) async {
    final api = _relay();
    if (api == null || _linking) return;
    setState(() => _linking = true);
    try {
      final id = await (widget.installId ?? const ConnectionStore().installId)();
      if (!mounted) return;
      // 같은 허용으로 그 공급자의 일 권한(Gmail·PR)까지 붙인다 — 「일 권한」에서 따로 연결하지 않는다.
      final r = await showOAuthSheet(context, api: api, provider: provider, machineId: id, link: true, work: true);
      if (r == null || !mounted) return;
      final text = switch ((r.connected, r.linked)) {
        (true, true) => '${provider.label} 로그인과 일 권한을 연결했어요.',
        (true, false) => '일 권한을 연결했어요. 이 ${provider.label} 은 다른 KASA 계정의 로그인이라 로그인은 그대로예요.',
        (false, true) => '이 계정에 ${provider.label} 로그인을 연결했어요.',
        _ => null,
      };
      if (text != null) {
        ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(text)));
      }
      if (r.installUrl case final url?) {
        unawaited(launchUrl(url, mode: LaunchMode.externalApplication));
      }
    } catch (_) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          const SnackBar(content: Text('연결을 시작하지 못했어요. 다시 시도해 주세요.')),
        );
      }
    } finally {
      api.close();
      if (mounted) setState(() => _linking = false);
    }
  }

  @override
  void initState() {
    super.initState();
    _reload();
  }

  Future<void> _reload() async {
    Map<String, Object?>? a;
    try {
      a = await server.appearance();
    } on ServerException {
      a = null;
    }
    if (!mounted) return;
    setState(() {
      _appearance = a;
      _loading = false;
    });
  }

  /// 데스크톱에 액션을 보내고, 바뀐 색을 되받아 폰도 같은 얼굴로.
  Future<void> _apply(String action, String id) async {
    setState(() => _pending = '$action:$id');
    try {
      await server.settingsAction(action, id: id);
      final t = await server.designTokens();
      if (t != null && mounted && !server.isClosed) designTokens.value = t;
    } on ServerException catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context)
            .showSnackBar(SnackBar(content: Text(e.message)));
      }
    } finally {
      if (mounted) setState(() => _pending = null);
      await _reload();
    }
  }

  Future<void> _setMode(ThemeMode m) async {
    final problem = await phoneThemeSync.setMode(m);
    if (!mounted) return;
    setState(() {});
    if (problem != null) {
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(problem)));
    }
  }

  Future<void> _forget(BuildContext context) async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => ModalLook(
        child: AlertDialog(
          title: Text(server.account == null ? '폰 주소 지우기' : '로그아웃'),
          content: Text(server.account == null ? '이 폰에 저장한 주소를 지웁니다.' :
            '이 폰의 로그인과 연결을 종료합니다. 다른 기기의 로그인은 유지됩니다.'),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(context, false),
              child: const Text('취소'),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(context, true),
              child: Text(server.account == null ? '지우기' : '로그아웃'),
            ),
          ],
        ),
      ),
    );
    if (ok != true || !context.mounted) return;
    Navigator.of(context).popUntil((r) => r.isFirst);
    await widget.onChangeAddress();
  }

  @override
  Widget build(BuildContext context) => TwinBackdrop(
    child: Scaffold(
      backgroundColor: Colors.transparent,
      appBar: AppBar(backgroundColor: Colors.transparent, title: const Text('설정')),
      body: FutureBuilder<OAuthProviders>(
        future: _providers,
        builder: (context, snap) => _list(snap.data?.enabled ?? const <OAuthProvider>[]),
      ),
    ),
  );

  Widget _list(List<OAuthProvider> providers) {
    final account = server.account;
    // 묶음 차례로 하늘·호박을 번갈아 — 계정 묶음이 빠지면 그 뒤가 한 칸씩 당겨진다.
    final linking = account != null && providers.isNotEmpty;
    const phone = 0;
    final desktop = account != null ? 2 : 1;
    return ListView(
          padding: const EdgeInsets.fromLTRB(Look.pagePad, 8, Look.pagePad, Look.groupGap * 2),
          children: [
            _AccountCard(server: server),
            SettingsGroup(
              title: '이 폰',
              children: [
                SettingsRow(
                  tone: phone,
                  icon: Icons.brightness_6_outlined,
                  title: '밝기',
                  subtitle: switch (phoneThemeMode.value) {
                    ThemeMode.system => '데스크톱 테마 그대로',
                    ThemeMode.light => '밝게 · 같은 테마 색을 밝게',
                    ThemeMode.dark => '어둡게 · 같은 테마 색을 어둡게',
                  },
                  trailing: IconChoice<ThemeMode>(
                    options: const [
                      (ThemeMode.system, Icons.desktop_windows_outlined, '데스크톱 따라감'),
                      (ThemeMode.light, Icons.light_mode_outlined, '밝게'),
                      (ThemeMode.dark, Icons.dark_mode_outlined, '어둡게'),
                    ],
                    selected: phoneThemeMode.value,
                    onSelect: _setMode,
                  ),
                ),
                ListenableBuilder(
                  listenable: weather.settings,
                  builder: (context, _) {
                    final w = weather.settings.value;
                    return SettingsRow(
                      tone: phone,
                      icon: Icons.umbrella_outlined,
                      title: '날씨',
                      subtitle: w.enabled ? '${w.amount.label} · ${w.target.label}' : '꺼짐 · 카드와 단추에 비를 내려요',
                      chevron: true,
                      onTap: () => showWeatherSheet(context),
                    );
                  },
                ),
              ],
            ),
            if (account != null)
              SettingsGroup(
                title: '계정',
                children: [
                  SettingsRow(
                    key: const Key('work-permissions'),
                    tone: 1,
                    icon: Icons.mail_lock_outlined,
                    title: '일 권한',
                    subtitle: '연결된 Gmail·GitHub · 메일·PR 승인',
                    chevron: true,
                    onTap: () => Navigator.of(context).push(
                      MaterialPageRoute<void>(
                        builder: (_) => WorkPermissionsScreen(
                          api: () => RelayAccountApi(account.origin, session: account),
                        ),
                      ),
                    ),
                  ),
                  SettingsRow(
                    key: const Key('approval-key'),
                    tone: 1,
                    icon: Icons.fingerprint_rounded,
                    title: 'Face ID 승인 열쇠',
                    subtitle: '학생의 1Password 요청을 Face ID 로 한 번씩 허락',
                    chevron: true,
                    onTap: () => Navigator.of(context).push(
                      MaterialPageRoute<void>(
                        builder: (_) => ApprovalKeyScreen(
                          api: () => RelayAccountApi(account.origin, session: account),
                        ),
                      ),
                    ),
                  ),
                  if (linking)
                  for (final p in providers)
                    SettingsRow(
                      key: Key('link-${p.id}'),
                      tone: 1,
                      icon: Icons.login_rounded,
                      title: '${p.label} 연결',
                      subtitle: p == OAuthProvider.google
                          ? '로그인과 Gmail 읽기·보내기를 한 번에 붙여요'
                          : '로그인과 PR 일 권한을 한 번에 붙여요',
                      chevron: true,
                      onTap: _linking ? null : () => unawaited(_link(p)),
                    ),
                ],
              ),
            SettingsGroup(title: '데스크톱', children: _desktopRows(desktop)),
            SettingsGroup(
              title: '앱',
              children: [
                FutureBuilder<AppRelease?>(
                  future: _release,
                  builder: (context, snap) {
                    final r = snap.data;
                    final fresh = r != null && r.newer;
                    return SettingsRow(
                      tone: desktop + 1,
                      icon: fresh ? Icons.system_update_outlined : Icons.info_outline_rounded,
                      title: '앱 판',
                      subtitle: fresh
                          ? '새 판 ${r.version} (${r.build}) · 눌러서 설치'
                          : (kasaBuild.isEmpty ? '개발 설치' : '빌드 $kasaBuild'),
                      trailing: fresh ? const _Pill('새 판') : null,
                      chevron: fresh,
                      onTap: fresh ? () => unawaited(installRelease(context, r)) : null,
                    );
                  },
                ),
              ],
            ),
            SettingsGroup(
              children: [
                SettingsRow(
                  danger: true,
                  icon: Icons.logout_rounded,
                  title: account == null ? '주소 바꾸기 · 지우기' : '로그아웃',
                  subtitle: '이 폰의 연결을 끝내고 로그인 화면으로 돌아가요',
                  onTap: () => _forget(context),
                ),
              ],
            ),
          ],
        );
  }

  /// 데스크톱 설정을 폰에서 바꾸는 줄 — 강조색·모서리는 데스크톱 「외형」 값이고 누르면 데스크톱이 바뀐다.
  /// 테마(밝기)는 여기 없다 — 폰에서 밝은 테마를 고르면 맥북까지 밝아졌다(2026-09-10 지적).
  List<Widget> _desktopRows(int tone) {
    final a = _appearance;
    final busy = _pending != null;
    return [
      if (_loading)
        SettingsRow(tone: tone, icon: Icons.palette_outlined, title: '외형', subtitle: '데스크톱 설정을 받는 중이에요')
      else if (a == null)
        SettingsRow(
          tone: tone,
          icon: Icons.cloud_off_outlined,
          title: '외형',
          subtitle: '데스크톱 설정을 못 받았어요 · 옛 데스크톱이거나 연결이 끊겼어요',
          trailing: TextButton(onPressed: _reload, child: const Text('다시')),
        )
      else ...[
        SettingsRow(
          tone: tone,
          icon: Icons.palette_outlined,
          title: '강조색',
          subtitle: '누르면 데스크톱 강조색이 바뀌어요',
          below: Wrap(
            spacing: 6,
            runSpacing: 6,
            children: [
              for (final c in _entries(a, 'accents'))
                _AccentDot(
                  name: c['name'] as String? ?? '',
                  color: parseHexColor(c['hex'] as String?) ?? Theme.of(context).colorScheme.primary,
                  selected: c['name'] == a['accent'],
                  onTap: busy || c['name'] is! String ? null : () => _apply('accent', c['name'] as String),
                ),
            ],
          ),
        ),
        SettingsRow(
          tone: tone,
          icon: Icons.rounded_corner_rounded,
          title: '모서리',
          subtitle: [
            for (final sh in _entries(a, 'shapes'))
              if (sh['key'] == a['shape']) sh['label'] as String? ?? '',
            '데스크톱 모양',
          ].where((t) => t.isNotEmpty).join(' · '),
          trailing: IconChoice<String>(
            options: [
              for (final sh in _entries(a, 'shapes'))
                if (sh['key'] case final String key)
                  (key, _shapeIcon(key), sh['label'] as String? ?? key),
            ],
            selected: a['shape'] as String?,
            onSelect: busy ? null : (key) => _apply('shape', key),
          ),
        ),
      ],
      // 학생 명단은 계정 공통 설정이라 계정으로 붙었을 때만 — 옛 주소 연결은 계정 동기화가 없다.
      if (server.account case final account?)
        SettingsRow(
          key: const Key('character-picks'),
          tone: tone,
          icon: Icons.groups_2_outlined,
          title: '학생 고르기',
          subtitle: '학생 테마와 새 창에 나올 학생 · 모든 기기 같이',
          chevron: true,
          onTap: () => Navigator.of(context).push(
            MaterialPageRoute<void>(
              builder: (_) => CharacterPicksScreen(
                server: server,
                api: () => RelayAccountApi(account.origin, session: account),
              ),
            ),
          ),
        ),
      if (server.account case final account?)
        SettingsRow(
          key: const Key('device-names'),
          tone: tone,
          icon: Icons.drive_file_rename_outline_rounded,
          title: '기기 이름',
          subtitle: '여기서 바꾼 이름이 모든 PC·폰 화면에 같이',
          chevron: true,
          onTap: () => Navigator.of(context).push(
            MaterialPageRoute<void>(
              builder: (_) => DeviceNamesScreen(
                server: server,
                api: () => RelayAccountApi(account.origin, session: account),
              ),
            ),
          ),
        ),
      SettingsRow(
        tone: tone,
        icon: Icons.public_outlined,
        title: '브라우저 기기',
        subtitle: '학생이 보여 주려 여는 페이지가 갈 곳',
        chevron: true,
        onTap: () => showBrowserDeviceSheet(context, server: server),
      ),
      SettingsRow(
        tone: tone,
        icon: Icons.web_outlined,
        title: '개발 서버 열기',
        subtitle: '데스크톱 localhost 페이지를 이 앱 안에서',
        chevron: true,
        onTap: () => openDevServer(context, server: server),
      ),
    ];
  }

  /// 데스크톱 모양 프리셋(`rounded`·`sharp`·`pixel`)의 그림. 모르는 새 프리셋은 일반 모양 그림으로.
  static IconData _shapeIcon(String key) => switch (key) {
    'rounded' => Icons.rounded_corner_rounded,
    'sharp' => Icons.crop_square_rounded,
    'pixel' => Icons.grid_4x4_rounded,
    _ => Icons.category_outlined,
  };

  static List<Map<String, Object?>> _entries(Map<String, Object?> a, String key) {
    final v = a[key];
    if (v is! List) return const [];
    return [
      for (final e in v)
        if (e is Map) e.cast<String, Object?>(),
    ];
  }
}

/// 맨 위 계정 판 — 쌍둥이 그림 · 계정 이름 · 데스크톱까지 가는 길 · 주소.
class _AccountCard extends StatelessWidget {
  const _AccountCard({required this.server});

  final Server server;

  String _path() => switch (server.pathOf(null)) {
    (true, final int ms) => '${_way()} · ${ms}ms',
    (true, null) => _way(),
    (false, _) => '관문 경유 · 직통을 찾는 중',
    null => '관문 경유',
  };

  String _way() => server.relayedOf(null) ? '카사넷 국내 중계' : '카사넷 직통';

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final tone = TwinTone.of(context);
    final dim = theme.textTheme.bodySmall?.copyWith(color: theme.colorScheme.onSurfaceVariant);
    Widget body() => Row(
      children: [
        Container(
          width: Look.accountArt,
          height: Look.accountArt,
          decoration: BoxDecoration(
            borderRadius: Look.corners,
            gradient: LinearGradient(
              begin: Alignment.topLeft,
              end: Alignment.bottomRight,
              colors: [tone.skyWash, tone.amberWash],
            ),
          ),
          child: Image.asset(
            'assets/original/twins.png',
            cacheWidth: (Look.accountArt * MediaQuery.devicePixelRatioOf(context)).round(),
          ),
        ),
        const SizedBox(width: Look.cardPad),
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              Text(
                server.account?.label ?? '연결된 주소',
                style: theme.textTheme.titleLarge,
                overflow: TextOverflow.ellipsis,
              ),
              const SizedBox(height: Look.rowGap),
              Text(_path(), style: theme.textTheme.bodySmall),
              Text(server.describe(), style: dim, overflow: TextOverflow.ellipsis),
            ],
          ),
        ),
      ],
    );
    final changes = server.routeChanges;
    return Container(
      padding: const EdgeInsets.all(Look.cardPad),
      decoration: tone.cardBox(),
      child: changes == null ? body() : ListenableBuilder(listenable: changes, builder: (context, _) => body()),
    );
  }
}

/// 「새 판」 같은 짧은 알림 알약 — 강조 물 위 강조 글자.
class _Pill extends StatelessWidget {
  const _Pill(this.text);

  final String text;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Container(
      height: Look.chipH,
      padding: const EdgeInsets.symmetric(horizontal: Look.chipPadX),
      alignment: Alignment.center,
      decoration: ShapeDecoration(shape: const StadiumBorder(), color: scheme.primary.withValues(alpha: 0.16)),
      child: Text(text, style: theme.textTheme.labelSmall?.copyWith(color: scheme.primary)),
    );
  }
}

/// 강조색 동그라미 — 고른 것은 바깥 고리. 누름 영역은 44.
class _AccentDot extends StatelessWidget {
  const _AccentDot({
    required this.name,
    required this.color,
    required this.selected,
    this.onTap,
  });

  final String name;
  final Color color;
  final bool selected;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Tooltip(
      message: name,
      child: InkResponse(
        onTap: onTap,
        radius: Look.tap / 2,
        child: SizedBox.square(
          dimension: Look.tap,
          child: Center(
            child: Container(
              width: Look.swatch,
              height: Look.swatch,
              padding: const EdgeInsets.all(3),
              decoration: BoxDecoration(
                shape: BoxShape.circle,
                border: Border.all(color: selected ? scheme.onSurface : Colors.transparent, width: 2),
              ),
              child: DecoratedBox(decoration: BoxDecoration(color: color, shape: BoxShape.circle)),
            ),
          ),
        ),
      ),
    );
  }
}
