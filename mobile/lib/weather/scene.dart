import 'dart:async';
import 'dart:math' as math;
import 'dart:ui' as ui;

import 'package:flutter/cupertino.dart';
import 'package:flutter/material.dart';
import 'package:flutter/scheduler.dart';

import 'model.dart';
import 'rain.dart';
import 'store.dart';

/// 카드가 화면 층에 알려 주는 자기 자리와 지금 비 양.
abstract interface class WeatherPlace {
  Rect? rectIn(RenderBox scene);
  RainAmount get amount;
}

/// 날씨가 이 화면에서 켜져 있는지와 움직이는지. 카드·층이 같은 답을 본다.
({bool active, bool moving}) weatherVerdictOf(BuildContext context) => verdict(
  weather.settings.value,
  OsMotion(
    reduceMotion: weather.reduceMotion.value || (MediaQuery.maybeDisableAnimationsOf(context) ?? false),
    reduceTransparency: weather.reduceTransparency.value,
  ),
);

/// 화면 하나의 날씨 층 — 카드 밖 바탕의 빗줄기(카드·입력칸은 비움)와 그 화면에 그려진 단추마다의
/// 물방울. 단추는 화면을 훑어 찾는다(데스크톱이 그 장에 그려진 조작을 모으는 것과 같다) — 화면마다
/// 단추를 따로 감싸지 않는다. 날씨가 꺼져 있으면 훑기·틱·셰이더가 없다.
class WeatherScene extends StatefulWidget {
  const WeatherScene({super.key, required this.child});

  final Widget child;

  static WeatherSceneState? maybeOf(BuildContext context) =>
      context.getInheritedWidgetOfExactType<_SceneScope>()?.state;

  @override
  State<WeatherScene> createState() => WeatherSceneState();
}

class _SceneScope extends InheritedWidget {
  const _SceneScope({required this.state, required super.child});

  final WeatherSceneState state;

  @override
  bool updateShouldNotify(_SceneScope old) => false;
}

class _Spot {
  _Spot(this.box, this.enabled) : inflate = enabled ? 1 : 0;

  RenderBox box;
  bool enabled;
  bool seen = true;
  Rect rect = Rect.zero;
  int row = 0;
  double press = 0, pressV = 0, pressTarget = 0, inflate, inflateV = 0, ring = -1;

  bool get busy => (press - pressTarget).abs() > 0.01 || pressV.abs() > 0.01 || ring >= 0 ||
      (inflate - (enabled ? 1 : 0)).abs() > 0.01;
}

class WeatherSceneState extends State<WeatherScene> with SingleTickerProviderStateMixin {
  static ui.FragmentProgram? _buttonsProgram;
  static ui.FragmentShader? _rainShader;

  final _places = <WeatherPlace>{};
  final _spots = <Element, _Spot>{};
  final _guards = <RenderBox>[];
  final _repaint = ValueNotifier<int>(0);
  final _time = ValueNotifier<double>(0);
  final _paintKey = GlobalKey();
  late final Ticker _ticker = createTicker(_tick);
  ui.FragmentShader? _buttons;
  Timer? _scan;
  Duration _last = Duration.zero;
  ({bool active, bool moving}) _v = (active: false, moving: false);
  bool _current = true;

  void register(WeatherPlace p) => _places.add(p);
  void unregister(WeatherPlace p) => _places.remove(p);

  RenderBox? get _sceneBox {
    final r = _paintKey.currentContext?.findRenderObject();
    return r is RenderBox && r.attached && r.hasSize ? r : null;
  }

  @override
  void initState() {
    super.initState();
    weather.changes.addListener(_sync);
  }

  // 셰이더는 날씨가 처음 켜질 때 올린다 — 끈 사람은 올리지도 않는다.
  Future<void> _load() async {
    _buttonsProgram ??= await ui.FragmentProgram.fromAsset('shaders/weather_buttons.frag');
    _rainShader ??= (await RainPainter.program()).fragmentShader();
    if (!mounted) return;
    setState(() => _buttons = _buttonsProgram!.fragmentShader());
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    _current = ModalRoute.of(context)?.isCurrent ?? true;
    WidgetsBinding.instance.addPostFrameCallback((_) => _sync());
  }

  @override
  void dispose() {
    weather.changes.removeListener(_sync);
    _scan?.cancel();
    _ticker.dispose();
    _repaint.dispose();
    _time.dispose();
    _buttons?.dispose();
    super.dispose();
  }

  void _sync() {
    if (!mounted) return;
    final v = weatherVerdictOf(context);
    final wasActive = _v.active;
    _v = v;
    if (v.active && _buttons == null) _load();
    final s = weather.settings.value;
    final wantsButtons = v.active && _current && s.has(Effect.buttons);
    if (wantsButtons && _scan == null) {
      _scan = Timer.periodic(const Duration(milliseconds: 300), (_) => _collect());
      _collect();
    } else if (!wantsButtons && _scan != null) {
      _scan?.cancel();
      _scan = null;
      _spots.clear();
    }
    _kick();
    if (wasActive != v.active) setState(() {});
    _repaint.value++;
  }

  bool get _raining =>
      _v.moving && _current && weather.settings.value.has(Effect.streaks) &&
      backgroundAmount(weather.settings.value) != RainAmount.none;

  void _kick() {
    final want = _v.moving && _current && (_raining || _spots.values.any((s) => s.busy));
    if (want && !_ticker.isActive) {
      _last = Duration.zero;
      _ticker.start();
    } else if (!want && _ticker.isActive) {
      _ticker.stop();
    }
  }

  void _tick(Duration now) {
    final dt = _last == Duration.zero ? 1 / 60 : math.min(0.05, (now - _last).inMicroseconds / 1e6);
    _last = now;
    _time.value = (_time.value + dt) % 1000;
    for (final s in _spots.values) {
      final steps = (dt * 480).ceil().clamp(1, 64);
      final h = dt / steps;
      final inflateT = s.enabled ? 1.0 : 0.0;
      for (var i = 0; i < steps; i++) {
        s.pressV += ((s.pressTarget - s.press) * 900 - s.pressV * 16) * h;
        s.press += s.pressV * h;
        s.inflateV += ((inflateT - s.inflate) * 220 - s.inflateV * 13) * h;
        s.inflate += s.inflateV * h;
      }
      if (s.ring >= 0) s.ring = s.ring + dt / 0.9 > 1 ? -1 : s.ring + dt / 0.9;
    }
    _repaint.value++;
    _kick();
  }

  // 화면을 훑어 단추·입력칸을 모은다. 단추 모양은 앱 테마 단추 그대로라 종류로 알아본다.
  void _collect() {
    if (!mounted) return;
    for (final s in _spots.values) {
      s.seen = false;
    }
    _guards.clear();
    void visit(Element e) {
      final w = e.widget;
      bool? enabled;
      if (w is ButtonStyleButton) {
        enabled = w.enabled;
      } else if (w is IconButton) {
        enabled = w.onPressed != null;
      } else if (w is FloatingActionButton) {
        enabled = w.onPressed != null;
      } else if (w is RawChip) {
        enabled = w.isEnabled;
      } else if (w is CupertinoButton) {
        enabled = w.onPressed != null;
      } else if (w is EditableText) {
        final r = e.findRenderObject();
        if (r is RenderBox) _guards.add(r);
        return;
      }
      if (enabled != null) {
        final r = e.findRenderObject();
        if (r is RenderBox) {
          final spot = _spots[e];
          if (spot == null) {
            _spots[e] = _Spot(r, enabled);
          } else {
            spot
              ..box = r
              ..enabled = enabled
              ..seen = true;
          }
        }
        // 단추 안의 단추(아이콘 단추 속 칩 따위)는 하나로 본다.
        return;
      }
      e.visitChildren(visit);
    }

    (_paintKey.currentContext as Element?)?.visitChildren(visit);
    _spots.removeWhere((_, s) => !s.seen);
    _kick();
    _repaint.value++;
  }

  // 카드 안의 단추는 그 카드의 비 양, 바깥은 바탕의 비 양을 따른다. 비가 없는 자리의 단추는 마른다.
  RainAmount _amountAt(RenderBox scene, Offset p) {
    for (final place in _places) {
      final r = place.rectIn(scene);
      if (r != null && r.contains(p)) return place.amount;
    }
    return backgroundAmount(weather.settings.value);
  }

  void _press(Offset global, bool down) {
    final scene = _sceneBox;
    if (scene == null) return;
    final local = scene.globalToLocal(global);
    for (final s in _spots.values) {
      final hit = down && s.rect.inflate(4).contains(local);
      if (hit && s.enabled) {
        s.pressTarget = 1;
        s.ring = 0;
      } else {
        s.pressTarget = 0;
      }
    }
    _kick();
  }

  Path _backgroundClip(Size size) {
    final scene = _sceneBox;
    var path = Path()..addRect(Offset.zero & size);
    if (scene == null) return path;
    final holes = Path();
    for (final p in _places) {
      final r = p.rectIn(scene);
      if (r != null) holes.addRect(r);
    }
    for (final g in _guards) {
      if (!g.attached || !g.hasSize) continue;
      holes.addRect((g.localToGlobal(Offset.zero, ancestor: scene) & g.size).inflate(6));
    }
    return Path.combine(PathOperation.difference, path, holes);
  }

  @override
  Widget build(BuildContext context) {
    final active = _v.active && _current;
    final dark = Theme.of(context).brightness == Brightness.dark;
    final rain = _rainShader;
    return _SceneScope(
      state: this,
      child: Listener(
        behavior: HitTestBehavior.translucent,
        onPointerDown: active ? (e) => _press(e.position, true) : null,
        onPointerUp: active ? (e) => _press(e.position, false) : null,
        onPointerCancel: active ? (e) => _press(e.position, false) : null,
        child: NotificationListener<ScrollNotification>(
          onNotification: (_) {
            if (active) _repaint.value++;
            return false;
          },
          child: Stack(
            key: _paintKey,
            fit: StackFit.passthrough,
            children: [
              if (active && rain != null)
                Positioned.fill(
                  child: IgnorePointer(
                    child: RepaintBoundary(
                      child: CustomPaint(
                        painter: RainPainter(
                          shader: rain,
                          time: _time,
                          amount: _raining ? backgroundAmount(weather.settings.value) : RainAmount.none,
                          wind: weather.settings.value.wind,
                          dpr: MediaQuery.devicePixelRatioOf(context),
                          dark: dark,
                          clip: _backgroundClip,
                          repaint: Listenable.merge([_time, _repaint]),
                        ),
                      ),
                    ),
                  ),
                ),
              KeyedSubtree(key: const ValueKey('weather-scene-child'), child: widget.child),
              if (active && _buttons != null && weather.settings.value.has(Effect.buttons))
                Positioned.fill(
                  child: IgnorePointer(
                    child: CustomPaint(painter: _ButtonsPainter(this, _buttons!, dark)),
                  ),
                ),
            ],
          ),
        ),
      ),
    );
  }
}

class _ButtonsPainter extends CustomPainter {
  _ButtonsPainter(this.s, this.shader, this.dark) : super(repaint: s._repaint);

  final WeatherSceneState s;
  final ui.FragmentShader shader;
  final bool dark;

  static const max = 24;

  @override
  void paint(Canvas canvas, Size size) {
    final scene = s._sceneBox;
    if (scene == null) return;
    final bounds = Offset.zero & size;
    final shown = <_Spot>[];
    for (final spot in s._spots.values) {
      if (!spot.box.attached || !spot.box.hasSize) continue;
      spot.rect = spot.box.localToGlobal(Offset.zero, ancestor: scene) & spot.box.size;
      if (!bounds.overlaps(spot.rect) || spot.rect.isEmpty) continue;
      if (s._amountAt(scene, spot.rect.center) == RainAmount.none) continue;
      shown.add(spot);
    }
    if (shown.isEmpty) return;
    // 한 줄에 이웃한 단추가 한 무리 — 셰이더는 같은 무리끼리만 잇는다.
    shown.sort((a, b) => a.rect.center.dy != b.rect.center.dy
        ? a.rect.center.dy.compareTo(b.rect.center.dy)
        : a.rect.left.compareTo(b.rect.left));
    var row = 0;
    for (var i = 0; i < shown.length; i++) {
      if (i > 0) {
        final a = shown[i - 1].rect, b = shown[i].rect;
        final sameRow = (a.center.dy - b.center.dy).abs() < math.min(a.height, b.height) / 2 && b.left - a.right < 12;
        if (!sameRow) row++;
      }
      shown[i].row = row;
    }
    final n = math.min(max, shown.length);
    final tint = dark ? const [0.62, 0.8, 1.0] : const [0.2, 0.45, 0.75];
    shader
      ..setFloat(0, size.width)
      ..setFloat(1, size.height)
      ..setFloat(2, n.toDouble())
      ..setFloat(3, tint[0])
      ..setFloat(4, tint[1])
      ..setFloat(5, tint[2])
      ..setFloat(6, dark ? 1 : 0);
    for (var i = 0; i < max; i++) {
      final spot = i < n ? shown[i] : null;
      // 누름 영역 44 로 넓힌 단추는 보이는 테보다 크다 — 물방울은 조금 안쪽에 앉힌다.
      final r = spot?.rect.deflate(2);
      final b = 7 + i * 4, st = 7 + max * 4 + i * 4;
      shader
        ..setFloat(b, r?.center.dx ?? 0)
        ..setFloat(b + 1, r?.center.dy ?? 0)
        ..setFloat(b + 2, r == null ? 0 : r.width / 2)
        ..setFloat(b + 3, r == null ? 0 : r.height / 2)
        ..setFloat(st, spot?.press ?? 0)
        ..setFloat(st + 1, spot?.inflate ?? 0)
        ..setFloat(st + 2, spot?.ring ?? -1)
        ..setFloat(st + 3, spot?.row.toDouble() ?? -1);
    }
    final paint = Paint()..shader = shader;
    for (var g = 0; g <= row; g++) {
      Rect? box;
      for (final spot in shown.take(n)) {
        if (spot.row == g) box = box?.expandToInclude(spot.rect) ?? spot.rect;
      }
      if (box != null) canvas.drawRect(box.inflate(40), paint);
    }
  }

  @override
  bool shouldRepaint(_ButtonsPainter old) => true;
}
