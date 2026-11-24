import 'package:flutter/material.dart';

import '../look.dart';
import '../main.dart' show designTokens;
import '../server.dart';
import '../theme_prefs.dart';
import '../twins_loading.dart';
import '../weather/controls.dart';
import '../weather/model.dart';
import '../weather/sheet.dart';
import '../weather/store.dart';
import 'controls.dart';
import 'hub.dart' show parseHexColor;
import 'settings_glyph.dart';

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
  bool _weatherTab = false;
  IndicatorPhase _phase = IndicatorPhase.working;

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
        final previewAccent = _entries(_appearance ?? const {}, 'accents')
            .where((entry) => entry['name'] == _appearance?['accent'])
            .map((entry) => parseHexColor(entry['hex'] as String?))
            .firstOrNull;
        return Column(
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(Look.pagePad, 4, Look.pagePad, 10),
              child: ValueListenableBuilder<WeatherSettings>(
                valueListenable: weather.settings,
                builder: (context, value, _) => Theme(
                  data: previewAccent == null ? theme : theme.copyWith(
                    colorScheme: theme.colorScheme.copyWith(primary: previewAccent),
                  ),
                  child: WeatherPreview(
                    key: const Key('appearance-preview'),
                    settings: value,
                    phase: _phase,
                    height: (constraints.maxHeight * .27).clamp(56.0, 208.0),
                    cornerRadius: switch (_appearance?['shape']) {
                      'sharp' => 3,
                      'pixel' => 0,
                      _ => Look.radius,
                    },
                  ),
                ),
              ),
            ),
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: Look.pagePad),
              child: WeatherChoices(
                options: const [(false, '색과 모양'), (true, '날씨')],
                selected: {_weatherTab},
                symbol: (weather, ink) => SettingsMark(
                  weather ? SettingsGlyph.weather : SettingsGlyph.appearance,
                  size: 17,
                  color: ink,
                ),
                onPressed: (value) => setState(() => _weatherTab = value),
              ),
            ),
            Expanded(
              child: _weatherTab
                  ? WeatherSettingsList(phase: _phase, onPhaseChanged: (phase) => setState(() => _phase = phase))
                  : _lookControls(),
            ),
          ],
        );
      }),
    ),
  );

  Widget _lookControls() => ListView(
    key: const PageStorageKey('appearance-look-controls'),
    padding: const EdgeInsets.fromLTRB(Look.pagePad, 0, Look.pagePad, Look.groupGap * 2),
    children: [
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
    ],
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
