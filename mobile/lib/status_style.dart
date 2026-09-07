import 'package:flutter/material.dart';

import 'claude_style.dart';
import 'server.dart';

/// 학생 상태의 갈래 — 색·아이콘·말이 여기 하나로 묶인다.
enum PaneMood { waiting, working, done, resting, closed }

/// 상태 하나 = 색 하나·아이콘 하나·말 한 마디. 허브 칩·미니맵 점·터미널 상태줄이
/// 전부 이걸 쓴다 — 자리마다 따로 칠하면 「기다림」이 어디선 주황이고 어디선
/// 파랑이 된다(2026-09-07 지시 「뱃지색은 통일해야지 상태는」). 색은 데스크톱
/// DESIGN.md 의 status-attention·status-success 와 같은 값이라 두 화면이 한 말을 한다.
class StatusStyle {
  const StatusStyle({
    required this.mood,
    required this.label,
    required this.icon,
    required this.color,
  });

  final PaneMood mood;
  final String label;
  final IconData icon;
  final Color color;

  /// 사람 손이 필요한가 — 칩을 꽉 채워 눈에 띄게 하는 기준.
  bool get needsYou => mood == PaneMood.waiting;

  /// 지금 움직이는가 — 점이 숨 쉬듯 깜빡인다.
  bool get live => mood == PaneMood.working;

  static const attention = Color(0xffFA8C2A);
  static const success = Color(0xff3FB950);

  static StatusStyle of(Pane p, ColorScheme scheme) {
    if (p.closed) {
      return StatusStyle(
        mood: PaneMood.closed,
        label: '닫힘',
        icon: Icons.inventory_2_rounded,
        color: scheme.outline,
      );
    }
    if (p.isWaiting) {
      final (label, icon) = switch (p.kind) {
        'permission' => ('승인 기다림', Icons.pan_tool_alt_rounded),
        'question' => ('질문 기다림', Icons.help_rounded),
        'idle' => ('오래 기다림', Icons.hourglass_bottom_rounded),
        _ => ('답 기다림', Icons.chat_bubble_rounded),
      };
      return StatusStyle(
        mood: PaneMood.waiting,
        label: label,
        icon: icon,
        color: attention,
      );
    }
    if (p.isBusy) {
      return StatusStyle(
        mood: PaneMood.working,
        label: '작업 중',
        icon: Icons.bolt_rounded,
        color: scheme.primary,
      );
    }
    if (p.justDone) {
      return StatusStyle(
        mood: PaneMood.done,
        label: '방금 끝냄',
        icon: Icons.check_circle_rounded,
        color: success,
      );
    }
    return StatusStyle(
      mood: PaneMood.resting,
      label: '쉼',
      icon: Icons.bedtime_rounded,
      color: scheme.onSurfaceVariant,
    );
  }
}

/// 숨 쉬는 점 — 「작업 중」의 표시. 크기와 진하기가 1.2초마다 오간다.
class PulseDot extends StatefulWidget {
  const PulseDot({
    super.key,
    required this.color,
    this.size = 8,
    this.live = true,
  });

  final Color color;
  final double size;

  /// false 면 멈춘 점 — 같은 자리에 같은 크기로 서서 목록이 안 흔들린다.
  final bool live;

  @override
  State<PulseDot> createState() => _PulseDotState();
}

class _PulseDotState extends State<PulseDot>
    with SingleTickerProviderStateMixin {
  late final AnimationController _ctl = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 1200),
  );

  @override
  void initState() {
    super.initState();
    if (widget.live) _ctl.repeat(reverse: true);
  }

  @override
  void didUpdateWidget(PulseDot old) {
    super.didUpdateWidget(old);
    if (widget.live && !_ctl.isAnimating) _ctl.repeat(reverse: true);
    if (!widget.live && _ctl.isAnimating) _ctl.stop();
  }

  @override
  void dispose() {
    _ctl.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final s = widget.size;
    return SizedBox(
      width: s * 1.6,
      height: s * 1.6,
      child: AnimatedBuilder(
        animation: _ctl,
        builder: (context, _) {
          final t = Curves.easeInOut.transform(_ctl.value);
          return Stack(
            alignment: Alignment.center,
            children: [
              // 바깥 물결 — 점 둘레로 번지며 옅어진다.
              if (widget.live)
                Container(
                  width: s * (1.0 + 0.6 * t),
                  height: s * (1.0 + 0.6 * t),
                  decoration: BoxDecoration(
                    color: widget.color.withValues(alpha: 0.28 * (1 - t)),
                    shape: BoxShape.circle,
                  ),
                ),
              Container(
                width: s,
                height: s,
                decoration: BoxDecoration(
                  color: widget.color,
                  shape: BoxShape.circle,
                ),
              ),
            ],
          );
        },
      ),
    );
  }
}

/// 상태 칩 — 아이콘(또는 숨 쉬는 점) + 한 마디. 사람 손이 필요하면 꽉 채운 주황,
/// 나머지는 그 색을 옅게 깐다. 상태가 바뀌면 옛 칩이 스르르 새 칩으로 바뀐다.
class StatusChip extends StatelessWidget {
  const StatusChip({super.key, required this.pane, this.compact = false});

  final Pane pane;

  /// 미니맵처럼 좁은 자리 — 말 없이 아이콘·점만.
  final bool compact;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final st = StatusStyle.of(pane, scheme);
    final filled = st.needsYou;
    final fg = filled ? Colors.white : st.color;
    final bg = filled ? st.color : st.color.withValues(alpha: 0.13);
    final lead = st.live
        ? PulseDot(color: st.color, size: 7)
        : Icon(st.icon, size: 13, color: fg);
    return AnimatedSwitcher(
      duration: const Duration(milliseconds: 260),
      switchInCurve: Curves.easeOutCubic,
      switchOutCurve: Curves.easeIn,
      transitionBuilder: (child, anim) => FadeTransition(
        opacity: anim,
        child: ScaleTransition(
          scale: Tween(begin: 0.92, end: 1.0).animate(anim),
          child: child,
        ),
      ),
      child: Container(
        key: ValueKey('${st.mood}-${st.label}'),
        padding: EdgeInsets.symmetric(horizontal: compact ? 5 : 9, vertical: 4),
        decoration: BoxDecoration(
          color: bg,
          borderRadius: BorderRadius.circular(999),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            lead,
            if (!compact) ...[
              const SizedBox(width: 5),
              Text(
                st.label,
                style: Theme.of(context).textTheme.labelMedium?.copyWith(
                  color: fg,
                  fontWeight: filled ? FontWeight.w700 : FontWeight.w600,
                ),
              ),
            ],
          ],
        ),
      ),
    );
  }
}

/// 처음 그려질 때 살짝 떠오르며 나타난다 — 목록이 한꺼번에 툭 박히지 않게.
/// 다시 그려도 값이 그대로면 움직이지 않는다(5초 폴링마다 흔들리면 안 된다).
class Appear extends StatelessWidget {
  const Appear({super.key, required this.child, this.delayIndex = 0});

  final Widget child;

  /// 목록 순서 — 위부터 차례로 뜬다(한 칸 40ms).
  final int delayIndex;

  @override
  Widget build(BuildContext context) => TweenAnimationBuilder<double>(
    tween: Tween(begin: 0, end: 1),
    duration: Duration(milliseconds: 320 + 40 * delayIndex.clamp(0, 8)),
    curve: Curves.easeOutCubic,
    builder: (context, t, child) => Opacity(
      opacity: t,
      child: Transform.translate(offset: Offset(0, 10 * (1 - t)), child: child),
    ),
    child: child,
  );
}

/// 얼굴 둘레의 상태 테 — 작업 중이면 호 하나가 돌고(진행 중이라는 뜻), 사람을
/// 기다리면 주황 테가 서 있다. 폰에선 도트가 너무 작아 얼굴에 상태를 얹는다
/// (2026-09-07 지시 「그냥 프사만 뜨게 하자, 작업 중 애니메이션」).
class StatusRing extends StatefulWidget {
  const StatusRing({
    super.key,
    required this.style,
    required this.child,
    this.size = 40,
    this.stroke = 2.5,
  });

  final StatusStyle style;
  final Widget child;
  final double size;
  final double stroke;

  @override
  State<StatusRing> createState() => _StatusRingState();
}

class _StatusRingState extends State<StatusRing>
    with SingleTickerProviderStateMixin {
  late final AnimationController _ctl = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 1400),
  );

  @override
  void initState() {
    super.initState();
    _sync();
  }

  @override
  void didUpdateWidget(StatusRing old) {
    super.didUpdateWidget(old);
    if (old.style.mood != widget.style.mood) _sync();
  }

  void _sync() {
    if (widget.style.live) {
      if (!_ctl.isAnimating) _ctl.repeat();
    } else {
      _ctl.stop();
    }
  }

  @override
  void dispose() {
    _ctl.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final st = widget.style;
    final pad = widget.stroke + 2;
    final box = widget.size + pad * 2;
    return SizedBox(
      width: box,
      height: box,
      child: AnimatedBuilder(
        animation: _ctl,
        builder: (context, child) => CustomPaint(
          painter: _RingPainter(
            color: st.color,
            stroke: widget.stroke,
            // 도는 호는 작업 중에만, 꽉 찬 테는 기다림에만. 나머지는 테 없음.
            sweep: st.live ? 0.28 : (st.needsYou ? 1.0 : 0.0),
            turn: _ctl.value,
          ),
          child: child,
        ),
        child: Padding(padding: EdgeInsets.all(pad), child: widget.child),
      ),
    );
  }
}

class _RingPainter extends CustomPainter {
  const _RingPainter({
    required this.color,
    required this.stroke,
    required this.sweep,
    required this.turn,
  });

  final Color color;
  final double stroke;

  /// 테가 차지하는 비율(0~1). 1 이면 온 테, 0 이면 안 그린다.
  final double sweep;

  /// 호의 시작 각(0~1 바퀴).
  final double turn;

  @override
  void paint(Canvas canvas, Size size) {
    if (sweep <= 0) return;
    final rect = Rect.fromLTWH(
      stroke / 2,
      stroke / 2,
      size.width - stroke,
      size.height - stroke,
    );
    final paint = Paint()
      ..color = color
      ..style = PaintingStyle.stroke
      ..strokeWidth = stroke
      ..strokeCap = StrokeCap.round;
    if (sweep >= 1) {
      canvas.drawOval(rect, paint);
      return;
    }
    // 뒤에 옅은 온 테를 깔아 호가 어디를 도는지 보이게.
    canvas.drawOval(
      rect,
      Paint()
        ..color = color.withValues(alpha: 0.18)
        ..style = PaintingStyle.stroke
        ..strokeWidth = stroke,
    );
    const tau = 6.283185307179586;
    canvas.drawArc(rect, turn * tau - tau / 4, sweep * tau, false, paint);
  }

  @override
  bool shouldRepaint(_RingPainter o) =>
      o.turn != turn || o.sweep != sweep || o.color != color;
}

/// 타일 바닥의 진행 막대 — 작업 중이면 빛이 흐르고, 아니면 자리를 안 차지한다
/// (2026-09-07 지시 「프로세스바 애니메이션」). 끝을 모르는 일이라 정해진 길이가
/// 아니라 흐름으로 보인다.
class WorkingBar extends StatelessWidget {
  const WorkingBar({super.key, required this.style});

  final StatusStyle style;

  @override
  Widget build(BuildContext context) => AnimatedSize(
    duration: const Duration(milliseconds: 240),
    curve: Curves.easeOut,
    alignment: Alignment.topCenter,
    child: style.live
        ? SizedBox(
            height: 3,
            child: LinearProgressIndicator(
              minHeight: 3,
              color: style.color,
              backgroundColor: style.color.withValues(alpha: 0.16),
            ),
          )
        : const SizedBox(height: 0, width: double.infinity),
  );
}

/// 우리가 붙인 세션 이름 — 데스크톱 pane 머리의 이름 옆 자리와 같다. 허브 타일과
/// 터미널 화면 머리가 같은 모양으로 단다.
class SessionTag extends StatelessWidget {
  const SessionTag(this.name, {super.key});

  final String name;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Text(
      name,
      style: theme.textTheme.labelSmall?.copyWith(
        color: theme.colorScheme.primary,
        fontWeight: FontWeight.w600,
      ),
      overflow: TextOverflow.ellipsis,
    );
  }
}

/// 데스크톱 pane 머리의 셋째 줄 — 하네스 아이콘 · 모델 · 브랜치 · 컨텍스트% · effort.
/// 컨텍스트가 80% 를 넘으면 그 숫자만 주황으로 도드라진다.
class PaneStatusLine extends StatelessWidget {
  const PaneStatusLine({super.key, required this.pane});

  final Pane pane;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final mute = theme.textTheme.labelSmall?.copyWith(
      color: scheme.onSurfaceVariant,
      fontFamily: 'TermMono',
    );
    final parts = pane.statusParts;
    final pct = pane.contextPct;
    return Padding(
      padding: const EdgeInsets.only(top: 2),
      child: Row(
        children: [
          // 하네스 로고 — 데스크톱 상태줄의 모델 표식과 같은 파랑(2026-09-08 지시
          // 「상태줄에 로고 뜨는 거도」). 회색으로 작게 두니 안 보였다.
          if (pane.harness == 'claude' || pane.harness == 'codex') ...[
            Image.asset(
              'assets/icons/${pane.harness}.png',
              width: 13,
              height: 13,
              color: statusModelColor,
              errorBuilder: (_, _, _) => const SizedBox.shrink(),
            ),
            const SizedBox(width: 5),
          ],
          Flexible(
            child: Text.rich(
              TextSpan(
                children: [
                  for (final (i, p) in parts.indexed) ...[
                    // 앞 공백은 안 끊어지는 것 — 두 줄로 접힐 때 「· xhigh」처럼
                    // 구분점이 줄머리에 오지 않고 앞 줄 꼬리에 남는다.
                    if (i > 0) const TextSpan(text: '\u00A0· '),
                    TextSpan(
                      text: p,
                      style: pct != null && p == '$pct%' && pct >= 80
                          ? TextStyle(
                              color: StatusStyle.attention,
                              fontWeight: FontWeight.w700,
                            )
                          : null,
                    ),
                  ],
                ],
              ),
              style: mute,
              // 폰 폭엔 넷이 한 줄에 안 들어갈 때가 있다 — 컨텍스트%·effort 가 「…」로
              // 사라지느니 한 줄 더 쓴다.
              maxLines: 2,
              overflow: TextOverflow.ellipsis,
            ),
          ),
        ],
      ),
    );
  }
}
