import 'package:flutter/material.dart';

import '../contrast.dart';
import '../look.dart';

class WeatherPalette {
  const WeatherPalette(
    this.canvas,
    this.panel,
    this.sky,
    this.ink,
    this.muted,
    this.accent,
    this.umbrella,
    this.line,
  );

  static const night = WeatherPalette(
    Color(0xFF0E2029),
    Color(0xFF172E37),
    Color(0xFF102831),
    Color(0xFFF1F5EF),
    Color(0xFFAFCBCC),
    Color(0xFF79E0CE),
    Color(0xFFF6C677),
    Color(0xFF32535B),
  );
  static const day = WeatherPalette(
    Color(0xFFEAF4F2),
    Color(0xFFF8FCF7),
    Color(0xFFDCEFEB),
    Color(0xFF16353B),
    Color(0xFF476367),
    Color(0xFF00695D),
    Color(0xFF865000),
    Color(0xFFB2CEC8),
  );

  static WeatherPalette of(BuildContext context) =>
      Theme.of(context).brightness == Brightness.dark ? night : day;

  final Color canvas, panel, sky, ink, muted, accent, umbrella, line;

  ThemeData theme(ThemeData base) => base.copyWith(
    scaffoldBackgroundColor: canvas,
    colorScheme: base.colorScheme.copyWith(
      primary: accent,
      surface: panel,
      surfaceContainerHigh: panel,
      onSurface: ink,
      onSurfaceVariant: muted,
      outlineVariant: line,
    ),
    textTheme: base.textTheme.apply(bodyColor: ink, displayColor: ink),
  );
}

Color weatherControlFill(BuildContext context) {
  final scheme = Theme.of(context).colorScheme;
  return Color.alphaBlend(
    scheme.onSurface.withValues(alpha: 0.075),
    scheme.surface,
  );
}

class WeatherToggle extends StatelessWidget {
  const WeatherToggle({
    super.key,
    required this.title,
    required this.value,
    required this.onChanged,
    this.subtitle,
    this.secondary,
  });

  final Widget title;
  final Widget? subtitle, secondary;
  final bool value;
  final ValueChanged<bool> onChanged;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final idle = Color.alphaBlend(
      colors.onSurface.withValues(alpha: 0.18),
      colors.surface,
    );
    return SwitchListTile.adaptive(
      title: title,
      subtitle: subtitle,
      secondary: secondary,
      value: value,
      onChanged: onChanged,
      minTileHeight: 48,
      activeTrackColor: Theme.of(context).brightness == Brightness.dark
          ? Color.alphaBlend(
              colors.primary.withValues(alpha: 0.60),
              WeatherPalette.of(context).canvas,
            )
          : colors.primary,
      inactiveTrackColor: idle,
      activeThumbColor: Colors.white,
      inactiveThumbColor: Colors.white,
    );
  }
}

class WeatherChoices<T> extends StatelessWidget {
  const WeatherChoices({
    super.key,
    required this.options,
    required this.selected,
    required this.onPressed,
  });

  final List<(T, String)> options;
  final Set<T> selected;
  final ValueChanged<T> onPressed;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = theme.colorScheme;
    final idle = weatherControlFill(context);
    final selectedFill = Color.alphaBlend(
      colors.primary.withValues(alpha: 0.19),
      colors.surface,
    );
    return Row(
      children: [
        for (final (index, (value, label)) in options.indexed) ...[
          if (index > 0) const SizedBox(width: 6),
          Expanded(child: _button(context, value, label, idle, selectedFill)),
        ],
      ],
    );
  }

  Widget _button(
    BuildContext context,
    T value,
    String label,
    Color idle,
    Color selectedFill,
  ) {
    final theme = Theme.of(context);
    final colors = theme.colorScheme;
    final on = selected.contains(value);
    final fill = on ? selectedFill : idle;
    final ink = enforceContrast(
      on ? colors.primary : colors.onSurfaceVariant,
      fill,
      4.5,
    );
    return Semantics(
      selected: on,
      button: true,
      child: Material(
        color: fill,
        shape: RoundedRectangleBorder(
          borderRadius: Look.corners,
          side: BorderSide(
            color: on
                ? colors.primary.withValues(alpha: 0.48)
                : Colors.transparent,
          ),
        ),
        clipBehavior: Clip.antiAlias,
        child: InkWell(
          onTap: () => onPressed(value),
          child: ConstrainedBox(
            constraints: const BoxConstraints(minHeight: Look.tap),
            child: Center(
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: 2),
                child: Text(
                  label,
                  style: theme.textTheme.labelLarge?.copyWith(color: ink),
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

class WeatherEdgePad extends StatelessWidget {
  const WeatherEdgePad({
    super.key,
    required this.edges,
    required this.onPressed,
  });

  final int edges;
  final ValueChanged<int> onPressed;

  @override
  Widget build(BuildContext context) {
    Widget button(int edge, String label) => WeatherChoices(
      options: [(edge, label)],
      selected: {if (edges & edge != 0) edge},
      onPressed: onPressed,
    );
    final palette = WeatherPalette.of(context);
    return LayoutBuilder(
      builder: (context, box) {
        final width = (box.maxWidth / 3).clamp(32.0, 112.0);
        return Center(
          child: SizedBox(
            width: width * 3,
            child: Column(
              children: [
                SizedBox(width: width - 8, child: button(1, '위')),
                const SizedBox(height: 5),
                Row(
                  children: [
                    Expanded(child: button(8, '왼쪽')),
                    SizedBox(
                      width: width,
                      child: ExcludeSemantics(
                        child: CustomPaint(
                          size: Size(width, 45),
                          painter: _WeatherEdgeDiagram(edges, palette),
                        ),
                      ),
                    ),
                    Expanded(child: button(2, '오른쪽')),
                  ],
                ),
                const SizedBox(height: 5),
                SizedBox(width: width - 8, child: button(4, '아래')),
              ],
            ),
          ),
        );
      },
    );
  }
}

class _WeatherEdgeDiagram extends CustomPainter {
  const _WeatherEdgeDiagram(this.edges, this.palette);
  final int edges;
  final WeatherPalette palette;

  @override
  void paint(Canvas canvas, Size size) {
    final rect = Rect.fromLTWH(11, 5, size.width - 22, size.height - 10);
    final frame = RRect.fromRectAndRadius(rect, const Radius.circular(7));
    canvas.drawRRect(frame, Paint()..color = palette.sky);
    canvas.drawRRect(
      frame,
      Paint()
        ..style = PaintingStyle.stroke
        ..color = palette.line,
    );
    final bar = Paint()
      ..color = palette.accent
      ..strokeWidth = 3
      ..strokeCap = StrokeCap.round;
    if (edges & 1 != 0) {
      canvas.drawLine(
        rect.topLeft + const Offset(8, 0),
        rect.topRight - const Offset(8, 0),
        bar,
      );
    }
    if (edges & 4 != 0) {
      canvas.drawLine(
        rect.bottomLeft + const Offset(8, 0),
        rect.bottomRight - const Offset(8, 0),
        bar,
      );
    }
    if (edges & 8 != 0) {
      canvas.drawLine(
        rect.topLeft + const Offset(0, 8),
        rect.bottomLeft - const Offset(0, 8),
        bar,
      );
    }
    if (edges & 2 != 0) {
      canvas.drawLine(
        rect.topRight + const Offset(0, 8),
        rect.bottomRight - const Offset(0, 8),
        bar,
      );
    }
  }

  @override
  bool shouldRepaint(_WeatherEdgeDiagram old) =>
      edges != old.edges || palette != old.palette;
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
      decoration: BoxDecoration(color: fill, borderRadius: Look.corners),
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
