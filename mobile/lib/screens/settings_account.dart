import 'dart:async';

import 'package:flutter/material.dart';
import 'package:url_launcher/url_launcher.dart';

import '../connection_store.dart';
import '../look.dart';
import '../relay_account.dart';
import '../status_style.dart';
import '../twins_loading.dart';
import 'account_profile.dart';
import 'controls.dart';
import 'oauth_sheet.dart';
import 'work_permissions.dart';

/// 설정 「에이전트」 → 「계정」 — 데스크톱 설정 「계정」과 같이 KASA 계정의 로그인 방법만 두고, 일 권한을 그 아래에.
/// 일 권한이 여기인 까닭: 그 권한은 이 화면의 GitHub 연결 한 번으로 붙고, 일 연결 끊기와 폰의 PR 승인이 이 길뿐이다.
class AccountSettingsScreen extends StatefulWidget {
  const AccountSettingsScreen({
    super.key,
    required this.api,
    required this.providers,
    this.profile,
    this.installId,
    this.onProfile,
  });

  final RelayAccountApi Function() api;
  final Future<OAuthProviders> providers;

  /// 앞 화면이 이미 받은 값 — 들어오자마자 그리고, 곧 다시 받는다.
  final AccountProfile? profile;
  final Future<String> Function()? installId;

  /// 받거나 바꾼 프로필을 첫 화면에 — 계정 판·계정 줄 요약이 같이 바뀐다(두 칸이면 이 페이지가 닫히지 않는다).
  final ValueChanged<AccountProfile>? onProfile;

  @override
  State<AccountSettingsScreen> createState() => _AccountSettingsScreenState();
}

class _AccountSettingsScreenState extends State<AccountSettingsScreen> {
  late AccountProfile? _profile = widget.profile;
  String? _profileError;
  bool _linking = false;

  @override
  void initState() {
    super.initState();
    unawaited(_loadProfile());
  }

  Future<void> _loadProfile() async {
    final api = widget.api();
    try {
      final profile = await api.profile();
      if (mounted) {
        setState(() {
          _profile = profile;
          _profileError = null;
        });
        widget.onProfile?.call(profile);
      }
    } on AccountException catch (e) {
      if (mounted) setState(() => _profileError = e.message);
    } finally {
      api.close();
    }
  }

  Future<void> _editSecret({required bool password}) async {
    final profile = _profile;
    if (profile == null) return;
    final next = await showSecretSheet(context, profile: profile, api: widget.api, password: password);
    if (next == null || !mounted) return;
    setState(() => _profile = next);
    widget.onProfile?.call(next);
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(
        content: Text(
          password ? '비밀번호를 바꿨어요. 다른 기기의 로그인은 그대로예요.' : '로그인 아이디를 바꿨어요. 다음 로그인부터 새 아이디를 써요.',
        ),
      ),
    );
  }

  /// 지금 계정에 Google·GitHub 로그인을 더한다 — 다음부터 그 단추로 이 계정에 들어온다.
  Future<void> _link(OAuthProvider provider) async {
    if (_linking) return;
    setState(() => _linking = true);
    final api = widget.api();
    try {
      final id = await (widget.installId ?? const ConnectionStore().installId)();
      if (!mounted) return;
      // GitHub 은 같은 허용으로 PR 일 권한까지 붙인다 — 「일 권한」에서 따로 연결하지 않는다.
      final r = await showOAuthSheet(context, api: api, provider: provider, machineId: id, link: true, work: true);
      if (r == null || !mounted) return;
      final text = switch ((r.connected, r.linked)) {
        (true, true) => '${provider.label} 로그인과 일 권한을 연결했어요.',
        (true, false) => '일 권한을 연결했어요. 이 ${provider.label} 은 다른 KASA 계정의 로그인이라 로그인은 그대로예요.',
        (false, true) => '이 계정에 ${provider.label} 로그인을 연결했어요.',
        _ => null,
      };
      if (text != null) ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(text)));
      if (r.installUrl case final url?) unawaited(launchUrl(url, mode: LaunchMode.externalApplication));
      if (r.connected || r.linked) unawaited(_loadProfile());
    } catch (_) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('연결을 시작하지 못했어요. 다시 시도해 주세요.')));
      }
    } finally {
      api.close();
      if (mounted) setState(() => _linking = false);
    }
  }

  @override
  Widget build(BuildContext context) => TwinBackdrop(
    child: Scaffold(
      backgroundColor: Colors.transparent,
      appBar: AppBar(backgroundColor: Colors.transparent, title: const Text('계정')),
      body: FutureBuilder<OAuthProviders>(
        future: widget.providers,
        builder: (context, snap) => ListView(
          padding: const EdgeInsets.fromLTRB(Look.pagePad, 0, Look.pagePad, Look.groupGap * 2),
          children: [
            SettingsGroup(title: '로그인 방법', children: _loginRows(snap.data?.enabled ?? const [])),
            SettingsGroup(
              title: '일 권한',
              children: [
                SettingsRow(
                  key: const Key('work-permissions'),
                  tone: 1,
                  icon: Icons.merge_type_rounded,
                  title: 'PR 승인 · 연결 끊기',
                  subtitle: '연결된 GitHub · 학생·나쵸가 낸 PR 을 보고 올려요',
                  chevron: true,
                  onTap: () => Navigator.of(
                    context,
                  ).push(MaterialPageRoute<void>(builder: (_) => WorkPermissionsScreen(api: widget.api))),
                ),
              ],
            ),
          ],
        ),
      ),
    ),
  );

  /// 로그인 방법(Google·GitHub·아이디·비밀번호)마다 이 계정에 이어졌는지와 그 메일·아이디 — 데스크톱 「로그인 방법」과 같은 줄.
  List<Widget> _loginRows(List<OAuthProvider> providers) {
    final profile = _profile;
    final unknown = profile == null ? (_profileError ?? '확인 중이에요') : null;
    return [
      for (final p in OAuthProvider.values) _loginRow(p, profile?.linked(p), enabled: providers.contains(p), unknown: unknown),
      if (profile?.login case final login?) ...[
        SettingsRow(
          key: const Key('change-login'),
          icon: Icons.badge_outlined,
          title: '아이디',
          subtitle: login,
          chevron: true,
          onTap: () => unawaited(_editSecret(password: false)),
        ),
        SettingsRow(
          key: const Key('change-password'),
          icon: Icons.password_rounded,
          title: '비밀번호',
          subtitle: '지금 비밀번호를 확인하고 바꿔요',
          chevron: true,
          onTap: () => unawaited(_editSecret(password: true)),
        ),
      ] else
        SettingsRow(
          icon: Icons.badge_outlined,
          title: '아이디·비밀번호',
          subtitle: unknown ?? '없음 · Google·GitHub 로만 들어와요',
        ),
    ];
  }

  /// 공급자 한 줄 — 이어졌으면 메일·아이디와 체크, 아니면 누르면 로그인을 붙인다(GitHub 은 PR 일 권한도 한 번에).
  Widget _loginRow(OAuthProvider p, LinkedLogin? linked, {required bool enabled, String? unknown}) => SettingsRow(
    key: Key('link-${p.id}'),
    icon: Icons.login_rounded,
    logo: ProviderLogo(p),
    title: p.label,
    subtitle: switch (linked) {
      final LinkedLogin l => l.display.isEmpty ? '연결됨' : l.display,
      null when unknown != null => unknown,
      null when !enabled => '이 서버는 아직 연결을 받지 않아요',
      null => p == OAuthProvider.google ? '연결하면 이 계정에 Google 로그인을 더해요' : '연결하면 로그인과 PR 일 권한을 한 번에 붙여요',
    },
    trailing: linked != null
        ? Icon(Icons.check_circle_rounded, key: Key('linked-${p.id}'), color: StatusStyle.success, size: Look.iconSize)
        : null,
    chevron: enabled,
    onTap: enabled && !_linking ? () => unawaited(_link(p)) : null,
  );
}
