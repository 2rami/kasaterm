import 'package:flutter/material.dart';

import '../look.dart';
import '../screens/controls.dart';
import 'model.dart';
import 'store.dart';

/// 설정 「날씨」 — docs/weather.md 의 표 전부와 「날씨는 기기마다」.
Future<void> showWeatherSheet(BuildContext context) => showModalBottomSheet<void>(
  context: context,
  showDragHandle: true,
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
      if (!weather.settings.value.enabled || id == null) return const SizedBox.shrink();
      return ListTile(
        leading: const Icon(Icons.umbrella_outlined),
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

  static const _rewet = [30, 60, 120, 180, 300, 600];

  @override
  Widget build(BuildContext context) => DraggableScrollableSheet(
    expand: false,
    initialChildSize: 0.85,
    maxChildSize: 0.95,
    builder: (context, scroll) => ListenableBuilder(
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
          controller: scroll,
          padding: const EdgeInsets.fromLTRB(0, 0, 0, Look.groupGap),
          children: [
            SwitchListTile(
              title: const Text('날씨'),
              subtitle: const Text('카드·단추·바탕에 비를 내린다. 끄면 그리는 비용이 없다'),
              value: s.enabled,
              onChanged: (v) => set(s.copyWith(enabled: v)),
            ),
            _Title('비 양'),
            _Pad(
              SegmentedButton<RainAmount>(
                showSelectedIcon: false,
                segments: [for (final a in RainAmount.values) ButtonSegment(value: a, label: Text(a.label))],
                selected: {s.amount},
                onSelectionChanged: (v) => set(s.copyWith(amount: v.first)),
              ),
            ),
            _Title('바람'),
            _Slider(
              label: '방향',
              value: s.windDir,
              min: -1,
              max: 1,
              divisions: 8,
              text: s.windDir == 0 ? '곧게' : (s.windDir < 0 ? '왼쪽 ${(-s.windDir * 100).round()}%' : '오른쪽 ${(s.windDir * 100).round()}%'),
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
              SwitchListTile(
                title: Text(e.label),
                value: s.has(e),
                onChanged: (v) => set(s.withEffect(e, v)),
              ),
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
            SwitchListTile(
              title: const Text('상태마다 비 양'),
              subtitle: const Text('비 양만 바꾸고 「어디에」는 그대로 따른다'),
              value: s.byStatus,
              onChanged: (v) => set(s.copyWith(byStatus: v)),
            ),
            if (s.byStatus)
              for (final m in Mood.values) ...[
                _Pad(Text(m.label, style: TextStyle(color: dim, fontSize: Look.sub))),
                _Pad(
                  SegmentedButton<RainAmount>(
                    showSelectedIcon: false,
                    segments: [for (final a in RainAmount.values) ButtonSegment(value: a, label: Text(a.label))],
                    selected: {s.moodAmount(m)},
                    onSelectionChanged: (v) => set(s.withMood(m, v.first)),
                  ),
                ),
              ],
            _Title('접근성'),
            SwitchListTile(
              title: const Text('OS 설정 무시'),
              subtitle: Text(
                '끄면 「동작 줄이기」에선 맺힌 물방울만 남기고, 「투명도 줄이기」에선 날씨를 끈다.'
                ' 지금 이 폰: 동작 줄이기 ${motion ? '켬' : '끔'} · 투명도 줄이기 ${glassOff ? '켬' : '끔'}',
              ),
              value: s.ignoreOs,
              onChanged: (v) => set(s.copyWith(ignoreOs: v)),
            ),
            _Title('동기화'),
            SwitchListTile(
              title: const Text('날씨는 기기마다'),
              subtitle: const Text('켜면 이 폰만 따로 고른다(배터리). 끄면 계정으로 다른 기기와 같이 맞춘다'),
              value: weather.perDevice.value,
              onChanged: weather.setPerDevice,
            ),
            if (weather.syncNote.value != null && !weather.perDevice.value)
              _Note(weather.syncNote.value!, dim),
          ],
        );
      },
    ),
  );

  static int _nearest(int secs) =>
      _rewet.reduce((a, b) => (a - secs).abs() <= (b - secs).abs() ? a : b);
}

class _Title extends StatelessWidget {
  const _Title(this.text);

  final String text;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(Look.pagePad, Look.groupGap, Look.pagePad, Look.groupTitleGap),
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
    child: Text(text, style: TextStyle(fontSize: Look.sub, color: color)),
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
