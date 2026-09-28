import 'package:flutter/material.dart';

import 'original_assets.dart';
import 'server.dart';
import 'server_image.dart';

/// 학생 얼굴. 번들 프로필이 먼저 뜨고, 서버 프사(사용자가 바꾼 그림)가 오면 덮는다 —
/// 터널 너머라 서버 것은 늦고, 끊기면 아예 없다.
class StudentFace extends StatelessWidget {
  const StudentFace({
    super.key,
    required this.slug,
    this.url,
    this.size = 40,
    this.shell = false,
    this.server,
  });

  final String? slug;
  final Uri? url;
  final double size;
  final bool shell;
  final Server? server;

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
    final asset = originalCharacterAssets[slug];
    final bundled = asset == null
        ? blank
        : Image.asset(
            asset,
            width: size,
            height: size,
            fit: BoxFit.cover,
            errorBuilder: (_, _, _) => blank,
          );
    final u = url;
    return ClipOval(
      child: u == null
          ? bundled
          : server != null ? ServerImage(server: server!, uri: u,
              width: size, height: size, fit: BoxFit.cover, fallback: bundled)
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

/// 사용자 그림을 우선하고 연결이 늦으면 자체 정지 원화를 보인다.
class StudentSprite extends StatelessWidget {
  const StudentSprite({
    super.key,
    required this.slug,
    this.url,
    this.size = 40,
    this.server,
  });

  final String? slug;
  final Uri? url;
  final double size;
  final Server? server;

  @override
  Widget build(BuildContext context) {
    final asset = originalCharacterAssets[slug];
    if (asset == null) return StudentFace(slug: slug, url: url, size: size, server: server);
    final bundled = Image.asset(
      asset,
      width: size,
      height: size,
      fit: BoxFit.contain,
      filterQuality: FilterQuality.medium,
      errorBuilder: (_, _, _) => StudentFace(slug: null, url: url, size: size, server: server),
    );
    if (url != null && server != null) {
      return ServerImage(server: server!, uri: url!, width: size, height: size,
          fit: BoxFit.contain, fallback: bundled);
    }
    return bundled;
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
