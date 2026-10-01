import 'dart:async';

import 'package:flutter/material.dart';

import '../app_release.dart';
import '../main.dart' show designTokens;
import '../server.dart';
import 'browser_device.dart';
import 'dev_server.dart';
import '../theme_prefs.dart';
import '../weather/sheet.dart';
import '../weather/store.dart';
import '../look.dart';
import 'controls.dart';
import 'hub.dart' show parseHexColor;

class SettingsScreen extends StatefulWidget {
  const SettingsScreen({
    super.key,
    required this.server,
    required this.onChangeAddress,
  });

  final Server server;
  final Future<void> Function() onChangeAddress;

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
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Scaffold(
      appBar: AppBar(title: const Text('설정')),
      body: ListView(
        padding: const EdgeInsets.symmetric(vertical: 8),
        children: [
          Card(
            child: Column(
              children: [
                ListTile(
                  leading: const Icon(Icons.link),
                  title: Text(server.account?.account ?? '연결된 주소'),
                  subtitle: Text(server.describe()),
                ),
                const Divider(height: 1),
                if (server.routeChanges case final changes?)
                  ListenableBuilder(
                    listenable: changes,
                    builder: (context, _) => ListTile(
                      leading: const Icon(Icons.bolt_outlined),
                      title: const Text('데스크톱 길'),
                      subtitle: Text(switch (server.pathOf(null)) {
                        (true, final int ms) => '카사넷 직통 · ${ms}ms',
                        (true, null) => '카사넷 직통',
                        (false, _) => '관문 경유 — 직통을 찾는 중',
                        null => '관문 경유',
                      }),
                    ),
                  ),
                const Divider(height: 1),
                if (server.account != null)
                  const ListTile(leading: Icon(Icons.notifications_off_outlined),
                    title: Text('계정 알림은 준비 중'),
                    subtitle: Text('로그아웃 뒤 다른 계정의 알림이 오지 않도록, 계정 연결에서는 푸시 알림을 아직 등록하지 않습니다.')),
                ListTile(
                  leading: Icon(
                    Icons.delete_outline,
                    color: theme.colorScheme.error,
                  ),
                  title: Text(server.account == null ? '주소 바꾸기 · 지우기' : '로그아웃'),
                  subtitle: const Text('연결을 종료하고 로그인 화면으로 돌아갑니다'),
                  onTap: () => _forget(context),
                ),
              ],
            ),
          ),
          const SizedBox(height: Look.groupGap),
          Card(
            child: FutureBuilder<AppRelease?>(
              future: _release,
              builder: (context, snap) {
                final r = snap.data;
                final fresh = r != null && r.newer;
                return ListTile(
                  leading: Icon(fresh ? Icons.system_update_outlined : Icons.info_outline),
                  title: Text(kasaBuild.isEmpty ? '앱 판 · 개발 설치' : '앱 판 · 빌드 $kasaBuild'),
                  subtitle: fresh ? Text('새 판 ${r.version} (${r.build}) — 눌러서 설치') : null,
                  trailing: fresh ? const Icon(Icons.chevron_right) : null,
                  onTap: fresh ? () => unawaited(r.open()) : null,
                );
              },
            ),
          ),
          const SizedBox(height: Look.groupGap),
          _SectionTitle('학생'),
          Card(
            child: ListTile(
              leading: const Icon(Icons.public_outlined),
              title: const Text('브라우저 기기'),
              subtitle: const Text('학생이 보여 주려 여는 페이지가 갈 곳 — 이 폰이면 쪽지로'),
              trailing: const Icon(Icons.chevron_right),
              onTap: () => showBrowserDeviceSheet(context, server: server),
            ),
          ),
          Card(
            child: ListTile(
              leading: const Icon(Icons.web_outlined),
              title: const Text('데스크톱 개발 서버 열기'),
              subtitle: const Text('데스크톱 localhost 페이지를 이 앱 안에서 — 직통이면 카사넷으로'),
              trailing: const Icon(Icons.chevron_right),
              onTap: () => openDevServer(context, server: server),
            ),
          ),
          const SizedBox(height: Look.groupGap),
          _SectionTitle('이 폰'),
          Card(
            child: Padding(
              padding: const EdgeInsets.fromLTRB(16, 12, 16, 14),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text('밝기', style: theme.textTheme.titleSmall),
                  const SizedBox(height: 8),
                  SizedBox(
                    width: double.infinity,
                    child: SegmentedButton<ThemeMode>(
                      showSelectedIcon: false,
                      segments: const [
                        ButtonSegment(
                          value: ThemeMode.system,
                          label: Text('데스크톱 따라감'),
                        ),
                        ButtonSegment(
                          value: ThemeMode.light,
                          label: Text('밝게'),
                        ),
                        ButtonSegment(
                          value: ThemeMode.dark,
                          label: Text('어둡게'),
                        ),
                      ],
                      selected: {phoneThemeMode.value},
                      onSelectionChanged: (s) => _setMode(s.first),
                    ),
                  ),
                  const SizedBox(height: 8),
                  Text(
                    phoneThemeMode.value == ThemeMode.system
                        ? '연결된 데스크톱의 테마 색을 그대로 입는다. 못 받으면 폰 시스템 설정.'
                        : '데스크톱 색은 접고 폰 기본 얼굴을 이 밝기로 입는다.',
                    style: theme.textTheme.bodySmall?.copyWith(
                      color: theme.colorScheme.onSurfaceVariant,
                    ),
                  ),
                ],
              ),
            ),
          ),
          Card(
            child: ListenableBuilder(
              listenable: weather.settings,
              builder: (context, _) {
                final w = weather.settings.value;
                return ListTile(
                  leading: const Icon(Icons.umbrella_outlined),
                  title: const Text('날씨'),
                  subtitle: Text(w.enabled ? '${w.amount.label} · ${w.target.label}' : '꺼짐 — 카드·단추에 비를 내린다'),
                  trailing: const Icon(Icons.chevron_right),
                  onTap: () => showWeatherSheet(context),
                );
              },
            ),
          ),
          const SizedBox(height: Look.groupGap),
          _SectionTitle('데스크톱 외형'),
          if (_loading)
            const Padding(
              padding: EdgeInsets.all(24),
              child: Center(child: CircularProgressIndicator()),
            )
          else if (_appearance == null)
            Card(
              child: ListTile(
                leading: const Icon(Icons.cloud_off),
                title: const Text('데스크톱 설정을 못 받았다'),
                subtitle: const Text('옛 데스크톱이거나 연결이 끊겼다. 당겨서 다시.'),
                onTap: _reload,
              ),
            )
          else
            _AppearanceCard(
              appearance: _appearance!,
              pending: _pending,
              onPick: _apply,
            ),
        ],
      ),
    );
  }
}

class _SectionTitle extends StatelessWidget {
  const _SectionTitle(this.text);

  final String text;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(Look.pagePad, 0, Look.pagePad, Look.groupTitleGap),
      child: Text(
        text,
        style: theme.textTheme.labelMedium?.copyWith(
          color: theme.colorScheme.onSurfaceVariant,
        ),
      ),
    );
  }
}

/// 데스크톱 설정 화면의 외형 칸 — 강조색·모서리. 목록도 고른 것도 데스크톱이 준
/// 것이고, 누르면 데스크톱이 바뀐다. 테마(밝기)는 여기 없다 — 폰에서 밝은 테마를
/// 고르면 맥북까지 밝아졌다(2026-09-10 지적). 폰의 밝기는 위 「이 폰」 칸이 폰만 바꾼다.
class _AppearanceCard extends StatelessWidget {
  const _AppearanceCard({
    required this.appearance,
    required this.pending,
    required this.onPick,
  });

  final Map<String, Object?> appearance;
  final String? pending;
  final Future<void> Function(String action, String id) onPick;

  List<Map<String, Object?>> _list(String key) {
    final v = appearance[key];
    if (v is! List) return const [];
    return [
      for (final e in v)
        if (e is Map) e.cast<String, Object?>(),
    ];
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final accent = appearance['accent'] as String?;
    final shape = appearance['shape'] as String?;
    final busy = pending != null;
    return Card(
      child: Padding(
        padding: const EdgeInsets.fromLTRB(16, 12, 16, 14),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('강조색', style: theme.textTheme.titleSmall),
            const SizedBox(height: 8),
            Wrap(
              spacing: 10,
              runSpacing: 10,
              children: [
                for (final a in _list('accents'))
                  _AccentDot(
                    name: a['name'] as String? ?? '',
                    color: parseHexColor(a['hex'] as String?) ?? scheme.primary,
                    selected: a['name'] == accent,
                    onTap: busy || a['name'] is! String
                        ? null
                        : () => onPick('accent', a['name'] as String),
                  ),
              ],
            ),
            const SizedBox(height: 16),
            Text('모서리', style: theme.textTheme.titleSmall),
            const SizedBox(height: 8),
            Wrap(
              spacing: 8,
              runSpacing: 8,
              children: [
                for (final s in _list('shapes'))
                  ChoiceChip(
                    label: Text(s['label'] as String? ?? s['key'] as String? ?? ''),
                    selected: s['key'] == shape,
                    onSelected: busy || s['key'] is! String
                        ? null
                        : (_) => onPick('shape', s['key'] as String),
                  ),
              ],
            ),
            if (busy) ...[
              const SizedBox(height: 12),
              const LinearProgressIndicator(minHeight: 2),
            ],
          ],
        ),
      ),
    );
  }
}

/// 테마 카드 — 그 테마의 바탕·글자·ansi 색으로 그려야 고르기 전에 색이 보인다.
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
      child: InkWell(
        borderRadius: BorderRadius.circular(20),
        onTap: onTap,
        child: Container(
          width: 34,
          height: 34,
          decoration: BoxDecoration(
            color: color,
            shape: BoxShape.circle,
            border: Border.all(
              color: selected ? scheme.onSurface : scheme.outlineVariant,
              width: selected ? 2.5 : 1,
            ),
          ),
          child: selected
              ? Icon(
                  Icons.check,
                  size: 18,
                  color: color.computeLuminance() > 0.5
                      ? Colors.black87
                      : Colors.white,
                )
              : null,
        ),
      ),
    );
  }
}
