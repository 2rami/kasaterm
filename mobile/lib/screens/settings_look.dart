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
import 'accent_picker.dart';
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

enum _LookColorRole {
  accent('강조색', null),
  border('테두리', 5),
  background('터미널 배경', 27),
  foreground('터미널 글자', 1);

  const _LookColorRole(this.label, this.slot);
  final String label;
  final int? slot;

  Color? read(Map<String, Object?> appearance) {
    final index = slot;
    if (index == null) return null;
    final values = appearance[index < 27 ? 'palette_hex' : 'palette_surface_hex'];
    final offset = index < 27 ? index : index - 27;
    if (values is! List || offset >= values.length) return null;
    final value = values[offset];
    return value is String ? parseHexColor(value) : null;
  }
}

class _LookSettingsScreenState extends State<LookSettingsScreen> {
  Server get server => widget.server;

  /// 데스크톱 「외형」 값 — 못 받으면 null 이고 그 칸은 안 그린다.
  Map<String, Object?>? _appearance;
  bool _loading = true;
  String? _pending;
  IndicatorPhase _phase = IndicatorPhase.working;
  final _previewKey = GlobalKey();
  final _controls = ScrollController();
  (_LookColorRole, HSVColor)? _colorDraft;
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
    _controls.dispose();
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

  Color _accentColor(Map<String, Object?> a, Color fallback) {
    final id = a['accent'];
    return parseHexColor(id is String ? id : null)
        ?? _entries(a, 'accents')
            .where((entry) => entry['name'] == id)
            .map((entry) => parseHexColor(entry['hex'] as String?))
            .firstOrNull
        ?? fallback;
  }

  void _chooseColor(_LookColorRole role, Color color) {
    if (_colorDraft == null && _controls.hasClients) _controls.jumpTo(0);
    setState(() => _colorDraft = (role, HSVColor.fromColor(color)));
  }

  Future<void> _commitColor() async {
    final draft = _colorDraft;
    if (draft == null || _pending != null) return;
    final (role, hsv) = draft;
    final color = hsv.toColor();
    final hex = AccentPicker.hex(color);
    final slot = role.slot;
    bool saved;
    if (slot != null) {
      saved = await _apply('palette-hex', '$slot', label: hex);
    } else {
      final preset = _entries(_appearance ?? const {}, 'accents')
          .where((entry) => parseHexColor(entry['hex'] as String?)?.toARGB32() == color.toARGB32())
          .firstOrNull?['name'];
      saved = await _apply('accent', preset as String? ?? hex);
    }
    if (saved && mounted) _closeColor();
  }

  void _closeColor() {
    if (_controls.hasClients) _controls.jumpTo(0);
    setState(() => _colorDraft = null);
  }

  Widget _shellGrid(TermSession shell, Color accent, Color? background, Color? foreground) => ListenableBuilder(
    listenable: shell,
    builder: (context, _) {
      if (!shell.canSend && shell.grid.version == 0) {
        return Center(child: Text(shell.note ?? '셸에 연결하는 중'));
      }
      return WrappedCanvas(
        grid: shell.grid,
        version: shell.grid.version + shell.historyVersion,
        palette: TerminalPalette.forViewer(
          context, mode: phoneThemeMode.value, source: designTokens.value ?? shell.tokens, cursorOverride: accent,
        ).withTerminalColors(background: background, foreground: foreground),
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
  Future<bool> _apply(String action, String id, {String? label}) async {
    setState(() => _pending = '$action:$id');
    try {
      await server.settingsAction(action, id: id, label: label);
      final t = await server.designTokens();
      if (t != null && mounted && !server.isClosed) designTokens.value = t;
      return true;
    } on ServerException catch (e) {
      if (mounted) ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(e.message)));
      return false;
    } finally {
      if (mounted) {
        await _reload();
        if (mounted) setState(() => _pending = null);
      }
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
        final savedAccent = _accentColor(_appearance ?? const {}, theme.colorScheme.primary);
        final draft = _colorDraft;
        final appearance = _appearance ?? const <String, Object?>{};
        Color? previewColor(_LookColorRole role) {
          if (draft != null && draft.$1 == role) return draft.$2.toColor();
          return role == _LookColorRole.accent ? savedAccent : role.read(appearance);
        }
        final previewAccent = previewColor(_LookColorRole.accent) ?? savedAccent;
        final background = previewColor(_LookColorRole.background);
        final foreground = previewColor(_LookColorRole.foreground);
        final shell = _shell;
        final pinned = constraints.maxHeight >= (draft == null ? 430 : 300);
        final preview = Padding(
          key: _previewKey,
          padding: const EdgeInsets.fromLTRB(Look.pagePad, 4, Look.pagePad, 10),
          child: Column(
            children: [
              ValueListenableBuilder<WeatherSettings>(
                valueListenable: weather.settings,
                builder: (context, value, _) => Theme(
                  data: theme.copyWith(
                    colorScheme: theme.colorScheme.copyWith(primary: previewAccent),
                  ),
                  child: WeatherPreview(
                    key: const Key('appearance-preview'),
                    settings: value,
                    phase: _phase,
                    height: draft == null
                        ? (constraints.maxHeight * .27).clamp(62.0, 245.0)
                        : (constraints.maxHeight * .20).clamp(48.0, 180.0),
                    cornerRadius: switch (_appearance?['shape']) {
                      'sharp' => 3,
                      'pixel' => 0,
                      _ => Look.radius,
                    },
                    terminal: shell == null ? null : _shellGrid(shell, previewAccent, background, foreground),
                    accentOutline: draft?.$1 == _LookColorRole.accent,
                    paneBackground: background,
                    paneForeground: foreground,
                    paneBorderColor: previewColor(_LookColorRole.border),
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
                controller: _controls,
                slivers: [
                  if (!pinned) SliverToBoxAdapter(child: preview),
                  if (_appearance case final appearance?)
                    SliverToBoxAdapter(
                      child: _colorControls(appearance, savedAccent, draft, constraints.maxHeight < 640),
                    ),
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

  Widget _colorControls(Map<String, Object?> appearance, Color saved, (_LookColorRole, HSVColor)? draft, bool compact) {
    final selected = draft?.$1 == _LookColorRole.accent ? draft!.$2.toColor() : saved;
    final savedRole = draft?.$1.read(appearance);
    final busy = _pending != null;
    return Padding(
      padding: const EdgeInsets.fromLTRB(Look.pagePad, 2, Look.pagePad, 6),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Text('강조색', style: Theme.of(context).textTheme.titleSmall),
          Wrap(
            spacing: 3,
            crossAxisAlignment: WrapCrossAlignment.center,
            children: [
              for (final entry in _entries(appearance, 'accents'))
                if (parseHexColor(entry['hex'] as String?) case final color?)
                  _AccentDot(
                    name: entry['name'] as String? ?? '',
                    color: color,
                    selected: color.toARGB32() == selected.toARGB32(),
                    onTap: busy ? null : () => _chooseColor(_LookColorRole.accent, color),
                  ),
              TextButton.icon(
                key: const Key('appearance-custom-accent'),
                onPressed: busy || draft?.$1 == _LookColorRole.accent ? null : () => _chooseColor(_LookColorRole.accent, saved),
                icon: Icon(Icons.circle, color: selected, size: 17),
                label: const Text('색 고르기'),
              ),
            ],
          ),
          Wrap(
            spacing: 6,
            runSpacing: 4,
            children: [
              for (final role in _LookColorRole.values)
                if (role.read(appearance) case final swatch?)
                  OutlinedButton.icon(
                    key: ValueKey('appearance-color-${role.name}'),
                    style: OutlinedButton.styleFrom(
                      backgroundColor: draft?.$1 == role ? Theme.of(context).colorScheme.secondaryContainer : null,
                    ),
                    icon: Icon(Icons.square_rounded,
                      color: draft?.$1 == role ? draft!.$2.toColor() : swatch, size: 16),
                    label: Text(role.label),
                    onPressed: busy || draft?.$1 == role ? null : () => _chooseColor(role, swatch),
                  ),
            ],
          ),
          if (draft != null)
            AccentPicker(
              key: ValueKey(draft.$1),
              original: savedRole ?? saved,
              color: draft.$2,
              busy: busy,
              compact: compact,
              onChanged: (next) => setState(() => _colorDraft = (draft.$1, next)),
              onApply: _commitColor,
              onCancel: _closeColor,
            ),
        ],
      ),
    );
  }

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
