import 'dart:async';
import 'dart:io';
import 'dart:math' show max;

import 'package:crypto/crypto.dart';
import 'package:flutter/material.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/inline_media.dart';
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
const _tampered = '4444444444444444';

final _fixtures = loadMediaFixtures('test/fixtures/media');

http.Response _reply(http.Request req) {
  final r = inlineMediaReply(_fixtures, req.url);
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

  group('받기(앱 저장소)', () {
    late Directory dir;
    late IoMediaStore store;
    setUp(() {
      dir = Directory.systemTemp.createTempSync('kasa-media-');
      store = IoMediaStore(dir);
    });
    tearDown(() => dir.deleteSync(recursive: true));

    Server server(Future<http.Response> Function(http.Request) handler) =>
        Server(
          Uri.parse('https://example.test/u/fixture/'),
          client: MockClient(handler),
        );

    test('조각으로 차례로 받고 전체 sha256 을 대조한 뒤 해시 이름으로 둔다', () async {
      final asked = <(int, int)>[];
      final s = server((req) async {
        if (req.url.path.endsWith('/file')) {
          expect(req.url.path, contains('m/~src/term/inline-media/file'));
          asked.add((
            int.parse(req.url.queryParameters['offset']!),
            int.parse(req.url.queryParameters['len']!),
          ));
        }
        return _reply(req);
      });
      addTearDown(s.close);
      final media = _info(_video);
      final fetch = MediaFetch(media);
      final seen = <int>[];
      fetch.addListener(() => seen.add(fetch.received));
      final path = await fetch.run(
        server: s,
        id: _video,
        machine: '~src',
        store: store,
        chunk: 16 * 1024,
      );
      expect(asked, [
        (0, 16384),
        (16384, 16384),
        (32768, 16384),
        (49152, 2625),
      ]);
      expect(seen, containsAllInOrder([16384, 32768, 49152, 51777]));
      expect(path, '${dir.path}/${media.sha256}.mp4');
      expect(
        sha256.convert(File(path!).readAsBytesSync()).toString(),
        media.sha256,
      );
      expect(fetch.phase, MediaPhase.ready);

      asked.clear();
      expect(
        await MediaFetch(
          media,
        ).run(server: s, id: _video, machine: '~src', store: store),
        path,
      );
      expect(asked, isEmpty, reason: '받아 둔 파일은 다시 받지 않는다');
    });

    test('원본과 다른 해시면 조각을 지우고 실패로 남긴다', () async {
      final s = server((req) async => _reply(req));
      addTearDown(s.close);
      final media = _info(_tampered);
      final fetch = MediaFetch(media);
      expect(
        await fetch.run(server: s, id: _tampered, machine: null, store: store),
        isNull,
      );
      expect(fetch.phase, MediaPhase.failed);
      expect(fetch.error, '받은 파일이 원본과 달라요');
      expect(dir.listSync().whereType<File>(), isEmpty);
    });

    test('중간에 끊기면 받은 데까지 남기고 다시 시도는 그 뒤부터 잇는다', () async {
      var broken = true;
      final offsets = <int>[];
      final s = server((req) async {
        if (req.url.path.endsWith('/file')) {
          final offset = int.parse(req.url.queryParameters['offset']!);
          offsets.add(offset);
          if (broken && offset == 32768) {
            broken = false;
            return http.Response('busy', 503);
          }
        }
        return _reply(req);
      });
      addTearDown(s.close);
      final fetch = MediaFetch(_info(_video));
      Future<String?> go() => fetch.run(
        server: s,
        id: _video,
        machine: null,
        store: store,
        chunk: 16 * 1024,
      );
      expect(await go(), isNull);
      expect(fetch.phase, MediaPhase.failed);
      expect(fetch.error, '미디어를 받지 못했어요 (HTTP 503)');
      expect(fetch.received, 32768);
      offsets.clear();
      expect(await go(), isNotNull);
      expect(offsets, [32768, 49152]);
    });

    test('취소하면 받던 요청을 끊고 조각을 지운다', () async {
      final hold = Completer<void>();
      final s = server((req) async {
        if (req.url.path.endsWith('/file') &&
            req.url.queryParameters['offset'] == '16384') {
          await hold.future;
        }
        return _reply(req);
      });
      addTearDown(s.close);
      final fetch = MediaFetch(_info(_video));
      final run = fetch.run(
        server: s,
        id: _video,
        machine: null,
        store: store,
        chunk: 16 * 1024,
      );
      while (fetch.received < 16384) {
        await Future<void>.delayed(Duration.zero);
      }
      fetch.cancel();
      hold.complete();
      expect(await run, isNull);
      expect(fetch.phase, MediaPhase.idle);
      expect(dir.listSync().whereType<File>(), isEmpty);
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
    setUp(() {
      platform = FakeVideoPlayer();
      VideoPlayerPlatform.instance = platform;
      store = MemoryMediaStore();
      InlineMediaHub.debugReset(
        InlineMediaHub(store: () async => store, chunk: 16 * 1024),
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

    testWidgets('누르면 받는 동안 진행·취소가 서고, 다 받으면 앱 안에서 연다', (tester) async {
      final gate = <Completer<void>>[];
      await pumpChat(
        tester,
        mediaBlock(_video, 12),
        handler: (req) async {
          if (req.url.path.endsWith('/file')) {
            final c = Completer<void>();
            gate.add(c);
            await c.future;
          }
          return _reply(req);
        },
      );
      await tester.tap(find.text('demo clip.mp4'));
      await tester.pump();
      await tester.pump();
      expect(find.text('받는 중 0KB / 51KB'), findsOneWidget);
      final cancel = find.byTooltip('받기 취소');
      expect(tester.getSize(cancel), const Size(Look.tap, Look.tap));
      gate.removeAt(0).complete();
      await tester.pump();
      await tester.pump();
      expect(find.text('받는 중 16KB / 51KB'), findsOneWidget);
      await expectLater(
        find.byType(Scaffold),
        matchesGoldenFile('goldens/inline_media_downloading.png'),
      );
      while (gate.isNotEmpty ||
          find.byType(MediaPlayerScreen).evaluate().isEmpty) {
        if (gate.isNotEmpty) gate.removeAt(0).complete();
        await tester.pump(const Duration(milliseconds: 50));
      }
      await tester.pump(const Duration(milliseconds: 400));
      final sha = _info(_video).sha256;
      // 열 때 video_player 가 멈춤 상태를 한 번 맞추고, 카드를 누른 뜻대로 튼다.
      expect(platform.calls, ['open file:///cache/$sha.mp4', 'pause', 'play']);
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
      expect(
        find.text('0:03 · 51KB'),
        findsOneWidget,
        reason: '카드는 「재생」으로 돌아온다',
      );

      platform.calls.clear();
      await tester.tap(find.text('demo clip.mp4'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      expect(
        platform.calls.first,
        'open file:///cache/$sha.mp4',
        reason: '두 번째는 받지 않고 연다',
      );
      await tester.tap(find.byTooltip('닫기'));
      await tester.pumpAndSettle();
    });

    testWidgets('취소하면 카드가 처음으로, 실패하면 이유와 다시', (tester) async {
      final hold = Completer<void>();
      await pumpChat(
        tester,
        '${mediaBlock(_audio, 4)}\n${mediaBlock(_tampered, 12)}',
        handler: (req) async {
          if (req.url.path.endsWith('/file') &&
              req.url.queryParameters['id'] == _audio) {
            await hold.future;
          }
          return _reply(req);
        },
      );
      await tester.tap(find.text('tone.m4a'));
      await tester.pump();
      await tester.pump();
      expect(find.byTooltip('받기 취소'), findsOneWidget);
      await tester.tap(find.byTooltip('받기 취소'));
      hold.complete();
      await tester.pumpAndSettle();
      expect(find.byTooltip('받기 취소'), findsNothing);
      expect(find.text('0:03 · 20KB'), findsOneWidget);
      expect(store.parts, isEmpty);
      expect(find.byType(MediaPlayerScreen), findsNothing);

      await tester.tap(find.text('tampered.mp4'));
      await tester.pumpAndSettle();
      expect(find.text('받은 파일이 원본과 달라요 · 눌러서 다시'), findsOneWidget);
      expect(find.byType(MediaPlayerScreen), findsNothing);
      expect(tester.takeException(), isNull);
    });

    testWidgets('정보를 기다리는 사이 카드가 B 로 바뀌면 늦게 온 A 는 버린다', (tester) async {
      final infoA = Completer<void>(), infoB = Completer<void>();
      final fileA = Completer<void>();
      final live = ValueNotifier(mediaBlock(_video, 12));
      addTearDown(live.dispose);
      final server = Server(
        Uri.parse('https://example.test/u/fixture/'),
        client: MockClient((req) async {
          final id = req.url.queryParameters['id'];
          if (req.url.path.endsWith('/file')) {
            await fileA.future;
          } else if (id == _video) {
            await infoA.future;
          } else if (id == _audio) {
            await infoB.future;
          }
          return _reply(req);
        }),
      );
      addTearDown(server.close);
      // A 는 다른 카드에서 이미 받는 중 — 늦게 온 A 정보가 그 진행을 B 카드에 붙이면 안 된다.
      final running = InlineMediaHub.instance.fetch(_info(_video));
      final run = running.run(
        server: server,
        id: _video,
        machine: '~src',
        store: store,
      );
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
      expect(find.byTooltip('받기 취소'), findsNothing);
      infoB.complete();
      await tester.pumpAndSettle();
      expect(find.text('tone.m4a'), findsOneWidget);
      expect(find.byTooltip('받기 취소'), findsNothing);
      expect(find.textContaining('받는 중'), findsNothing);
      running.cancel();
      fileA.complete();
      expect(await run, isNull);
    });

    testWidgets('받는 사이 카드가 B 로 바뀌면 A 가 다 받아져도 B 위에 열지 않는다', (tester) async {
      final gate = <Completer<void>>[];
      final infoB = Completer<void>();
      final live = ValueNotifier(mediaBlock(_video, 12));
      addTearDown(live.dispose);
      await pumpChat(
        tester,
        live.value,
        live: live,
        handler: (req) async {
          if (req.url.path.endsWith('/file') &&
              req.url.queryParameters['id'] == _video) {
            final c = Completer<void>();
            gate.add(c);
            await c.future;
          }
          if (req.url.queryParameters['id'] == _audio) await infoB.future;
          return _reply(req);
        },
      );
      await tester.tap(find.text('demo clip.mp4'));
      await tester.pump();
      await tester.pump();
      expect(find.byTooltip('받기 취소'), findsOneWidget);
      live.value = mediaBlock(_audio, 4);
      await tester.pump();
      expect(
        find.text('demo clip.mp4'),
        findsNothing,
        reason: 'B 정보를 기다리는 동안 옛 A 카드를 그리지 않는다',
      );
      infoB.complete();
      await tester.pumpAndSettle();
      expect(find.text('tone.m4a'), findsOneWidget);
      expect(
        find.byTooltip('받기 취소'),
        findsNothing,
        reason: 'A 의 진행이 B 카드에 붙지 않는다',
      );
      while (gate.isNotEmpty) {
        gate.removeAt(0).complete();
        await tester.pump();
        await tester.pump();
      }
      await tester.pumpAndSettle();
      expect(find.byType(MediaPlayerScreen), findsNothing);
      expect(platform.calls, isEmpty);
      expect(store.done.keys, [_info(_video).sha256], reason: '받은 A 는 캐시에 남는다');
      expect(find.text('0:03 · 20KB'), findsOneWidget);
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

    testWidgets('재생 못 하는 형식은 이유·다시 시도·미리보기를 보인다', (tester) async {
      platform.failInit = true;
      await pumpChat(tester, mediaBlock(_audio, 4));
      await tester.tap(find.text('tone.m4a'));
      await tester.pumpAndSettle();
      expect(find.text('이 파일은 폰에서 재생하지 못했어요'), findsOneWidget);
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
