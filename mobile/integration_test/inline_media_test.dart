// 시뮬레이터에서 진짜 AVPlayer 로 — 카드 → 조각 받기 → 해시 대조 → 앱 안 재생·멈춤·찾기·끝·다시·닫기.
// 가짜 등록소(tool/inline_media_fixture_server.dart)를 맥에서 띄우고 그 주소를 KASA_MEDIA_FIXTURE 로 준다.
//
//   dart run tool/inline_media_fixture_server.dart 8791 &
//   flutter drive -d <시뮬레이터> --driver test_driver/integration_test.dart \
//     --target integration_test/inline_media_test.dart \
//     --dart-define=KASA_MEDIA_FIXTURE=http://127.0.0.1:8791/
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:kasaterm_mobile/inline_media.dart';
import 'package:kasaterm_mobile/inline_result.dart';
import 'package:kasaterm_mobile/look.dart';
import 'package:kasaterm_mobile/main.dart' show buildTheme;
import 'package:kasaterm_mobile/screens/controls.dart';
import 'package:kasaterm_mobile/screens/media_player.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:path_provider/path_provider.dart';
import 'package:video_player/video_player.dart';

const _root = String.fromEnvironment('KASA_MEDIA_FIXTURE');

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

  testWidgets('대화 미디어 카드 — 받기·재생·멈춤·찾기·끝·닫기·취소·실패', (tester) async {
    expect(_root, isNotEmpty, reason: 'KASA_MEDIA_FIXTURE 를 주세요');
    final cache = Directory(
      '${(await getApplicationCacheDirectory()).path}/inline-media',
    );
    if (cache.existsSync()) cache.deleteSync(recursive: true);
    InlineMediaHub.debugReset(InlineMediaHub(chunk: 16 * 1024));
    final server = Server(Uri.parse(_root));
    addTearDown(server.close);
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
                          '${_block('4444444444444444', 12)}',
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

    // 영상: 누르면 조각으로 받고(진행·취소가 선다) 다 받으면 앱 안 플레이어가 열려 재생한다.
    await tester.tap(find.text('demo clip.mp4'));
    await until(
      tester,
      () => find.textContaining('받는 중').evaluate().isNotEmpty,
      '받는 중',
    );
    expect(find.byTooltip('받기 취소'), findsOneWidget);
    await shot(tester, '02-downloading');
    await until(
      tester,
      () => player(tester)?.value.isPlaying ?? false,
      '재생 시작',
    );
    await idle(tester, const Duration(milliseconds: 1200));
    final c = player(tester)!;
    expect(
      c.value.position.inMilliseconds,
      greaterThan(500),
      reason: '재생이 흘러간다',
    );
    expect(c.value.aspectRatio, closeTo(320 / 180, 0.02));
    await shot(tester, '03-video-playing');

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
      reason: '막대로 찾아간다',
    );
    await shot(tester, '04-video-seek');

    await tester.tap(find.byTooltip('재생'));
    await until(
      tester,
      () => find.byTooltip('처음부터').evaluate().isNotEmpty,
      '끝까지',
      limit: const Duration(seconds: 6),
    );
    expect(find.text('0:03'), findsNWidgets(2), reason: '끝나면 지난 시간도 길이와 같다');
    await shot(tester, '05-video-ended');
    await tester.tap(find.byTooltip('처음부터'));
    await idle(tester, const Duration(milliseconds: 600));
    expect(c.value.isPlaying, isTrue);
    expect(c.value.position.inMilliseconds, lessThan(1500), reason: '처음부터 다시');

    await tester.tap(find.byTooltip('닫기'));
    await until(
      tester,
      () => find.byType(MediaPlayerScreen).evaluate().isEmpty,
      '닫기',
    );
    await idle(tester, const Duration(milliseconds: 300));
    expect(player(tester), isNull);

    // 두 번째는 받지 않고 바로 연다.
    await tester.tap(find.text('demo clip.mp4'));
    await until(
      tester,
      () => player(tester)?.value.isPlaying ?? false,
      '캐시에서 재생',
    );
    expect(find.textContaining('받는 중'), findsNothing);
    await tester.tap(find.byTooltip('닫기'));
    await until(
      tester,
      () => find.byType(MediaPlayerScreen).evaluate().isEmpty,
      '닫기',
    );

    // 소리: 받다가 취소하면 처음으로, 다시 누르면 끝까지 받아 튼다.
    await tester.tap(find.text('tone.m4a'));
    await until(
      tester,
      () => find.byTooltip('받기 취소').evaluate().isNotEmpty,
      '소리 받는 중',
    );
    await tester.tap(find.byTooltip('받기 취소'));
    await until(tester, () => find.byTooltip('받기 취소').evaluate().isEmpty, '취소');
    await idle(tester, const Duration(milliseconds: 600));
    expect(find.byType(MediaPlayerScreen), findsNothing);
    expect(
      cache.listSync().whereType<File>().where((f) => f.path.endsWith('.part')),
      isEmpty,
      reason: '취소한 조각은 지운다',
    );
    await tester.tap(find.text('tone.m4a'));
    await until(
      tester,
      () => player(tester)?.value.isPlaying ?? false,
      '소리 재생',
    );
    await idle(tester, const Duration(milliseconds: 1000));
    expect(player(tester)!.value.position.inMilliseconds, greaterThan(400));
    await shot(tester, '06-audio-playing');
    await tester.tap(find.byTooltip('닫기'));
    await until(
      tester,
      () => find.byType(MediaPlayerScreen).evaluate().isEmpty,
      '닫기',
    );

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
    await shot(tester, '07-portrait');
    await tester.tap(find.byTooltip('닫기'));
    await until(
      tester,
      () => find.byType(MediaPlayerScreen).evaluate().isEmpty,
      '닫기',
    );

    // 해시가 다른 파일은 열지 않고 이유를 보인다.
    await tester.ensureVisible(find.text('tampered.mp4'));
    await idle(tester, const Duration(milliseconds: 400));
    await tester.tap(find.text('tampered.mp4'));
    await until(
      tester,
      () => find.textContaining('원본과 달라요').evaluate().isNotEmpty,
      '대조 실패',
    );
    expect(find.byType(MediaPlayerScreen), findsNothing);
    await shot(tester, '08-integrity-failed');
    expect(tester.takeException(), isNull);
  });
}
