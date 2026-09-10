import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/image_attachment.dart';
import 'package:kasaterm_mobile/photo_attachment_button.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/term_session.dart';
import 'package:kasaterm_mobile/screens/terminal.dart';

const slug = 'abcdefghij0123456789abcde';
const target = Pane(
  id: '%7',
  name: '세이아',
  title: '',
  status: 'idle',
  window: 0,
  cwd: '/',
  harness: 'codex',
  machine: '~mini-stable',
);

class AttachmentServer extends Server {
  AttachmentServer() : super(Uri.parse('https://example.com/u/$slug/'));
  final requests = <String>[];
  bool fail = false;
  Completer<void>? pending;
  @override
  Future<void> pasteImage(String pane, List<int> png, {String? machine}) async {
    requests.add('$machine/$pane');
    if (fail) throw const ServerException('첨부 실패');
    await pending?.future;
  }
}

class AttachmentSession extends TermSession {
  AttachmentSession(super.server, super.pane) {
    state = TermState.connected;
  }
  final entered = <String>[];
  @override
  void connect() {}
  @override
  void sendText(String text) {
    entered.add(text);
  }
}

void main() {
  testWidgets('draft mode can send a photo without text, only after upload', (
    tester,
  ) async {
    final server = AttachmentServer()..pending = Completer<void>();
    final session = AttachmentSession(server, target);
    await tester.pumpWidget(
      MaterialApp(
        home: TerminalScreen(
          server: server,
          pane: target,
          session: session,
          pickImage: () async => Uint8List.fromList([1]),
        ),
      ),
    );
    await tester.tap(find.byTooltip('적어 두고 한 번에 보내기'));
    await tester.pump();
    await tester.tap(find.byTooltip('사진 첨부'));
    await tester.pump();
    final send = tester.widget<IconButton>(
      find.byWidgetPredicate(
        (widget) => widget is IconButton && widget.tooltip == '보내기',
      ),
    );
    expect(send.onPressed, isNull);
    expect(session.entered, isEmpty);
    server.pending!.complete();
    await tester.pump();
    await tester.pump();
    await tester.tap(find.byTooltip('보내기'));
    await tester.pump();
    expect(session.entered, ['\r']);
    // A second empty draft must not resend the attachment.
    await tester.tap(find.byTooltip('보내기'));
    await tester.pump();
    expect(session.entered, ['\r']);
    await tester.tap(find.byTooltip('사진 첨부'));
    await tester.pump();
    await tester.pump();
    await tester.ensureVisible(find.byIcon(Icons.keyboard_return));
    await tester.tap(find.byIcon(Icons.keyboard_return));
    await tester.pump();
    await tester.tap(find.byTooltip('보내기'));
    await tester.pump();
    expect(session.entered, [
      '\r',
      '\r',
    ], reason: 'key-bar Enter also consumes the pending photo');
    await tester.pumpWidget(const SizedBox());
  });
  test(
    'image POST preserves authentication path, stable machine, and pane',
    () async {
      final server = Server(
        Uri.parse('https://example.com/u/$slug/'),
        client: MockClient((request) async {
          expect(request.url.path, '/u/$slug/m/~mini-stable/paste-image');
          expect(request.url.queryParameters['surface'], '%7');
          expect(request.headers['content-type'], 'image/png');
          expect(request.bodyBytes, [137, 80, 78, 71]);
          return http.Response('{"ok":true}', 200);
        }),
      );
      await server.pasteImage('%7', [137, 80, 78, 71], machine: '~mini-stable');
    },
  );

  for (final response in [
    http.Response('{"ok":false,"error":"$slug"}', 200),
    http.Response('not json', 200),
    http.Response('', 413),
  ]) {
    test(
      'rejected/malformed upload is never reported as success ${response.body}',
      () async {
        final server = Server(
          Uri.parse('https://example.com/u/$slug/'),
          client: MockClient((_) async => response),
        );
        await expectLater(
          server.pasteImage('%7', [1]),
          throwsA(
            isA<ServerException>().having(
              (e) => e.message,
              'safe error',
              isNot(contains(slug)),
            ),
          ),
        );
      },
    );
  }

  test('empty and oversized images never reach network', () async {
    var calls = 0;
    final server = Server(
      Uri.parse('https://example.com/'),
      client: MockClient((_) async {
        calls++;
        return http.Response('{}', 200);
      }),
    );
    await expectLater(
      server.pasteImage('%7', []),
      throwsA(isA<ServerException>()),
    );
    await expectLater(
      server.pasteImage('%7', Uint8List(Server.maxImageBytes + 1)),
      throwsA(isA<ServerException>()),
    );
    expect(calls, 0);
  });

  Future<void> mount(
    WidgetTester tester,
    AttachmentServer server,
    Future<Uint8List?> Function() pick, {
    Pane pane = target,
    bool enabled = true,
  }) => tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: PhotoAttachmentButton(
          server: server,
          pane: pane,
          enabled: enabled,
          pickImage: pick,
        ),
      ),
    ),
  );

  testWidgets('cancel reads/sends nothing and re-enables the button', (
    tester,
  ) async {
    final server = AttachmentServer();
    await mount(tester, server, () async => null);
    await tester.tap(find.byTooltip('사진 첨부'));
    await tester.pumpAndSettle();
    expect(server.requests, isEmpty);
    expect(
      tester.widget<IconButton>(find.byType(IconButton)).onPressed,
      isNotNull,
    );
  });

  testWidgets('upload has progress and failure can be retried explicitly', (
    tester,
  ) async {
    final server = AttachmentServer()..pending = Completer<void>();
    await mount(tester, server, () async => Uint8List.fromList([1]));
    await tester.tap(find.byTooltip('사진 첨부'));
    await tester.pump();
    expect(find.byTooltip('사진 첨부 중'), findsOneWidget);
    expect(
      tester.widget<IconButton>(find.byType(IconButton)).onPressed,
      isNull,
    );
    expect(server.requests, ['~mini-stable/%7']);
    server.pending!.complete();
    await tester.pumpAndSettle();
    expect(find.textContaining('사진을 입력창에 첨부'), findsOneWidget);
    server.fail = true;
    await tester.tap(find.byTooltip('사진 첨부'));
    await tester.pumpAndSettle();
    await tester.pump(const Duration(seconds: 5));
    await tester.pumpAndSettle();
    expect(find.text('첨부 실패'), findsOneWidget);
    expect(
      tester.widget<IconButton>(find.byType(IconButton)).onPressed,
      isNotNull,
    );
  });

  testWidgets('leaving the pane while picking cancels upload', (tester) async {
    final server = AttachmentServer();
    final picked = Completer<Uint8List?>();
    await mount(tester, server, () => picked.future);
    await tester.tap(find.byTooltip('사진 첨부'));
    await tester.pumpWidget(const SizedBox());
    picked.complete(Uint8List.fromList([1]));
    await tester.pump();
    expect(server.requests, isEmpty);
  });

  testWidgets('connection lost during selection cancels upload', (
    tester,
  ) async {
    final server = AttachmentServer();
    final picked = Completer<Uint8List?>();
    await mount(tester, server, () => picked.future);
    await tester.tap(find.byTooltip('사진 첨부'));
    await mount(tester, server, () => picked.future, enabled: false);
    picked.complete(Uint8List.fromList([1]));
    await tester.pumpAndSettle();
    expect(server.requests, isEmpty);
    expect(find.textContaining('사진을 보내지 않았어요'), findsOneWidget);
  });

  testWidgets('covered but mounted pane cannot upload after picking', (
    tester,
  ) async {
    final server = AttachmentServer();
    final picked = Completer<Uint8List?>();
    await mount(tester, server, () => picked.future);
    final context = tester.element(find.byType(PhotoAttachmentButton));
    await tester.tap(find.byTooltip('사진 첨부'));
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => const Scaffold(body: Text('다른 화면')),
      ),
    );
    await tester.pumpAndSettle();
    picked.complete(Uint8List.fromList([1]));
    await tester.pumpAndSettle();
    expect(server.requests, isEmpty);
  });

  testWidgets('changing target during selection does not redirect image', (
    tester,
  ) async {
    final server = AttachmentServer();
    final picked = Completer<Uint8List?>();
    await mount(tester, server, () => picked.future);
    await tester.tap(find.byTooltip('사진 첨부'));
    await mount(
      tester,
      server,
      () => picked.future,
      pane: const Pane(
        id: '%9',
        name: '다른 학생',
        title: '',
        status: 'idle',
        window: 0,
        cwd: '/',
      ),
    );
    picked.complete(Uint8List.fromList([1]));
    await tester.pumpAndSettle();
    expect(server.requests, isEmpty);
  });

  testWidgets('photo normalization preserves aspect ratio and emits PNG', (
    tester,
  ) async {
    await tester.runAsync(() async {
      final recorder = ui.PictureRecorder();
      final canvas = Canvas(recorder)..drawColor(Colors.red, BlendMode.src);
      // A non-square fixture catches accidental resizing into a square.
      canvas.drawRect(const Rect.fromLTWH(0, 0, 40, 20), Paint());
      final picture = recorder.endRecording();
      final image = await picture.toImage(40, 20);
      final input = await image.toByteData(format: ui.ImageByteFormat.png);
      final png = await normalizeAttachmentImage(input!.buffer.asUint8List());
      expect(png.take(8), [137, 80, 78, 71, 13, 10, 26, 10]);
      final codec = await ui.instantiateImageCodec(png);
      final output = await codec.getNextFrame();
      expect(output.image.width, 40);
      expect(output.image.height, 20);
      output.image.dispose();
      codec.dispose();
      image.dispose();
      picture.dispose();
    });
  });

  testWidgets(
    'unsupported image gives actionable failure, not mislabeled bytes',
    (tester) async {
      await tester.runAsync(() async {
        await expectLater(
          normalizeAttachmentImage(Uint8List.fromList(utf8.encode('not HEIC'))),
          throwsA(
            isA<ServerException>().having(
              (e) => e.message,
              'format guidance',
              contains('JPEG'),
            ),
          ),
        );
      });
    },
  );
}
