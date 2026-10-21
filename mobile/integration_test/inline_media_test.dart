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
//
// 배경→복귀를 진짜로 보려면 --dart-define=KASA_MEDIA_BACKGROUND=true 를 더하고, 맥 쪽에서 출력의
// 「MEDIA … BACKGROUND <표>」 줄마다 `xcrun simctl launch <시뮬레이터> com.apple.Preferences` 로 앱을 뒤로 보냈다가
// 몇 초 뒤 `xcrun simctl launch <시뮬레이터> com.debimarlene.kasaterm` 으로 되돌린다.
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
const _background = bool.fromEnvironment('KASA_MEDIA_BACKGROUND');

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

  /// 카드 정보는 제각기 늦게 온다 — 필요한 카드가 모두 선 뒤에 누른다.
  Future<void> cards(WidgetTester tester, List<String> titles) => until(
    tester,
    () => titles.every((t) => find.text(t).evaluate().isNotEmpty),
    '카드 정보 ${titles.join('·')}',
  );

  /// 새 플레이어가 준비를 끝냈나 — 초기화만이 아니라 찾아가기까지 끝나 받는 중 표시가 걷히고 재생 단추가 풀렸다.
  bool settled(WidgetTester tester) {
    final c = player(tester);
    if (c == null || !c.value.isInitialized) return false;
    final spinning = find
        .descendant(
          of: find.byType(MediaPlayerScreen),
          matching: find.byType(CircularProgressIndicator),
        )
        .evaluate()
        .isNotEmpty;
    final button = find
        .descendant(
          of: find.byType(MediaPlayerScreen),
          matching: find.byWidgetPredicate(
            (w) =>
                w is IconButton &&
                w.onPressed != null &&
                const ['재생', '일시정지', '처음부터'].contains(w.tooltip),
          ),
        )
        .evaluate()
        .isNotEmpty;
    return button && (!spinning || c.value.isPlaying);
  }

  Future<void> shot(WidgetTester tester, String name) async {
    await tester.pump();
    await binding.takeScreenshot(name);
  }

  void log(String line) =>
      debugPrint('MEDIA ${DateTime.now().millisecondsSinceEpoch} $line');

  /// 창구를 열 때마다 모은다 — 받은 양·살아 있나를 시험이 본다.
  List<MediaBridge> captureBridges() {
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
    return bridges;
  }

  Future<void> origin({required bool down}) async {
    await http.get(Uri.parse(_root).resolve('/__origin?down=${down ? 1 : 0}'));
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

  Future<void> pumpChat(WidgetTester tester, String text) async {
    expect(_root, isNotEmpty, reason: 'KASA_MEDIA_FIXTURE 를 주세요');
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
                      text: text,
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
  }

  testWidgets('대화 미디어 카드 — 창구로 곧바로 재생·멈춤·찾기·끝·닫기·끊김', (tester) async {
    final bridges = captureBridges();
    await pumpChat(
      tester,
      '영상을 만들었어요.\n\n${_block('1111111111111111', 12)}\n'
      '소리도요.\n\n${_block('2222222222222222', 4)}\n'
      '세로 영상.\n\n${_block('3333333333333333', 12)}\n'
      '${_long ? '긴 영상.\n\n${_block('6666666666666666', 12)}' : ''}',
    );
    await cards(tester, [
      'demo clip.mp4',
      'tone.m4a',
      'portrait.mov',
      if (_long) 'long.mp4',
    ]);
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
  // 배경→복귀. KASA_MEDIA_BACKGROUND=true 면 「BACKGROUND <표>」 줄을 본 맥 쪽이 simctl 로 앱을 진짜 뒤로 보냈다
  // 되돌린다. 아니면 생애 상태만 흉내 낸다. 거둠은 paused 를 받은 순간 창구의 듣기 소켓·연결을 끊어 흉내 낸다.
  Future<void> roundTrip(
    WidgetTester tester,
    String tag, {
    MediaBridge? reclaim,
    void Function()? onPaused,
  }) async {
    final seen = <AppLifecycleState>[];
    final watch = _LifecycleWatch((s) {
      seen.add(s);
      if (s != AppLifecycleState.paused) return;
      onPaused?.call();
      reclaim?.debugReclaim();
    });
    WidgetsBinding.instance.addObserver(watch);
    try {
      if (_background) {
        log('BACKGROUND $tag');
      } else {
        for (final s in [
          AppLifecycleState.inactive,
          AppLifecycleState.hidden,
          AppLifecycleState.paused,
        ]) {
          binding.handleAppLifecycleStateChanged(s);
        }
        // 멈춘 동안 프레임이 서지 않아 pump 로 기다리지 않는다.
        await Future<void>.delayed(const Duration(seconds: 1));
        for (final s in [
          AppLifecycleState.hidden,
          AppLifecycleState.inactive,
          AppLifecycleState.resumed,
        ]) {
          binding.handleAppLifecycleStateChanged(s);
        }
      }
      await until(
        tester,
        () =>
            seen.contains(AppLifecycleState.paused) &&
            seen.last == AppLifecycleState.resumed,
        '배경 왕복 $tag',
        limit: const Duration(seconds: 90),
      );
      log('RESUMED $tag ${seen.map((s) => s.name).join(',')}');
    } finally {
      WidgetsBinding.instance.removeObserver(watch);
    }
  }

  testWidgets('앱이 뒤로 갔다 오면 — 거둬진 창구를 같은 원본·자리로 다시 세운다', (tester) async {
    expect(_long, isTrue, reason: '긴 영상(KASA_MEDIA_LONG)이 필요해요');
    final bridges = captureBridges();
    await pumpChat(
      tester,
      '긴 영상.\n\n${_block('6666666666666666', 12)}\n'
      '짧은 영상.\n\n${_block('1111111111111111', 12)}',
    );
    await cards(tester, ['long.mp4', 'demo clip.mp4']);

    // 틀던 것: 거둬졌으면 새 창구로 그 자리에서 이어 튼다.
    await tester.tap(find.text('long.mp4'));
    await until(
      tester,
      () =>
          (player(tester)?.value.isPlaying ?? false) &&
          player(tester)!.value.position > const Duration(seconds: 2),
      '긴 영상 재생',
    );
    final first = bridges.single;
    final old = player(tester)!;
    var awayAt = Duration.zero;
    await roundTrip(
      tester,
      'playing',
      reclaim: first,
      onPaused: () => awayAt = old.value.position,
    );
    // 새 플레이어가 서자마자 시계는 떠난 자리를 보인다 — video_player 는 찾기가 끝날 때까지 0 이다.
    await until(
      tester,
      () =>
          bridges.length == 2 &&
          player(tester) != null &&
          !identical(player(tester), old),
      '새 창구',
    );
    final shown = DateTime.now();
    expect(
      find.text(clockLabel(awayAt)),
      findsOneWidget,
      reason: '준비 중에도 그 자리',
    );
    // 새 플레이어는 찾아간 뒤에 튼다 — 처음 틀린 순간의 자리가 이어 받은 자리다.
    await until(
      tester,
      () => player(tester)?.value.isPlaying ?? false,
      '새 창구로 이어 재생',
    );
    final resumedAt = player(tester)!.value.position;
    log(
      'playing: away ${awayAt.inMilliseconds}ms → new player starts ${resumedAt.inMilliseconds}ms (ready ${DateTime.now().difference(shown).inMilliseconds}ms)',
    );
    expect(await first.alive(), isFalse);
    expect(await refused(first.uri), isTrue);
    expect(await bridges.last.alive(), isTrue);
    expect(bridges.last.uri.port, isNot(first.uri.port));
    expect(awayAt, greaterThan(const Duration(seconds: 2)));
    expect(
      (resumedAt - awayAt).inMilliseconds.abs(),
      lessThan(700),
      reason: '처음이 아니라 그 자리에서',
    );
    await idle(tester, const Duration(milliseconds: 1000));
    expect(
      player(tester)!.value.position,
      greaterThan(resumedAt),
      reason: '흘러간다',
    );
    await shot(tester, '10-bg-resumed-playing');

    // 멈춰 둔 것: 거둬졌으면 새로 세우되 멈춘 채.
    await tester.tap(find.byTooltip('일시정지'));
    await idle(tester, const Duration(milliseconds: 400));
    final pausedAt = player(tester)!.value.position;
    await roundTrip(tester, 'paused', reclaim: bridges.last);
    await until(
      tester,
      () => bridges.length == 3 && player(tester) != null,
      '새 창구',
    );
    final pausedShown = DateTime.now();
    expect(
      find.text(clockLabel(pausedAt)),
      findsOneWidget,
      reason: '준비 중에도 그 자리',
    );
    await until(tester, () => settled(tester), '새 창구로 멈춘 채 준비');
    final readyMs = DateTime.now().difference(pausedShown).inMilliseconds;
    await idle(tester, const Duration(milliseconds: 800));
    final p = player(tester)!.value;
    log(
      'paused: before ${pausedAt.inMilliseconds}ms → after ${p.position.inMilliseconds}ms playing ${p.isPlaying} (ready ${readyMs}ms)',
    );
    expect(p.isPlaying, isFalse);
    expect(
      (p.position - pausedAt).inMilliseconds.abs(),
      lessThan(600),
      reason: '멈춘 자리 그대로',
    );
    expect(find.byTooltip('재생'), findsOneWidget);
    expect(
      find.text(clockLabel(p.position)),
      findsOneWidget,
      reason: '시계도 그 자리',
    );
    await shot(tester, '11-bg-resumed-paused');

    // 거두지 않았으면 손대지 않는다.
    final kept = player(tester);
    await roundTrip(tester, 'alive');
    await idle(tester, const Duration(milliseconds: 800));
    expect(bridges, hasLength(3), reason: '살아 있는 창구는 그대로');
    expect(identical(player(tester), kept), isTrue);
    expect(player(tester)!.value.isPlaying, isFalse);
    await close(tester);

    // 끝난 것: 거둬졌어도 끝에 두고 다시 틀지 않는다.
    await tester.tap(find.text('demo clip.mp4'));
    await until(
      tester,
      () => player(tester)?.value.isPlaying ?? false,
      '짧은 영상 재생',
    );
    await until(
      tester,
      () => find.byTooltip('처음부터').evaluate().isNotEmpty,
      '끝까지',
      limit: const Duration(seconds: 6),
    );
    final ended = bridges.length;
    await roundTrip(tester, 'ended', reclaim: bridges.last);
    await until(
      tester,
      () => bridges.length == ended + 1 && settled(tester),
      '새 창구로 끝에 준비',
    );
    await idle(tester, const Duration(milliseconds: 800));
    final e = player(tester)!.value;
    log(
      'ended: after ${e.position.inMilliseconds}ms of ${e.duration.inMilliseconds}ms playing ${e.isPlaying}',
    );
    expect(e.isPlaying, isFalse, reason: '끝난 것은 다시 틀지 않는다');
    expect(find.byTooltip('처음부터'), findsOneWidget);
    await shot(tester, '12-bg-resumed-ended');
    await close(tester);
    for (final b in bridges) {
      expect(await b.alive(), isFalse);
    }
    expect(tester.takeException(), isNull);
  });
}

class _LifecycleWatch with WidgetsBindingObserver {
  _LifecycleWatch(this.on);
  final void Function(AppLifecycleState) on;

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) => on(state);
}
