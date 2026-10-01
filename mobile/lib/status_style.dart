import 'package:flutter/material.dart';

import 'look.dart';
import 'machine_look.dart';
import 'server.dart';

/// 학생 상태의 갈래 — 색·아이콘·말이 여기 하나로 묶인다. 데스크톱의 셋(내 차례 · 하는 중 · 쉬는 중)에
/// 닫힘 하나.
enum PaneMood { waiting, working, resting, closed }

/// 상태 하나 = 색 하나·아이콘 하나·말 한 마디. 허브 칩·미니맵 점·터미널 상태줄이
/// 전부 이걸 쓴다 — 자리마다 따로 칠하면 「기다림」이 어디선 주황이고 어디선
/// 파랑이 된다(2026-09-07 지시 「뱃지색은 통일해야지 상태는」). 판정·색은 데스크톱과 같다 —
/// 내 차례(승인·질문만) attention · 하는 중 accent · 쉬는 중 흐림.
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

  /// 흰 바탕 위 글자·테두리용. 위 두 색은 점·띠에는 되지만 글자로는 대비가 모자란다(4.5:1 미만).
  static const attentionInk = Color(0xffB25A00);
  static const successInk = Color(0xff2A7D37);

  static StatusStyle of(Pane p, ColorScheme scheme) {
    if (p.closed) {
      return StatusStyle(
        mood: PaneMood.closed,
        label: p.statusWord,
        icon: Icons.inventory_2_rounded,
        color: scheme.outline,
      );
    }
    if (p.needsYou) {
      return StatusStyle(
        mood: PaneMood.waiting,
        label: p.statusWord,
        icon: p.kind == 'permission' ? Icons.pan_tool_alt_rounded : Icons.help_rounded,
        color: attention,
      );
    }
    if (p.isBusy) {
      // 데스크톱 둘째 줄처럼 「낱말 · 사정」 — 낱말이 앞이라 칩이 잘려도 상태는 같은 말로 남는다.
      final detail = p.busyDetail;
      return StatusStyle(
        mood: PaneMood.working,
        label: detail == null ? p.statusWord : '${p.statusWord} · $detail',
        icon: p.isCompacting ? Icons.compress_rounded : Icons.bolt_rounded,
        color: scheme.primary,
      );
    }
    return StatusStyle(
      mood: PaneMood.resting,
      label: p.statusWord,
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
                    color: widget.color.withValues(alpha: 0.35 * (1 - t)),
                    shape: BoxShape.circle,
                  ),
                ),
              // 점 자체가 커졌다 작아진다 — 물결만으론 칩 바탕에 묻혀 멈춘 점으로
              // 보였다(2026-09-08 지적 「커졌다가 작아지는 애니메이션 왜 안 해줘」).
              Container(
                width: widget.live ? s * (0.72 + 0.42 * t) : s,
                height: widget.live ? s * (0.72 + 0.42 * t) : s,
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

class Appear extends StatelessWidget {
  const Appear({super.key, required this.child, this.delayIndex = 0});

  final Widget child;

  /// 목록 순서 — 위부터 차례로 뜬다(한 칸 40ms).
  final int delayIndex;

  @override
  Widget build(BuildContext context) => Look.still(context)
      ? child
      : TweenAnimationBuilder<double>(
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
              borderRadius: BorderRadius.circular(3),
              color: style.color,
              backgroundColor: style.color.withValues(alpha: 0.16),
            ),
          )
        : const SizedBox(height: 0, width: double.infinity),
  );
}

/// 도는 시간 — 데스크톱 배치도 칸·사이드바 목록 줄(`render::elapsed_mark`)과 같은 말·같은 단계.
/// 1분 미만은 없다(잠깐 도는 일에 숫자가 붙으면 정작 오래 도는 것이 묻힌다). 두 시간까지 분으로
/// 버틴다 — 한 시간에서 단위를 갈면 「99분」 다음이 「1시간」이 되어 숫자가 거꾸로 간다.
String? elapsedLabel(int? secs) {
  if (secs == null || secs < 60) return null;
  if (secs < 7200) return '${secs ~/ 60}분';
  return '${secs ~/ 3600}시간';
}

/// 오래 돌수록 눈에 띄게 — 10분 전 흐림, 30분 전 보조, 그 뒤 강조 굵게(데스크톱과 같은 문턱).
TextStyle elapsedStyle(int secs, ThemeData theme) {
  final scheme = theme.colorScheme;
  final base = theme.textTheme.bodySmall ?? const TextStyle(fontSize: Look.sub);
  if (secs < 600) return base.copyWith(color: scheme.onSurfaceVariant.withValues(alpha: 0.7));
  if (secs < 1800) return base.copyWith(color: scheme.onSurfaceVariant);
  return base.copyWith(color: scheme.primary, fontWeight: FontWeight.w600);
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

/// 「이 자리는 저 기계 pane 의 거울」 — 기기색 작은 칩. 이름 옆에 붙는다.
class MirrorTag extends StatelessWidget {
  const MirrorTag(this.machine, {super.key});

  final String machine;

  @override
  Widget build(BuildContext context) => ValueListenableBuilder(
    valueListenable: machineLooks,
    builder: (context, looks, _) {
      final theme = Theme.of(context);
      final color = looks.color(machine);
      return Container(
        padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 1),
        decoration: ShapeDecoration(
          shape: StadiumBorder(side: BorderSide(color: color)),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(looks.icon(machine), size: 11, color: color),
            const SizedBox(width: 3),
            Text(
              machine,
              style: theme.textTheme.labelSmall?.copyWith(
                color: machineInk(color, theme.colorScheme.surface),
                fontWeight: FontWeight.w600,
              ),
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
            ),
          ],
        ),
      );
    },
  );
}
