import 'dart:math' as math;
import 'dart:ui' show lerpDouble;

import 'package:flutter/cupertino.dart';
import 'package:flutter/material.dart';

import 'look.dart';
import 'original_assets.dart';

/// 켜자마자 계정을 확인하는 동안 — 쌍둥이가 번갈아 깡총 뛴다. 언니(하늘)는 차분하게 낮게,
/// 동생(호박)은 높게. 「동작 줄이기」면 나란히 서 있는 그림 하나로 멈춘다.
class TwinsLoading extends StatefulWidget {
  const TwinsLoading({
    super.key,
    this.label = '계정 확인 중',
    this.size = Look.twins,
  });

  final String label;

  /// 그림 한 장의 정사각 — 켤 때 136, 목록 받는 중 96.
  final double size;

  @override
  State<TwinsLoading> createState() => _TwinsLoadingState();
}

class _TwinsLoadingState extends State<TwinsLoading>
    with SingleTickerProviderStateMixin {
  late final _beat = AnimationController(vsync: this, duration: Look.twinsBeat);
  bool _still = false;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    _still = Look.still(context);
    if (_still) {
      _beat.stop();
    } else if (!_beat.isAnimating) {
      _beat.repeat();
    }
  }

  @override
  void dispose() {
    _beat.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final ink = theme.colorScheme.onSurfaceVariant;
    final text = theme.textTheme.bodyMedium?.copyWith(color: ink);
    return Semantics(
      label: widget.label,
      liveRegion: true,
      child: ExcludeSemantics(
        child: Center(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              _still
                  ? TwinsStage(t: 0, size: widget.size)
                  : AnimatedBuilder(
                      animation: _beat,
                      builder: (context, _) =>
                          TwinsStage(t: _beat.value, size: widget.size),
                    ),
              const SizedBox(height: Look.groupGap),
              Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Text(widget.label, style: text),
                  for (var i = 0; i < 3; i++)
                    _still
                        ? Text('.', style: text)
                        : AnimatedBuilder(
                            animation: _beat,
                            builder: (context, _) => Opacity(
                              opacity: _dot(_beat.value, i),
                              child: Text('.', style: text),
                            ),
                          ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }

  /// 점 셋이 왼쪽부터 차례로 켜진다 — 한 박에 한 바퀴.
  static double _dot(double t, int i) {
    final p = (t - i / 6) % 1.0;
    return 0.25 + 0.75 * math.max(0.0, math.sin(p * 2 * math.pi));
  }
}

/// 빈·실패 화면 — 서 있는 쌍둥이와 문장 하나, 다음 행동 단추(design.md 「빈·실패 화면」).
class TwinsNotice extends StatelessWidget {
  const TwinsNotice({super.key, required this.text, this.action, this.color});

  final String text;

  /// 다음 행동 — 「다시 시도」「새 방」 같은 단추.
  final Widget? action;

  /// 문장 색. 실패면 위험색.
  final Color? color;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: Look.groupGap),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          const ExcludeSemantics(
            child: TwinsStage(t: 0, size: Look.twinsSmall),
          ),
          const SizedBox(height: Look.fieldGap),
          Text(
            text,
            textAlign: TextAlign.center,
            style: theme.textTheme.bodyMedium?.copyWith(
              color: color ?? theme.colorScheme.onSurfaceVariant,
            ),
          ),
          if (action != null) ...[
            const SizedBox(height: Look.fieldGap),
            action!,
          ],
        ],
      ),
    );
  }
}

/// 두 그림과 발밑 그림자. 그림은 1024 정사각에 여백이 있어 동생을 언니 쪽으로 당겨 놓는다.
/// [t] 는 한 박 안의 자리(0~1). 0 이면 둘 다 땅에 서 있다.
class TwinsStage extends StatelessWidget {
  const TwinsStage({super.key, required this.t, this.size = Look.twins});

  final double t;
  final double size;

  @override
  Widget build(BuildContext context) {
    final s = size;
    final k = s / Look.twins;
    final hopH = Look.twinsHop * k;
    final shadowH = Look.twinsShadow * k;
    final ink = Theme.of(context).colorScheme.onSurface;
    // 언니 그림의 오른쪽 끝(.83)과 동생 그림의 왼쪽 끝(.245)이 거의 맞닿게 — 둘이 한 쌍으로 읽힌다.
    final amberX = s * 0.6;
    final cache = (s * MediaQuery.devicePixelRatioOf(context)).round();
    Widget figure(String key, double phase, double hop, double tilt) {
      final h = _hop(phase);
      final squash = _squash(phase);
      return SizedBox(
        width: s,
        height: s + hopH,
        child: Stack(
          alignment: Alignment.bottomCenter,
          children: [
            Positioned(
              bottom: 0,
              child: Container(
                width: s * 0.42 * (1 - 0.3 * h),
                height: shadowH,
                decoration: BoxDecoration(
                  color: ink.withValues(alpha: 0.12 * (1 - 0.5 * h)),
                  borderRadius: BorderRadius.circular(shadowH),
                ),
              ),
            ),
            Positioned(
              // 그림 아래 여백(3%)보다 조금 더 내려 장화 밑창이 그림자 가운데에 놓이게.
              bottom: shadowH / 2 - s * 0.05 + hop * h,
              child: Transform(
                alignment: Alignment.bottomCenter,
                transform: Matrix4.identity()
                  ..rotateZ(tilt)
                  ..scaleByDouble(1 + 0.03 * squash, 1 - 0.05 * squash, 1, 1),
                child: Image.asset(
                  originalCharacterAssets[key]!,
                  width: s,
                  height: s,
                  cacheWidth: cache,
                  gaplessPlayback: true,
                ),
              ),
            ),
          ],
        ),
      );
    }

    final sky = t;
    final amber = (t + 0.5) % 1.0;
    return SizedBox(
      width: amberX + s,
      height: s + hopH,
      child: Stack(
        children: [
          Positioned(
            left: 0,
            child: figure(
              'kasa_sky',
              sky,
              hopH * 0.5,
              0.03 * math.sin(sky * 2 * math.pi),
            ),
          ),
          Positioned(
            left: amberX,
            child: figure('kasa_amber', amber, hopH, -0.06 * _hop(amber)),
          ),
        ],
      ),
    );
  }
}

/// 박의 앞 45% 는 공중, 나머지는 땅.
double _hop(double p) => p < 0.45 ? math.sin(math.pi * p / 0.45) : 0;

/// 내려앉은 직후 잠깐 눌린다.
double _squash(double p) =>
    p >= 0.45 && p < 0.6 ? math.sin(math.pi * (p - 0.45) / 0.15) : 0;

/// 앱바 왼쪽의 쌍둥이 표 — 얼굴 동그라미 둘이 겹친다. [hopping] 이면 번갈아 깡총.
/// 멈추라 하면 그 박을 마저 뛰고 땅에 선다 — 공중에서 얼지 않게.
class TwinsMark extends StatefulWidget {
  const TwinsMark({super.key, this.hopping = false, this.face = Look.markFace});

  final bool hopping;
  final double face;

  @override
  State<TwinsMark> createState() => _TwinsMarkState();
}

class _TwinsMarkState extends State<TwinsMark>
    with SingleTickerProviderStateMixin {
  late final _beat = AnimationController(vsync: this, duration: Look.twinsBeat);

  void _sync() {
    if (widget.hopping && !Look.still(context)) {
      if (!_beat.isAnimating) _beat.repeat();
    } else if (_beat.isAnimating) {
      _beat.animateTo(1).whenComplete(() {
        if (mounted && !widget.hopping) _beat.value = 0;
      });
    }
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    _sync();
  }

  @override
  void didUpdateWidget(TwinsMark old) {
    super.didUpdateWidget(old);
    _sync();
  }

  @override
  void dispose() {
    _beat.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final d = widget.face;
    final hop = Look.markHop * d / Look.markFace;
    final tone = TwinTone.of(context);
    final ring = Theme.of(context).scaffoldBackgroundColor;
    return Semantics(
      label: '카사 쌍둥이',
      child: AnimatedBuilder(
        animation: _beat,
        builder: (context, _) {
          final t = _beat.value % 1.0;
          return SizedBox(
            width: d * 2 - Look.markOverlap * d / Look.markFace,
            height: d + hop,
            child: Stack(
              clipBehavior: Clip.none,
              children: [
                Positioned(
                  left: 0,
                  bottom: hop * 0.6 * _hop(t),
                  child: _Face(
                    'kasa_sky',
                    size: d,
                    wash: tone.skyWash,
                    ring: ring,
                    cx: 0.49,
                    cy: 0.27,
                  ),
                ),
                Positioned(
                  right: 0,
                  bottom: hop * _hop((t + 0.5) % 1.0),
                  child: _Face(
                    'kasa_amber',
                    size: d,
                    wash: tone.amberWash,
                    ring: ring,
                    cx: 0.51,
                    cy: 0.26,
                  ),
                ),
              ],
            ),
          );
        },
      ),
    );
  }
}

/// 전신 그림에서 머리만 동그랗게 — 그림을 키워 머리 가운데([cx]·[cy], 그림 비율)를 동그라미 가운데에 둔다.
class _Face extends StatelessWidget {
  const _Face(
    this.asset, {
    required this.size,
    required this.wash,
    required this.ring,
    required this.cx,
    required this.cy,
  });

  final String asset;
  final double size;
  final Color wash, ring;
  final double cx, cy;

  /// 머리가 그림 폭의 약 43% — 동그라미에 머리가 꽉 차게 키우는 배율.
  static const _zoom = 2.3;

  @override
  Widget build(BuildContext context) {
    final s = size * _zoom;
    return Container(
      width: size,
      height: size,
      decoration: BoxDecoration(
        shape: BoxShape.circle,
        color: wash,
        border: Border.all(color: ring, width: 2),
      ),
      clipBehavior: Clip.antiAlias,
      child: Stack(
        clipBehavior: Clip.hardEdge,
        children: [
          Positioned(
            left: size / 2 - cx * s,
            top: size / 2 - cy * s,
            width: s,
            height: s,
            child: Image.asset(
              originalCharacterAssets[asset]!,
              cacheWidth: (s * MediaQuery.devicePixelRatioOf(context)).round(),
              gaplessPlayback: true,
            ),
          ),
        ],
      ),
    );
  }
}

/// 당겨서 새로 고침 — 당긴 만큼 쌍둥이 표가 내려오며 커지고, 놓으면 새로 고치는 동안 깡총.
/// `CupertinoSliverRefreshControl.builder` 에 그대로 넣는다.
Widget twinsRefresh(
  BuildContext context,
  RefreshIndicatorMode mode,
  double pulled,
  double trigger,
  double extent,
) {
  final p = (pulled / trigger).clamp(0.0, 1.0);
  if (p == 0) return const SizedBox.shrink();
  final busy =
      mode == RefreshIndicatorMode.refresh ||
      mode == RefreshIndicatorMode.armed;
  return Center(
    child: Opacity(
      opacity: math.min(1, p * 1.6),
      child: TwinsMark(
        hopping: busy,
        face: lerpDouble(Look.markFace * 0.6, Look.pullFace, p)!,
      ),
    ),
  );
}

/// 쌍둥이 당겨서 새로 고침 조각 — `CustomScrollView` 맨 앞에 두고 [twinsScroll] 물리와 함께 쓴다.
Widget twinsRefreshSliver(Future<void> Function() onRefresh) => CupertinoSliverRefreshControl(
  onRefresh: onRefresh,
  refreshTriggerPullDistance: Look.pullTrigger,
  refreshIndicatorExtent: Look.pullFace + Look.groupGap,
  builder: twinsRefresh,
);

/// 당김이 서려면 목록이 짧아도 늘 튕겨야 한다.
const twinsScroll = AlwaysScrollableScrollPhysics(parent: BouncingScrollPhysics());

/// 지난번 목록을 먼저 그린 동안 맨 위의 얇은 띠 — 하늘→호박 빛이 흐른다. 동작 줄이기면 반반 멈춘 띠.
class TwinBar extends StatefulWidget {
  const TwinBar({super.key});

  @override
  State<TwinBar> createState() => _TwinBarState();
}

class _TwinBarState extends State<TwinBar> with SingleTickerProviderStateMixin {
  late final _sweep = AnimationController(
    vsync: this,
    duration: Look.twinBarSweep,
  );

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    if (Look.still(context)) {
      _sweep.stop();
    } else if (!_sweep.isAnimating) {
      _sweep.repeat();
    }
  }

  @override
  void dispose() {
    _sweep.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final tone = TwinTone.of(context);
    final still = Look.still(context);
    return Semantics(
      label: '확인 중',
      child: SizedBox(
        height: Look.twinBarH,
        child: AnimatedBuilder(
          animation: _sweep,
          builder: (context, _) => DecoratedBox(
            decoration: BoxDecoration(
              gradient: still
                  ? LinearGradient(
                      colors: [
                        tone.skyInk,
                        tone.skyInk,
                        tone.amberInk,
                        tone.amberInk,
                      ],
                      stops: const [0, 0.5, 0.5, 1],
                    )
                  : LinearGradient(
                      colors: [tone.skyInk, tone.amberInk, tone.skyInk],
                      tileMode: TileMode.repeated,
                      transform: _Slide(_sweep.value),
                    ),
            ),
          ),
        ),
      ),
    );
  }
}

class _Slide extends GradientTransform {
  const _Slide(this.t);
  final double t;

  @override
  Matrix4 transform(Rect bounds, {TextDirection? textDirection}) =>
      Matrix4.translationValues(bounds.width * t, 0, 0);
}

/// 화면 바탕 — 테마 바탕색 위쪽에 하늘(왼쪽)·호박(오른쪽) 둥근 빛. Scaffold 를 투명하게 두고 이걸 밑에 깐다.
class TwinBackdrop extends StatelessWidget {
  const TwinBackdrop({super.key, required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) {
    final tone = TwinTone.of(context);
    RadialGradient glow(Alignment at, Color c) => RadialGradient(
      center: at,
      radius: 0.9,
      colors: [c, c.withValues(alpha: 0)],
    );
    return ColoredBox(
      color: Theme.of(context).scaffoldBackgroundColor,
      child: Stack(
        fit: StackFit.expand,
        children: [
          Positioned(
            top: 0,
            left: 0,
            right: 0,
            height: Look.glowH,
            child: IgnorePointer(
              child: DecoratedBox(
                decoration: BoxDecoration(
                  gradient: glow(const Alignment(-1, -1), tone.skyGlow),
                ),
                child: DecoratedBox(
                  decoration: BoxDecoration(
                    gradient: glow(const Alignment(1, -1), tone.amberGlow),
                  ),
                ),
              ),
            ),
          ),
          child,
        ],
      ),
    );
  }
}
