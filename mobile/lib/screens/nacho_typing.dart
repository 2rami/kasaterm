import 'dart:math' as math;

import 'package:flutter/material.dart';

/// 나쵸가 답을 만드는 동안 나쵸 말풍선 자리에 서는 점 세 개. 차례로 통통 뛴다.
///
/// 컨트롤러는 하나 — 세 점은 같은 시계의 위상만 다르게 읽는다. 동작 줄이기가 켜져 있으면
/// 시계를 아예 안 돌리고 멈춘 점만 둔다(프레임을 계속 요청하지 않게).
class NachoTyping extends StatefulWidget {
  const NachoTyping({super.key, this.progress});

  /// 나쵸가 지금 하는 일 한 줄(서버가 적은 진행). 없으면 점만.
  final String? progress;

  static const period = Duration(milliseconds: 1200);

  /// 한 바퀴의 앞 36% 동안만 떠올랐다 내려온다 — 세 점이 그 구간을 12% 씩 늦게 지나서 차례로 뛴다.
  static double lift(double t, int dot) {
    final p = (t - dot * 0.12) % 1.0;
    const hop = 0.36;
    return p < hop ? math.sin(p / hop * math.pi) : 0;
  }

  @override
  State<NachoTyping> createState() => _NachoTypingState();
}

class _NachoTypingState extends State<NachoTyping>
    with SingleTickerProviderStateMixin {
  late final AnimationController _clock = AnimationController(
    vsync: this,
    duration: NachoTyping.period,
  );

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    if (MediaQuery.disableAnimationsOf(context)) {
      _clock
        ..stop()
        ..value = 0;
    } else if (!_clock.isAnimating) {
      _clock.repeat();
    }
  }

  @override
  void dispose() {
    _clock.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final progress = widget.progress;
    return Semantics(
      liveRegion: true,
      label: '나쵸가 답하는 중',
      child: Align(
        alignment: Alignment.centerLeft,
        child: Padding(
          padding: const EdgeInsets.fromLTRB(12, 4, 40, 4),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Container(
                padding: const EdgeInsets.fromLTRB(16, 14, 16, 12),
                decoration: BoxDecoration(
                  color: scheme.surfaceContainerHighest,
                  borderRadius: BorderRadius.circular(16),
                ),
                child: ExcludeSemantics(
                  child: AnimatedBuilder(
                    animation: _clock,
                    builder: (context, _) => Row(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        for (var i = 0; i < 3; i++)
                          Padding(
                            padding: EdgeInsets.only(left: i == 0 ? 0 : 5),
                            child: Transform.translate(
                              offset: Offset(0, -4 * NachoTyping.lift(_clock.value, i)),
                              child: Container(
                                width: 7,
                                height: 7,
                                decoration: BoxDecoration(
                                  color: scheme.onSurfaceVariant.withValues(
                                    alpha: 0.55 + 0.45 * NachoTyping.lift(_clock.value, i),
                                  ),
                                  shape: BoxShape.circle,
                                ),
                              ),
                            ),
                          ),
                      ],
                    ),
                  ),
                ),
              ),
              if (progress != null && progress.isNotEmpty)
                Padding(
                  padding: const EdgeInsets.only(top: 3, left: 4),
                  child: Text(
                    progress,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(
                      fontSize: 12,
                      color: scheme.onSurfaceVariant,
                    ),
                  ),
                ),
            ],
          ),
        ),
      ),
    );
  }
}
