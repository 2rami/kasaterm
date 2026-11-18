import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../app_release.dart';
import '../look.dart';
import '../relay_account.dart';
import '../server.dart';
import '../theme_prefs.dart';
import '../twins_loading.dart';
import '../weather/sheet.dart';
import '../weather/store.dart';
import 'account_profile.dart';
import 'character_picks.dart';
import 'claude_accounts.dart';
import 'controls.dart';
import 'settings_account.dart';
import 'settings_devices.dart';
import 'settings_look.dart';

class SettingsScreen extends StatefulWidget {
  const SettingsScreen({
    super.key,
    required this.server,
    required this.onChangeAddress,
    this.installId,
    this.accountApi,
  });

  final Server server;
  final Future<void> Function() onChangeAddress;

  /// 검사용 — 이 설치의 고정 id 를 키체인 대신 준다.
  final Future<String> Function()? installId;

  /// 검사용 — 계정 길(`/relay/…`)을 가짜 관문으로. 없으면 [server] 의 계정 세션으로 붙는다.
  final RelayAccountApi Function()? accountApi;

  @override
  State<SettingsScreen> createState() => _SettingsScreenState();
}

/// 설정 첫 화면 — 데스크톱 설정 옆 목록과 같은 묶음 셋(화면·에이전트·앱)에 같은 이름의 페이지 줄.
/// 줄을 누르면 그 페이지로 들어간다. 페이지가 화면 하나뿐이면(코딩 에이전트·캐릭터·날씨) 그 화면을 바로 연다.
class _SettingsScreenState extends State<SettingsScreen> {
  Server get server => widget.server;

  late final Future<AppRelease?> _release = server.latestRelease();
  late final Future<OAuthProviders> _providers = _loadProviders();

  /// 새 판 설치 기록 — 비었으면 줄을 안 세운다(빈 자리에 줄 사이 선만 남지 않게).
  List<String> _trail = const [];

  /// 이 계정의 얼굴과 로그인 방법. 계정으로 붙지 않았거나 못 받았으면 null.
  AccountProfile? _profile;
  String? _profileError;

  /// 계정 길 — 계정으로 붙었을 때만. 옛 주소 연결은 null.
  RelayAccountApi Function()? get _api {
    if (widget.accountApi case final api?) return api;
    final session = server.account;
    return session == null ? null : () => RelayAccountApi(session.origin, session: session);
  }

  @override
  void initState() {
    super.initState();
    unawaited(_loadProfile());
    unawaited(_loadTrail());
  }

  Future<void> _loadTrail() async {
    try {
      final lines = await releaseTrail();
      if (mounted && lines.isNotEmpty) setState(() => _trail = lines);
    } on Object {
      // 기록을 못 받으면 줄 없이 둔다 — 설정을 막을 일이 아니다.
    }
  }

  Future<void> _loadProfile() async {
    final api = _api?.call();
    if (api == null) return;
    try {
      final profile = await api.profile();
      if (mounted) {
        setState(() {
          _profile = profile;
          _profileError = null;
        });
      }
    } on AccountException catch (e) {
      if (mounted) setState(() => _profileError = e.message);
    } finally {
      api.close();
    }
  }

  Future<OAuthProviders> _loadProviders() async {
    final api = _api?.call();
    if (api == null) return const OAuthProviders([]);
    try {
      return await api.oauthProviders();
    } finally {
      api.close();
    }
  }

  Future<void> _editProfile() async {
    final api = _api;
    final profile = _profile;
    if (api == null || profile == null) return;
    final next = await showProfileSheet(context, profile: profile, api: api);
    if (next != null && mounted) setState(() => _profile = next);
    // 사진은 시트 안에서 바로 바뀌니 닫기만 해도 다시 받는다.
    await _loadProfile();
  }

  /// 페이지에 들어갔다 돌아오면 계정 판·줄 요약을 다시 받는다 — 페이지 안에서 로그인 방법·밝기가 바뀌었을 수 있다.
  Future<void> _open(Widget page) async {
    await Navigator.of(context).push(MaterialPageRoute<void>(builder: (_) => page));
    if (!mounted) return;
    setState(() {});
    unawaited(_loadProfile());
  }

  /// 새 판 설치가 실기에서 어디까지 갔는지(`ReleaseTrail`) — 원인을 찾는 사람에게 넘길 수 있게 복사·데스크톱 보내기.
  Future<void> _showTrail(List<String> lines) {
    final text = lines.join('\n');
    return showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (sheet) {
        final theme = Theme.of(sheet);
        void done(String msg) {
          Navigator.of(sheet).pop();
          ScaffoldMessenger.of(
            context,
          ).showSnackBar(SnackBar(content: Text(msg)));
        }

        return SafeArea(
          child: ConstrainedBox(
            constraints: BoxConstraints(
              maxHeight: MediaQuery.sizeOf(sheet).height * 0.8,
            ),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                Flexible(
                  child: SingleChildScrollView(
                    reverse: true,
                    padding: const EdgeInsets.fromLTRB(20, 0, 20, 8),
                    child: SelectableText(
                      text,
                      style: theme.textTheme.bodySmall?.copyWith(
                        fontFamily: 'TermMono',
                      ),
                    ),
                  ),
                ),
                ListTile(
                  leading: const Icon(Icons.copy),
                  title: const Text('복사'),
                  onTap: () async {
                    await Clipboard.setData(ClipboardData(text: text));
                    done('설치 기록을 복사했어요');
                  },
                ),
                ListTile(
                  leading: const Icon(Icons.desktop_mac_outlined),
                  title: const Text('데스크톱 클립보드로 보내기'),
                  onTap: () async {
                    try {
                      await server.clipboardPush(text);
                      done('데스크톱 클립보드로 보냈어요');
                    } on Object {
                      done('데스크톱에 못 보냈어요 — 복사해서 붙여 주세요');
                    }
                  },
                ),
              ],
            ),
          ),
        );
      },
    );
  }

  Future<void> _forget(BuildContext context) async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => ModalLook(
        child: AlertDialog(
          title: Text(server.account == null ? '폰 주소 지우기' : '로그아웃'),
          content: Text(
            server.account == null
                ? '이 폰에 저장한 주소를 지웁니다.'
                : '이 폰의 로그인과 연결을 종료합니다. 다른 기기의 로그인은 유지됩니다.',
          ),
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
  Widget build(BuildContext context) {
    final account = server.account;
    return TwinBackdrop(
      child: Scaffold(
        backgroundColor: Colors.transparent,
        appBar: AppBar(backgroundColor: Colors.transparent, title: const Text('설정')),
        body: ListView(
          padding: const EdgeInsets.fromLTRB(Look.pagePad, 8, Look.pagePad, Look.groupGap * 2),
          children: [
            _AccountCard(
              server: server,
              profile: _profile,
              api: _api,
              onTap: _api != null && _profile != null ? () => unawaited(_editProfile()) : null,
            ),
            // 묶음 차례로 하늘·호박을 번갈아 — 묶음 셋은 늘 선다(에이전트엔 코딩 에이전트가 늘 있다).
            SettingsGroup(key: const Key('group-screen'), title: '화면', children: _screenRows(0)),
            SettingsGroup(key: const Key('group-agent'), title: '에이전트', children: _agentRows(1)),
            SettingsGroup(key: const Key('group-app'), title: '앱', children: _appRows(0)),
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
        ),
      ),
    );
  }

  /// 「화면」 — 데스크톱 「외형」·「날씨」. 「앱 하단바」는 폰에 없다.
  List<Widget> _screenRows(int tone) => [
    SettingsRow(
      key: const Key('settings-look'),
      tone: tone,
      icon: Icons.palette_outlined,
      title: '외형',
      subtitle: '${phoneModes.firstWhere((m) => m.$1 == phoneThemeMode.value).$3} · 강조색 · 모서리',
      chevron: true,
      onTap: () => unawaited(_open(LookSettingsScreen(server: server))),
    ),
    ListenableBuilder(
      listenable: weather.settings,
      builder: (context, _) {
        final w = weather.settings.value;
        return SettingsRow(
          key: const Key('settings-weather'),
          tone: tone,
          icon: Icons.umbrella_outlined,
          title: '날씨',
          subtitle: w.enabled ? '${w.amount.label} · ${w.target.label}' : '꺼짐 · 카드와 단추에 비를 내려요',
          chevron: true,
          onTap: () => showWeatherSheet(context),
        );
      },
    ),
  ];

  /// 「에이전트」 — 데스크톱 「계정」·「코딩 에이전트」·「캐릭터」. 계정과 학생 명단은 계정 공통 설정이라 계정으로 붙었을 때만.
  List<Widget> _agentRows(int tone) {
    final api = _api;
    return [
      if (api != null)
        SettingsRow(
          key: const Key('settings-account'),
          tone: tone,
          icon: Icons.manage_accounts_outlined,
          title: '계정',
          subtitle: _accountLine(),
          chevron: true,
          onTap: () => unawaited(
            _open(
              AccountSettingsScreen(api: api, providers: _providers, profile: _profile, installId: widget.installId),
            ),
          ),
        ),
      SettingsRow(
        key: const Key('claude-accounts'),
        tone: tone,
        icon: Icons.smart_toy_outlined,
        title: '코딩 에이전트',
        subtitle: 'Claude 계정 · 폰에서 다시 로그인',
        chevron: true,
        onTap: () => unawaited(_open(ClaudeAccountsScreen(server: server))),
      ),
      if (api != null)
        SettingsRow(
          key: const Key('character-picks'),
          tone: tone,
          icon: Icons.groups_2_outlined,
          title: '캐릭터',
          subtitle: '학생 테마와 새 창에 나올 학생 · 모든 기기 같이',
          chevron: true,
          onTap: () => unawaited(_open(CharacterPicksScreen(server: server, api: api))),
        ),
    ];
  }

  /// 계정 줄 보조글 — 이어진 로그인 방법. 못 받았으면 그 까닭을 그대로.
  String _accountLine() {
    final profile = _profile;
    if (profile == null) return _profileError ?? '로그인 방법 · 일 권한';
    final ways = [
      for (final p in OAuthProvider.values)
        if (profile.linked(p) != null) p.label,
      if (profile.login != null) '아이디',
    ];
    return ways.isEmpty ? '로그인 방법 · 일 권한' : '${ways.join('·')} 로그인 · 일 권한';
  }

  /// 「앱」 — 데스크톱 「연결 기기」와 이 앱의 판. 「앱 일반」·「피드백」은 폰에 없다.
  List<Widget> _appRows(int tone) => [
    SettingsRow(
      key: const Key('settings-devices'),
      tone: tone,
      icon: Icons.devices_outlined,
      title: '연결 기기',
      subtitle: _api != null ? '기기 이름 · PC 기능 · 비밀 승인 · 브라우저' : 'PC 기능 · 브라우저 · 개발 서버',
      chevron: true,
      onTap: () => unawaited(_open(DeviceSettingsScreen(server: server, api: _api))),
    ),
    FutureBuilder<AppRelease?>(
      future: _release,
      builder: (context, snap) {
        final r = snap.data;
        final fresh = r != null && r.newer;
        return SettingsRow(
          key: const Key('app-release'),
          tone: tone,
          icon: fresh ? Icons.system_update_outlined : Icons.info_outline_rounded,
          title: '앱 판',
          subtitle: fresh
              ? '새 판 ${r.version} (${r.build}) · 눌러서 설치'
              : (kasaBuild.isEmpty ? '개발 설치' : '빌드 $kasaBuild'),
          trailing: fresh ? const _Pill('새 판') : null,
          chevron: fresh,
          onTap: fresh ? () => unawaited(installRelease(context, r, from: '설정 앱 판')) : null,
        );
      },
    ),
    if (_trail.isNotEmpty)
      SettingsRow(
        key: const Key('release-trail'),
        tone: tone,
        icon: Icons.receipt_long_outlined,
        title: '지난 설치 기록',
        subtitle: '${_trail.length}줄 · 눌러서 복사하거나 데스크톱으로 보내요',
        chevron: true,
        onTap: () => unawaited(_showTrail(_trail)),
      ),
  ];
}

/// 맨 위 계정 판 — 계정 사진(없으면 쌍둥이 그림) · 닉네임 · 데스크톱까지 가는 길 · 주소. 누르면 프로필을 바꾼다.
class _AccountCard extends StatelessWidget {
  const _AccountCard({required this.server, this.profile, this.api, this.onTap});

  final Server server;
  final AccountProfile? profile;

  /// 계정 사진을 받는 길 — 옛 주소 연결이면 null 이고 쌍둥이 그림.
  final RelayAccountApi Function()? api;
  final VoidCallback? onTap;

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
    final dim = theme.textTheme.bodySmall?.copyWith(
      color: theme.colorScheme.onSurfaceVariant,
    );
    final twins = Container(
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
        cacheWidth: (Look.accountArt * MediaQuery.devicePixelRatioOf(context))
            .round(),
      ),
    );
    final api = this.api;
    Widget body() => Row(
      children: [
        if (api == null)
          twins
        else
          AccountFace(
            key: ValueKey(profile?.avatar?.key),
            avatar: profile?.avatar,
            api: api,
            size: Look.accountArt,
            fallback: twins,
          ),
        const SizedBox(width: Look.cardPad),
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              Text(
                profile?.name ?? server.account?.label ?? '연결된 주소',
                style: theme.textTheme.titleLarge,
                overflow: TextOverflow.ellipsis,
              ),
              const SizedBox(height: Look.rowGap),
              Text(_path(), style: theme.textTheme.bodySmall),
              Text(
                server.describe(),
                style: dim,
                overflow: TextOverflow.ellipsis,
              ),
            ],
          ),
        ),
      ],
    );
    final changes = server.routeChanges;
    final card = Container(
      padding: const EdgeInsets.all(Look.cardPad),
      decoration: tone.cardBox(),
      child: changes == null
          ? body()
          : ListenableBuilder(
              listenable: changes,
              builder: (context, _) => body(),
            ),
    );
    return onTap == null
        ? card
        : Semantics(
            button: true,
            label: '프로필 바꾸기',
            child: GestureDetector(
              key: const Key('account-card'),
              onTap: onTap,
              child: card,
            ),
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
      decoration: ShapeDecoration(
        shape: const StadiumBorder(),
        color: scheme.primary.withValues(alpha: 0.16),
      ),
      child: Text(
        text,
        style: theme.textTheme.labelSmall?.copyWith(color: scheme.primary),
      ),
    );
  }
}
