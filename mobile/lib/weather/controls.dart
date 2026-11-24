import 'package:flutter/material.dart';

import '../contrast.dart';
import '../look.dart';

Color weatherControlFill(BuildContext context) {
  final scheme = Theme.of(context).colorScheme;
  return Color.alphaBlend(scheme.onSurface.withValues(alpha: 0.075), scheme.surface);
}

class WeatherToggle extends StatelessWidget {
  const WeatherToggle({
    super.key,
    required this.title,
    required this.value,
    required this.onChanged,
    this.subtitle,
  });

  final Widget title;
  final Widget? subtitle;
  final bool value;
  final ValueChanged<bool> onChanged;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final idle = Color.alphaBlend(colors.onSurface.withValues(alpha: 0.18), colors.surface);
    return SwitchListTile(
      title: title,
      subtitle: subtitle,
      value: value,
      onChanged: onChanged,
      minTileHeight: 48,
      contentPadding: const EdgeInsets.symmetric(horizontal: Look.pagePad),
      activeTrackColor: colors.primary,
      inactiveTrackColor: idle,
      activeThumbColor: Colors.white,
      inactiveThumbColor: Colors.white,
      trackOutlineColor: const WidgetStatePropertyAll(Colors.transparent),
    );
  }
}

class WeatherChoices<T> extends StatelessWidget {
  const WeatherChoices({
    super.key,
    required this.options,
    required this.selected,
    required this.onPressed,
    this.symbol,
  });

  final List<(T, String)> options;
  final Set<T> selected;
  final ValueChanged<T> onPressed;
  final Widget Function(T, Color)? symbol;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = theme.colorScheme;
    final idle = weatherControlFill(context);
    final selectedFill = Color.alphaBlend(colors.primary.withValues(alpha: 0.19), colors.surface);
    return Row(
      children: [
        for (final (index, (value, label)) in options.indexed) ...[
          if (index > 0) const SizedBox(width: 6),
          Expanded(
            child: _button(context, value, label, idle, selectedFill),
          ),
        ],
      ],
    );
  }

  Widget _button(BuildContext context, T value, String label, Color idle, Color selectedFill) {
    final theme = Theme.of(context);
    final colors = theme.colorScheme;
    final on = selected.contains(value);
    final fill = on ? selectedFill : idle;
    final ink = enforceContrast(on ? colors.primary : colors.onSurfaceVariant, fill, 4.5);
    return Semantics(
      selected: on,
      button: true,
      child: Material(
        color: fill,
        shape: StadiumBorder(
          side: BorderSide(color: on ? colors.primary.withValues(alpha: 0.48) : Colors.transparent),
        ),
        clipBehavior: Clip.antiAlias,
        child: InkWell(
          onTap: () => onPressed(value),
          child: ConstrainedBox(
            constraints: BoxConstraints(minHeight: symbol == null ? Look.tap : 52),
            child: Column(
              mainAxisAlignment: MainAxisAlignment.center,
              children: [
                if (symbol != null) ...[
                  symbol!(value, ink),
                  const SizedBox(height: 3),
                ],
                Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 2),
                  child: Text(
                    label,
                    style: theme.textTheme.labelLarge?.copyWith(color: ink),
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

class WeatherEdgeGlyph extends StatelessWidget {
  const WeatherEdgeGlyph(this.edge, this.ink, {super.key});
  final int edge;
  final Color ink;

  @override
  Widget build(BuildContext context) => ExcludeSemantics(
    child: CustomPaint(size: const Size(20, 14), painter: _WeatherEdgePainter(edge, ink)),
  );
}

class _WeatherEdgePainter extends CustomPainter {
  const _WeatherEdgePainter(this.edge, this.ink);
  final int edge;
  final Color ink;

  @override
  void paint(Canvas canvas, Size size) {
    final box = (Offset.zero & size).deflate(1);
    final paint = Paint()
      ..color = ink.withValues(alpha: 0.42)
      ..style = PaintingStyle.stroke
      ..strokeWidth = 1
      ..strokeCap = StrokeCap.round;
    canvas.drawRRect(RRect.fromRectAndRadius(box, const Radius.circular(3)), paint);
    paint
      ..strokeWidth = 2
      ..color = ink;
    final (a, b) = switch (edge) {
      1 => (box.topLeft + const Offset(4, 0), box.topRight - const Offset(4, 0)),
      4 => (box.bottomLeft + const Offset(4, 0), box.bottomRight - const Offset(4, 0)),
      8 => (box.topLeft + const Offset(0, 3), box.bottomLeft - const Offset(0, 3)),
      _ => (box.topRight + const Offset(0, 3), box.bottomRight - const Offset(0, 3)),
    };
    canvas.drawLine(a, b, paint);
  }

  @override
  bool shouldRepaint(_WeatherEdgePainter old) => edge != old.edge || ink != old.ink;
}

class WeatherPick<T> extends StatelessWidget {
  const WeatherPick({
    super.key,
    required this.value,
    required this.items,
    required this.onChanged,
  });

  final T value;
  final List<DropdownMenuItem<T>> items;
  final ValueChanged<T?> onChanged;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final fill = weatherControlFill(context);
    return DecoratedBox(
      decoration: ShapeDecoration(color: fill, shape: const StadiumBorder()),
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 12),
        child: DropdownButtonHideUnderline(
          child: DropdownButton<T>(
            value: value,
            isExpanded: true,
            itemHeight: 48,
            menuMaxHeight: 360,
            dropdownColor: colors.surfaceContainerHigh,
            borderRadius: Look.corners,
            icon: const Icon(Icons.expand_more_rounded, size: 21),
            items: items,
            onChanged: onChanged,
          ),
        ),
      ),
    );
  }
}
