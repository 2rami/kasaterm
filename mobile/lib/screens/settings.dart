import 'package:flutter/material.dart';

import '../main.dart' show designTokens;
import '../server.dart';
import '../theme_prefs.dart';
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

  /// 브라우징 대상 목록 — 옛 서버는 못 준다(null). 「이 폰」은 `phone:<내 이름>`.
  BrowseDevices? _browse;
  String _myName = '';
  bool _loading = true;
  String? _pending;

  @override
  void initState() {
    super.initState();
    _reload();
  }

  Future<void> _reload() async {
    Map<String, Object?>? a;
    BrowseDevices? b;
    var name = _myName;
    try {
      a = await server.appearance();
    } on ServerException {
      a = null;
    }
    try {
      b = await server.browseDevices();
      if (name.isEmpty) name = (await server.me()).name;
    } on ServerException {
      b = null;
    }
    if (!mounted) return;
    setState(() {
      _appearance = a;
      _browse = b;
      _myName = name;
      _loading = false;
    });
  }

  /// 브라우징 기기·목적지 — 데스크톱 설정과 같은 액션. 바뀐 목록을 되받는다.
  Future<void> _applyBrowse(String action, String id) async {
    setState(() => _pending = '$action:$id');
    try {
      await server.settingsAction(action, id: id);
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

  /// 데스크톱에 액션을 보내고, 바뀐 색을 되받아 폰도 같은 얼굴로.
  Future<void> _apply(String action, String id) async {
    setState(() => _pending = '$action:$id');
    try {
      await server.settingsAction(action, id: id);
      final t = await server.designTokens();
      if (t != null) designTokens.value = t;
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
    phoneThemeMode.value = m;
    await const ThemePrefs().save(m);
    if (mounted) setState(() {});
  }

  Future<void> _forget(BuildContext context) async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('주소를 지울까'),
        content: const Text('이 폰에서 저장한 주소를 지운다. 데스크톱 허브에서 다시 복사해 넣으면 된다.'),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('취소'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('지우기'),
          ),
        ],
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
        padding: const EdgeInsets.all(12),
        children: [
          Card(
            child: Column(
              children: [
                ListTile(
                  leading: const Icon(Icons.link),
                  title: const Text('연결된 주소'),
                  subtitle: Text(server.describe()),
                ),
                const Divider(height: 1),
                ListTile(
                  leading: Icon(
                    Icons.delete_outline,
                    color: theme.colorScheme.error,
                  ),
                  title: const Text('주소 바꾸기 · 지우기'),
                  subtitle: const Text('지우면 연결 화면으로 돌아간다'),
                  onTap: () => _forget(context),
                ),
              ],
            ),
          ),
          const SizedBox(height: 16),
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
          const SizedBox(height: 16),
          _SectionTitle('브라우징'),
          if (_loading)
            const SizedBox.shrink()
          else if (_browse == null)
            Card(
              child: ListTile(
                leading: const Icon(Icons.cloud_off),
                title: const Text('브라우징 설정을 못 받았다'),
                subtitle: const Text('옛 데스크톱이거나 연결이 끊겼다. 눌러서 다시.'),
                onTap: _reload,
              ),
            )
          else
            BrowseCard(
              data: _browse!,
              myName: _myName,
              pending: _pending,
              onPick: _applyBrowse,
            ),
          const SizedBox(height: 16),
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
      padding: const EdgeInsets.fromLTRB(4, 0, 4, 6),
      child: Text(
        text,
        style: theme.textTheme.labelLarge?.copyWith(
          color: theme.colorScheme.onSurfaceVariant,
        ),
      ),
    );
  }
}

/// 데스크톱 설정 화면의 외형 칸 — 테마·강조색·모서리. 목록도 고른 것도 데스크톱이
/// 준 것이고, 누르면 데스크톱이 바뀐다.
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
    final themeKey = appearance['theme'] as String?;
    final accent = appearance['accent'] as String?;
    final shape = appearance['shape'] as String?;
    final busy = pending != null;
    return Card(
      child: Padding(
        padding: const EdgeInsets.fromLTRB(16, 12, 16, 14),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('테마', style: theme.textTheme.titleSmall),
            const SizedBox(height: 8),
            Wrap(
              spacing: 8,
              runSpacing: 8,
              children: [
                for (final t in _list('themes'))
                  _ThemeChip(
                    label: t['label'] as String? ?? t['key'] as String? ?? '',
                    bg: parseHexColor(t['bg'] as String?),
                    text: parseHexColor(t['text'] as String?),
                    ansi: [
                      if (t['ansi'] is List)
                        for (final c in t['ansi'] as List)
                          if (c is String) parseHexColor(c),
                    ],
                    selected: t['key'] == themeKey,
                    onTap: busy || t['key'] is! String
                        ? null
                        : () => onPick('theme-mode', t['key'] as String),
                  ),
              ],
            ),
            const SizedBox(height: 16),
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

/// 「사람이 볼 페이지」가 어느 기기의 무엇으로 열리는지(docs/browse-target.md).
/// 목록도 고른 것도 데스크톱이 준 것이고, 누르면 데스크톱 설정이 바뀐다.
/// 액션 이름은 데스크톱과 같다 — `browse-device`(id·`auto`) · `browse-open`(web·chrome).
class BrowseCard extends StatelessWidget {
  const BrowseCard({
    super.key,
    required this.data,
    required this.myName,
    required this.pending,
    required this.onPick,
  });

  final BrowseDevices data;

  /// 이 폰의 이름(`mobile/me`) — 목록의 `phone:<이름>` 을 「이 폰」으로 보인다.
  final String myName;
  final String? pending;
  final Future<void> Function(String action, String id) onPick;

  String get myId => 'phone:$myName';

  /// 목록에 보일 이름 — 이 폰은 「이 폰」, 나머지는 서버가 준 라벨.
  String labelOf(BrowseDevice d) => d.id == myId && myName.isNotEmpty
      ? '이 폰'
      : d.label;

  static String kindOf(BrowseDevice d) {
    final parts = <String>[
      if (d.isPhone) '폰' else '데스크톱',
      if ((d.model ?? '').isNotEmpty) d.model!,
      if (d.viewport != null) '${d.viewport!.width}×${d.viewport!.height}',
      if (!d.online) '오프라인',
    ];
    return parts.join(' · ');
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final busy = pending != null;
    final selected = data.selectedItem;
    Widget check(bool on) => Icon(
      on ? Icons.radio_button_checked : Icons.radio_button_off,
      color: on ? scheme.primary : scheme.outline,
    );
    return Card(
      child: Padding(
        padding: const EdgeInsets.fromLTRB(16, 12, 16, 14),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('어느 기기로', style: theme.textTheme.titleSmall),
            const SizedBox(height: 4),
            ListTile(
              contentPadding: EdgeInsets.zero,
              leading: check(selected == BrowseDevices.autoId),
              title: const Text('자동'),
              subtitle: Text(
                data.auto && data.selected.isNotEmpty
                    ? '거울로 보는 쪽이 있으면 그 기기 — 지금은 ${_labelById(data.selected)}'
                    : '거울로 보는 쪽이 있으면 그 기기, 없으면 데스크톱',
              ),
              onTap: busy ? null : () => onPick('browse-device', BrowseDevices.autoId),
            ),
            for (final d in data.devices)
              ListTile(
                contentPadding: EdgeInsets.zero,
                leading: check(selected == d.id),
                title: Text(labelOf(d)),
                subtitle: Text(kindOf(d)),
                enabled: !busy,
                onTap: busy ? null : () => onPick('browse-device', d.id),
              ),
            const SizedBox(height: 12),
            Text('무엇으로', style: theme.textTheme.titleSmall),
            const SizedBox(height: 8),
            SizedBox(
              width: double.infinity,
              child: SegmentedButton<String>(
                showSelectedIcon: false,
                segments: const [
                  ButtonSegment(value: 'web', label: Text('내장 웹')),
                  ButtonSegment(value: 'chrome', label: Text('브라우저')),
                ],
                selected: {data.open == 'web' ? 'web' : 'chrome'},
                onSelectionChanged: busy
                    ? null
                    : (s) => onPick('browse-open', s.first),
              ),
            ),
            const SizedBox(height: 8),
            Text(
              data.open == 'web'
                  ? '폰이면 앱 안 웹 화면, 데스크톱이면 그 pane 의 탭으로 연다.'
                  : '그 기기의 브라우저로 연다 — 폰은 사파리, 맥은 기본 브라우저.',
              style: theme.textTheme.bodySmall?.copyWith(
                color: scheme.onSurfaceVariant,
              ),
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

  String _labelById(String id) {
    for (final d in data.devices) {
      if (d.id == id) return labelOf(d);
    }
    return id.isEmpty ? '데스크톱' : id;
  }
}

/// 테마 카드 — 그 테마의 바탕·글자·ansi 색으로 그려야 고르기 전에 색이 보인다.
class _ThemeChip extends StatelessWidget {
  const _ThemeChip({
    required this.label,
    required this.bg,
    required this.text,
    required this.ansi,
    required this.selected,
    this.onTap,
  });

  final String label;
  final Color? bg;
  final Color? text;
  final List<Color?> ansi;
  final bool selected;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final fill = bg ?? scheme.surfaceContainerHighest;
    final ink = text ?? scheme.onSurface;
    return Material(
      color: fill,
      borderRadius: BorderRadius.circular(10),
      child: InkWell(
        borderRadius: BorderRadius.circular(10),
        onTap: onTap,
        child: Container(
          width: 104,
          padding: const EdgeInsets.fromLTRB(10, 8, 10, 8),
          decoration: BoxDecoration(
            borderRadius: BorderRadius.circular(10),
            border: Border.all(
              color: selected ? scheme.primary : scheme.outlineVariant,
              width: selected ? 2 : 1,
            ),
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                label,
                style: TextStyle(
                  color: ink,
                  fontSize: 12,
                  fontWeight: selected ? FontWeight.w600 : FontWeight.w500,
                ),
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
              ),
              const SizedBox(height: 6),
              Row(
                children: [
                  for (final c in ansi)
                    if (c != null)
                      Container(
                        width: 10,
                        height: 10,
                        margin: const EdgeInsets.only(right: 3),
                        decoration: BoxDecoration(
                          color: c,
                          borderRadius: BorderRadius.circular(3),
                        ),
                      ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }
}

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
