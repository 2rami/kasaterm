import 'dart:math' as math;

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:markdown/markdown.dart' as md;

import 'copy_notice.dart';
import 'links.dart';
import 'look.dart';

/// 말풍선 속 마크다운의 글꼴 — 대화 본문과 인라인 이미지 앞뒤의 글이 같은 서체를 쓰도록 한다.
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
    fontFamilyFallback: Look.flowMonoFallback,
    fontSize: size - 2,
    color: base.color,
    backgroundColor: codeBg,
  );
  final cell = base.copyWith(fontSize: size - 2, height: 1.4);
  const cellPad = EdgeInsets.symmetric(
    horizontal: Look.tableCellPadX,
    vertical: Look.tableCellPadY,
  );
  return MarkdownStyleSheet.fromTheme(theme).copyWith(
    p: base,
    // 굵기는 400·600 두 가지 — 마크다운 굵게도 600.
    strong: base.copyWith(fontWeight: FontWeight.w600),
    listBullet: base,
    tableBody: cell,
    tableHead: cell.copyWith(fontWeight: FontWeight.w600),
    tableHeadAlign: TextAlign.left,
    tableColumnWidth: const _CappedIntrinsicWidth(Look.tableCellMax),
    tableScrollbarThumbVisibility: true,
    // 가로 스크롤 막대가 마지막 줄을 덮지 않을 자리.
    tablePadding: const EdgeInsets.only(bottom: Look.tableCellPadY),
    tableBorder: TableBorder(
      horizontalInside: BorderSide(color: scheme.outline, width: Look.border),
      borderRadius: Look.smallCorners,
    ),
    tableCellsPadding: cellPad,
    tableHeadCellsPadding: cellPad,
    tableHeadCellsDecoration: BoxDecoration(color: codeBg),
    tableVerticalAlignment: TableCellVerticalAlignment.top,
    h1: base.copyWith(fontSize: size + 3, fontWeight: FontWeight.w600),
    h2: base.copyWith(fontSize: size + 1, fontWeight: FontWeight.w600),
    h3: base.copyWith(fontSize: size, fontWeight: FontWeight.w600),
    code: mono,
    codeblockPadding: const EdgeInsets.all(10),
    codeblockDecoration: BoxDecoration(
      color: codeBg,
      borderRadius: Look.smallCorners,
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

/// 표 칸 폭 — 말풍선 폭을 칸 수로 나누면 폰에서 한 칸이 두세 글자로 꺾였다. 글 길이만큼 세우되 긴 칸은
/// [cap] 에서 꺾고, 그래도 넘치면 표만 옆으로 민다. 패키지는 [IntrinsicColumnWidth] 일 때만 가로 스크롤로 감싼다.
class _CappedIntrinsicWidth extends IntrinsicColumnWidth {
  const _CappedIntrinsicWidth(this.cap);

  final double cap;

  @override
  double minIntrinsicWidth(Iterable<RenderBox> cells, double containerWidth) =>
      math.min(super.minIntrinsicWidth(cells, containerWidth), cap);

  @override
  double maxIntrinsicWidth(Iterable<RenderBox> cells, double containerWidth) =>
      math.min(super.maxIntrinsicWidth(cells, containerWidth), cap);
}

/// 말풍선 속 마크다운 — 코드 칸은 머리 줄의 단추로 통째 복사, 인라인 코드는 누르면 복사(주소면 열기·복사 시트),
/// 코드 칸 속 주소도 누르면 [onLink]. 일부 고르기는 둘레의 고르기 영역이 그대로 맡는다.
class ChatMarkdownBody extends StatefulWidget {
  const ChatMarkdownBody({
    super.key,
    required this.data,
    required this.style,
    required this.onLink,
    this.imageBuilder,
  });

  final String data;
  final MarkdownStyleSheet style;
  final ValueChanged<String> onLink;
  final MarkdownImageBuilder? imageBuilder;

  @override
  State<ChatMarkdownBody> createState() => _ChatMarkdownBodyState();
}

/// 글 조각의 누름 — 같은 글이면 같은 인식기를 다시 쓰고 화면이 사라질 때 버린다. 마크다운은 글·꼴이 바뀔 때만
/// 다시 짓기 때문에 짓는 차례마다 새로 만들고 지우면 남은 조각이 버린 인식기를 쥔다.
mixin _SpanTaps<T extends StatefulWidget> on State<T> {
  final _taps = <String, TapGestureRecognizer>{};

  TapGestureRecognizer tapFor(String key, VoidCallback onTap) =>
      (_taps[key] ??= TapGestureRecognizer())..onTap = onTap;

  @override
  void dispose() {
    for (final t in _taps.values) {
      t.dispose();
    }
    super.dispose();
  }
}

class _ChatMarkdownBodyState extends State<ChatMarkdownBody> with _SpanTaps {
  late final _builders = <String, MarkdownElementBuilder>{
    'pre': _CodeBlockBuilder(this),
    'code': _InlineCodeBuilder(this),
  };

  /// 코드 칸 속 `code` 는 코드 칸이 통째로 그린다 — 인라인 코드로 따로 세우지 않는다.
  bool _inPre = false;

  void _tapCode(String code) {
    final pieces = urlPieces(code.trim());
    if (pieces.length == 1 && pieces.single.$2 != null) {
      widget.onLink(pieces.single.$2!);
    } else {
      copyAndTell(context, code);
    }
  }

  @override
  Widget build(BuildContext context) => MarkdownBody(
    data: widget.data,
    styleSheet: widget.style,
    softLineBreak: true,
    imageBuilder: widget.imageBuilder,
    builders: _builders,
    onTapLink: (_, href, _) {
      if (href != null) widget.onLink(href);
    },
  );
}

class _CodeBlockBuilder extends MarkdownElementBuilder {
  _CodeBlockBuilder(this.owner);
  final _ChatMarkdownBodyState owner;

  @override
  void visitElementBefore(md.Element element) => owner._inPre = true;

  @override
  Widget? visitElementAfterWithContext(
    BuildContext context,
    md.Element element,
    TextStyle? preferredStyle,
    TextStyle? parentStyle,
  ) {
    owner._inPre = false;
    final code = element.children?.firstOrNull;
    final language = code is md.Element
        ? (code.attributes['class'] ?? '').replaceFirst('language-', '')
        : '';
    return ChatCodeBlock(
      code: element.textContent.replaceFirst(RegExp(r'\n$'), ''),
      language: language,
      style: owner.widget.style,
      onLink: owner.widget.onLink,
    );
  }
}

class _InlineCodeBuilder extends MarkdownElementBuilder {
  _InlineCodeBuilder(this.owner);
  final _ChatMarkdownBodyState owner;

  @override
  Widget? visitElementAfterWithContext(
    BuildContext context,
    md.Element element,
    TextStyle? preferredStyle,
    TextStyle? parentStyle,
  ) {
    // 링크 글 속 코드는 링크가 누름을 가진다 — 감싼 링크는 꾸밈(밑줄)으로 안다.
    if (owner._inPre ||
        (parentStyle?.decoration?.contains(TextDecoration.underline) ?? false)) {
      return null;
    }
    final code = element.textContent;
    // 글 조각(Text)으로 돌려줘야 패키지가 앞뒤 글과 한 문단으로 합친다 — 고르기·줄바꿈이 문단 그대로다.
    return Text.rich(
      TextSpan(
        text: code,
        style: parentStyle?.merge(preferredStyle) ?? preferredStyle,
        recognizer: owner.tapFor(code, () => owner._tapCode(code)),
      ),
    );
  }
}

/// 말풍선 속 코드 칸. 글은 칸 폭에서 꺾는다 — 옆으로 밀면 긴 줄 끝까지 고르기 어렵고 쪽 넘김과 겨뤘다.
/// 머리 줄(말 이름 · 통째 복사)은 고르기에서 뺀다.
class ChatCodeBlock extends StatelessWidget {
  const ChatCodeBlock({
    super.key,
    required this.code,
    required this.language,
    required this.style,
    required this.onLink,
  });

  final String code;
  final String language;
  final MarkdownStyleSheet style;
  final ValueChanged<String> onLink;

  @override
  Widget build(BuildContext context) {
    final dim = Theme.of(context).colorScheme.onSurfaceVariant;
    final mono = style.code ?? const TextStyle(fontFamily: 'TermMono');
    final pad = style.codeblockPadding ?? const EdgeInsets.all(10);
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        SelectionContainer.disabled(
          child: Row(
            children: [
              SizedBox(width: pad.left),
              Expanded(
                child: Text(
                  language,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: TextStyle(
                    fontFamily: 'TermMono',
                    fontFamilyFallback: Look.flowMonoFallback,
                    fontSize: Look.sub,
                    color: dim,
                  ),
                ),
              ),
              IconButton(
                tooltip: '코드 복사',
                onPressed: () => copyAndTell(context, code),
                iconSize: 18,
                color: dim,
                icon: const Icon(Icons.content_copy_outlined),
              ),
            ],
          ),
        ),
        Padding(
          padding: pad.copyWith(top: 0),
          child: LinkedText(
            code,
            style: mono,
            linkStyle: TextStyle(
              color: style.a?.color,
              decoration: TextDecoration.underline,
            ),
            onLink: onLink,
          ),
        ),
      ],
    );
  }
}

/// 주소를 누를 수 있는 글 — 코드 칸·내 말풍선. 누르면 [onLink](열기·복사 시트).
class LinkedText extends StatefulWidget {
  const LinkedText(
    this.text, {
    super.key,
    required this.style,
    required this.onLink,
    this.linkStyle = const TextStyle(decoration: TextDecoration.underline),
  });

  final String text;
  final TextStyle style;
  final TextStyle linkStyle;
  final ValueChanged<String> onLink;

  @override
  State<LinkedText> createState() => _LinkedTextState();
}

class _LinkedTextState extends State<LinkedText> with _SpanTaps {
  @override
  Widget build(BuildContext context) => Text.rich(
    TextSpan(
      style: widget.style,
      children: [
        for (final (text, url) in urlPieces(widget.text))
          url == null
              ? TextSpan(text: text)
              : TextSpan(
                  text: text,
                  style: widget.linkStyle,
                  recognizer: tapFor(url, () => widget.onLink(url)),
                ),
      ],
    ),
  );
}
