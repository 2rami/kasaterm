import 'dart:math' as math;

import 'package:flutter/material.dart';

import 'look.dart';
import 'original_assets.dart';

/// 켜자마자 계정을 확인하는 동안 — 쌍둥이가 번갈아 깡총 뛴다. 언니(하늘)는 차분하게 낮게,
/// 동생(호박)은 높게. 「동작 줄이기」면 나란히 서 있는 그림 하나로 멈춘다.
class TwinsLoading extends StatefulWidget {
  const TwinsLoading({super.key, this.label = '계정 확인 중'});

  final String label;

  @override
  State<TwinsLoading> createState() => _TwinsLoadingState();
}

class _TwinsLoadingState extends State<TwinsLoading> with SingleTickerProviderStateMixin {
  late final _beat = AnimationController(vsync: this, duration: Look.twinsBeat);
  bool _still = false;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    // iOS 「동작 줄이기」는 엔진이 reduceMotion 으로만 알린다 — MediaQuery 에는 안 실린다.
    _still = MediaQuery.disableAnimationsOf(context) ||
        View.of(context).platformDispatcher.accessibilityFeatures.reduceMotion;
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
                  ? _Stage(t: 0, ink: theme.colorScheme.onSurface)
                  : AnimatedBuilder(
                      animation: _beat,
                      builder: (context, _) =>
                          _Stage(t: _beat.value, ink: theme.colorScheme.onSurface),
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

/// 두 그림과 발밑 그림자. 그림은 1024 정사각에 여백이 있어 동생을 언니 쪽으로 당겨 놓는다.
class _Stage extends StatelessWidget {
  const _Stage({required this.t, required this.ink});

  final double t;
  final Color ink;

  @override
  Widget build(BuildContext context) {
    const s = Look.twins;
    // 언니 그림의 오른쪽 끝(.83)과 동생 그림의 왼쪽 끝(.245)이 거의 맞닿게 — 둘이 한 쌍으로 읽힌다.
    const amberX = s * 0.6;
    final cache = (s * MediaQuery.devicePixelRatioOf(context)).round();
    Widget figure(String key, double phase, double hop, double tilt) {
      final h = _hop(phase);
      final squash = _squash(phase);
      return SizedBox(
        width: s,
        height: s + Look.twinsHop,
        child: Stack(
          alignment: Alignment.bottomCenter,
          children: [
            Positioned(
              bottom: 0,
              child: Container(
                width: s * 0.42 * (1 - 0.3 * h),
                height: Look.twinsShadow,
                decoration: BoxDecoration(
                  color: ink.withValues(alpha: 0.12 * (1 - 0.5 * h)),
                  borderRadius: BorderRadius.circular(Look.twinsShadow),
                ),
              ),
            ),
            Positioned(
              // 그림 아래 여백(3%)보다 조금 더 내려 장화 밑창이 그림자 가운데에 놓이게.
              bottom: Look.twinsShadow / 2 - s * 0.05 + hop * h,
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
      height: s + Look.twinsHop,
      child: Stack(
        children: [
          Positioned(
            left: 0,
            child: figure('kasa_sky', sky, Look.twinsHop * 0.5,
                0.03 * math.sin(sky * 2 * math.pi)),
          ),
          Positioned(
            left: amberX,
            child: figure('kasa_amber', amber, Look.twinsHop, -0.06 * _hop(amber)),
          ),
        ],
      ),
    );
  }

  /// 박의 앞 45% 는 공중, 나머지는 땅.
  static double _hop(double p) => p < 0.45 ? math.sin(math.pi * p / 0.45) : 0;

  /// 내려앉은 직후 잠깐 눌린다.
  static double _squash(double p) =>
      p >= 0.45 && p < 0.6 ? math.sin(math.pi * (p - 0.45) / 0.15) : 0;
}
