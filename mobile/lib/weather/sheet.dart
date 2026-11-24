import 'package:flutter/material.dart';

import '../look.dart';
import '../screens/controls.dart';
import 'card.dart';
import 'controls.dart';
import 'model.dart';
import 'scene.dart';
import 'store.dart';

Color _weatherSurface(BuildContext context) => WeatherPalette.of(context).canvas;

/// 설정 「날씨」 — docs/weather.md 의 표 전부와 「날씨는 기기마다」.
Future<void> showWeatherSheet(BuildContext context) => showModalBottomSheet<void>(
  context: context,
  showDragHandle: true,
  backgroundColor: _weatherSurface(context),
  isScrollControlled: true,
  builder: (_) => const ModalLook(child: _WeatherSheet()),
);

/// 카드 길게 누르기 「이 카드 날씨」 — 이 폰에만 남는다.
Future<void> showCardWeatherSheet(BuildContext context, String id) {
  if (!weather.settings.value.enabled) return Future.value();
  return showModalBottomSheet<void>(
    context: context,
    showDragHandle: true,
    builder: (sheet) => ModalLook(
      child: SafeArea(
        child: ValueListenableBuilder(
          valueListenable: weather.cards,
          builder: (context, _, _) {
            final now = weather.cardOf(id);
            return Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                const ListTile(
                  leading: Icon(Icons.umbrella_outlined),
                  title: Text('이 카드 날씨'),
                  subtitle: Text('이 폰에만 — 「어디에」 설정보다 이긴다'),
                ),
                const Divider(height: 1),
                for (final w in CardWeather.menu)
                  ListTile(
                    title: Text(w.label),
                    trailing: w == now ? const Icon(Icons.check) : null,
                    onTap: () {
                      weather.setCard(id, w);
                      Navigator.pop(sheet);
                    },
                  ),
                const SizedBox(height: 8),
              ],
            );
          },
        ),
      ),
    ),
  );
}

/// 카드를 길게 눌러 뜨는 다른 시트(학생·방·정리)에 붙는 한 줄. 길게 누른 카드가 곧 초점 카드다.
class CardWeatherRow extends StatelessWidget {
  const CardWeatherRow({super.key});

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: weather.changes,
    builder: (context, _) {
      final id = weather.focused.value;
      final settings = weather.settings.value;
      if (!settings.enabled || settings.style != RainStyle.liquid || id == null) return const SizedBox.shrink();
      return ListTile(
        leading: const _OpenUmbrella(size: 24),
        title: const Text('이 카드 날씨'),
        subtitle: Text(weather.cardOf(id).label),
        trailing: const Icon(Icons.chevron_right),
        onTap: () {
          final nav = Navigator.of(context);
          nav.pop();
          showCardWeatherSheet(nav.context, id);
        },
      );
    },
  );
}

class _WeatherSheet extends StatelessWidget {
  const _WeatherSheet();

  @override
  Widget build(BuildContext context) => DraggableScrollableSheet(
    expand: false,
    initialChildSize: 0.93,
    maxChildSize: 0.95,
    builder: (context, scroll) => WeatherSettingsList(controller: scroll),
  );
}

/// 넓은 설정의 오른쪽 칸 「날씨」 — 폰 판의 시트와 같은 줄을 페이지로.
class WeatherSettingsScreen extends StatelessWidget {
  const WeatherSettingsScreen({super.key});

  @override
  Widget build(BuildContext context) => Scaffold(
    backgroundColor: _weatherSurface(context),
    appBar: AppBar(backgroundColor: _weatherSurface(context), title: const Text('날씨')),
    body: const WeatherSettingsList(),
  );
}

/// 날씨 설정 줄 전부 — 시트와 페이지가 같이 쓴다.
class WeatherSettingsList extends StatefulWidget {
  const WeatherSettingsList({super.key, this.controller});

  final ScrollController? controller;

  @override
  State<WeatherSettingsList> createState() => _WeatherSettingsListState();
}

class _WeatherSettingsListState extends State<WeatherSettingsList> {
  IndicatorPhase _phase = IndicatorPhase.working;
  static const _rewet = [30, 60, 120, 180, 300, 600];

  @override
  Widget build(BuildContext context) => Theme(
    data: WeatherPalette.of(context).theme(Theme.of(context)),
    child: ListenableBuilder(
    listenable: Listenable.merge([
      weather.settings,
      weather.perDevice,
      weather.syncNote,
      weather.reduceTransparency,
      weather.reduceMotion,
    ]),
    builder: (context, _) {
      final s = weather.settings.value;
      void set(WeatherSettings n) => weather.set(n);
      final dim = Theme.of(context).colorScheme.onSurfaceVariant;
      final motion = weather.reduceMotion.value || MediaQuery.disableAnimationsOf(context);
      final glassOff = weather.reduceTransparency.value;
      return ListView(
        controller: widget.controller,
        padding: const EdgeInsets.fromLTRB(0, 0, 0, Look.groupGap),
        children: [
          WeatherToggle(
            title: const Text('날씨 켜기'),
            secondary: const _OpenUmbrella(size: 22),
            value: s.enabled,
            onChanged: (v) => set(s.copyWith(enabled: v)),
          ),
          _Pad(WeatherChoices(
            options: const [(RainStyle.liquid, '리퀴드 비'), (RainStyle.showers, '작업 날씨')],
            selected: {s.style},
            onPressed: (style) => set(s.copyWith(style: style)),
          )),
          Padding(
            padding: const EdgeInsets.fromLTRB(Look.pagePad, 12, Look.pagePad, 0),
            child: _WeatherPreview(settings: s, phase: _phase),
          ),
          if (s.style == RainStyle.showers) ...[
            _Title('상태'),
            _Pad(WeatherPick(
              value: _phase,
              items: [
                for (final phase in IndicatorPhase.values) DropdownMenuItem(value: phase, child: Text(phase.label)),
              ],
              onChanged: (phase) { if (phase != null) setState(() => _phase = phase); },
            )),
            _IndicatorControls(rule: s.indicator(_phase), onChanged: (rule) {
              set(s.copyWith(indicators: {...s.indicators, _phase: rule}));
            }),
            WeatherToggle(
              title: const Text('표면에 물방울 맺힘'), value: s.beads,
              onChanged: (v) => set(s.copyWith(beads: v)),
            ),
          ],
          _Title(s.style == RainStyle.showers ? '효과 강도' : '비 양'),
          _Pad(WeatherChoices(
            options: [
              for (final a in RainAmount.values)
                (a, s.style == RainStyle.liquid ? a.label : const ['없음', '약하게', '보통', '강하게'][a.index]),
            ],
            selected: {s.amount},
            onPressed: (amount) => set(s.copyWith(amount: amount)),
          )),
          _Title('바람'),
          _Slider(
            label: '방향',
            value: s.windDir,
            min: -1,
            max: 1,
            divisions: 8,
            text: s.windDir == 0
                ? '곧게'
                : (s.windDir < 0 ? '왼쪽 ${(-s.windDir * 100).round()}%' : '오른쪽 ${(s.windDir * 100).round()}%'),
            onChanged: (v, commit) => weather.set(s.copyWith(windDir: v), commit: commit),
          ),
          _Slider(
            label: '세기',
            value: s.windStrength,
            min: 0,
            max: 1,
            divisions: 4,
            text: '${(s.windStrength * 100).round()}%',
            onChanged: (v, commit) => weather.set(s.copyWith(windStrength: v), commit: commit),
          ),
          if (s.style == RainStyle.liquid) ...[
            _Title('어디에'),
            for (final t in WeatherTarget.values)
              ListTile(
                title: Text(t.label),
                subtitle: t == WeatherTarget.focusedOnly
                    ? const Text('방금 만지거나 스크롤한 카드')
                    : t == WeatherTarget.pickedOnly
                    ? const Text('카드를 길게 눌러 「이 카드도 비」로 고른 것')
                    : null,
                trailing: t == s.target ? const Icon(Icons.check) : null,
                onTap: () => set(s.copyWith(target: t)),
              ),
            _Title('효과'),
            for (final e in Effect.values)
              WeatherToggle(title: Text(e.label), value: s.has(e), onChanged: (v) => set(s.withEffect(e, v))),
            _Title('젖는 시간'),
            _Slider(
              label: '',
              value: _rewet.indexOf(_nearest(s.rewetSecs)).toDouble(),
              min: 0,
              max: (_rewet.length - 1).toDouble(),
              divisions: _rewet.length - 1,
              text: rewetLabel(s.rewetSecs),
              onChanged: (v, commit) => weather.set(s.copyWith(rewetSecs: _rewet[v.round()]), commit: commit),
            ),
            _Title('학생 상태 날씨'),
            WeatherToggle(
              title: const Text('상태마다 비 양'),
              subtitle: const Text('비 양만 바꾸고 「어디에」는 그대로 따른다'),
              value: s.byStatus,
              onChanged: (v) => set(s.copyWith(byStatus: v)),
            ),
            if (s.byStatus)
              for (final m in Mood.values) ...[
                _Pad(
                  Text(
                    m.label,
                    style: TextStyle(color: dim, fontSize: Look.sub),
                  ),
                ),
                _Pad(
                  WeatherChoices(
                    options: [for (final a in RainAmount.values) (a, a.label)],
                    selected: {s.moodAmount(m)},
                    onPressed: (amount) => set(s.withMood(m, amount)),
                  ),
                ),
              ],
          ],
          _Title('접근성'),
          WeatherToggle(
            title: const Text('OS 설정 무시'),
            subtitle: Text(
              '「동작 줄이기」에서는 날씨가 멈추고, 「투명도 줄이기」에서는 날씨를 숨겨요.'
              ' 지금 이 폰: 동작 줄이기 ${motion ? '켬' : '끔'} · 투명도 줄이기 ${glassOff ? '켬' : '끔'}',
            ),
            value: s.ignoreOs,
            onChanged: (v) => set(s.copyWith(ignoreOs: v)),
          ),
          _Title('동기화'),
          WeatherToggle(
            title: const Text('날씨는 기기마다'),
            subtitle: const Text('켜면 이 폰만 따로 고른다(배터리). 끄면 계정으로 다른 기기와 같이 맞춘다'),
            value: weather.perDevice.value,
            onChanged: weather.setPerDevice,
          ),
          if (weather.syncNote.value != null && !weather.perDevice.value) _Note(weather.syncNote.value!, dim),
        ],
      );
    },
  ),
  );

  static int _nearest(int secs) => _rewet.reduce((a, b) => (a - secs).abs() <= (b - secs).abs() ? a : b);
}

class _Title extends StatelessWidget {
  const _Title(this.text);

  final String text;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(Look.pagePad, 15, Look.pagePad, 6),
    child: Text(
      text,
      style: TextStyle(
        fontSize: Look.sub,
        fontWeight: FontWeight.w600,
        color: Theme.of(context).colorScheme.onSurfaceVariant,
      ),
    ),
  );
}

class _Pad extends StatelessWidget {
  const _Pad(this.child);

  final Widget child;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(Look.pagePad, 0, Look.pagePad, 8),
    child: SizedBox(width: double.infinity, child: child),
  );
}

class _Note extends StatelessWidget {
  const _Note(this.text, this.color);

  final String text;
  final Color color;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(Look.pagePad, 0, Look.pagePad, 4),
    child: Text(
      text,
      style: TextStyle(fontSize: Look.sub, color: color),
    ),
  );
}

class _Slider extends StatelessWidget {
  const _Slider({
    required this.label,
    required this.value,
    required this.min,
    required this.max,
    required this.divisions,
    required this.text,
    required this.onChanged,
  });

  final String label, text;
  final double value, min, max;
  final int divisions;
  final void Function(double v, bool commit) onChanged;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.symmetric(horizontal: Look.pagePad),
    child: Row(
      children: [
        if (label.isNotEmpty) SizedBox(width: 40, child: Text(label)),
        Expanded(
          child: Slider(
            value: value.clamp(min, max),
            min: min,
            max: max,
            divisions: divisions,
            onChanged: (v) => onChanged(v, false),
            onChangeEnd: (v) => onChanged(v, true),
          ),
        ),
        SizedBox(width: 72, child: Text(text, textAlign: TextAlign.end)),
      ],
    ),
  );
}

IconData _weatherIcon(WeatherCondition? condition) => switch (condition?.material) {
  null => Icons.horizontal_rule,
  0 => Icons.umbrella_outlined,
  1 when condition!.variant >= 5 && condition.variant <= 7 => Icons.nights_stay_outlined,
  1 when condition!.variant >= 2 && condition.variant <= 4 => Icons.cloud_outlined,
  1 || 6 => Icons.wb_sunny_outlined,
  4 => Icons.ac_unit,
  5 => Icons.flash_on_outlined,
  _ => Icons.air,
};

Widget _conditionSymbol(WeatherCondition? condition, double size, Color color) =>
    condition?.material == 0 ? _OpenUmbrella(size: size, color: color) : Icon(_weatherIcon(condition), size: size, color: color);

class _OpenUmbrella extends StatelessWidget {
  const _OpenUmbrella({required this.size, this.color, this.filled = false});
  final double size;
  final Color? color;
  final bool filled;

  @override
  Widget build(BuildContext context) => CustomPaint(
    size: Size.square(size),
    painter: _UmbrellaOutline(color ?? Theme.of(context).colorScheme.onSurfaceVariant, filled),
  );
}

class _UmbrellaOutline extends CustomPainter {
  const _UmbrellaOutline(this.color, this.filled);
  final Color color;
  final bool filled;

  @override
  void paint(Canvas canvas, Size size) {
    canvas.scale(size.width / 24, size.height / 24);
    final paint = Paint()
      ..color = color
      ..strokeWidth = 1.8
      ..style = PaintingStyle.stroke
      ..strokeCap = StrokeCap.round
      ..strokeJoin = StrokeJoin.round;
    final canopy = Path()
      ..moveTo(2.2, 12.5)
      ..quadraticBezierTo(3.8, 4, 12, 4)
      ..quadraticBezierTo(20.2, 4, 21.8, 12.5)
      ..close();
    if (filled) canvas.drawPath(canopy, Paint()..color = color.withValues(alpha: 0.32));
    canvas.drawPath(canopy, paint);
    canvas.drawLine(const Offset(12, 2), const Offset(12, 4), paint);
    canvas.drawPath(Path()
      ..moveTo(12, 12.5)
      ..lineTo(12, 19.4)
      ..quadraticBezierTo(12, 22, 14, 22)
      ..quadraticBezierTo(16, 22, 16, 19.4), paint);
  }

  @override
  bool shouldRepaint(_UmbrellaOutline oldDelegate) => oldDelegate.color != color || oldDelegate.filled != filled;
}

class _RainRings extends CustomPainter {
  const _RainRings(this.line);
  final Color line;

  @override
  void paint(Canvas canvas, Size size) {
    final origin = Offset(size.width / 2, size.height / 2 + 8);
    final paint = Paint()..color = line..style = PaintingStyle.stroke;
    final unit = size.height / 4.8;
    for (final k in [1.1, 1.65, 2.2]) {
      canvas.drawCircle(origin, unit * k, paint);
    }
  }

  @override
  bool shouldRepaint(_RainRings old) => old.line != line;
}

class _WeatherPreview extends StatelessWidget {
  const _WeatherPreview({required this.settings, required this.phase});

  final WeatherSettings settings;
  final IndicatorPhase phase;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final work = settings.style == RainStyle.showers;
    final palette = WeatherPalette.of(context);
    final sky = palette.sky;
    final condition = work ? settings.indicator(phase).weather : WeatherCondition.rain;
    final symbol = condition?.material == 0
        ? _OpenUmbrella(size: 86, color: palette.umbrella, filled: true)
        : _conditionSymbol(condition, 80, palette.umbrella);
    final label = work ? phase.label : '작업실';
    final sample = ColoredBox(
      color: sky,
      child: Stack(
        fit: StackFit.expand,
        children: [
          Positioned.fill(child: CustomPaint(painter: _RainRings(palette.line))),
          Center(child: Padding(padding: const EdgeInsets.only(top: 14), child: symbol)),
          Positioned(top: 10, left: 12, right: 12, child: Row(children: [
            Icon(Icons.terminal_outlined, size: 15, color: scheme.onSurfaceVariant),
            const SizedBox(width: 7),
            Flexible(child: Text(label, maxLines: 1, overflow: TextOverflow.ellipsis, style: theme.textTheme.labelMedium)),
          ])),
        ],
      ),
    );
    final pane = work
        ? WorkWeather(id: 'settings-weather-sample', phase: phase, doneUntilMs: 0,
            radius: Look.smallCorners, builder: (_) => sample)
        : settings.target == WeatherTarget.backgroundOnly
            ? Stack(fit: StackFit.expand, children: [
                Positioned.fill(child: CustomPaint(painter: _RainRings(palette.line))),
                Center(child: symbol),
                Positioned(top: 10, left: 12, child: Text(label, style: theme.textTheme.labelMedium)),
              ])
            : WeatherCard(id: 'settings-liquid-sample', mood: Mood.busy, preview: true, child: sample);
    return DecoratedBox(
      decoration: BoxDecoration(
        color: palette.panel,
        borderRadius: Look.corners,
        border: Border.all(color: scheme.outlineVariant),
      ),
      child: Padding(
        padding: const EdgeInsets.all(9),
        child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(3, 2, 3, 8),
            child: Row(children: [
              _OpenUmbrella(size: 16, color: scheme.primary),
              const SizedBox(width: 7),
              Text('미리보기', style: theme.textTheme.labelLarge),
            ]),
          ),
          ClipRRect(
            borderRadius: Look.smallCorners,
            child: ColoredBox(
              color: sky,
              child: SizedBox(height: 148, child: IgnorePointer(child: WeatherScene(child: pane))),
            ),
          ),
        ]),
      ),
    );
  }
}

class _IndicatorControls extends StatelessWidget {
  const _IndicatorControls({required this.rule, required this.onChanged});
  final StateIndicator rule;
  final ValueChanged<StateIndicator> onChanged;

  @override
  Widget build(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      const _Title('날씨 효과'),
      _Pad(WeatherPick(
        value: rule.weather?.name ?? '',
        items: [for (final c in [null, ...WeatherCondition.values])
          DropdownMenuItem(value: c?.name ?? '', child: Row(children: [
            _conditionSymbol(c, 19, Theme.of(context).colorScheme.onSurfaceVariant),
            const SizedBox(width: 9),
            Flexible(child: Text(c?.label ?? '날씨 없음', overflow: TextOverflow.ellipsis)),
          ])),
        ],
        onChanged: (id) {
          if (id == null) return;
          onChanged(StateIndicator(
            weather: WeatherCondition.values.where((c) => c.name == id).firstOrNull, edges: rule.edges,
          ));
        },
      )),
      const _Title('프로세스바 위치'),
      _Pad(WeatherEdgePad(
        edges: rule.edges,
        onPressed: (side) => onChanged(StateIndicator(weather: rule.weather, edges: rule.edges ^ side)),
      )),
    ],
  );
}
