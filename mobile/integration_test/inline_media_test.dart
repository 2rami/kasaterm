// 시뮬레이터에서 진짜 AVPlayer 로 — 카드 → 곧바로 플레이어 → 루프백 창구가 필요한 구간만 받아 재생·멈춤·찾기·끝·
// 다시·닫기. 가짜 등록소(tool/inline_media_fixture_server.dart)를 맥에서 띄우고 그 주소를 KASA_MEDIA_FIXTURE 로 준다.
// 긴 영상(끝에 moov, 긴 GOP)까지 보려면 등록소에 그 파일을 주고 KASA_MEDIA_LONG=true 를 더한다.
//
//   ffmpeg -f lavfi -i testsrc2=size=1280x720:rate=30 -f lavfi -i sine -t 60 -c:v libx264 -b:v 8M -g 300 \
//     -pix_fmt yuv420p -c:a aac long.mp4
//   dart run tool/inline_media_fixture_server.dart 8791 150 long.mp4 &
//   flutter drive -d <시뮬레이터> --driver test_driver/integration_test.dart \
//     --target integration_test/inline_media_test.dart \
//     --dart-define=KASA_MEDIA_FIXTURE=http://127.0.0.1:8791/ --dart-define=KASA_MEDIA_LONG=true
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:integration_test/integration_test.dart';
import 'package:kasaterm_mobile/inline_media.dart';
import 'package:kasaterm_mobile/inline_media_bridge_io.dart';
import 'package:kasaterm_mobile/inline_result.dart';
import 'package:kasaterm_mobile/look.dart';
import 'package:kasaterm_mobile/main.dart' show buildTheme;
import 'package:kasaterm_mobile/screens/controls.dart';
import 'package:kasaterm_mobile/screens/media_player.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:video_player/video_player.dart';

const _root = String.fromEnvironment('KASA_MEDIA_FIXTURE');
const _long = bool.fromEnvironment('KASA_MEDIA_LONG');

String _block(String id, int rows) =>
    '```text\n${List.generate(rows, (i) => '[[km:$id:${i.toString().padLeft(2, '0')}/${rows.toString().padLeft(2, '0')}]]').join('\n')}\n```\n';

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  Future<void> until(
    WidgetTester tester,
    bool Function() ok,
    String what, {
    Duration limit = const Duration(seconds: 20),
  }) async {
    final end = DateTime.now().add(limit);
    while (!ok()) {
      if (DateTime.now().isAfter(end)) fail('기다리다 멈춤: $what');
      await tester.pump(const Duration(milliseconds: 50));
      await Future<void>.delayed(const Duration(milliseconds: 50));
    }
  }

  Future<void> idle(WidgetTester tester, Duration d) async {
    final end = DateTime.now().add(d);
    while (DateTime.now().isBefore(end)) {
      await tester.pump(const Duration(milliseconds: 50));
      await Future<void>.delayed(const Duration(milliseconds: 50));
    }
  }

  VideoPlayerController? player(WidgetTester tester) {
    final f = find.byWidgetPredicate(
      (w) => w is ValueListenableBuilder<VideoPlayerValue>,
    );
    if (f.evaluate().isEmpty) return null;
    return tester
            .widget<ValueListenableBuilder<VideoPlayerValue>>(f.first)
            .valueListenable
        as VideoPlayerController;
  }

  Future<void> shot(WidgetTester tester, String name) async {
    await tester.pump();
    await binding.takeScreenshot(name);
  }

  void log(String line) =>
      debugPrint('MEDIA ${DateTime.now().millisecondsSinceEpoch} $line');

  testWidgets('대화 미디어 카드 — 창구로 곧바로 재생·멈춤·찾기·끝·닫기·끊김', (tester) async {
    expect(_root, isNotEmpty, reason: 'KASA_MEDIA_FIXTURE 를 주세요');
    final bridges = <MediaBridge>[];
    InlineMediaHub.debugReset(
      InlineMediaHub(
        bridge:
            ({
              required server,
              required id,
              required machine,
              required media,
              required chunk,
            }) async {
              final b = await MediaBridge.open(
                server: server,
                id: id,
                machine: machine,
                media: media,
                chunk: chunk,
              );
              bridges.add(b);
              return b;
            },
      ),
    );
    final server = Server(Uri.parse(_root));
    addTearDown(server.close);
    Future<void> origin({required bool down}) async {
      await http.get(
        Uri.parse(_root).resolve('/__origin?down=${down ? 1 : 0}'),
      );
    }

    Future<bool> refused(Uri u) async {
      try {
        (await Socket.connect(u.host, u.port)).destroy();
        return false;
      } on SocketException {
        return true;
      }
    }

    Future<void> close(WidgetTester tester) async {
      await tester.tap(find.byTooltip('닫기'));
      await until(
        tester,
        () => find.byType(MediaPlayerScreen).evaluate().isEmpty,
        '닫기',
      );
    }

    final theme = buildTheme(Brightness.light);
    await tester.pumpWidget(
      MaterialApp(
        theme: theme,
        debugShowCheckedModeBanner: false,
        home: Scaffold(
          body: SafeArea(
            child: ListView(
              padding: const EdgeInsets.all(Look.pagePad),
              children: [
                Align(
                  alignment: Alignment.centerLeft,
                  child: SpeechBubble(
                    mine: false,
                    maxWidth: 402 - Look.pagePad * 2 - Look.bubbleFar,
                    child: InlineResultBody(
                      server: server,
                      text:
                          '영상을 만들었어요.\n\n${_block('1111111111111111', 12)}\n'
                          '소리도요.\n\n${_block('2222222222222222', 4)}\n'
                          '세로 영상.\n\n${_block('3333333333333333', 12)}\n'
                          '${_long ? '긴 영상.\n\n${_block('6666666666666666', 12)}' : ''}',
                      style: MarkdownStyleSheet.fromTheme(theme),
                      onLink: (_) {},
                    ),
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
    await until(
      tester,
      () => find.text('tone.m4a').evaluate().isNotEmpty,
      '카드 정보',
    );
    await idle(tester, const Duration(milliseconds: 300));
    expect(tester.takeException(), isNull);
    await shot(tester, '01-cards');

    // 영상: 누르면 받기 없이 플레이어가 서고 창구로 재생한다.
    await tester.tap(find.text('demo clip.mp4'));
    await tester.pump();
    await tester.pump();
    expect(find.byType(MediaPlayerScreen), findsOneWidget, reason: '누르자마자 선다');
    expect(find.textContaining('받는 중'), findsNothing);
    await until(
      tester,
      () => player(tester)?.value.isPlaying ?? false,
      '재생 시작',
    );
    await idle(tester, const Duration(milliseconds: 1200));
    final c = player(tester)!;
    expect(c.dataSource, startsWith('http://127.0.0.1:'));
    expect(
      c.value.position.inMilliseconds,
      greaterThan(500),
      reason: '재생이 흘러간다',
    );
    expect(c.value.aspectRatio, closeTo(320 / 180, 0.02));
    await shot(tester, '02-video-playing');

    await tester.tap(find.byTooltip('일시정지'));
    await idle(tester, const Duration(milliseconds: 300));
    final paused = c.value.position;
    await idle(tester, const Duration(milliseconds: 700));
    expect(c.value.isPlaying, isFalse);
    expect(
      (c.value.position - paused).inMilliseconds.abs(),
      lessThan(100),
      reason: '멈추면 선다',
    );

    final slider = tester.getRect(find.byType(Slider));
    await tester.tapAt(
      Offset(slider.left + slider.width * 0.5, slider.center.dy),
    );
    await idle(tester, const Duration(milliseconds: 500));
    expect(
      c.value.position.inMilliseconds,
      inInclusiveRange(1000, 2000),
      reason: '멈춘 채 막대로 찾아간다',
    );
    expect(c.value.isPlaying, isFalse);
    await shot(tester, '03-video-paused-seek');

    await tester.tap(find.byTooltip('재생'));
    await until(
      tester,
      () => find.byTooltip('처음부터').evaluate().isNotEmpty,
      '끝까지',
      limit: const Duration(seconds: 6),
    );
    expect(find.text('0:03'), findsNWidgets(2), reason: '끝나면 지난 시간도 길이와 같다');
    await shot(tester, '04-video-ended');
    await tester.tap(find.byTooltip('처음부터'));
    await idle(tester, const Duration(milliseconds: 600));
    expect(c.value.isPlaying, isTrue);
    expect(c.value.position.inMilliseconds, lessThan(1500), reason: '처음부터 다시');

    await close(tester);
    await idle(tester, const Duration(milliseconds: 300));
    expect(player(tester), isNull);
    expect(await refused(bridges.last.uri), isTrue, reason: '닫으면 창구도 닫힌다');

    // 소리.
    await tester.tap(find.text('tone.m4a'));
    await until(
      tester,
      () => player(tester)?.value.isPlaying ?? false,
      '소리 재생',
    );
    await idle(tester, const Duration(milliseconds: 1000));
    expect(player(tester)!.value.position.inMilliseconds, greaterThan(400));
    expect(bridges.last.media.streamMime, 'audio/mp4');
    await shot(tester, '05-audio-playing');
    await close(tester);

    // 세로 영상: 90° 돌린 mov 가 세로로 선다.
    await tester.ensureVisible(find.text('portrait.mov'));
    await idle(tester, const Duration(milliseconds: 400));
    await tester.tap(find.text('portrait.mov'));
    await until(
      tester,
      () => player(tester)?.value.isPlaying ?? false,
      '세로 영상 재생',
    );
    await idle(tester, const Duration(milliseconds: 800));
    final portrait = tester.getSize(find.byType(VideoPlayer));
    expect(portrait.height, greaterThan(portrait.width), reason: '회전 정보를 따른다');
    await shot(tester, '06-portrait');
    await close(tester);

    if (_long) {
      // 긴 영상(61MB, 끝에 moov, 10초 GOP): 다 받기 전에 시작하고, 멈춘 채 찾으면 그 자리만 받고, 닫으면 멎는다.
      await tester.ensureVisible(find.text('long.mp4'));
      await idle(tester, const Duration(milliseconds: 400));
      final tapped = DateTime.now();
      await tester.tap(find.text('long.mp4'));
      await until(
        tester,
        () =>
            (player(tester)?.value.isPlaying ?? false) &&
            player(tester)!.value.position > const Duration(milliseconds: 300),
        '긴 영상 재생 시작',
      );
      final b = bridges.last;
      final size = b.media.size;
      final firstMs = DateTime.now().difference(tapped).inMilliseconds;
      log('long first-play ${firstMs}ms received ${b.received} of $size');
      expect(b.received, lessThan(size ~/ 4), reason: '다 받기 전에 재생');
      await shot(tester, '07-long-playing');

      await tester.tap(find.byTooltip('일시정지'));
      await idle(tester, const Duration(milliseconds: 300));
      final lc = player(tester)!;
      final beforeSeek = b.received;
      log('long seek-start received $beforeSeek');
      final ls = tester.getRect(find.byType(Slider));
      await tester.tapAt(Offset(ls.left + ls.width * 0.8, ls.center.dy));
      await until(
        tester,
        () => lc.value.position.inSeconds >= 40 && !lc.value.isBuffering,
        '멈춘 채 80% 로',
      );
      await idle(tester, const Duration(milliseconds: 1500));
      log(
        'long paused-seek at ${lc.value.position.inMilliseconds}ms received ${b.received}',
      );
      expect(lc.value.isPlaying, isFalse);
      expect(lc.value.position.inSeconds, inInclusiveRange(40, 54));
      expect(b.received, lessThan(size ~/ 2), reason: '찾은 자리 구간만');
      await shot(tester, '08-long-paused-seek');

      await tester.tap(find.byTooltip('재생'));
      final from = lc.value.position;
      await idle(tester, const Duration(milliseconds: 1500));
      expect(
        lc.value.position - from,
        greaterThan(const Duration(milliseconds: 500)),
      );
      await close(tester);
      final closedAt = b.received;
      log('long closed received $closedAt');
      await idle(tester, const Duration(milliseconds: 1500));
      expect(b.received, closedAt, reason: '닫은 뒤로 더 받지 않는다');
      expect(await refused(b.uri), isTrue);

      // 원본 끊김: 까닭과 다시 시도, 살아나면 다시 시도로 재생.
      await origin(down: true);
      await tester.tap(find.text('long.mp4'));
      await until(
        tester,
        () => find.text('다시 시도').evaluate().isNotEmpty,
        '끊김 표시',
      );
      expect(find.textContaining('미디어를 받지 못했어요'), findsOneWidget);
      await shot(tester, '09-origin-down');
      await origin(down: false);
      await tester.tap(find.text('다시 시도'));
      await until(
        tester,
        () => player(tester)?.value.isPlaying ?? false,
        '다시 시도로 재생',
      );
      await close(tester);
    }
    expect(tester.takeException(), isNull);
  });
}
