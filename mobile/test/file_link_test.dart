import 'dart:convert';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/file_link.dart';
import 'package:kasaterm_mobile/server.dart';

void main() {
  test(
    'file links resolve against the producing task and preserve Unicode',
    () {
      expect(
        resultFilePath('main.rs:42:3', '/work/project'),
        '/work/project/main.rs',
      );
      expect(
        resultFilePath('../out/사진.png', '/work/project'),
        '/work/out/사진.png',
      );
      expect(
        resultFilePath('file:///Users/me/hello%20world.pdf#page=2', '/work'),
        '/Users/me/hello world.pdf',
      );
      expect(resultFilePath('/tmp/a.png:12', ''), '/tmp/a.png');
      expect(
        resultFilePath('/tmp/hello%20world.pdf#page=2', ''),
        '/tmp/hello world.pdf',
      );
      expect(resultFilePath('~/hello%20world.pdf', ''), '~/hello world.pdf');
      expect(resultFilePath('~/사진.png', ''), '~/사진.png');
      expect(
        resultFilePath('hello%23world.pdf', '/work'),
        '/work/hello#world.pdf',
      );
      for (final value in [
        'https://example.test:443',
        'mailto:a@b.test',
        'javascript:alert(1)',
        'file://remote/private',
        '#anchor',
        '?q=1',
        '',
        'relative.txt',
      ]) {
        expect(resultFilePath(value, ''), isNull, reason: value);
      }
    },
  );

  testWidgets(
    'file tap downloads from its source device then opens the native preview',
    (tester) async {
      final calls = <Uri>[];
      final server = Server(
        Uri.parse('https://example.test/u/fixture/'),
        client: MockClient((req) async {
          calls.add(req.url);
          if (req.url.path.endsWith('/term/path')) {
            return http.Response(jsonEncode({'home': '/Users/source'}), 200);
          }
          return http.Response.bytes([1, 2, 3], 200);
        }),
      );
      addTearDown(server.close);
      MethodCall? opened;
      const channel = MethodChannel('kasaterm/file_preview');
      tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(channel, (
        call,
      ) async {
        opened = call;
        return null;
      });
      addTearDown(
        () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
          channel,
          null,
        ),
      );
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: Builder(
              builder: (context) => TextButton(
                onPressed: () => showResultLink(
                  context,
                  server,
                  '~/result.pdf',
                  cwd: '/work',
                  machine: '~producer',
                ),
                child: const Text('result'),
              ),
            ),
          ),
        ),
      );
      await tester.tap(find.text('result'));
      await tester.pumpAndSettle();
      expect(calls, isEmpty);
      final action = find.text('파일 열기');
      expect(action, findsOneWidget);
      await tester.tap(action);
      await tester.pumpAndSettle();
      expect(
        calls.map((u) => u.path),
        everyElement(contains('~producer/term/')),
      );
      expect(calls.first.queryParameters['path'], '/');
      expect(calls.last.queryParameters['path'], '/Users/source/result.pdf');
      expect(opened?.method, 'preview');
      expect(opened?.arguments['name'], 'result.pdf');
      expect(opened?.arguments['bytes'], [1, 2, 3]);
      expect(tester.takeException(), isNull);
    },
  );

  test('failed downloads never return bytes to a preview', () async {
    final server = Server(
      Uri.parse('https://example.test/u/fixture/'),
      client: MockClient((req) async => http.Response('missing', 404)),
    );
    addTearDown(server.close);
    await expectLater(
      server.resultFileBytes('/missing'),
      throwsA(isA<ServerException>()),
    );
  });
}
