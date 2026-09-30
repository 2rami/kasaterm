import 'dart:math' as math;
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/services.dart';

import '../status_style.dart';
import 'model.dart';
import 'rain.dart';
import 'scene.dart';
import 'sheet.dart';
import 'sim.dart';
import 'store.dart';

/// 학생 상태 → 날씨 기분. 기다림 = 내 차례, 작업 중 = 하는 중, 나머지 = 쉬는 중.
Mood weatherMood(PaneMood m) => switch (m) {
  PaneMood.waiting => Mood.yourTurn,
  PaneMood.working => Mood.busy,
  _ => Mood.resting,
};

/// 제 유리를 가진 카드(허브 방·작업 줄). 날씨가 꺼져 있으면 [child] 그대로다 — 틱·그림·셰이더 없음.
///
/// 켜져 있으면 이 카드에 오는 비 양(설정·초점·카드별 덮어쓰기·학생 상태)만큼 유리에 물이 맺혀
/// 흐르고 아래 테두리에 고인다. 만지면 이 카드가 초점이 되고, 「닦기」 설정대로 닦인다 — 단추만
/// 누르면 누른 자리만, 빈 곳을 만지거나 스크롤하면 한 번 쓸어 닦는다. 길게 누르면 「이 카드 날씨」.
class WeatherCard extends StatefulWidget {
  const WeatherCard({super.key, required this.id, this.mood, required this.child});

  final String id;

  /// 학생 상태 날씨가 켜져 있을 때 비 양을 고르는 기분. 모르면 null(전역 비 양).
  final Mood? mood;
  final Widget child;

  @override
  State<WeatherCard> createState() => _WeatherCardState();
}

class _WeatherCardState extends State<WeatherCard> with SingleTickerProviderStateMixin implements WeatherPlace {
  static ui.FragmentProgram? _program;
  static ui.Image? _sprite;
  static Future<void>? _loading;

  late final Ticker _ticker = createTicker(_tick);
  final _sim = GlassSim(0);
  final _snapKey = GlobalKey();
  final _boxKey = GlobalKey();
  final _frame = ValueNotifier<int>(0);
  final _time = ValueNotifier<double>(0);
  WeatherSceneState? _scene;
  ui.FragmentShader? _glass, _rain;
  ui.Image? _drops, _snap;
  bool _snapBusy = false, _onButton = false;
  Duration _last = Duration.zero;
  double _acc = 0, _sinceSnap = 99, _dpr = 2;
  DateTime _paintedAt = DateTime(0);
  DateTime _wipedAt = DateTime.now();
  Offset? _down, _prev;
  ({bool active, bool moving}) _v = (active: false, moving: false);
  RainAmount _amount = RainAmount.none;
  bool _dark = true;

  @override
  RainAmount get amount => _amount;

  @override
  Rect? rectIn(RenderBox scene) {
    final box = _boxKey.currentContext?.findRenderObject();
    if (box is! RenderBox || !box.attached || !box.hasSize) return null;
    return box.localToGlobal(Offset.zero, ancestor: scene) & box.size;
  }

  static Future<void> _loadShared() => _loading ??= () async {
    _program = await ui.FragmentProgram.fromAsset('shaders/weather_glass.frag');
    final data = await rootBundle.load('assets/weather/raindrop_normal.png');
    final codec = await ui.instantiateImageCodec(data.buffer.asUint8List());
    _sprite = (await codec.getNextFrame()).image;
  }();

  @override
  void initState() {
    super.initState();
    weather.changes.addListener(_sync);
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final scene = WeatherScene.maybeOf(context);
    if (!identical(scene, _scene)) {
      _scene?.unregister(this);
      _scene = scene?..register(this);
    }
    WidgetsBinding.instance.addPostFrameCallback((_) => _sync());
  }

  @override
  void dispose() {
    weather.changes.removeListener(_sync);
    _scene?.unregister(this);
    _ticker.dispose();
    _frame.dispose();
    _time.dispose();
    _glass?.dispose();
    _rain?.dispose();
    _drops?.dispose();
    _snap?.dispose();
    super.dispose();
  }

  void _sync() {
    if (!mounted) return;
    final v = weatherVerdictOf(context);
    final s = weather.settings.value;
    final amount = v.active
        ? cardAmount(s, weather.cardOf(widget.id), focused: weather.focused.value == widget.id, mood: widget.mood)
        : RainAmount.none;
    final changed = v.active != _v.active || amount != _amount || v.moving != _v.moving;
    _v = v;
    _amount = amount;
    if (v.active && _glass == null) {
      _loadShared().then((_) async {
        final rain = (await RainPainter.program()).fragmentShader();
        if (!mounted) return;
        setState(() {
          _glass = _program!.fragmentShader();
          _rain = rain;
        });
        _sync();
      });
    }
    final wet = v.active && _glass != null && (amount != RainAmount.none || !_sim.settled);
    if (wet && !_ticker.isActive) {
      _last = Duration.zero;
      _ticker.start();
    } else if (!wet && _ticker.isActive) {
      _ticker.stop();
    }
    if (changed) setState(() {});
  }

  bool get _wet => _v.active && _glass != null && (_amount != RainAmount.none || !_sim.settled);

  void _tick(Duration now) {
    _acc += _last == Duration.zero ? 1 / 30 : (now - _last).inMicroseconds / 1e6;
    _last = now;
    const dt = 1 / 30;
    // 목록 속 카드는 높이 제한 없이 놓인다 — 크기는 배치가 끝난 상자에서 읽는다.
    final box = _boxKey.currentContext?.findRenderObject();
    if (box is RenderBox && box.hasSize) _sim.size = box.size;
    if (_acc < dt || _sim.size.isEmpty) return;
    _acc = 0;
    final s = weather.settings.value;
    if (_v.moving || _amount == RainAmount.none) {
      _sim.step(
        dt,
        s.has(Effect.drops) ? _amount : RainAmount.none,
        rate: (180 / s.rewetSecs).clamp(0.3, 6.0),
        wind: s.wind,
        impacts: _v.moving && s.has(Effect.ripples),
      );
      _time.value = (_time.value + dt) % 1000;
    }
    if (!_wet) {
      _ticker.stop();
      _drops?.dispose();
      _drops = null;
      setState(() {});
      return;
    }
    // 화면 밖(스크롤로 가려짐)이면 계산만 하고 그림은 굽지 않는다.
    if (DateTime.now().difference(_paintedAt).inMilliseconds > 400) return;
    _sinceSnap += dt;
    if (_sinceSnap > 3) _snapshot();
    _render();
    _frame.value++;
  }

  // 물방울이 비출 카드 모습. 그린 직후에 떠야 한다 — 틱 시점엔 다시 그릴 표시가 붙어 있을 수 있다.
  void _snapshot() {
    if (_snapBusy) return;
    _snapBusy = true;
    _sinceSnap = 0;
    SchedulerBinding.instance.addPostFrameCallback((_) async {
      final box = _snapKey.currentContext?.findRenderObject();
      if (!mounted || box is! RenderRepaintBoundary) {
        _snapBusy = false;
        return;
      }
      final img = await box.toImage(pixelRatio: 1.5);
      _snapBusy = false;
      if (!mounted) return img.dispose();
      _snap?.dispose();
      _snap = img;
    });
  }

  void _render() {
    final sprite = _sprite!;
    final size = _sim.size;
    final rec = ui.PictureRecorder();
    final c = Canvas(rec)..scale(_dpr);
    const half = 128.0;
    // 스프라이트 알파 0.7 선이 가장자리 — 반지름 r 방울이면 지름 5r 로 찍는다.
    final beads = _sim.beads;
    c.drawAtlas(
      sprite,
      [
        for (final b in beads)
          RSTransform.fromComponents(
            rotation: 0,
            scale: b.r * 2.5 / half,
            anchorX: half,
            anchorY: half,
            translateX: b.p.dx,
            translateY: b.p.dy,
          ),
      ],
      [for (var i = 0; i < beads.length; i++) const Rect.fromLTWH(0, 0, 256, 256)],
      null,
      null,
      null,
      Paint()..filterQuality = FilterQuality.medium,
    );
    const src = Rect.fromLTWH(0, 0, 256, 256);
    final paint = Paint()..filterQuality = FilterQuality.medium;
    for (final d in _sim.runs) {
      final stretch = math.min(0.5, d.vy / 260);
      c.drawImageRect(sprite, src, Rect.fromCenter(center: d.p, width: d.r * 5, height: d.r * 5 * (1 + stretch)), paint);
    }
    final img = rec.endRecording().toImageSync((size.width * _dpr).ceil(), (size.height * _dpr).ceil());
    _drops?.dispose();
    _drops = img;
  }

  void _onDown(PointerDownEvent e) {
    if (!_v.active) return;
    final wasFocused = weather.focused.value == widget.id;
    weather.focus(widget.id);
    final s = weather.settings.value;
    if (s.has(Effect.ripples)) _sim.ripple(e.localPosition);
    switch (s.wipe) {
      case WipeMode.onInput:
        _down = _prev = e.localPosition;
        _onButton = _scene?.isOnButton(e.position) ?? false;
        _sim.wipePath(e.localPosition, e.localPosition);
        if (!_onButton) _wipedAt = DateTime.now();
      case WipeMode.onFocus:
        if (!wasFocused) _sweep(e.localPosition.dx < _sim.size.width / 2);
      case WipeMode.never:
        break;
    }
    _sync();
  }

  void _onMove(PointerMoveEvent e) {
    final prev = _prev, down = _down;
    if (prev == null || down == null) return;
    _sim.wipePath(prev, e.localPosition);
    _prev = e.localPosition;
    // 단추에서 시작했어도 끌고 나가면 스크롤이다 — 그 카드는 쓴 카드로 친다.
    if (_onButton && (e.localPosition - down).distance > 18) {
      _onButton = false;
      _wipedAt = DateTime.now();
    }
  }

  void _onUp(PointerEvent e) {
    final down = _down;
    if (down == null) return;
    if (!_onButton) _sweep(e.localPosition.dx >= down.dx);
    _down = _prev = null;
    _onButton = false;
  }

  void _sweep(bool fromLeft) {
    _sim.startSweep(fromLeft: fromLeft);
    _wipedAt = DateTime.now();
    if (!_ticker.isActive && _wet) _ticker.start();
  }

  @override
  Widget build(BuildContext context) {
    if (!_v.active) return widget.child;
    _dpr = MediaQuery.devicePixelRatioOf(context).clamp(1.0, 2.0);
    final s = weather.settings.value;
    final wet = _wet;
    final bg = Theme.of(context).scaffoldBackgroundColor;
    _dark = ThemeData.estimateBrightnessForColor(bg) == Brightness.dark;
    final streaks = wet && _amount != RainAmount.none && _v.moving && s.has(Effect.streaks) && _rain != null;
    return Listener(
      behavior: HitTestBehavior.translucent,
      onPointerDown: _onDown,
      onPointerMove: _onMove,
      onPointerUp: _onUp,
      onPointerCancel: _onUp,
      child: GestureDetector(
        onLongPress: () => showCardWeatherSheet(context, widget.id),
        child: Stack(
          key: _boxKey,
          children: [
            RepaintBoundary(
              key: _snapKey,
              child: Stack(
                children: [
                  // 젖은 카드는 바탕을 깐다 — 물방울이 비출 카드 모습이 투명하면 방울이 검게 뜬다.
                  if (wet) Positioned.fill(child: ColoredBox(color: bg)),
                  if (streaks)
                    Positioned.fill(
                      child: IgnorePointer(
                        child: CustomPaint(
                          painter: RainPainter(
                            shader: _rain!,
                            time: _time,
                            amount: _amount,
                            wind: s.wind,
                            dpr: MediaQuery.devicePixelRatioOf(context),
                            dark: _dark,
                            ground: 1.2,
                          ),
                        ),
                      ),
                    ),
                  KeyedSubtree(key: const ValueKey('weather-card-child'), child: widget.child),
                ],
              ),
            ),
            if (wet)
              Positioned.fill(
                child: IgnorePointer(child: CustomPaint(painter: _GlassPainter(this))),
              ),
          ],
        ),
      ),
    );
  }
}

class _GlassPainter extends CustomPainter {
  _GlassPainter(this.s) : super(repaint: s._frame);

  final _WeatherCardState s;

  @override
  void paint(Canvas canvas, Size size) {
    s._paintedAt = DateTime.now();
    final drops = s._drops, snap = s._snap, shader = s._glass;
    if (drops == null || snap == null || shader == null) return;
    final sim = s._sim;
    final set = weather.settings.value;
    final since = DateTime.now().difference(s._wipedAt).inMilliseconds / 1000;
    final fog = set.has(Effect.mist) && s._amount != RainAmount.none
        ? math.min(1.0, since / set.rewetSecs) * 0.12
        : 0.0;
    final dark = s._dark;
    final fogColor = dark ? const [0.62, 0.74, 0.86] : const [0.97, 0.98, 1.0];
    shader
      ..setFloat(0, size.width)
      ..setFloat(1, size.height)
      ..setFloat(2, fog)
      ..setFloat(3, sim.level);
    for (var i = 0; i < GlassSim.poolCells; i++) {
      shader.setFloat(4 + i, sim.pool[i]);
    }
    shader
      ..setFloat(36, fogColor[0])
      ..setFloat(37, fogColor[1])
      ..setFloat(38, fogColor[2]);
    final rip = set.has(Effect.ripples) ? sim.ripples : const <Ripple>[];
    for (var i = 0; i < Ripple.max; i++) {
      final w = i < rip.length ? rip[i] : null;
      shader
        ..setFloat(39 + i * 4, w?.at.dx ?? 0)
        ..setFloat(40 + i * 4, w?.at.dy ?? 0)
        ..setFloat(41 + i * 4, w == null ? 0 : w.age / Ripple.life)
        ..setFloat(42 + i * 4, w?.strength ?? 0);
    }
    shader
      ..setImageSampler(0, snap)
      ..setImageSampler(1, drops);
    canvas.drawRect(Offset.zero & size, Paint()..shader = shader);

    // 쓸어 닦는 막대 — 물기를 밀고 지나가는 자리를 옅게 보인다.
    final x = sim.sweepX;
    if (x != null) {
      final bar = Rect.fromLTWH(x - 10, 0, 20, size.height);
      canvas.drawRect(
        bar,
        Paint()
          ..shader = LinearGradient(
            colors: dark
                ? const [Color(0x00ffffff), Color(0x33dff0ff), Color(0x00ffffff)]
                : const [Color(0x00000000), Color(0x1a2a4a6a), Color(0x00000000)],
          ).createShader(bar),
      );
    }
  }

  @override
  bool shouldRepaint(_GlassPainter old) => true;
}
