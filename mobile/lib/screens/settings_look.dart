import 'dart:async';

import 'package:flutter/material.dart';

import '../grid_canvas.dart';
import '../hub_actions.dart';
import '../look.dart';
import '../term_session.dart';
import '../main.dart' show designTokens;
import '../server.dart';
import '../theme_prefs.dart';
import '../twins_loading.dart';
import '../weather/model.dart';
import '../weather/sheet.dart';
import '../weather/store.dart';
import 'controls.dart';
import 'hub.dart' show parseHexColor;

/// 폰 밝기의 고르기 칸 — 짧은 이름은 칸 말풍선과 최상위 「외형」 줄이 같이 쓴다.
const phoneModes = [
  (ThemeMode.system, Icons.desktop_windows_outlined, '데스크톱 따라감'),
  (ThemeMode.light, Icons.light_mode_outlined, '밝게'),
  (ThemeMode.dark, Icons.dark_mode_outlined, '어둡게'),
];

String _modeLabel(ThemeMode mode) => switch (mode) {
  ThemeMode.system => '데스크톱 테마 그대로',
  ThemeMode.light => '밝게 · 같은 테마 색을 밝게',
  ThemeMode.dark => '어둡게 · 같은 테마 색을 어둡게',
};

/// 설정 「외형」 — 이 폰의 밝기, 날씨, 데스크톱 강조색과 모서리.
class LookSettingsScreen extends StatefulWidget {
  const LookSettingsScreen({super.key, required this.server});

  final Server server;

  @override
  State<LookSettingsScreen> createState() => _LookSettingsScreenState();
}

class _LookSettingsScreenState extends State<LookSettingsScreen> {
  Server get server => widget.server;

  /// 데스크톱 「외형」 값 — 못 받으면 null 이고 그 칸은 안 그린다.
  Map<String, Object?>? _appearance;
  bool _loading = true;
  String? _pending;
  IndicatorPhase _phase = IndicatorPhase.working;
  final _previewKey = GlobalKey();
  final _command = TextEditingController();
  TermSession? _shell;
  bool _openingShell = false;
  int _bottomTick = 0;

  @override
  void initState() {
    super.initState();
    _reload();
  }

  @override
  void dispose() {
    final shell = _shell;
    if (shell != null) {
      shell.dispose();
      unawaited(_discardShell(server, shell.pane));
    }
    _command.dispose();
    super.dispose();
  }

  static Future<void> _closeShell(Server server, Pane pane) async {
    final current = (await server.panes(machine: pane.machine))
        .where((row) => !row.closed && row.id == pane.id && row.surfaceKey == pane.surfaceKey)
        .firstOrNull;
    if (current == null) return;
    final target = (await HubActions(server).prepare([current])).single;
    await server.cmd(
      'window.close_checked',
      {'idx': current.window, 'addresses': [target.address]},
      machine: current.machine,
    );
  }

  static Future<void> _discardShell(Server server, Pane pane) async {
    try {
      await _closeShell(server, pane);
    } on ServerException {
      // The desktop may already have disconnected or moved the pane out of its temporary room.
    }
  }

  Future<void> _toggleShell() async {
    if (_openingShell) return;
    setState(() => _openingShell = true);
    final host = server;
    try {
      final active = _shell;
      if (active != null) {
        await _closeShell(host, active.pane);
        if (mounted) {
          setState(() => _shell = null);
          active.dispose();
        }
        return;
      }
      final reply = await host.desktopAction('spawn-shell', query: const {'window': 'new'});
      final surface = reply['surface'];
      if (surface is! String || surface.isEmpty) {
        throw const ServerException('시험용 셸을 찾지 못했어요.');
      }
      Pane? pane;
      for (var attempt = 0; attempt < 5 && pane == null; attempt++) {
        pane = (await host.panes()).where((row) =>
            !row.closed && row.id == surface && (row.surfaceKey?.isNotEmpty ?? false)).firstOrNull;
        if (pane == null) await Future<void>.delayed(const Duration(milliseconds: 150));
      }
      if (pane == null) {
        throw const ServerException('시험용 셸의 주소를 확인하지 못했어요.');
      }
      if (!mounted) {
        await _discardShell(host, pane);
        return;
      }
      final session = TermSession(host, pane)..holdViewport = true;
      session.connect();
      setState(() => _shell = session);
    } on ServerException catch (error) {
      if (mounted) ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(error.message)));
    } finally {
      if (mounted) setState(() => _openingShell = false);
    }
  }

  Future<void> _customAccent() async {
    final current = _appearance?['accent'];
    var value = current is String && current.startsWith('#')
        ? current
        : '#${(Theme.of(context).colorScheme.primary.toARGB32() & 0xffffff).toRadixString(16).padLeft(6, '0')}';
    final form = GlobalKey<FormState>();
    final choice = await showDialog<String>(
      context: context,
      builder: (dialog) => AlertDialog(
        title: const Text('강조색 직접 입력'),
        content: Form(
          key: form,
          child: TextFormField(
            autofocus: true,
            initialValue: value,
            decoration: const InputDecoration(labelText: 'HEX', hintText: '#5B9BF5'),
            autocorrect: false,
            onChanged: (next) => value = next,
            validator: (text) => RegExp(r'^#?[0-9a-fA-F]{6}$').hasMatch(text?.trim() ?? '')
                ? null : '6자리 색상 코드를 입력하세요',
          ),
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(dialog), child: const Text('취소')),
          FilledButton(
            onPressed: () {
              if (form.currentState?.validate() != true) return;
              final hex = value.trim().toLowerCase();
              Navigator.pop(dialog, hex.startsWith('#') ? hex : '#$hex');
            },
            child: const Text('적용'),
          ),
        ],
      ),
    );
    if (choice != null && mounted) await _apply('accent', choice);
  }

  Widget _shellGrid(TermSession shell) => ListenableBuilder(
    listenable: shell,
    builder: (context, _) {
      if (!shell.canSend && shell.grid.version == 0) {
        return Center(child: Text(shell.note ?? '셸에 연결하는 중'));
      }
      return WrappedCanvas(
        grid: shell.grid,
        version: shell.grid.version + shell.historyVersion,
        palette: TerminalPalette.forViewer(
          context, mode: phoneThemeMode.value, source: designTokens.value ?? shell.tokens,
        ),
        history: shell.history,
        historyVersion: shell.historyVersion,
        bottomTick: _bottomTick,
        onViewport: shell.setViewport,
        fullScreen: shell.grid.alt,
      );
    },
  );

  Widget _shellInput(TermSession shell) => ListenableBuilder(
    listenable: shell,
    builder: (context, _) => TextField(
      controller: _command,
      decoration: InputDecoration(
        labelText: '셸 명령',
        isDense: true,
        helperText: shell.canSend ? null : shell.note ?? '셸에 연결하는 중',
      ),
      textInputAction: TextInputAction.send,
      autocorrect: false,
      enabled: shell.canSend,
      onSubmitted: (text) {
        if (!shell.canSend) return;
        shell.sendText('$text\r');
        _command.clear();
        setState(() => _bottomTick++);
      },
    ),
  );

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
      if (mounted) ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(e.message)));
    } finally {
      if (mounted) setState(() => _pending = null);
      await _reload();
    }
  }

  Future<void> _setMode(ThemeMode m) async {
    final problem = await phoneThemeSync.setMode(m);
    if (!mounted) return;
    setState(() {});
    if (problem != null) ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(problem)));
  }

  @override
  Widget build(BuildContext context) => TwinBackdrop(
    child: Scaffold(
      backgroundColor: Colors.transparent,
      appBar: AppBar(backgroundColor: Colors.transparent, title: const Text('외형')),
      body: LayoutBuilder(builder: (context, constraints) {
        final theme = Theme.of(context);
        final current = _appearance?['accent'];
        final previewAccent = parseHexColor(current is String ? current : null)
            ?? _entries(_appearance ?? const {}, 'accents')
                .where((entry) => entry['name'] == current)
                .map((entry) => parseHexColor(entry['hex'] as String?))
                .firstOrNull;
        final shell = _shell;
        final pinned = constraints.maxHeight >= 430;
        final preview = Padding(
          key: _previewKey,
          padding: const EdgeInsets.fromLTRB(Look.pagePad, 4, Look.pagePad, 10),
          child: Column(
            children: [
              ValueListenableBuilder<WeatherSettings>(
                valueListenable: weather.settings,
                builder: (context, value, _) => Theme(
                  data: previewAccent == null ? theme : theme.copyWith(
                    colorScheme: theme.colorScheme.copyWith(primary: previewAccent),
                  ),
                  child: WeatherPreview(
                    key: const Key('appearance-preview'),
                    settings: value,
                    phase: _phase,
                    height: (constraints.maxHeight * .27).clamp(62.0, 245.0),
                    cornerRadius: switch (_appearance?['shape']) {
                      'sharp' => 3,
                      'pixel' => 0,
                      _ => Look.radius,
                    },
                    terminal: shell == null ? null : _shellGrid(shell),
                    action: TextButton(
                      onPressed: _openingShell ? null : _toggleShell,
                      child: Text(shell != null ? '셸 닫기' : _openingShell ? '연결 중' : '셸 열기'),
                    ),
                  ),
                ),
              ),
              if (shell != null) _shellInput(shell),
            ],
          ),
        );
        return Column(
          children: [
            if (pinned) preview,
            Expanded(
              child: CustomScrollView(
                key: const PageStorageKey('appearance-controls'),
                slivers: [
                  if (!pinned) SliverToBoxAdapter(child: preview),
                  _lookControls(),
                  WeatherSettingsControls(
                    phase: _phase,
                    onPhaseChanged: (phase) => setState(() => _phase = phase),
                  ),
                  const SliverToBoxAdapter(child: SizedBox(height: Look.groupGap)),
                ],
              ),
            ),
          ],
        );
      }),
    ),
  );

  Widget _lookControls() => SliverPadding(
    padding: const EdgeInsets.symmetric(horizontal: Look.pagePad),
    sliver: SliverList.list(children: [
      SettingsGroup(
        title: '이 폰',
        children: [
          SettingsRow(
            key: const Key('phone-brightness'),
            icon: Icons.brightness_6_outlined,
            title: '밝기',
            subtitle: _modeLabel(phoneThemeMode.value),
            trailing: IconChoice<ThemeMode>(
              options: phoneModes,
              selected: phoneThemeMode.value,
              onSelect: _setMode,
            ),
          ),
        ],
      ),
      SettingsGroup(title: '데스크톱', children: _desktopRows(1)),
      const SizedBox(height: Look.groupGap / 2),
    ]),
  );

  /// 데스크톱 「외형」 값을 폰에서 바꾸는 줄 — 강조색·모서리는 누르면 데스크톱이 바뀐다.
  /// 테마(밝기)는 데스크톱에 안 보낸다 — 폰에서 밝은 테마를 고르면 맥북까지 밝아졌다(2026-09-10 지적).
  List<Widget> _desktopRows(int tone) {
    final a = _appearance;
    final busy = _pending != null;
    if (_loading) {
      return [SettingsRow(tone: tone, icon: Icons.palette_outlined, title: '외형', subtitle: '데스크톱 설정을 받는 중이에요')];
    }
    if (a == null) {
      return [
        SettingsRow(
          tone: tone,
          icon: Icons.cloud_off_outlined,
          title: '외형',
          subtitle: '데스크톱 설정을 못 받았어요 · 옛 데스크톱이거나 연결이 끊겼어요',
          trailing: TextButton(onPressed: _reload, child: const Text('다시')),
        ),
      ];
    }
    return [
      SettingsRow(
        tone: tone,
        icon: Icons.palette_outlined,
        title: '강조색',
        subtitle: a['accent'] is String && (a['accent'] as String).startsWith('#') ? a['accent'] as String : null,
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
            TextButton(
              key: const Key('appearance-custom-accent'),
              onPressed: busy ? null : _customAccent,
              child: const Text('직접 입력'),
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
              if (sh['key'] case final String key) (key, _shapeIcon(key), sh['label'] as String? ?? key),
          ],
          selected: a['shape'] as String?,
          onSelect: busy ? null : (key) => _apply('shape', key),
        ),
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

/// 강조색 동그라미 — 고른 것은 바깥 고리. 누름 영역은 44.
class _AccentDot extends StatelessWidget {
  const _AccentDot({required this.name, required this.color, required this.selected, this.onTap});

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
              child: DecoratedBox(
                decoration: BoxDecoration(color: color, shape: BoxShape.circle),
              ),
            ),
          ),
        ),
      ),
    );
  }
}
