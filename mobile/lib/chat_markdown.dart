import 'package:flutter/material.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';

/// 말풍선 속 마크다운의 글꼴 — 학생 대화와 나쵸 대화가 같이 쓴다.
///
/// [codeBg] 는 말풍선 바탕과 달라야 코드가 보여서 부르는 쪽이 고른다.
MarkdownStyleSheet chatMarkdownStyle(
  ThemeData theme, {
  required TextStyle base,
  required Color codeBg,
}) {
  final scheme = theme.colorScheme;
  final size = base.fontSize ?? 15;
  final mono = TextStyle(
    fontFamily: 'TermMono',
    fontSize: size - 2,
    color: base.color,
    backgroundColor: codeBg,
  );
  return MarkdownStyleSheet.fromTheme(theme).copyWith(
    p: base,
    listBullet: base,
    tableBody: base.copyWith(fontSize: size - 2),
    tableHead: base.copyWith(fontSize: size - 2, fontWeight: FontWeight.w700),
    tableHeadAlign: TextAlign.left,
    h1: base.copyWith(fontSize: size + 3, fontWeight: FontWeight.w700),
    h2: base.copyWith(fontSize: size + 2, fontWeight: FontWeight.w700),
    h3: base.copyWith(fontSize: size + 1, fontWeight: FontWeight.w700),
    code: mono,
    codeblockPadding: const EdgeInsets.all(10),
    codeblockDecoration: BoxDecoration(
      color: codeBg,
      borderRadius: BorderRadius.circular(8),
    ),
    blockquoteDecoration: BoxDecoration(
      border: Border(left: BorderSide(color: scheme.outline, width: 3)),
    ),
    blockquotePadding: const EdgeInsets.fromLTRB(10, 2, 0, 2),
    a: base.copyWith(
      color: scheme.primary,
      decoration: TextDecoration.underline,
    ),
  );
}
