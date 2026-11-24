import 'package:flutter/material.dart';
import 'package:flutter/semantics.dart' show CustomSemanticsAction;
import 'package:flutter/services.dart';

class AccentPicker extends StatefulWidget {
  const AccentPicker({
    super.key,
    required this.original,
    required this.color,
    required this.onChanged,
    required this.onApply,
    required this.onCancel,
    required this.busy,
    required this.compact,
  });

  final Color original;
  final HSVColor color;
  final ValueChanged<HSVColor> onChanged;
  final VoidCallback onApply;
  final VoidCallback onCancel;
  final bool busy;
  final bool compact;

  static String hex(Color color) =>
      '#${(color.toARGB32() & 0xffffff).toRadixString(16).padLeft(6, '0')}';

  @override
  State<AccentPicker> createState() => _AccentPickerState();
}

class _AccentPickerState extends State<AccentPicker> {
  late final _hex = TextEditingController(
    text: AccentPicker.hex(widget.color.toColor()),
  );
  bool _showHex = false;
  bool _valid = true;

  @override
  void didUpdateWidget(covariant AccentPicker oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (widget.color == oldWidget.color) return;
    final color = AccentPicker.hex(widget.color.toColor());
    final typed = _hex.text.trim().toLowerCase().replaceFirst(
      RegExp(r'^#'),
      '',
    );
    if (typed != color.substring(1)) _hex.text = color;
    _valid = true;
  }

  void _change(HSVColor color) {
    if (widget.busy) return;
    if (!_valid) {
      setState(() => _valid = true);
      _hex.text = AccentPicker.hex(color.toColor());
    }
    widget.onChanged(color);
  }

  void _typeHex(String text) {
    final rgb = text.trim().replaceFirst(RegExp(r'^#'), '');
    final parsed = RegExp(r'^[0-9a-fA-F]{6}$').hasMatch(rgb)
        ? int.parse(rgb, radix: 16)
        : null;
    setState(() => _valid = parsed != null);
    if (parsed != null) {
      widget.onChanged(HSVColor.fromColor(Color(0xff000000 | parsed)));
    }
  }

  @override
  void dispose() {
    _hex.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final selected = widget.color.toColor();
    return Focus(
      onKeyEvent: (_, event) {
        if (!widget.busy &&
            event is KeyDownEvent &&
            event.logicalKey == LogicalKeyboardKey.escape) {
          widget.onCancel();
          return KeyEventResult.handled;
        }
        return KeyEventResult.ignored;
      },
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Padding(
            padding: const EdgeInsets.only(bottom: 6),
            child: Text(
              '밝기와 채도',
              style: Theme.of(
                context,
              ).textTheme.labelSmall?.copyWith(color: scheme.onSurfaceVariant),
            ),
          ),
          _ColorPlane(
            color: widget.color,
            height: widget.compact ? 88 : 120,
            onChanged: widget.busy ? null : _change,
          ),
          const SizedBox(height: 8),
          Semantics(
            label: '색 계열',
            child: Stack(
              alignment: Alignment.center,
              children: [
                Container(
                  margin: const EdgeInsets.symmetric(horizontal: 12),
                  height: 14,
                  decoration: BoxDecoration(
                    borderRadius: const BorderRadius.all(Radius.circular(7)),
                    gradient: const LinearGradient(
                      colors: [
                        Color(0xffff0000),
                        Color(0xffffff00),
                        Color(0xff00ff00),
                        Color(0xff00ffff),
                        Color(0xff0000ff),
                        Color(0xffff00ff),
                        Color(0xffff0000),
                      ],
                    ),
                  ),
                ),
                SliderTheme(
                  data: SliderTheme.of(context).copyWith(
                    trackHeight: 0,
                    activeTrackColor: Colors.transparent,
                    inactiveTrackColor: Colors.transparent,
                    thumbColor: scheme.onSurface,
                    thumbShape: const RoundSliderThumbShape(
                      enabledThumbRadius: 8,
                      elevation: 2,
                    ),
                    overlayShape: const RoundSliderOverlayShape(
                      overlayRadius: 13,
                    ),
                  ),
                  child: Slider(
                    key: const Key('appearance-accent-hue'),
                    value: widget.color.hue,
                    max: 359.99,
                    onChanged: widget.busy
                        ? null
                        : (hue) => _change(widget.color.withHue(hue)),
                  ),
                ),
              ],
            ),
          ),

          Wrap(
            spacing: 14,
            runSpacing: 4,
            crossAxisAlignment: WrapCrossAlignment.center,
            children: [
              _Sample(label: '현재', color: widget.original),
              _Sample(label: '미리보기', color: selected),
              TextButton(
                onPressed: widget.busy
                    ? null
                    : () {
                        setState(() {
                          _showHex = !_showHex;
                          if (!_showHex) {
                            _hex.text = AccentPicker.hex(selected);
                            _valid = true;
                          }
                        });
                      },
                child: Text(_showHex ? 'HEX 접기' : 'HEX'),
              ),
            ],
          ),
          if (_showHex)
            TextField(
              key: const Key('appearance-accent-hex'),
              controller: _hex,
              enabled: !widget.busy,
              decoration: InputDecoration(
                labelText: 'HEX 색상 값',
                isDense: true,
                errorText: _valid ? null : '6자리 색상 코드를 입력하세요',
              ),
              autocorrect: false,
              onChanged: _typeHex,
            ),
          const SizedBox(height: 6),
          Wrap(
            alignment: WrapAlignment.end,
            spacing: 8,
            children: [
              TextButton(
                onPressed: widget.busy ? null : widget.onCancel,
                child: const Text('취소'),
              ),
              FilledButton(
                onPressed: widget.busy || !_valid ? null : widget.onApply,
                child: Text(widget.busy ? '적용 중' : '적용'),
              ),
            ],
          ),
        ],
      ),
    );
  }
}

class _Sample extends StatelessWidget {
  const _Sample({required this.label, required this.color});

  final String label;
  final Color color;

  @override
  Widget build(BuildContext context) => Row(
    mainAxisSize: MainAxisSize.min,
    children: [
      Container(
        width: 20,
        height: 20,
        decoration: BoxDecoration(
          color: color,
          shape: BoxShape.circle,
          border: Border.all(color: Theme.of(context).colorScheme.outline),
        ),
      ),
      const SizedBox(width: 6),
      Text(label),
    ],
  );
}

class _ColorPlane extends StatelessWidget {
  const _ColorPlane({
    required this.color,
    required this.height,
    required this.onChanged,
  });

  final HSVColor color;
  final double height;
  final ValueChanged<HSVColor>? onChanged;

  void _nudge(double saturation, double value) {
    onChanged?.call(
      color
          .withSaturation((color.saturation + saturation).clamp(0, 1))
          .withValue((color.value + value).clamp(0, 1)),
    );
  }

  @override
  Widget build(BuildContext context) => Semantics(
    label: '밝기와 채도. 화살표 키로 조절',
    onIncrease: onChanged == null ? null : () => _nudge(0.04, 0),
    onDecrease: onChanged == null ? null : () => _nudge(-0.04, 0),
    value:
        '밝기 ${(color.value * 100).round()}%, 채도 ${(color.saturation * 100).round()}%',
    increasedValue:
        '채도 ${((color.saturation + .04).clamp(0.0, 1.0) * 100).round()}%',
    decreasedValue:
        '채도 ${((color.saturation - .04).clamp(0.0, 1.0) * 100).round()}%',
    customSemanticsActions: onChanged == null
        ? const {}
        : {
            const CustomSemanticsAction(label: '밝게'): () => _nudge(0, .04),
            const CustomSemanticsAction(label: '어둡게'): () => _nudge(0, -.04),
          },
    child: Focus(
      canRequestFocus: onChanged != null,
      onKeyEvent: (_, event) {
        if (event is! KeyDownEvent && event is! KeyRepeatEvent) {
          return KeyEventResult.ignored;
        }
        final key = event.logicalKey;
        if (key == LogicalKeyboardKey.arrowLeft) {
          _nudge(-0.03, 0);
        } else if (key == LogicalKeyboardKey.arrowRight) {
          _nudge(0.03, 0);
        } else if (key == LogicalKeyboardKey.arrowUp) {
          _nudge(0, 0.03);
        } else if (key == LogicalKeyboardKey.arrowDown) {
          _nudge(0, -0.03);
        } else {
          return KeyEventResult.ignored;
        }
        return KeyEventResult.handled;
      },
      child: SizedBox(
        height: height,
        child: LayoutBuilder(
          builder: (context, constraints) {
            final width = constraints.maxWidth;
            void pick(Offset position) {
              onChanged?.call(
                color
                    .withSaturation((position.dx / width).clamp(0, 1))
                    .withValue((1 - position.dy / height).clamp(0, 1)),
              );
            }

            final hue = HSVColor.fromAHSV(1, color.hue, 1, 1).toColor();
            return GestureDetector(
              key: const Key('appearance-accent-plane'),
              behavior: HitTestBehavior.opaque,
              onTapUp: onChanged == null
                  ? null
                  : (event) => pick(event.localPosition),
              onPanDown: onChanged == null
                  ? null
                  : (event) => pick(event.localPosition),
              onPanUpdate: onChanged == null
                  ? null
                  : (event) => pick(event.localPosition),
              child: ClipRRect(
                borderRadius: BorderRadius.circular(18),
                child: Stack(
                  fit: StackFit.expand,
                  children: [
                    DecoratedBox(
                      decoration: BoxDecoration(
                        gradient: LinearGradient(colors: [Colors.white, hue]),
                      ),
                    ),
                    const DecoratedBox(
                      decoration: BoxDecoration(
                        gradient: LinearGradient(
                          begin: Alignment.topCenter,
                          end: Alignment.bottomCenter,
                          colors: [Colors.transparent, Colors.black],
                        ),
                      ),
                    ),
                    Positioned(
                      left: (color.saturation * width).clamp(8, width - 8) - 8,
                      top:
                          ((1 - color.value) * height).clamp(8, height - 8) - 8,
                      child: Container(
                        width: 16,
                        height: 16,
                        decoration: BoxDecoration(
                          color: color.toColor(),
                          shape: BoxShape.circle,
                          border: Border.all(color: Colors.white, width: 2),
                          boxShadow: const [
                            BoxShadow(color: Colors.black87, blurRadius: 3),
                          ],
                        ),
                      ),
                    ),
                  ],
                ),
              ),
            );
          },
        ),
      ),
    ),
  );
}
