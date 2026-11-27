import 'dart:io';
import 'dart:typed_data';

import 'package:crypto/crypto.dart';
import 'package:flutter/material.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/inline_card_visibility.dart';
import 'package:kasaterm_mobile/inline_result.dart';
import 'package:kasaterm_mobile/server.dart';
import 'inline_result_fonts.dart';

String block([String id = '0123456789abcdef']) =>
    '```text\n${List.generate(6, (i) => '[[ki:$id:${i.toString().padLeft(2, '0')}/06]]').join('\n')}\n```\n';

void main() {
  setUpAll(loadInlineResultFonts);
  setUp(() => InlineCardVisibility.showAll = true);
  tearDown(() {
    InlineCardVisibility.showAll = false;
    InlineCardVisibility.flipped.value = const {};
  });
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

  test('the animation mark is read from the still image head only', () {
    final marked = File(
      'test/fixtures/inline-result-marked.png',
    ).readAsBytesSync();
    final gif = File('test/fixtures/inline-result-anim.gif').readAsBytesSync();
    expect(inlineAnimMark(marked), sha256.convert(gif).toString());
    expect(
      inlineAnimMark(File('test/fixtures/inline-result.png').readAsBytesSync()),
      isNull,
    );
    expect(inlineAnimMark(gif), isNull);
    expect(inlineAnimMark(Uint8List.fromList(marked.sublist(0, 40))), isNull);
  });

  for (final served in ['match', 'tampered', 'missing']) {
    testWidgets(
      'a marked image plays its original only when it matches ($served)',
      (tester) async {
        final marked = File(
          'test/fixtures/inline-result-marked.png',
        ).readAsBytesSync();
        final gif = File(
          'test/fixtures/inline-result-anim.gif',
        ).readAsBytesSync();
        final calls = <String>[];
        final server = Server(
          Uri.parse('https://example.test/u/fixture/'),
          client: MockClient((req) async {
            calls.add(req.url.path);
            if (!req.url.path.endsWith('/anim')) {
              return http.Response.bytes(
                marked,
                200,
                headers: {'content-type': 'image/png'},
              );
            }
            return switch (served) {
              'match' => http.Response.bytes(
                gif,
                200,
                headers: {'content-type': 'image/gif'},
              ),
              'tampered' => http.Response.bytes(
                [...gif]..[gif.length - 2] ^= 1,
                200,
              ),
              _ => http.Response('animation unavailable', 404),
            };
          }),
        );
        addTearDown(server.close);
        await tester.pumpWidget(
          MaterialApp(
            home: Scaffold(
              body: InlineResultImage(
                server: server,
                id: '7aeaa8e78c63c877',
                machine: '~source',
              ),
            ),
          ),
        );
        await tester.runAsync(
          () => Future<void>.delayed(const Duration(milliseconds: 50)),
        );
        await tester.pump();
        final image = tester.widget<Image>(find.byType(Image));
        final bytes = (image.image as MemoryImage).bytes;
        expect(calls.map((c) => c.substring(c.indexOf('~source/'))), [
          '~source/term/inline-result',
          '~source/term/inline-result/anim',
        ]);
        expect(bytes, served == 'match' ? gif : marked);
      },
    );
  }

  testWidgets('그림 카드는 기본으로 가려 받지도 않고, 띠를 누르면 받아 보이고 눈으로 다시 가린다', (
    tester,
  ) async {
    InlineCardVisibility.showAll = false;
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
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: InlineResultBody(
            server: server,
            text: block(),
            style: MarkdownStyleSheet(),
            onLink: (_) {},
          ),
        ),
      ),
    );
    await tester.pump();
    expect(find.text('가려짐'), findsOneWidget);
    expect(find.byType(Image), findsNothing);
    expect(calls, isEmpty, reason: '가린 그림은 받지 않는다');
    await tester.tap(find.text('가려짐'));
    await tester.pump();
    await tester.runAsync(
      () => Future<void>.delayed(const Duration(milliseconds: 50)),
    );
    await tester.pump();
    expect(find.byType(Image), findsOneWidget);
    expect(calls, hasLength(1));
    // 눈은 그림 크기에 붙는다 — 풀리기 전엔 그림 크기가 0이라 먼저 풀어 둔다.
    final image = tester.widget<Image>(find.byType(Image));
    await tester.runAsync(
      () => precacheImage(image.image, tester.element(find.byType(Image))),
    );
    await tester.pump();
    await tester.tap(find.byIcon(Icons.visibility_outlined));
    await tester.pump();
    expect(find.text('가려짐'), findsOneWidget);
  });
}
