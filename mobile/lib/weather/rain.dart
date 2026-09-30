import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';

import 'model.dart';

/// 빗줄기 한 장(`shaders/weather_rain.frag`) — 화면 바탕과 젖은 카드 안쪽이 같이 쓴다.
/// [clip] 을 주면 그 모양 안에만 내린다(카드·입력칸을 비운 바탕).
class RainPainter extends CustomPainter {
  RainPainter({
    required this.shader,
    required this.time,
    required this.amount,
    required this.wind,
    required this.dpr,
    required this.dark,
    this.ground = 0.8,
    this.clip,
    Listenable? repaint,
  }) : super(repaint: repaint ?? time);

  final ui.FragmentShader shader;
  final ValueListenable<double> time;
  final RainAmount amount;
  final double wind, dpr, ground;
  final bool dark;
  final Path Function(Size)? clip;

  static Future<ui.FragmentProgram>? _program;
  static Future<ui.FragmentProgram> program() =>
      _program ??= ui.FragmentProgram.fromAsset('shaders/weather_rain.frag');

  @override
  void paint(Canvas canvas, Size size) {
    if (amount == RainAmount.none) return;
    final (streaks, speed, length) = switch (amount) {
      RainAmount.drizzle => (0.3, 0.55, 0.55),
      RainAmount.rain => (0.65, 1.0, 1.0),
      _ => (1.0, 1.3, 1.35),
    };
    final color = dark ? const [0.78, 0.87, 1.0] : const [0.28, 0.4, 0.56];
    shader
      ..setFloat(0, size.width)
      ..setFloat(1, size.height)
      ..setFloat(2, time.value)
      ..setFloat(3, dpr)
      ..setFloat(4, streaks)
      ..setFloat(5, 0.12 + wind * 0.35)
      ..setFloat(6, ground)
      ..setFloat(7, speed)
      ..setFloat(8, length)
      ..setFloat(9, color[0])
      ..setFloat(10, color[1])
      ..setFloat(11, color[2]);
    final c = clip;
    if (c != null) {
      canvas.save();
      canvas.clipPath(c(size));
    }
    canvas.drawRect(Offset.zero & size, Paint()..shader = shader);
    if (c != null) canvas.restore();
  }

  @override
  bool shouldRepaint(RainPainter old) => true;
}
