import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/chat_markdown.dart';
import 'package:kasaterm_mobile/inline_result.dart';
import 'package:kasaterm_mobile/links.dart';
import 'package:kasaterm_mobile/main.dart' show buildTheme;
import 'package:kasaterm_mobile/server.dart';

import 'mobile_terminal_qa_test.dart' show loadFonts;

/// 말풍선 속 마크다운 한 덩이 — 누른 주소는 [links] 에 남는다.
Future<void> _pump(WidgetTester tester, String text, List<String> links) async {
  tester.view.physicalSize = const Size(390, 844);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  final server = Server(Uri.parse('https://example.test/'));
  addTearDown(server.close);
  final theme = buildTheme(Brightness.light);
  await tester.pumpWidget(
    MaterialApp(
      theme: theme,
      home: Scaffold(
        body: SelectionArea(
          child: Padding(
            padding: const EdgeInsets.all(16),
            child: InlineResultBody(
              server: server,
              text: text,
              style: chatMarkdownStyle(
                theme,
                base: const TextStyle(fontFamily: 'Pretendard', fontSize: 15, height: 1.5),
                codeBg: Colors.black12,
              ),
              onLink: links.add,
            ),
          ),
        ),
      ),
    ),
  );
  await tester.pump();
}

/// 담긴 글 — 사람 클립보드 대신 플랫폼 길을 가로챈다.
List<String> _clipboard(WidgetTester tester) {
  final clips = <String>[];
  tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
    SystemChannels.platform,
    (call) async {
      if (call.method == 'Clipboard.setData') {
        clips.add((call.arguments as Map)['text'] as String);
      }
      return null;
    },
  );
  addTearDown(
    () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      SystemChannels.platform,
      null,
    ),
  );
  return clips;
}

void main() {
  setUpAll(loadFonts);

  test('흐르는 글의 주소 조각은 격자와 같은 규칙으로 자른다', () {
    expect(urlPieces('열어 봐: https://example.com/a?b=1. 그리고 www.kasa.dev'), [
      ('열어 봐: ', null),
      ('https://example.com/a?b=1', 'https://example.com/a?b=1'),
      ('. 그리고 ', null),
      ('www.kasa.dev', 'https://www.kasa.dev'),
    ]);
    expect(urlPieces('(https://x.test/p)'), [
      ('(', null),
      ('https://x.test/p', 'https://x.test/p'),
      (')', null),
    ]);
    expect(urlPieces('주소 없음'), [('주소 없음', null)]);
  });

  test('말풍선 원문은 그림 표시 블록만 걷는다', () {
    final block =
        '```text\n${List.generate(2, (i) => '[[ki:0123456789abcdef:0$i/02]]').join('\n')}\n```\n';
    expect(withoutInlineResults('앞 `코드`\n\n$block\n\n\n뒤'), '앞 `코드`\n\n뒤');
  });

  testWidgets('코드 칸 머리의 단추가 코드를 통째로 담고 알린다', (tester) async {
    final clips = _clipboard(tester);
    final links = <String>[];
    await _pump(tester, '돌려 봐\n\n```sh\ncd ~/work\nflutter test --name "복사"\n```\n', links);
    expect(find.text('sh'), findsOneWidget);
    await tester.tap(find.byTooltip('코드 복사'));
    await tester.pump();
    expect(clips, ['cd ~/work\nflutter test --name "복사"']);
    expect(find.text('복사했어요'), findsOneWidget);
    expect(tester.getSize(find.byTooltip('코드 복사')), const Size(44, 44));
  });

  testWidgets('인라인 코드는 누르면 담고, 주소면 열기·복사로 넘긴다', (tester) async {
    final clips = _clipboard(tester);
    final links = <String>[];
    await _pump(
      tester,
      '토큰 `sk-test-123` 을 넣고 `https://example.com/setup` 을 열어. [`링크 코드`](https://link.test/)',
      links,
    );
    await tester.tapOnText(find.textRange.ofSubstring('sk-test-123'));
    await tester.pump();
    expect(clips, ['sk-test-123']);
    expect(find.text('복사했어요'), findsOneWidget);
    expect(links, isEmpty);

    await tester.tapOnText(find.textRange.ofSubstring('https://example.com/setup'));
    await tester.pump();
    expect(links, ['https://example.com/setup']);
    expect(clips, hasLength(1), reason: '주소는 바로 담지 않고 열기·복사 중 고르게 한다');

    // 링크 글 속 코드는 링크가 누름을 가진다.
    await tester.tapOnText(find.textRange.ofSubstring('링크 코드'));
    await tester.pump();
    expect(links.last, 'https://link.test/');
    expect(clips, hasLength(1));
  });

  testWidgets('코드 칸 속 주소도 누르면 열기·복사로 넘긴다', (tester) async {
    final links = <String>[];
    await _pump(tester, '```\ncurl -fsSL https://example.com/install.sh | sh\n```', links);
    await tester.tapOnText(find.textRange.ofSubstring('https://example.com/install.sh'));
    await tester.pump();
    expect(links, ['https://example.com/install.sh']);
  });

  testWidgets('주소를 누르면 시트에서 열기와 복사를 고른다', (tester) async {
    final clips = _clipboard(tester);
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: Builder(
            builder: (context) => LinkedText(
              '설명서 https://example.com/docs 참고',
              style: const TextStyle(fontSize: 15),
              onLink: (url) => showLinkSheet(context, url),
            ),
          ),
        ),
      ),
    );
    await tester.tapOnText(find.textRange.ofSubstring('https://example.com/docs'));
    await tester.pumpAndSettle();
    expect(find.text('브라우저로 열기'), findsOneWidget);
    await tester.tap(find.text('주소 복사'));
    await tester.pumpAndSettle();
    expect(clips, ['https://example.com/docs']);
    expect(find.text('주소를 복사했어요'), findsOneWidget);
  });
}
