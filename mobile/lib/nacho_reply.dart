/// 나쵸 답 한 덩이를 폰 화면용으로 가른다 — **지우지 않고 자리만 옮긴다.**
///
/// 앱 창구에는 상태 카드를 그리는 길이 아직 없어, 나쵸 서버가 학생을 띄운 보고를 본문에 그대로
/// 싣게 시킨다(나쵸 `tools._intro_rule`). 그래서 답에 「기기: … board --all …」·「실행: POST …」·
/// 폰 링크·사용량 꼬리가 섞여 온다. 여기서는 그 줄들이 **서버가 정한 글꼴**일 때만 알아보고
/// 접힌 상세로 옮긴다. 원문은 [ReplyView.original] 로 늘 남아 상세에서 그대로 볼 수 있다.
library;

import 'package:markdown/markdown.dart' as md;

/// 답 밑 사용량 꼬리 — 나쵸 `agent._footer` 가 만드는 `_12s · 3턴 · 모델 · 추론 x · 0k/350k (0%)_`.
/// 나쵸의 `_FOOTER_LINE` 과 같은 문법만 잡는다.
final _footer = RegExp(r'^_(\d+s\s*·\s*\d+턴\s*·\s.*)_$');

/// 학생을 띄운 보고의 줄머리 — 나쵸 `spawnreport.describe`·`tools` 가 도구 결과에 적는 그 머리.
const _detailHeads = ['기기:', '실행:', '폰에서 볼 링크:'];

/// 줄 앞 목록 기호·인용 기호. 모델이 도구 결과를 옮기며 붙이기도 한다.
final _lead = RegExp(r'^(?:[-*•>]\s+)+');

/// 한 줄 전체가 마크다운 링크 하나인가.
final _lonelyLink = RegExp(r'^\[[^\]\n]+\]\(([^)\s]+)\)$');

class ReplyMeta {
  const ReplyMeta(this.raw, this.parts);

  /// 꼬리 줄 원문(밑줄 기울임 표시 포함).
  final String raw;

  /// `94s` · `9턴` · 모델 · 추론 · 토큰 — 서버가 적은 순서 그대로.
  final List<String> parts;

  /// 말풍선 밑 한 줄 — 걸린 시간·턴·모델만. 나머지는 상세에.
  String get brief => parts.take(3).join(' · ');
  List<String> get rest => parts.length > 3 ? parts.sublist(3) : const [];
}

class ReplyView {
  const ReplyView({
    required this.original,
    required this.body,
    this.meta,
    this.details = const [],
  });

  final String original;

  /// 말풍선에 보일 본문 — 옮긴 줄만 빠지고 나머지 순서·글자는 그대로.
  final String body;
  final ReplyMeta? meta;

  /// 접힌 상세로 옮긴 줄들, 원문 글자 그대로.
  final List<String> details;

  bool get folded => meta != null || details.isNotEmpty;
}

/// `seatPane` 이 주어지면(이 답이 가리키는 작업에 학생 카드가 선다) 그 학생 화면으로 가는 링크
/// 한 줄도 카드의 「작업 열기」와 같은 것이라 상세로 옮긴다.
ReplyView splitReply(String text, {Uri? root, String? seatPane}) {
  final lines = text.split('\n');
  ReplyMeta? meta;
  var last = lines.length - 1;
  while (last >= 0 && lines[last].trim().isEmpty) {
    last--;
  }
  if (last >= 0) {
    final m = _footer.firstMatch(lines[last].trim());
    if (m != null) {
      meta = ReplyMeta(lines[last].trim(), [
        for (final p in m.group(1)!.split(' · '))
          if (p.trim().isNotEmpty) p.trim(),
      ]);
      lines.removeRange(last, lines.length);
    }
  }
  final kept = <String>[];
  final details = <String>[];
  for (final line in lines) {
    if (_isDetail(line) || _isSeatLink(line, root, seatPane)) {
      details.add(line.trim());
    } else {
      kept.add(line);
    }
  }
  final body = kept.join('\n').replaceAll(RegExp(r'\n{3,}'), '\n\n').trim();
  return ReplyView(original: text, body: body, meta: meta, details: details);
}

bool _isDetail(String line) {
  final head = line.trim().replaceFirst(_lead, '').replaceAll('**', '').replaceAll('`', '');
  return _detailHeads.any(head.startsWith);
}

bool _isSeatLink(String line, Uri? root, String? seatPane) {
  if (root == null || seatPane == null) return false;
  final m = _lonelyLink.firstMatch(line.trim().replaceFirst(_lead, ''));
  if (m == null) return false;
  return termLinkOf(m.group(1)!, root)?.pane == seatPane;
}

/// 나쵸 답의 마크다운 — GFM 에서 한국어 글과 카오모지를 먹는 규칙만 뺐다.
///
/// 빼는 것과 그 까닭(빠지는 글자가 없게):
/// - `*기울임*`·`_기울임_`·`__굵게__` — `(*^▽^*)`·`(^_^) … (^_^)` 가 기울임으로 먹힌다. 한글엔 기울임 글꼴도 없다.
/// - `~취소선~` — `3~5개 그리고 7~9개` 사이가 그어진다.
/// - 인라인 HTML — `<pane-id>` 같은 글이 태그로 읽혀 통째로 사라진다.
/// - `\_`·`\~` 의 역슬래시 — 둘 다 여기선 기호가 아니라 `¯\_(ツ)_/¯` 의 팔이다.
///
/// 굵게는 `**…**` 한 줄 안의 짝으로만 본다 — CommonMark 경계 규칙은 `**(선택)**은` 처럼 한글이
/// 바로 붙으면 닫지 못해 별표가 그대로 남는다.
final nachoInlineSyntaxes = <md.InlineSyntax>[
  _PairedStrong(),
  md.TextSyntax(r'\\[_~]'),
  md.TextSyntax(r'[*_]'),
  md.AutolinkExtensionSyntax(),
];

/// 블록은 GFM 그대로(울타리 코드·표·체크 목록). 인라인 HTML·취소선은 [nachoInlineSyntaxes] 설명대로 뺀다.
final nachoMarkdown = md.ExtensionSet(
  const [
    md.FencedCodeBlockSyntax(),
    md.TableSyntax(),
    md.UnorderedListWithCheckboxSyntax(),
    md.OrderedListWithCheckboxSyntax(),
  ],
  const [],
);

class _PairedStrong extends md.InlineSyntax {
  _PairedStrong() : super(r'\*\*([^*\n]+)\*\*', startCharacter: 0x2A);

  @override
  bool onMatch(md.InlineParser parser, Match match) {
    parser.addNode(md.Element('strong', parser.document.parseInline(match[1]!)));
    return true;
  }
}

final _fence = RegExp(r'^ {0,3}(`{3,}|~{3,})');
final _faceQuote = RegExp(r'^( {0,3})(>+)(?=[^\s>])');

/// 줄머리 `>` 뒤에 빈칸이 없으면(`>_<`) 인용이 아니라 얼굴이다 — 글자로 두게 역슬래시를 붙인다.
/// 코드 울타리 안은 건드리지 않는다.
String guardMarkdown(String text) {
  String? open;
  final out = <String>[];
  for (final line in text.split('\n')) {
    final f = _fence.firstMatch(line);
    if (f != null) {
      final mark = f.group(1)!;
      if (open == null) {
        open = mark;
      } else if (mark[0] == open[0] && mark.length >= open.length) {
        open = null;
      }
      out.add(line);
    } else if (open == null) {
      out.add(line.replaceFirstMapped(_faceQuote, (m) => '${m[1]}\\${m[2]}'));
    } else {
      out.add(line);
    }
  }
  return out.join('\n');
}

/// 이 서버의 학생 화면 링크(`<root>[m/<기계>/]term[/grid]?pane=…`)면 그 자리.
class TermLink {
  const TermLink(this.pane, {this.machine});

  final String pane;
  final String? machine;
}

/// pane id 는 `%` 로 시작한다. 나쵸의 폰 링크는 그걸 인코딩하지 않고 싣기도 해서(`pane=%42`)
/// `Uri.parse` 에 맡기면 `%42` 가 `B` 로 풀린다 — 날 쿼리에서 직접 꺼낸다.
TermLink? termLinkOf(String url, Uri root) {
  final base = root.toString();
  if (!url.startsWith(base)) return null;
  var rest = url.substring(base.length);
  String? machine;
  if (rest.startsWith('m/')) {
    final slash = rest.indexOf('/', 2);
    if (slash < 0) return null;
    try {
      machine = Uri.decodeComponent(rest.substring(2, slash));
    } catch (_) {
      return null;
    }
    rest = rest.substring(slash + 1);
  }
  final q = rest.indexOf('?');
  if (q < 0) return null;
  final path = rest.substring(0, q);
  if (path != 'term' && path != 'term/grid') return null;
  for (final pair in rest.substring(q + 1).split('#').first.split('&')) {
    if (!pair.startsWith('pane=')) continue;
    final pane = _rawPane(pair.substring(5));
    return pane == null || pane.isEmpty ? null : TermLink(pane, machine: machine);
  }
  return null;
}

String? _rawPane(String v) {
  if (v.startsWith('%25')) return '%${v.substring(3)}';
  if (RegExp(r'^%\d+$').hasMatch(v)) return v;
  try {
    return Uri.decodeQueryComponent(v);
  } catch (_) {
    return v;
  }
}

/// 밖으로 여는 링크. 인코딩 안 된 pane id(`pane=%42`)를 `%2542` 로 감싸 `Uri` 가 글자로 풀지 않게 한다.
Uri? externalUri(String url) {
  final fixed = url.replaceAllMapped(
    RegExp(r'([?&]pane=)%(?!25)(\d+)'),
    (m) => '${m[1]}%25${m[2]}',
  );
  return Uri.tryParse(fixed);
}
