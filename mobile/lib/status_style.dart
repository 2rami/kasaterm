import 'package:flutter/material.dart';

import 'server.dart';
import 'student_art.dart';

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

  /// 도트가 하는 동작 — 데스크톱 사이드바와 같은 규칙: 일하면 걷고(walk), 승인을
  /// 기다리면 손 흔들고(wave), 답·질문을 기다리거나 쉬면 서서 숨 쉬고(idle), 방금
  /// 끝냈으면 만세(cheer). 닫힌 pane 만 멈춘 얼굴.
  StudentMotion? get motion => switch (mood) {
    PaneMood.working => StudentMotion.walk,
    PaneMood.waiting =>
      icon == Icons.pan_tool_alt_rounded
          ? StudentMotion.wave
          : StudentMotion.idle,
    PaneMood.done => StudentMotion.cheer,
    PaneMood.resting => StudentMotion.idle,
    PaneMood.closed => null,
  };

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
