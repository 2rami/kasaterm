import 'dart:async';
import 'dart:io';
import 'dart:math' show max;
import 'dart:typed_data';

import 'package:crypto/crypto.dart';
import 'package:flutter/material.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/inline_media.dart';
import 'package:kasaterm_mobile/inline_media_bridge_io.dart';
import 'package:kasaterm_mobile/inline_media_store_io.dart';
import 'package:kasaterm_mobile/inline_result.dart';
import 'package:kasaterm_mobile/look.dart';
import 'package:kasaterm_mobile/main.dart' show buildTheme;
import 'package:kasaterm_mobile/screens/controls.dart';
import 'package:kasaterm_mobile/screens/media_player.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:video_player_platform_interface/video_player_platform_interface.dart';

import 'fake_video_player.dart';
import 'fixtures/inline_media_fixture.dart';
import 'inline_result_fonts.dart';
import 'memory_media_store.dart';

const _video = '1111111111111111';
const _audio = '2222222222222222';

final _fixtures = loadMediaFixtures('test/fixtures/media');

http.Response _reply(http.Request req, [Map<String, MediaFixture>? media]) {
  final r = inlineMediaReply(media ?? _fixtures, req.url);
  return http.Response.bytes(
    r.body,
    r.status,
    headers: {'content-type': r.type},
  );
}

InlineMedia _info(String id) =>
    InlineMedia.fromJson(_fixtures[id]!.info, id: id)!;

/// video_player 의 해제는 이벤트 구독 취소를 진짜 시계에서 기다린다 — 가짜 시계 밖에서 한 번 흘려 준다.
Future<void> settleDispose(WidgetTester tester) => tester.runAsync(
  () => Future<void>.delayed(const Duration(milliseconds: 20)),
);

/// mobile-layout 관문 — 첫 카드는 첫 화면 위 300 안, 가로로 넘치는 상자 없음, 누름 44.
void expectPhoneLayout(WidgetTester tester, double width) {
  Rect card(String title) => tester.getRect(
    find.ancestor(of: find.text(title), matching: find.byType(InkWell)).first,
  );
  final video = card('demo clip.mp4'), audio = card('tone.m4a');
  expect(video.top, lessThanOrEqualTo(300), reason: '첫 미디어 카드 시작');
  var right = 0.0;
  for (final r in tester.allRenderObjects.whereType<RenderBox>()) {
    if (!r.attached || !r.hasSize) continue;
    right = max(right, r.localToGlobal(Offset(r.size.width, 0)).dx);
  }
  expect(right, lessThanOrEqualTo(width), reason: '가로 넘침');
  for (final r in [video, audio]) {
    expect(r.left, greaterThanOrEqualTo(0));
    expect(r.height, greaterThanOrEqualTo(Look.tap));
  }
  final dot = tester.getSize(
    find
        .ancestor(
          of: find.descendant(
            of: find.byType(InlineMediaCard).last,
            matching: find.byIcon(Icons.play_arrow_rounded),
          ),
          matching: find.byType(SizedBox),
        )
        .first,
  );
  expect(dot, const Size(Look.tap, Look.tap), reason: '소리 재생 원');
  if (const bool.fromEnvironment('KASA_LAYOUT_REPORT')) {
    debugPrint(
      'layout $width: 첫 카드 y ${video.top.round()} · 영상 카드 ${video.width.round()}x${video.height.round()} · '
      '소리 카드 ${audio.width.round()}x${audio.height.round()} · 재생 원 ${dot.width.round()}x${dot.height.round()} · '
      '가장 오른쪽 ${right.round()} (넘침 ${max(0, right - width).round()})',
    );
  }
}

void main() {
  group('블록 읽기', () {
    test('km 블록은 이미지 블록 곁에서 미디어로 읽히고 줄마다 검사한다', () {
      final image =
          '```text\n${List.generate(6, (i) => '[[ki:0123456789abcdef:0$i/06]]').join('\n')}\n```\n';
      final parts = inlineResultParts(
        '앞\n\n${mediaBlock(_video, 12)}\n가운데\n\n$image\n${mediaBlock(_audio, 4)}뒤',
      );
      expect(
        [
          for (final p in parts)
            if (p.id != null) (p.id, p.media, p.rows),
        ],
        [(_video, true, 12), ('0123456789abcdef', false, 0), (_audio, true, 4)],
      );
      expect(parts.first.text, contains('앞'));
      expect(parts.last.text, contains('뒤'));
    });

    test('들여쓴 줄·CRLF 는 이미지와 같이 받고, 섞이거나 어긋난 줄은 글로 남긴다', () {
      final gutter = mediaBlock(
        _audio,
        4,
      ).replaceAll('[[km', '  [[km').replaceAll('\n', '\r\n');
      expect(inlineResultParts(gutter).single.media, isTrue);
      for (final bad in [
        mediaBlock(
          _audio,
          4,
        ).replaceFirst('[[km:$_audio:01', '[[ki:$_audio:01'),
        mediaBlock(_audio, 4).replaceFirst(':02/04', ':03/04'),
        mediaBlock(_audio, 4).replaceFirst('$_audio:03', '$_video:03'),
        mediaBlock(_audio, 25),
        '```text\n[[km:../../etc/passwd:00/01]]\n```',
      ]) {
        expect(
          inlineResultParts(bad).every((p) => p.id == null),
          isTrue,
          reason: bad,
        );
      }
    });
  });

  group('정보', () {
    test('답을 그대로 믿지 않는다', () {
      final ok = _fixtures[_video]!.info;
      expect(InlineMedia.fromJson(ok, id: _video)?.kind, InlineMediaKind.video);
      for (final bad in <Map<String, Object?>>[
        {...ok, 'id': _audio},
        {...ok, 'version': 2},
        {...ok, 'kind': 'image'},
        {...ok, 'sha256': 'ABC'},
        {...ok, 'sha256': 'A' * 64},
        {...ok, 'size': 0},
        {...ok, 'size': inlineMediaMaxSize + 1},
        {...ok, 'name': null},
      ]) {
        expect(InlineMedia.fromJson(bad, id: _video), isNull, reason: '$bad');
      }
    });

    test('확장자는 등록소가 받는 것만, 아니면 mime 으로', () {
      InlineMedia m(String name, String mime) => InlineMedia(
        id: _video,
        kind: InlineMediaKind.audio,
        name: name,
        mime: mime,
        size: 1,
        sha256: 'a' * 64,
      );
      expect(m('a/b/song.FLAC', 'audio/flac').extension, 'flac');
      expect(m('voice', 'audio/mpeg').extension, 'mp3');
      expect(m('x.exe', 'audio/x-caf').extension, 'caf');
      expect(m('../../etc/x.m4b', 'audio/mp4').title, 'x.m4b');
      expect(_info(_video).caption, '0:03 · 51KB');
      expect(clockLabel(const Duration(seconds: 3725)), '1:02:05');
    });
  });

  group('앱 저장소', () {
    late Directory dir;
    late IoMediaStore store;
    setUp(() {
      dir = Directory.systemTemp.createTempSync('kasa-media-');
      store = IoMediaStore(dir);
    });
    tearDown(() => dir.deleteSync(recursive: true));

    test('전 판이 받아 둔 완성본은 크기가 맞을 때만 다시 쓴다', () async {
      final media = _info(_video);
      final file = File('${dir.path}/${media.sha256}.mp4');
      expect(await store.finished(media), isNull);
      file.writeAsBytesSync(_fixtures[_video]!.bytes.sublist(1));
      expect(await store.finished(media), isNull, reason: '모자란 파일은 안 쓴다');
      file.writeAsBytesSync(_fixtures[_video]!.bytes);
      expect(await store.finished(media), file.path);
    });

    test('받다 만 .part 와 오래 안 연 완성본은 걷고, 정보는 남긴다', () async {
      final media = _info(_video);
      final part = File('${dir.path}/${media.sha256}.part')
        ..writeAsBytesSync([1, 2, 3]);
      final old = File('${dir.path}/${'b' * 64}.mp4')..writeAsBytesSync([1]);
      old.setLastModifiedSync(
        DateTime.now().subtract(const Duration(days: 31)),
      );
      final fresh = File('${dir.path}/${'c' * 64}.m4a')..writeAsBytesSync([1]);
      await store.remember('k', media, null);
      await store.sweep();
      expect(part.existsSync(), isFalse);
      expect(old.existsSync(), isFalse);
      expect(fresh.existsSync(), isTrue);
      expect(await store.recall('k', _video), isNotNull);
    });

    test('정보·포스터를 기억해 원본이 없어도 옛 카드가 선다', () async {
      final media = _info(_video);
      final poster = _fixtures[_video]!.poster!;
      await store.remember('k', media, poster);
      final back = await store.recall('k', _video);
      expect(back!.$1.sha256, media.sha256);
      expect(back.$2, poster);
      expect(await store.recall('k', _audio), isNull, reason: '다른 id 로는 안 꺼낸다');
    });
  });

  group('루프백 창구', () {
    const chunk = 16 * 1024;
    final big = MediaFixture(
      id: '7777777777777777',
      kind: 'video',
      name: 'big.mp4',
      mime: 'video/mp4',
      bytes: Uint8List.fromList(
        List.generate(8 << 20, (i) => (i * 7 + (i >> 9)) & 0xff),
      ),
    );
    final media = {..._fixtures, big.id: big};
    final bridges = <MediaBridge>[];
    tearDown(() async {
      for (final b in bridges) {
        await b.close();
      }
      bridges.clear();
    });

    Future<MediaBridge> open(
      http.Client client,
      String id, {
      int chunk = chunk,
    }) async {
      final server = Server(
        Uri.parse('https://example.test/u/fixture/'),
        client: client,
      );
      addTearDown(server.close);
      final b = await MediaBridge.open(
        server: server,
        id: id,
        machine: '~src',
        media: InlineMedia.fromJson(media[id]!.info, id: id)!,
        chunk: chunk,
      );
      bridges.add(b);
      return b;
    }

    test('구간 꼴을 읽는다 — 파일 밖 단일 구간만 416, 못 읽는 꼴은 무시', () {
      expect(byteRange('bytes=0-1', 100), (0, 1));
      expect(byteRange('bytes=10-', 100), (10, 99));
      expect(byteRange('bytes=90-500', 100), (90, 99));
      expect(byteRange('bytes=-30', 100), (70, 99));
      expect(byteRange('bytes=-300', 100), (0, 99));
      expect(byteRange('BYTES=2-3', 100), (2, 3), reason: '단위는 대소문자를 안 가린다');
      expect(byteRange('Bytes=-1', 100), (99, 99));
      for (final outside in ['bytes=100-', 'bytes=-0']) {
        expect(byteRange(outside, 100), unsatisfiable, reason: outside);
      }
      for (final ignored in [
        'bytes=5-1',
        'bytes=0-1,4-5',
        'items=0-1',
        'bytes=a-b',
        'bytes=99999999999999999999-',
      ]) {
        expect(byteRange(ignored, 100), isNull, reason: ignored);
      }
    });

    test('요청한 구간을 덮는 조각만 원본에 묻고 206 으로 넘긴다', () async {
      final asked = <(int, int)>[];
      final b = await open(
        MockClient((req) async {
          expect(req.url.path, contains('/m/~src/term/inline-media/'));
          if (req.url.path.endsWith('/file')) {
            asked.add((
              int.parse(req.url.queryParameters['offset']!),
              int.parse(req.url.queryParameters['len']!),
            ));
          }
          return _reply(req);
        }),
        _video,
      );
      final bytes = _fixtures[_video]!.bytes;
      final size = bytes.length;
      expect(b.uri.host, '127.0.0.1');
      expect(b.uri.path, matches(RegExp(r'^/[0-9a-f]{32}/media\.mp4$')));

      var r = await _get(b.uri, range: 'bytes=0-1');
      expect(r.status, 206);
      expect(r.body, bytes.sublist(0, 2));
      expect(r.headers['content-range'], 'bytes 0-1/$size');
      expect(r.headers['content-type'], 'video/mp4');
      expect(r.headers['accept-ranges'], 'bytes');
      expect(r.headers['cache-control'], 'no-store');
      expect(asked, [(0, chunk)]);

      r = await _get(b.uri, range: 'bytes=40000-40099');
      expect(r.body, bytes.sublist(40000, 40100));
      expect(asked.last, (32768, chunk), reason: '그 구간의 조각 하나만');

      r = await _get(b.uri, range: 'bytes=-100');
      expect(r.body, bytes.sublist(size - 100));
      expect(
        r.headers['content-range'],
        'bytes ${size - 100}-${size - 1}/$size',
      );
      expect(asked.last, (49152, size - 49152));

      asked.clear();
      r = await _get(b.uri);
      expect(r.status, 200);
      expect(sha256.convert(r.body).toString(), _info(_video).sha256);
      expect(asked, [(16384, chunk)], reason: '받아 둔 조각은 다시 묻지 않는다');
      expect(b.received, size);
    });

    test(
      '파일 밖 구간·주소·Origin·방식은 원본에 묻지 않고 거절하고, HEAD·못 읽는 Range 는 무시한다',
      () async {
        final asked = <Uri>[];
        final b = await open(
          MockClient((req) async {
            asked.add(req.url);
            return _reply(req);
          }),
          _audio,
        );
        final size = _fixtures[_audio]!.bytes.length;
        final outside = await _get(b.uri, range: 'bytes=$size-');
        expect(outside.status, 416);
        expect(outside.headers['content-range'], 'bytes */$size');
        expect((await _get(b.uri.replace(path: '/0/media.m4a'))).status, 404);
        expect(
          (await _get(b.uri, headers: {'origin': 'https://evil.test'})).status,
          403,
        );
        expect((await _get(b.uri, method: 'POST')).status, 405);
        for (final range in [null, 'bytes=0-1', 'bytes=$size-']) {
          final head = await _get(b.uri, method: 'HEAD', range: range);
          expect(head.status, 200, reason: 'HEAD 는 Range 를 무시한다 ($range)');
          expect(head.headers['content-length'], '$size');
          expect(head.headers['content-type'], 'audio/mp4');
          expect(head.headers['content-range'], isNull);
          expect(head.body, isEmpty);
        }
        expect(asked, isEmpty);
        final upper = await _get(b.uri, range: 'BYTES=0-1');
        expect(upper.status, 206);
        expect(upper.body, _fixtures[_audio]!.bytes.sublist(0, 2));
        // RFC 9110 §14.2 — 못 읽는 Range 는 없던 것처럼 전체를 준다.
        for (final ignored in ['bytes=5-1', 'bytes=0-1,4-5', 'x']) {
          final r = await _get(b.uri, range: ignored);
          expect(r.status, 200, reason: ignored);
          expect(r.body, _fixtures[_audio]!.bytes, reason: ignored);
          expect(r.headers['content-range'], isNull, reason: ignored);
        }
      },
    );

    test('원본이 거절하거나 끊기면 상태로 돌려주고 까닭을 남긴다', () async {
      var status = 404;
      final b = await open(
        MockClient(
          (req) async => req.url.path.endsWith('/file')
              ? http.Response('no', status)
              : _reply(req),
        ),
        _video,
      );
      var r = await _get(b.uri, range: 'bytes=0-1');
      expect(r.status, 404);
      expect(b.error, '원본 기기에 이 미디어가 없어요');
      status = 503;
      r = await _get(b.uri, range: 'bytes=0-1');
      expect(r.status, 502);
      expect(b.error, '미디어를 받지 못했어요 (HTTP 503)');
    });

    test('받는 쪽이 끊기면 그 요청이 쥔 조각만 끊고, 같은 조각을 기다리는 요청은 잇는다', () async {
      final gate = _Gate(media);
      final b = await open(gate, _video);
      final a = await _rawGet(b.uri, 'bytes=0-');
      final keep = _get(b.uri, range: 'bytes=0-1');
      await gate.waitFor(0);
      // 둘째 요청이 창구에 닿아 같은 조각을 쥘 틈.
      await Future<void>.delayed(const Duration(milliseconds: 100));
      a.destroy();
      await Future<void>.delayed(const Duration(milliseconds: 100));
      expect(gate.aborted, isEmpty, reason: '다른 요청이 같은 조각을 기다린다');
      gate.release(0);
      expect((await keep).body, _fixtures[_video]!.bytes.sublist(0, 2));

      final c = await _rawGet(b.uri, 'bytes=40000-');
      await gate.waitFor(32768);
      c.destroy();
      await gate.abortedWait(32768);
      expect(gate.aborted, [32768]);
      expect(b.error, isNull, reason: '버린 요청은 실패가 아니다');
    });

    test('탐색으로 버린 긴 요청은 끊긴 뒤 원본에 더 묻지 않는다', () async {
      final asked = <int>[];
      final b = await open(
        MockClient((req) async {
          if (req.url.path.endsWith('/file')) {
            asked.add(int.parse(req.url.queryParameters['offset']!));
          }
          return _reply(req, media);
        }),
        big.id,
        chunk: 64 * 1024,
      );
      final s = await _rawGet(b.uri, 'bytes=0-');
      var got = 0;
      final enough = Completer<void>();
      s.listen((d) {
        got += d.length;
        if (got > 256 * 1024 && !enough.isCompleted) enough.complete();
      });
      await enough.future;
      s.destroy();
      await Future<void>.delayed(const Duration(milliseconds: 300));
      final after = asked.length;
      await Future<void>.delayed(const Duration(milliseconds: 300));
      expect(asked.length, after, reason: '끊긴 뒤로는 묻지 않는다');
      expect(
        b.received,
        lessThan(big.bytes.length ~/ 4),
        reason: '8MiB 중 받은 것 ${b.received}',
      );
    });

    test('살아 있나는 제 주소로 묻는다 — 거둬진 듣기 소켓은 창구가 몰라도 죽었다고 답한다', () async {
      final asked = <Uri>[];
      final b = await open(
        MockClient((req) async {
          asked.add(req.url);
          return _reply(req);
        }),
        _video,
      );
      expect(await b.alive(), isTrue);
      await b.debugReclaim();
      expect(await b.alive(), isFalse);
      await b.close();
      expect(await b.alive(), isFalse);
      expect(asked, isEmpty, reason: '묻기는 원본에 닿지 않는다');
    });

    test('닫으면 받던 요청을 모두 끊고 연결도 닫는다', () async {
      final gate = _Gate(media);
      final b = await open(gate, _video);
      final a = await _rawGet(b.uri, 'bytes=0-');
      final ended = Completer<void>();
      a.listen(
        (_) {},
        onDone: ended.complete,
        onError: (_) => ended.complete(),
      );
      await gate.waitFor(0);
      await b.close();
      await gate.abortedWait(0);
      await ended.future.timeout(const Duration(seconds: 2));
      await expectLater(
        Socket.connect('127.0.0.1', b.uri.port),
        throwsA(isA<SocketException>()),
      );
      expect(gate.asked, [0], reason: '닫은 뒤 더 묻지 않는다');
    });
  });

  group('끊기', () {
    testWidgets('조각이 시간 상한을 넘기면 요청을 실제로 끊고 이유를 남긴다', (tester) async {
      final hang = _Hang();
      final server = Server(
        Uri.parse('https://example.test/u/fixture/'),
        client: hang,
      );
      addTearDown(server.close);
      Object? error;
      unawaited(
        server
            .inlineMediaChunk(_video, offset: 0, length: 10)
            .then<void>((_) {}, onError: (Object e) => error = e),
      );
      await tester.pump(const Duration(seconds: 59));
      expect(error, isNull);
      await tester.pump(const Duration(seconds: 2));
      expect(hang.aborted, 1);
      expect((error as ServerException?)?.message, '원본 기기가 답하지 않아요');
    });

    testWidgets('부른 쪽이 끊으면 관문으로 다시 보내지 않고 끊김을 그대로 돌려준다', (tester) async {
      final hang = _Hang();
      final server = Server(
        Uri.parse('https://example.test/u/fixture/'),
        client: hang,
      );
      addTearDown(server.close);
      final stop = Completer<void>();
      Object? error;
      unawaited(
        server
            .inlineMediaChunk(_video, offset: 0, length: 10, abort: stop.future)
            .then<void>((_) {}, onError: (Object e) => error = e),
      );
      await tester.pump();
      stop.complete();
      await tester.pump();
      expect(error, isA<http.RequestAbortedException>());
      expect(hang.sent, 1, reason: '끊긴 GET 을 다시 보내지 않는다');
    });
  });

  group('카드와 플레이어', () {
    late FakeVideoPlayer platform;
    late MemoryMediaStore store;
    setUpAll(loadInlineResultFonts);
    late List<(Server, String, String?, String)> opened;
    late List<_FakeLink> links;
    Completer<void>? hold;
    String? failure;
    setUp(() {
      platform = FakeVideoPlayer();
      VideoPlayerPlatform.instance = platform;
      store = MemoryMediaStore();
      opened = [];
      links = [];
      hold = null;
      failure = null;
      InlineMediaHub.debugReset(
        InlineMediaHub(
          store: () async => store,
          chunk: 16 * 1024,
          bridge:
              ({
                required server,
                required id,
                required machine,
                required media,
                required chunk,
              }) async {
                opened.add((server, id, machine, media.sha256));
                await hold?.future;
                final link = _FakeLink(id, media.extension, failure);
                links.add(link);
                return link;
              },
        ),
      );
    });

    Future<Server> pumpChat(
      WidgetTester tester,
      String text, {
      Future<http.Response> Function(http.Request)? handler,
      List<Uri>? calls,
      Uri? root,
      ValueNotifier<String>? live,
      double width = 390,
    }) async {
      tester.view.physicalSize = Size(width * 3, 844 * 3);
      tester.view.devicePixelRatio = 3;
      addTearDown(tester.view.reset);
      final server = Server(
        root ?? Uri.parse('https://example.test/u/fixture/'),
        client: MockClient((req) async {
          calls?.add(req.url);
          return (handler ?? (r) async => _reply(r))(req);
        }),
      );
      addTearDown(server.close);
      final theme = buildTheme(Brightness.light);
      await tester.pumpWidget(
        MaterialApp(
          theme: theme,
          debugShowCheckedModeBanner: false,
          home: Scaffold(
            body: SafeArea(
              child: SingleChildScrollView(
                padding: const EdgeInsets.all(Look.pagePad),
                child: Align(
                  alignment: Alignment.centerLeft,
                  child: SpeechBubble(
                    mine: false,
                    maxWidth: width - Look.pagePad * 2 - Look.bubbleFar,
                    child: ValueListenableBuilder<String>(
                      valueListenable: live ?? ValueNotifier(text),
                      builder: (context, text, _) => InlineResultBody(
                        server: server,
                        machine: '~src',
                        text: text,
                        style: MarkdownStyleSheet.fromTheme(theme),
                        onLink: (_) {},
                      ),
                    ),
                  ),
                ),
              ),
            ),
          ),
        ),
      );
      await tester.pumpAndSettle();
      return server;
    }

    testWidgets('블록은 정보·포스터만 받고 카드는 폭을 넘지 않는다', (tester) async {
      final calls = <Uri>[];
      await pumpChat(
        tester,
        '만들었어요.\n\n${mediaBlock(_video, 12)}\n소리도요.\n\n${mediaBlock(_audio, 4)}',
        calls: calls,
      );
      final poster = find.byWidgetPredicate(
        (w) => w is Image && w.image is ResizeImage,
      );
      await tester.runAsync(
        () => precacheImage(
          tester.widget<Image>(poster).image,
          tester.element(poster),
        ),
      );
      await tester.pumpAndSettle();
      expect(tester.takeException(), isNull);
      expect(find.textContaining('[[km:', findRichText: true), findsNothing);
      expect(find.text('demo clip.mp4'), findsOneWidget);
      expect(find.text('tone.m4a'), findsOneWidget);
      expect(find.text('0:03 · 51KB'), findsOneWidget);
      expect(
        calls.map((u) => u.path.split('/term/').last).toSet(),
        {'inline-media', 'inline-media/poster'},
        reason: '누르기 전에는 파일을 받지 않는다',
      );
      expect(calls.every((u) => u.path.contains('/m/~src/')), isTrue);
      for (final card in [find.text('demo clip.mp4'), find.text('tone.m4a')]) {
        final ink = find
            .ancestor(of: card, matching: find.byType(InkWell))
            .first;
        final r = tester.getRect(ink);
        expect(r.left, greaterThanOrEqualTo(0));
        expect(r.right, lessThanOrEqualTo(390));
        expect(r.height, greaterThanOrEqualTo(Look.tap));
      }
      expectPhoneLayout(tester, 390);
      final box = tester.getRect(find.byType(AspectRatio).first);
      expect(box.height, lessThanOrEqualTo(Look.mediaMaxH));
      expect(box.width / box.height, closeTo(320 / 180, 0.02));
      await expectLater(
        find.byType(Scaffold),
        matchesGoldenFile('goldens/inline_media_cards.png'),
      );
    });

    for (final width in [320.0, 430.0]) {
      testWidgets('폭 $width 에서도 카드는 넘치지 않고 누름 44 를 지킨다', (tester) async {
        await pumpChat(
          tester,
          '만들었어요.\n\n${mediaBlock(_video, 12)}\n소리도요.\n\n${mediaBlock(_audio, 4)}',
          width: width,
        );
        expect(tester.takeException(), isNull);
        expectPhoneLayout(tester, width);
      });
    }

    testWidgets('누르면 곧바로 플레이어가 서고 창구로 재생하며, 닫으면 창구를 닫는다', (tester) async {
      final calls = <Uri>[];
      await pumpChat(tester, mediaBlock(_video, 12), calls: calls);
      final wait = hold = Completer<void>();
      await tester.tap(find.text('demo clip.mp4'));
      await tester.pump();
      await tester.pump();
      expect(
        find.byType(MediaPlayerScreen),
        findsOneWidget,
        reason: '창구가 서기 전에 플레이어가 먼저 선다',
      );
      expect(find.byType(CircularProgressIndicator), findsOneWidget);
      expect(
        tester.widget<IconButton>(find.byType(IconButton).last).onPressed,
        isNull,
        reason: '서기 전엔 재생 단추가 잠긴다',
      );
      expect(opened.single.$2, _video);
      expect(opened.single.$3, '~src');
      expect(opened.single.$4, _info(_video).sha256);
      wait.complete();
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      // 열 때 video_player 가 멈춤 상태를 한 번 맞추고, 카드를 누른 뜻대로 튼다.
      expect(platform.calls, [
        'open http://127.0.0.1:9/$_video/media.mp4',
        'pause',
        'play',
      ]);
      expect(
        calls.where((u) => u.path.endsWith('/file')),
        isEmpty,
        reason: '카드는 파일을 받지 않는다 — 구간은 플레이어가 창구로 묻는다',
      );
      expect(tester.takeException(), isNull);
      await expectLater(
        find.byType(MediaPlayerScreen),
        matchesGoldenFile('goldens/inline_media_player.png'),
      );

      await tester.tap(find.byTooltip('일시정지'));
      await tester.pump();
      expect(platform.calls.last, 'pause');

      final slider = tester.getRect(find.byType(Slider));
      await tester.tapAt(
        Offset(slider.left + slider.width * 0.75, slider.center.dy),
      );
      await tester.pump();
      expect(platform.calls.last, startsWith('seek '));
      final ms = int.parse(platform.calls.last.split(' ').last);
      expect(ms, inInclusiveRange(1900, 2500));
      expect(find.byTooltip('재생'), findsOneWidget, reason: '멈춘 채 찾아간다');

      platform.complete();
      await tester.pump();
      await tester.pump();
      expect(find.byTooltip('처음부터'), findsOneWidget);
      expect(find.text('0:03'), findsNWidgets(2), reason: '끝나면 지난 시간도 길이와 같다');
      await tester.tap(find.byTooltip('처음부터'));
      await tester.pump();
      expect(platform.calls.sublist(platform.calls.length - 2), [
        'seek 0',
        'play',
      ]);

      await tester.tap(find.byTooltip('닫기'));
      await tester.pumpAndSettle();
      expect(find.byType(MediaPlayerScreen), findsNothing);
      await settleDispose(tester);
      expect(platform.calls.last, 'dispose');
      expect(platform.live, 0);
      expect(links.single.closed, isTrue, reason: '닫으면 받던 요청이 끊긴다');
      expect(find.text('0:03 · 51KB'), findsOneWidget);

      // 전 판이 받아 둔 완성본이 있으면 창구 없이 그 파일로.
      final sha = _info(_video).sha256;
      store.done[sha] = _fixtures[_video]!.bytes;
      platform.calls.clear();
      await tester.tap(find.text('demo clip.mp4'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      expect(platform.calls.first, 'open file:///cache/$sha.mp4');
      expect(opened, hasLength(1), reason: '창구를 열지 않는다');
      await tester.tap(find.byTooltip('닫기'));
      await tester.pumpAndSettle();
      await settleDispose(tester);
    });

    testWidgets('창구가 서기 전에 닫으면 늦게 선 창구도 닫는다', (tester) async {
      await pumpChat(tester, mediaBlock(_audio, 4));
      final wait = hold = Completer<void>();
      await tester.tap(find.text('tone.m4a'));
      await tester.pump();
      await tester.pump();
      await tester.tap(find.byTooltip('닫기'));
      await tester.pumpAndSettle();
      wait.complete();
      await tester.pump();
      expect(links.single.closed, isTrue);
      expect(platform.calls, isEmpty, reason: '플레이어를 만들지 않는다');
    });

    testWidgets('원본에서 못 받으면 그 까닭과 다시 시도 — 다시 시도는 창구를 새로 연다', (tester) async {
      await pumpChat(tester, mediaBlock(_video, 12));
      platform.failInit = true;
      failure = '원본 기기에 닿지 못했어요';
      await tester.tap(find.text('demo clip.mp4'));
      await tester.pumpAndSettle();
      expect(find.text('원본 기기에 닿지 못했어요'), findsOneWidget);
      expect(find.text('원본 기기가 켜져 있는지 보고 다시 시도해 주세요'), findsOneWidget);
      expect(find.text('미리보기로 열기'), findsNothing, reason: '이 폰에 파일이 없다');
      expect(tester.takeException(), isNull);

      platform.failInit = false;
      failure = null;
      await tester.tap(find.text('다시 시도'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 100));
      expect(find.byTooltip('일시정지'), findsOneWidget);
      expect(links, hasLength(2));
      expect(links.first.closed, isTrue, reason: '옛 창구는 닫는다');
      expect(opened.map((o) => (o.$2, o.$3)).toSet(), {(_video, '~src')});
      await tester.tap(find.byTooltip('닫기'));
      await tester.pumpAndSettle();
      await settleDispose(tester);
      expect(links.last.closed, isTrue);
      expect(platform.live, 0);
    });

    testWidgets('정보를 기다리는 사이 카드가 B 로 바뀌면 늦게 온 A 는 버린다', (tester) async {
      final infoA = Completer<void>(), infoB = Completer<void>();
      final live = ValueNotifier(mediaBlock(_video, 12));
      addTearDown(live.dispose);
      final server = Server(
        Uri.parse('https://example.test/u/fixture/'),
        client: MockClient((req) async {
          final id = req.url.queryParameters['id'];
          if (id == _video) {
            await infoA.future;
          } else if (id == _audio) {
            await infoB.future;
          }
          return _reply(req);
        }),
      );
      addTearDown(server.close);
      tester.view.physicalSize = const Size(390 * 3, 844 * 3);
      tester.view.devicePixelRatio = 3;
      addTearDown(tester.view.reset);
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: ValueListenableBuilder<String>(
              valueListenable: live,
              builder: (context, text, _) => InlineResultBody(
                server: server,
                machine: '~src',
                text: text,
                style: MarkdownStyleSheet(),
                onLink: (_) {},
              ),
            ),
          ),
        ),
      );
      await tester.pump();
      final card = tester.state(find.byType(InlineMediaCard));
      live.value = mediaBlock(_audio, 4);
      await tester.pump();
      expect(
        tester.state(find.byType(InlineMediaCard)),
        same(card),
        reason: '같은 State 가 didUpdateWidget 을 받는다',
      );
      infoA.complete();
      await tester.pump();
      await tester.pump();
      expect(find.text('demo clip.mp4'), findsNothing);
      infoB.complete();
      await tester.pumpAndSettle();
      expect(find.text('tone.m4a'), findsOneWidget);
    });

    group('앱이 뒤로 갔다 오면', () {
      Future<void> roundTrip(
        WidgetTester tester, {
        void Function()? meanwhile,
      }) async {
        for (final s in [
          AppLifecycleState.inactive,
          AppLifecycleState.hidden,
          AppLifecycleState.paused,
        ]) {
          tester.binding.handleAppLifecycleStateChanged(s);
        }
        await tester.pump();
        meanwhile?.call();
        for (final s in [
          AppLifecycleState.hidden,
          AppLifecycleState.inactive,
          AppLifecycleState.resumed,
        ]) {
          tester.binding.handleAppLifecycleStateChanged(s);
        }
        await tester.pump();
        await tester.pump(const Duration(milliseconds: 400));
      }

      /// 새 플레이어를 연 뒤의 호출 — 옛 플레이어 해제는 뺀다.
      List<String> afterReopen() {
        final open = 'open http://127.0.0.1:9/$_video/media.mp4';
        final i = platform.calls.lastIndexOf(open);
        expect(
          platform.calls.where((c) => c == open),
          hasLength(2),
          reason: '같은 원본으로 한 번 더 연다',
        );
        return platform.calls
            .sublist(i + 1)
            .where((c) => c != 'dispose')
            .toList();
      }

      Future<void> openVideo(WidgetTester tester) async {
        await pumpChat(tester, mediaBlock(_video, 12));
        await tester.tap(find.text('demo clip.mp4'));
        await tester.pump();
        await tester.pump(const Duration(milliseconds: 400));
        expect(find.byTooltip('일시정지'), findsOneWidget);
      }

      Future<void> closePlayer(WidgetTester tester) async {
        await tester.tap(find.byTooltip('닫기'));
        await tester.pumpAndSettle();
        await settleDispose(tester);
        expect(platform.live, 0);
        expect(links.every((l) => l.closed), isTrue);
      }

      testWidgets('창구가 살아 있으면 손대지 않는다 — video_player 가 스스로 잇는다', (
        tester,
      ) async {
        await openVideo(tester);
        platform.calls.clear();
        await roundTrip(tester);
        expect(opened, hasLength(1));
        expect(platform.calls, ['pause', 'play']);
        expect(find.byTooltip('일시정지'), findsOneWidget);
        await closePlayer(tester);
        await roundTrip(tester);
        expect(opened, hasLength(1), reason: '닫은 플레이어는 복귀를 듣지 않는다');
      });

      testWidgets('거둬졌으면 같은 원본으로 새 창구·플레이어를 세워 그 자리에서 이어 튼다', (tester) async {
        await openVideo(tester);
        platform.moveTo(const Duration(milliseconds: 1500));
        await tester.pump(const Duration(milliseconds: 600));
        await roundTrip(tester, meanwhile: () => links.single.dead = true);
        expect(opened, hasLength(2));
        expect(
          opened.map((o) => (o.$1, o.$2, o.$3, o.$4)).toSet(),
          hasLength(1),
        );
        expect(afterReopen(), ['pause', 'seek 1500', 'play']);
        expect(find.byTooltip('일시정지'), findsOneWidget);
        await settleDispose(tester);
        expect(links.first.closed, isTrue, reason: '거둬진 창구는 닫는다');
        expect(platform.live, 1, reason: '옛 플레이어는 버린다');
        await closePlayer(tester);
      });

      testWidgets('옛 플레이어가 그사이 오류로 자리를 잃어도 떠날 때의 자리에서 잇는다', (tester) async {
        await openVideo(tester);
        platform.moveTo(const Duration(milliseconds: 1500));
        await tester.pump(const Duration(milliseconds: 600));
        await roundTrip(
          tester,
          meanwhile: () {
            links.single.dead = true;
            platform.fail();
          },
        );
        expect(afterReopen(), ['pause', 'seek 1500', 'play']);
        expect(find.byTooltip('일시정지'), findsOneWidget);
        await closePlayer(tester);
      });

      testWidgets('멈춰 둔 것은 멈춘 채 그 자리에 다시 세운다', (tester) async {
        await openVideo(tester);
        await tester.tap(find.byTooltip('일시정지'));
        await tester.pump();
        final slider = tester.getRect(find.byType(Slider));
        await tester.tapAt(Offset(slider.center.dx, slider.center.dy));
        await tester.pump();
        final ms = int.parse(platform.calls.last.split(' ').last);
        await roundTrip(tester, meanwhile: () => links.single.dead = true);
        expect(afterReopen(), ['pause', 'seek $ms']);
        expect(find.byTooltip('재생'), findsOneWidget);
        await closePlayer(tester);
      });

      testWidgets('다시 세워 찾아가는 동안 시계는 그 자리, 화면은 받는 중, 누름은 막힌다', (tester) async {
        await openVideo(tester);
        await tester.tap(find.byTooltip('일시정지'));
        await tester.pump();
        final slider = tester.getRect(find.byType(Slider));
        await tester.tapAt(Offset(slider.center.dx, slider.center.dy));
        await tester.pump();
        final ms = int.parse(platform.calls.last.split(' ').last);
        final label = clockLabel(Duration(milliseconds: ms));
        final seek = platform.seekGate = Completer<void>();
        await roundTrip(tester, meanwhile: () => links.single.dead = true);
        expect(platform.calls.last, 'seek $ms', reason: '찾아가는 중');
        expect(
          find.text(label),
          findsOneWidget,
          reason: 'video_player 는 아직 0 이다',
        );
        final spinner = find.descendant(
          of: find.byType(MediaPlayerScreen),
          matching: find.byType(CircularProgressIndicator),
        );
        expect(spinner, findsOneWidget);
        final button = find.ancestor(
          of: find.byTooltip('재생'),
          matching: find.byType(IconButton),
        );
        expect(tester.widget<IconButton>(button).onPressed, isNull);
        seek.complete();
        await tester.pump();
        await tester.pump();
        expect(find.text(label), findsOneWidget);
        expect(spinner, findsNothing);
        expect(tester.widget<IconButton>(button).onPressed, isNotNull);
        expect(platform.calls.last, isNot('play'), reason: '멈춰 둔 것');
        await closePlayer(tester);
      });

      void leave(WidgetTester tester) {
        for (final s in [
          AppLifecycleState.inactive,
          AppLifecycleState.hidden,
          AppLifecycleState.paused,
        ]) {
          tester.binding.handleAppLifecycleStateChanged(s);
        }
      }

      void back(WidgetTester tester) {
        for (final s in [
          AppLifecycleState.hidden,
          AppLifecycleState.inactive,
          AppLifecycleState.resumed,
        ]) {
          tester.binding.handleAppLifecycleStateChanged(s);
        }
      }

      for (final reclaimed in [false, true]) {
        testWidgets(
          '여는 사이 뒤로 가면 숨어서 틀지 않고 돌아와서 튼다${reclaimed ? ' — 그사이 거둬졌으면 새로 세워서' : ''}',
          (tester) async {
            await pumpChat(tester, mediaBlock(_video, 12));
            final wait = hold = Completer<void>();
            await tester.tap(find.text('demo clip.mp4'));
            await tester.pump();
            leave(tester);
            await tester.pump();
            wait.complete();
            await tester.pump();
            await tester.pump(const Duration(milliseconds: 400));
            expect(
              platform.calls,
              contains('open http://127.0.0.1:9/$_video/media.mp4'),
            );
            expect(
              platform.calls,
              isNot(contains('play')),
              reason: '뒤에서 몰래 틀지 않는다',
            );
            if (reclaimed) links.single.dead = true;
            back(tester);
            await tester.pump();
            await tester.pump(const Duration(milliseconds: 400));
            expect(platform.calls.last, 'play');
            expect(opened, hasLength(reclaimed ? 2 : 1));
            if (reclaimed) expect(afterReopen(), ['pause', 'play']);
            expect(find.byTooltip('일시정지'), findsOneWidget);
            await closePlayer(tester);
          },
        );
      }

      testWidgets('살아 있나 묻는 사이 다시 뒤로 가면 미뤘다가 다음 복귀에 그 뜻·자리로 잇는다', (
        tester,
      ) async {
        await openVideo(tester);
        platform.moveTo(const Duration(milliseconds: 1500));
        await tester.pump(const Duration(milliseconds: 600));
        final link = links.single;
        final asking = link.aliveGate = Completer<void>();
        leave(tester);
        await tester.pump();
        link.dead = true;
        back(tester);
        await tester.pump();
        leave(tester);
        await tester.pump();
        asking.complete();
        await tester.pump();
        await tester.pump(const Duration(milliseconds: 400));
        expect(opened, hasLength(1), reason: '뒤에 있는 동안 새로 세우지 않는다');
        link.aliveGate = null;
        back(tester);
        await tester.pump();
        await tester.pump(const Duration(milliseconds: 400));
        expect(afterReopen(), ['pause', 'seek 1500', 'play']);
        await closePlayer(tester);
      });

      testWidgets('틀다가 원본이 끊겨 오류가 나도 다시 시도는 마지막 자리에서 잇는다', (tester) async {
        await openVideo(tester);
        platform.moveTo(const Duration(milliseconds: 1500));
        await tester.pump(const Duration(milliseconds: 600));
        platform.fail();
        await tester.pump();
        await tester.pump();
        expect(find.text('다시 시도'), findsOneWidget);
        await tester.tap(find.text('다시 시도'));
        await tester.pump();
        await tester.pump(const Duration(milliseconds: 400));
        expect(afterReopen(), ['pause', 'seek 1500', 'play']);
        expect(find.byTooltip('일시정지'), findsOneWidget);
        await closePlayer(tester);
      });

      testWidgets('끝난 것은 끝에 두고 다시 틀지 않는다', (tester) async {
        await openVideo(tester);
        platform.complete();
        await tester.pump();
        await tester.pump();
        expect(find.byTooltip('처음부터'), findsOneWidget);
        await roundTrip(tester, meanwhile: () => links.single.dead = true);
        expect(afterReopen(), ['pause', 'seek 3000']);
        expect(find.byTooltip('처음부터'), findsOneWidget);
        await closePlayer(tester);
      });
    });

    testWidgets('연 플레이어는 누른 순간의 원본에 고정된다 — 밑의 카드가 B 로 바뀌어도 A 를 재생한다', (
      tester,
    ) async {
      final live = ValueNotifier(mediaBlock(_video, 12));
      addTearDown(live.dispose);
      await pumpChat(tester, live.value, live: live);
      await tester.tap(find.text('demo clip.mp4'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      live.value = mediaBlock(_audio, 4);
      await tester.pumpAndSettle();
      expect(find.byType(MediaPlayerScreen), findsOneWidget);
      expect(
        find.descendant(
          of: find.byType(MediaPlayerScreen),
          matching: find.text('demo clip.mp4'),
        ),
        findsOneWidget,
      );
      expect(platform.calls.first, 'open http://127.0.0.1:9/$_video/media.mp4');
      await tester.tap(find.byTooltip('닫기'));
      await tester.pumpAndSettle();
      await settleDispose(tester);
      expect(links.single.closed, isTrue);
      expect(opened.single.$2, _video, reason: 'B 로 바뀐 뒤에도 A 만 물었다');
      expect(find.text('tone.m4a'), findsOneWidget);
    });

    testWidgets('원본에 없으면 이유와 다시 시도, 기억한 카드는 그대로 선다', (tester) async {
      await pumpChat(tester, mediaBlock('5555555555555555', 4));
      expect(find.text('원본 기기에 이 미디어가 없어요'), findsOneWidget);
      final retry = find.widgetWithText(TextButton, '다시 시도');
      expect(tester.getSize(retry).height, greaterThanOrEqualTo(Look.tap));

      // 같은 연결에서 전에 본 카드 — 원본 기기가 꺼져 있어도 기억한 정보로.
      final root = Uri.parse('https://example.test/u/fixture/');
      final s = Server(root);
      store.meta[inlineMediaKey(s, '~src', _audio)] = (_info(_audio), null);
      s.close();
      await pumpChat(
        tester,
        mediaBlock(_audio, 4),
        root: root,
        handler: (_) async => http.Response('gone', 502),
      );
      expect(find.text('tone.m4a'), findsOneWidget);
    });

    testWidgets('재생 못 하는 형식은 이유·다시 시도, 받아 둔 파일이면 미리보기도', (tester) async {
      platform.failInit = true;
      store.done[_info(_audio).sha256] = _fixtures[_audio]!.bytes;
      await pumpChat(tester, mediaBlock(_audio, 4));
      await tester.tap(find.text('tone.m4a'));
      await tester.pumpAndSettle();
      expect(find.text('이 파일은 폰에서 재생하지 못했어요'), findsOneWidget);
      expect(find.text('형식을 이 기기가 지원하지 않을 수 있어요'), findsOneWidget);
      expect(find.text('미리보기로 열기'), findsOneWidget);
      platform.failInit = false;
      await tester.tap(find.text('다시 시도'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 100));
      expect(find.byTooltip('일시정지'), findsOneWidget);
      expect(
        find.descendant(
          of: find.byType(MediaPlayerScreen),
          matching: find.text('tone.m4a'),
        ),
        findsOneWidget,
        reason: '소리는 가운데 그림 밑 이름 하나 — 앱바에 겹쳐 쓰지 않는다',
      );
      await tester.tap(find.byTooltip('닫기'));
      await tester.pumpAndSettle();
      await settleDispose(tester);
      expect(platform.live, 0);
      expect(opened, isEmpty, reason: '받아 둔 파일이면 창구를 열지 않는다');
    });
  });
}

/// 답하지 않는 서버 — 끊기 신호가 오면 그때 끊긴다.
class _Hang extends http.BaseClient {
  int sent = 0;
  int aborted = 0;

  @override
  Future<http.StreamedResponse> send(http.BaseRequest request) async {
    sent++;
    if (request case http.Abortable(:final abortTrigger?)) {
      await abortTrigger;
      aborted++;
      throw http.RequestAbortedException(request.url);
    }
    await Completer<void>().future;
    throw StateError('unreachable');
  }
}

typedef _Reply = ({int status, Map<String, String> headers, Uint8List body});

/// 창구에 AVPlayer 처럼 묻는다 — 진짜 소켓으로.
Future<_Reply> _get(
  Uri u, {
  String? range,
  String method = 'GET',
  Map<String, String> headers = const {},
}) async {
  // 위젯 시험 바인딩은 HttpClient 를 모두 400 으로 막는다 — 창구 시험은 진짜 소켓이 필요하다.
  final c = HttpOverrides.runWithHttpOverrides(HttpClient.new, _RealHttp());
  try {
    final req = await c.openUrl(method, u);
    if (range != null) req.headers.set(HttpHeaders.rangeHeader, range);
    headers.forEach(req.headers.set);
    final res = await req.close();
    final body = BytesBuilder(copy: false);
    await for (final d in res) {
      body.add(d);
    }
    final h = <String, String>{};
    res.headers.forEach((k, v) => h[k] = v.join(','));
    return (status: res.statusCode, headers: h, body: body.takeBytes());
  } finally {
    c.close(force: true);
  }
}

class _RealHttp extends HttpOverrides {}

/// 요청만 보내 둔 맨 소켓 — 읽다 말고 끊는 손님(AVPlayer 의 탐색)을 흉내 낸다.
Future<Socket> _rawGet(Uri u, String range) async {
  final s = await Socket.connect(u.host, u.port);
  s.write(
    'GET ${u.path} HTTP/1.1\r\nHost: ${u.host}:${u.port}\r\nRange: $range\r\n\r\n',
  );
  await s.flush();
  return s;
}

/// 조각 요청을 손으로 풀어 주는 원본. 끊기 신호가 오면 그 요청을 끊고 센다.
class _Gate extends http.BaseClient {
  _Gate(this.media);
  final Map<String, MediaFixture> media;
  final asked = <int>[];
  final aborted = <int>[];
  final _go = <int, Completer<void>>{};
  final _seen = <int, Completer<void>>{};
  final _gone = <int, Completer<void>>{};

  Completer<void> _at(Map<int, Completer<void>> m, int offset) =>
      m.putIfAbsent(offset, Completer<void>.new);

  Future<void> waitFor(int offset) =>
      _at(_seen, offset).future.timeout(const Duration(seconds: 2));
  Future<void> abortedWait(int offset) =>
      _at(_gone, offset).future.timeout(const Duration(seconds: 2));
  void release(int offset) => _at(_go, offset).complete();

  @override
  Future<http.StreamedResponse> send(http.BaseRequest request) async {
    final url = request.url;
    if (url.path.endsWith('/file')) {
      final offset = int.parse(url.queryParameters['offset']!);
      asked.add(offset);
      _at(_seen, offset).complete();
      final trigger = request is http.Abortable ? request.abortTrigger : null;
      final go = await Future.any([
        _at(_go, offset).future.then((_) => true),
        if (trigger != null) trigger.then((_) => false),
      ]);
      if (!go) {
        aborted.add(offset);
        _at(_gone, offset).complete();
        throw http.RequestAbortedException(url);
      }
    }
    final r = inlineMediaReply(media, url);
    return http.StreamedResponse(
      Stream.value(r.body),
      r.status,
      headers: {'content-type': r.type},
    );
  }
}

/// 창구 대신 — 주소만 내주고 닫혔는지 기록한다.
class _FakeLink implements MediaLink {
  _FakeLink(this.id, this.ext, this.error);
  final String id;
  final String ext;
  bool closed = false;

  /// iOS 가 루프백 소켓을 거둬 간 것처럼.
  bool dead = false;

  @override
  final String? error;

  /// 살아 있나 묻기를 붙잡아 둔다 — 묻는 사이의 생애 변화를 본다.
  Completer<void>? aliveGate;

  @override
  Future<bool> alive() async {
    await aliveGate?.future;
    return !dead && !closed;
  }

  @override
  Uri get uri => Uri.parse('http://127.0.0.1:9/$id/media.$ext');

  @override
  String? get path => null;

  @override
  Future<void> close() async => closed = true;
}
