import 'dart:async';

import 'package:flutter/material.dart';

import '../contrast.dart';
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

class _LookSettingsScreenState extends State<LookSettingsScreen> {
  Server get server => widget.server;

  /// 데스크톱 「외형」 값 — 못 받으면 null 이고 그 칸은 안 그린다.
  Map<String, Object?>? _appearance;
  bool _loading = true;
  String? _pending;
  IndicatorPhase _phase = IndicatorPhase.working;
  final _previewKey = GlobalKey();
  final _controls = ScrollController();
  HSVColor? _accentDraft;
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

  void _chooseAccent(Color color) {
    if (_accentDraft == null && _controls.hasClients) _controls.jumpTo(0);
    setState(() => _accentDraft = HSVColor.fromColor(color));
  }

  Future<void> _commitAccent() async {
    final draft = _accentDraft;
    if (draft == null || _pending != null) return;
    final argb = draft.toColor().toARGB32();
    final preset = _entries(_appearance ?? const {}, 'accents')
        .where((entry) => parseHexColor(entry['hex'] as String?)?.toARGB32() == argb)
        .firstOrNull?['name'];
    if (await _apply('accent', preset as String? ?? AccentPicker.hex(draft.toColor())) && mounted) {
      _closeAccent();
    }
  }

  void _closeAccent() {
    if (_controls.hasClients) _controls.jumpTo(0);
    setState(() => _accentDraft = null);
  }

  Widget _shellGrid(TermSession shell, Color accent) => ListenableBuilder(
    listenable: shell,
    builder: (context, _) {
      if (!shell.canSend && shell.grid.version == 0) {
        return Center(child: Text(shell.note ?? '셸에 연결하는 중'));
      }
      final palette = TerminalPalette.forViewer(
        context,
        mode: phoneThemeMode.value,
        source: designTokens.value ?? shell.tokens,
        cursorOverride: accent,
      );
      return ColoredBox(
        color: palette.bg,
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 7),
          child: WrappedCanvas(
            grid: shell.grid,
            version: shell.grid.version + shell.historyVersion,
            palette: palette,
            history: shell.history,
            historyVersion: shell.historyVersion,
            bottomTick: _bottomTick,
            onViewport: shell.setViewport,
            fullScreen: shell.grid.alt,
          ),
        ),
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
  Future<bool> _apply(String action, String id) async {
    setState(() => _pending = '$action:$id');
    try {
      await server.settingsAction(action, id: id);
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
      appBar: AppBar(
        backgroundColor: Colors.transparent,
        title: const Text('외형'),
      ),
      body: LayoutBuilder(
        builder: (context, constraints) {
          final theme = Theme.of(context);
          final savedAccent = _accentColor(
            _appearance ?? const {},
            theme.colorScheme.primary,
          );
          final draft = _accentDraft;
          final previewAccent = draft?.toColor() ?? savedAccent;
          final shell = _shell;
          final pinned = constraints.maxHeight >= (draft == null ? 430 : 300);
          final preview = Padding(
            key: _previewKey,
            padding: const EdgeInsets.fromLTRB(
              Look.pagePad,
              4,
              Look.pagePad,
              10,
            ),
            child: Column(
              children: [
                ValueListenableBuilder<WeatherSettings>(
                  valueListenable: weather.settings,
                  builder: (context, value, _) => Theme(
                    data: theme.copyWith(
                      colorScheme: theme.colorScheme.copyWith(
                        primary: previewAccent,
                      ),
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
                      terminal: shell == null
                          ? null
                          : _shellGrid(shell, previewAccent),
                      accentOutline: draft != null,
                      action: TextButton(
                        onPressed: _openingShell ? null : _toggleShell,
                        child: Text(
                          shell != null
                              ? '셸 닫기'
                              : _openingShell
                              ? '연결 중'
                              : '셸 열기',
                        ),
                      ),
                    ),
                  ),
                ),
                if (shell != null)
                  Padding(
                    padding: const EdgeInsets.only(top: 8),
                    child: _shellInput(shell),
                  ),
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
                        child: _accentControls(
                          appearance,
                          savedAccent,
                          draft,
                          constraints.maxHeight < 640,
                        ),
                      ),
                    _lookControls(),
                    WeatherSettingsControls(
                      phase: _phase,
                      onPhaseChanged: (phase) => setState(() => _phase = phase),
                    ),
                    const SliverToBoxAdapter(
                      child: SizedBox(height: Look.groupGap),
                    ),
                  ],
                ),
              ),
            ],
          );
        },
      ),
    ),
  );

  Widget _accentControls(
    Map<String, Object?> appearance,
    Color saved,
    HSVColor? draft,
    bool compact,
  ) {
    final selected = draft?.toColor() ?? saved;
    final busy = _pending != null;
    return Padding(
      padding: const EdgeInsets.fromLTRB(Look.pagePad, 2, Look.pagePad, 6),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            mainAxisAlignment: MainAxisAlignment.spaceBetween,
            children: [
              Flexible(
                child: Text(
                  '강조색',
                  style: Theme.of(context).textTheme.titleSmall,
                ),
              ),
              if (draft == null)
                OutlinedButton.icon(
                  key: const Key('appearance-custom-accent'),
                  onPressed: busy
                      ? null
                      : () => _chooseAccent(saved),
                  icon: Icon(Icons.circle, color: selected, size: 16),
                  label: const Text('색 고르기'),
                ),
            ],
          ),
          Wrap(
            spacing: 5,
            runSpacing: 4,
            children: [
              for (final entry in _entries(appearance, 'accents'))
                if (parseHexColor(entry['hex'] as String?) case final color?)
                  _AccentDot(
                    name: entry['name'] as String? ?? '',
                    color: color,
                    selected: color.toARGB32() == selected.toARGB32(),
                    onTap: busy ? null : () => _chooseAccent(color),
                  ),
            ],
          ),
          if (draft != null) const SizedBox(height: 14),
          if (draft != null)
            AccentPicker(
              original: saved,
              color: draft,
              busy: busy,
              compact: compact,
              onChanged: (next) => setState(() => _accentDraft = next),
              onApply: _commitAccent,
              onCancel: _closeAccent,
            ),
        ],
      ),
    );
  }

  Widget _lookControls() => SliverPadding(
    padding: const EdgeInsets.symmetric(horizontal: Look.pagePad),
    sliver: SliverList.list(
      children: [
        SettingsGroup(
          title: '이 폰',
          flat: true,
          children: [
            SettingsRow(
              key: const Key('phone-brightness'),
              flat: true,
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
        SettingsGroup(flat: true, title: '데스크톱', children: _desktopRows(1)),
        const SizedBox(height: Look.groupGap / 2),
      ],
    ),
  );

  /// 데스크톱 「외형」 값을 폰에서 바꾸는 줄 — 강조색·모서리는 누르면 데스크톱이 바뀐다.
  /// 테마(밝기)는 데스크톱에 안 보낸다 — 폰에서 밝은 테마를 고르면 맥북까지 밝아졌다(2026-09-10 지적).
  List<Widget> _desktopRows(int tone) {
    final a = _appearance;
    final busy = _pending != null;
    if (_loading) {
      return [
        SettingsRow(
          flat: true,
          tone: tone,
          icon: Icons.palette_outlined,
          title: '외형',
          subtitle: '데스크톱 설정을 받는 중이에요',
        ),
      ];
    }
    if (a == null) {
      return [
        SettingsRow(
          flat: true,
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
        flat: true,
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
    ];
  }

  /// 데스크톱 모양 프리셋(`rounded`·`sharp`·`pixel`)의 그림. 모르는 새 프리셋은 일반 모양 그림으로.
  static IconData _shapeIcon(String key) => switch (key) {
    'rounded' => Icons.rounded_corner_rounded,
    'sharp' => Icons.crop_square_rounded,
    'pixel' => Icons.grid_4x4_rounded,
    _ => Icons.category_outlined,
  };

  static List<Map<String, Object?>> _entries(
    Map<String, Object?> a,
    String key,
  ) {
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
      child: Semantics(
        button: true,
        selected: selected,
        child: Material(
          type: MaterialType.transparency,
          shape: const CircleBorder(),
          child: InkWell(
            customBorder: const CircleBorder(),
            onTap: onTap,
            child: SizedBox.square(
              dimension: Look.tap,
              child: Stack(
                alignment: Alignment.center,
                children: [
                  if (selected)
                    Container(
                      decoration: BoxDecoration(
                        shape: BoxShape.circle,
                        border: Border.all(
                          color: enforceContrast(color, scheme.surface, 3),
                          width: 2,
                        ),
                      ),
                    ),
                  Ink(
                    width: 34,
                    height: 34,
                    decoration: BoxDecoration(
                      color: color,
                      shape: BoxShape.circle,
                      border: Border.all(
                        color: Color.lerp(
                          color,
                          enforceContrast(scheme.onSurface, color, 3),
                          0.19,
                        )!,
                      ),
                    ),
                    child: selected
                        ? Icon(
                            Icons.check_rounded,
                            size: 17,
                            color: enforceContrast(
                              scheme.onSurface,
                              color,
                              4.5,
                            ),
                          )
                        : null,
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}
