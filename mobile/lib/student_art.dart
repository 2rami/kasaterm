import 'package:flutter/material.dart';

import 'server.dart';

/// 학생 얼굴. 번들 프로필이 먼저 뜨고, 서버 프사(사용자가 바꾼 그림)가 오면 덮는다 —
/// 터널 너머라 서버 것은 늦고, 끊기면 아예 없다.
class StudentFace extends StatelessWidget {
  const StudentFace({
    super.key,
    required this.slug,
    this.url,
    this.size = 40,
    this.shell = false,
  });

  final String? slug;
  final Uri? url;
  final double size;
  final bool shell;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final blank = Container(
      width: size,
      height: size,
      color: scheme.surfaceContainerHighest,
      child: Icon(
        shell ? Icons.terminal : Icons.person_outline,
        size: size * 0.55,
        color: scheme.onSurfaceVariant,
      ),
    );
    final s = slug;
    final bundled = s == null
        ? blank
        : Image.asset(
            'assets/students/profile/$s.png',
            width: size,
            height: size,
            fit: BoxFit.cover,
            errorBuilder: (_, _, _) => blank,
          );
    final u = url;
    return ClipOval(
      child: u == null
          ? bundled
          : Image.network(
              u.toString(),
              width: size,
              height: size,
              fit: BoxFit.cover,
              frameBuilder: (_, child, frame, _) =>
                  frame == null ? bundled : child,
              errorBuilder: (_, _, _) => bundled,
            ),
    );
  }
}

/// 학생의 idle 애니메이션(데스크톱 사이드바에서 서 있는 그 그림). 없는 학생은 얼굴.
class StudentSprite extends StatelessWidget {
  const StudentSprite({
    super.key,
    required this.slug,
    this.url,
    this.size = 40,
  });

  final String? slug;
  final Uri? url;
  final double size;

  @override
  Widget build(BuildContext context) {
    final s = slug;
    if (s == null) return StudentFace(slug: null, url: url, size: size);
    return Image.asset(
      'assets/students/gif/$s.gif',
      width: size,
      height: size,
      fit: BoxFit.contain,
      filterQuality: FilterQuality.medium,
      errorBuilder: (_, _, _) => StudentFace(slug: s, url: url, size: size),
    );
  }
}

/// 학생 도트 동작 — 데스크톱 사이드바처럼 **일할 땐 걷고, 쉴 땐 서서 숨 쉰다**
/// (2026-09-07 지시 「walk 이런 것도 넣었어?」). 프레임은 번들 도트
/// (`walk` 6장·`idle` 4장, 학생 79명 전부 같은 수). 도트가 없는 학생은 얼굴로.
enum StudentMotion { walk, idle }

class StudentMotionSprite extends StatefulWidget {
  const StudentMotionSprite({
    super.key,
    required this.slug,
    required this.motion,
    this.url,
    this.size = 40,
    this.shell = false,
  });

  final String? slug;

  /// null 이면 움직이지 않는 얼굴 — 사람을 기다리는 학생은 멈춰 서서 눈에 띈다.
  final StudentMotion? motion;
  final Uri? url;
  final double size;
  final bool shell;

  static const _frames = {StudentMotion.walk: 6, StudentMotion.idle: 4};
  static const _step = {
    StudentMotion.walk: Duration(milliseconds: 110),
    StudentMotion.idle: Duration(milliseconds: 240),
  };

  @override
  State<StudentMotionSprite> createState() => _StudentMotionSpriteState();
}

class _StudentMotionSpriteState extends State<StudentMotionSprite>
    with SingleTickerProviderStateMixin {
  late final AnimationController _ctl = AnimationController(
    vsync: this,
    duration: const Duration(seconds: 1),
  );

  @override
  void initState() {
    super.initState();
    _sync();
  }

  @override
  void didUpdateWidget(StudentMotionSprite old) {
    super.didUpdateWidget(old);
    if (old.motion != widget.motion || old.slug != widget.slug) _sync();
  }

  /// 한 바퀴 = 프레임 수 × 한 장 시간. 동작이 바뀌면 처음 장부터 다시.
  void _sync() {
    final m = widget.motion;
    if (m == null || widget.slug == null) {
      _ctl.stop();
      return;
    }
    _ctl.duration =
        StudentMotionSprite._step[m]! * StudentMotionSprite._frames[m]!;
    _ctl.repeat();
  }

  @override
  void dispose() {
    _ctl.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final s = widget.slug;
    final m = widget.motion;
    final face = StudentFace(
      slug: s,
      url: widget.url,
      size: widget.size,
      shell: widget.shell,
    );
    if (s == null || m == null) return face;
    final n = StudentMotionSprite._frames[m]!;
    final dir = m.name;
    return SizedBox(
      width: widget.size,
      height: widget.size,
      child: AnimatedBuilder(
        animation: _ctl,
        builder: (context, _) {
          final i = (_ctl.value * n).floor().clamp(0, n - 1);
          return Image.asset(
            'assets/students/$dir/$s-$i.png',
            width: widget.size,
            height: widget.size,
            fit: BoxFit.contain,
            // 장이 바뀔 때 옛 장을 지우지 않는다 — 프레임 사이에 빈 칸이 번쩍이면 안 된다.
            gaplessPlayback: true,
            filterQuality: FilterQuality.medium,
            errorBuilder: (_, _, _) => face,
          );
        },
      ),
    );
  }
}

/// pane 의 색 → 없으면 데스크톱의 학생별 색 → 그것도 없으면 테마 강조색.
Color studentAccent(BuildContext context, Pane pane, DesignTokens? tokens) {
  final own = DesignTokens.parseHex(pane.color);
  if (own != null) return Color(own);
  final named = tokens?.characterAccents[pane.name];
  if (named != null) return Color(named);
  return Theme.of(context).colorScheme.primary;
}
