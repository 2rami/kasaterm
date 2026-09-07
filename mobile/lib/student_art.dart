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

/// pane 의 색 → 없으면 데스크톱의 학생별 색 → 그것도 없으면 테마 강조색.
Color studentAccent(BuildContext context, Pane pane, DesignTokens? tokens) {
  final own = DesignTokens.parseHex(pane.color);
  if (own != null) return Color(own);
  final named = tokens?.characterAccents[pane.name];
  if (named != null) return Color(named);
  return Theme.of(context).colorScheme.primary;
}
