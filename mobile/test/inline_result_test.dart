import 'dart:io';
import 'package:flutter/material.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/inline_result.dart';
import 'package:kasaterm_mobile/server.dart';
import 'inline_result_fonts.dart';

String block([String id = '0123456789abcdef']) =>
    '```text\n${List.generate(6, (i) => '[[ki:$id:${i.toString().padLeft(2, '0')}/06]]').join('\n')}\n```\n';

void main() {
  setUpAll(loadInlineResultFonts);
  test(
    'display blocks preserve surrounding markdown and validate every row',
    () {
      final parts = inlineResultParts('앞 문장\n\n${block()}\n뒤 문장');
      expect(parts.map((p) => p.id).whereType<String>(), ['0123456789abcdef']);
      expect(parts.first.text, contains('앞 문장'));
      expect(parts.last.text, contains('뒤 문장'));
      for (final source in [
        block().replaceFirst(':02/06', ':03/06'),
        block().replaceFirst('0123456789abcdef:02', '1111111111111111:02'),
        '```text\n[[ki:../../etc/passwd:00/01]]\n```',
      ]) {
        expect(inlineResultParts(source).every((p) => p.id == null), isTrue);
      }
    },
  );

  testWidgets(
    'phone inline image loads from the producing machine without marker text or overflow',
    (tester) async {
      tester.view.physicalSize = const Size(390, 844);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.reset);
      final png = File('test/fixtures/inline-result.png').readAsBytesSync();
      final calls = <Uri>[];
      final server = Server(
        Uri.parse('https://example.test/u/fixture/'),
        client: MockClient((req) async {
          calls.add(req.url);
          return http.Response.bytes(
            png,
            200,
            headers: {'content-type': 'image/png'},
          );
        }),
      );
      addTearDown(server.close);
      final theme = ThemeData(
        colorSchemeSeed: Colors.blue,
        fontFamily: 'Pretendard',
      );
      await tester.pumpWidget(
        MaterialApp(
          theme: theme,
          home: Scaffold(
            body: SafeArea(
              child: SingleChildScrollView(
                child: Padding(
                  padding: const EdgeInsets.all(16),
                  child: InlineResultBody(
                    server: server,
                    machine: '~source',
                    text: 'Before image\n\n${block()}\nAfter image',
                    style: MarkdownStyleSheet(
                      p: const TextStyle(
                        fontFamily: 'Pretendard',
                        fontSize: 15,
                        height: 1.5,
                      ),
                    ),
                    onLink: (_) {},
                  ),
                ),
              ),
            ),
          ),
        ),
      );
      await tester.pumpAndSettle();
      expect(find.byType(Image), findsOneWidget);
      final image = tester.widget<Image>(find.byType(Image));
      await tester.runAsync(
        () => precacheImage(image.image, tester.element(find.byType(Image))),
      );
      await tester.pumpAndSettle();
      expect(find.textContaining('[[ki:', findRichText: true), findsNothing);
      expect(
        find.textContaining('Before image', findRichText: true),
        findsOneWidget,
      );
      expect(
        find.textContaining('After image', findRichText: true),
        findsOneWidget,
      );
      expect(calls.single.path, contains('~source/term/inline-result'));
      expect(calls.single.queryParameters['id'], '0123456789abcdef');
      final rect = tester.getRect(find.byType(Image));
      expect(rect.top, lessThan(300));
      expect(rect.left, greaterThanOrEqualTo(0));
      expect(rect.right, lessThanOrEqualTo(390));
      expect(rect.height, greaterThan(100));
      expect(tester.takeException(), isNull);
      await expectLater(
        find.byType(Scaffold),
        matchesGoldenFile('goldens/inline_result.png'),
      );
    },
  );
}
