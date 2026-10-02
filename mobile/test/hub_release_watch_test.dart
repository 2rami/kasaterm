import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/app_release.dart';
import 'package:kasaterm_mobile/background_grace.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/screens/hub.dart';
import 'package:kasaterm_mobile/server.dart';

/// 관문 대신 — 허브가 물을 때마다 무엇을 들고 물었는지 적고, 답은 시험이 정한다.
class _WatchServer extends Server {
  _WatchServer() : super(Uri.parse('http://127.0.0.1:1/'), client: MockClient((_) async => http.Response('', 404)));

  final asked = <String?>[];
  final answers = <Completer<ReleaseWatch?>>[];

  @override
  Future<ReleaseWatch?> watchRelease({String? have, Duration hold = const Duration(seconds: 40)}) {
    asked.add(have);
    answers.add(Completer());
    return answers.last.future;
  }
}

ReleaseWatch _build(String build, {bool waits = true}) =>
    ReleaseWatch(AppRelease('1.0.0', build, Uri.parse('itms-services://?action=download-manifest')), waits: waits);

void main() {
  // 뒤에서 버틸 시간을 주지 않는 iOS 처럼 — 그 길은 예전 그대로 곧바로 멈춘다.
  const channel = MethodChannel('kasaterm/background');
  TestWidgetsFlutterBinding.ensureInitialized();
  final messenger = TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
  setUp(() {
    BackgroundGrace.instance
      ..attach()
      ..resetForTest();
    messenger.setMockMethodCallHandler(channel, (call) async => call.method == 'begin' ? false : null);
  });
  tearDown(() => messenger.setMockMethodCallHandler(channel, null));

  testWidgets('관문에 늘 하나 매달려 판이 바뀌면 곧바로 다시 묻고, 뒤로 가면 멈춘다', (tester) async {
    final server = _WatchServer();
    await tester.pumpWidget(MaterialApp(
      theme: buildTheme(Brightness.light),
      home: HubScreen(server: server, onChangeAddress: () async {}),
    ));
    await tester.pump();
    expect(server.asked, [null]);

    server.answers.last.complete(_build('2610011500'));
    await tester.pump();
    expect(server.asked, [null, '2610011500']);

    // 관문이 붙들다 새 판으로 답하면 그 판을 들고 곧바로 다시 매달린다.
    server.answers.last.complete(_build('2610011600'));
    await tester.pump();
    expect(server.asked.last, '2610011600');
    expect(server.asked, hasLength(3));

    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.hidden);
    await tester.pump();
    expect(BackgroundGrace.instance.live, isFalse);
    server.answers.last.complete(_build('2610011600'));
    await tester.pump(const Duration(minutes: 2));
    expect(server.asked, hasLength(3));

    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pump();
    expect(server.asked, hasLength(4));
    expect(server.asked.last, isNull);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('다른 앱에 잠깐 다녀오는 동안은 계속 매달려 있고, 받은 시간이 다 되면 멈춘다', (tester) async {
    messenger.setMockMethodCallHandler(channel, (call) async => call.method == 'begin' ? true : null);
    final server = _WatchServer();
    await tester.pumpWidget(MaterialApp(
      theme: buildTheme(Brightness.light),
      home: HubScreen(server: server, onChangeAddress: () async {}),
    ));
    await tester.pump();
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.hidden);
    await tester.pump();
    server.answers.last.complete(_build('2610011500'));
    await tester.pump(const Duration(seconds: 5));
    expect(server.asked, [null, '2610011500']);

    // 돌아와도 새로 붙지 않는다 — 이미 매달린 줄이 그대로다.
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pump();
    expect(server.asked, hasLength(2));

    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.hidden);
    await tester.pump(BackgroundGrace.cap);
    server.answers.last.complete(_build('2610011600'));
    await tester.pump(const Duration(minutes: 2));
    expect(server.asked, hasLength(2));
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pump();
    expect(server.asked, hasLength(3));
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('붙들어 주지 않는 옛 관문·끊김이면 쉬었다가 다시 묻는다', (tester) async {
    final server = _WatchServer();
    await tester.pumpWidget(MaterialApp(
      theme: buildTheme(Brightness.light),
      home: HubScreen(server: server, onChangeAddress: () async {}),
    ));
    await tester.pump();
    server.answers.last.complete(_build('2610011500', waits: false));
    await tester.pump(const Duration(seconds: 1));
    expect(server.asked, hasLength(1));
    await tester.pump(HubScreen.releaseRetry);
    expect(server.asked, [null, '2610011500']);

    server.answers.last.complete(null);
    await tester.pump(HubScreen.releaseRetry);
    expect(server.asked, [null, '2610011500', '2610011500']);
    await tester.pumpWidget(const SizedBox());
  });
}
