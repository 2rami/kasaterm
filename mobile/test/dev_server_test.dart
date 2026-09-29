import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/screens/dev_server.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:url_launcher_platform_interface/link.dart';
import 'package:url_launcher_platform_interface/url_launcher_platform_interface.dart';

import 'net_tcp_test.dart' show fakeDesktop;

class _Launcher extends UrlLauncherPlatform {
  final launched = <(String, PreferredLaunchMode)>[];

  @override
  LinkDelegate? get linkDelegate => null;

  @override
  Future<bool> launchUrl(String url, LaunchOptions options) async {
    launched.add((url, options.mode));
    return true;
  }
}

void main() {
  late _Launcher launcher;
  setUp(() {
    launcher = _Launcher();
    UrlLauncherPlatform.instance = launcher;
  });

  testWidgets('localhost 가 아닌 보여 주기 링크도 앱 안 Safari 화면으로', (tester) async {
    final server = Server(Uri.parse('http://127.0.0.1:9/'));
    addTearDown(server.close);
    final nav = GlobalKey<NavigatorState>();
    await tester.pumpWidget(MaterialApp(navigatorKey: nav, home: const SizedBox()));
    await openShownLink(
      nav.currentState!,
      server,
      Uri.parse('https://abc.trycloudflare.com/x'),
    );
    expect(launcher.launched, [
      ('https://abc.trycloudflare.com/x', PreferredLaunchMode.inAppBrowserView),
    ]);
  });

  testWidgets('데스크톱 localhost 는 입구를 세워 앱 안 Safari 로 열고, 화면을 닫으면 입구도 닫는다', (
    tester,
  ) async {
    final (desktop, _) = (await tester.runAsync(fakeDesktop))!;
    // 이 맥에서 아무도 안 듣는 번호 — 0.0.0.0 에 뜬 서버가 있으면 입구를 닫아도 그쪽이 받는다.
    final port = (await tester.runAsync(() async {
      final probe = await ServerSocket.bind(InternetAddress.anyIPv6, 0);
      final free = probe.port;
      await probe.close();
      return free;
    }))!;
    addTearDown(() => desktop.close(force: true));
    final server = Server(Uri.parse('http://127.0.0.1:${desktop.port}/'));
    addTearDown(server.close);
    final nav = GlobalKey<NavigatorState>();
    await tester.pumpWidget(MaterialApp(navigatorKey: nav, home: const SizedBox()));
    final opening = openShownLink(
      nav.currentState!,
      server,
      Uri.parse('http://0.0.0.0:$port/deals/7?tab=notes'),
    );
    await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 200)));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    expect(find.text('관문 경유'), findsOneWidget, reason: '열기 전에 길을 보인다');
    expect(launcher.launched, isEmpty);

    await tester.pump(const Duration(milliseconds: 400));
    await tester.pump();
    expect(launcher.launched, hasLength(1));
    final (url, mode) = launcher.launched.single;
    expect(mode, PreferredLaunchMode.inAppBrowserView);
    final opened = Uri.parse(url);
    expect(opened.host, 'localhost');
    expect(opened.port, port, reason: '데스크톱과 같은 번호가 먼저 — 로그인·저장소가 출처(포트)에 묶인다');
    expect(opened.path, '/deals/7');
    expect(opened.query, 'tab=notes');
    expect(find.text('다시 열기'), findsOneWidget);

    // Safari 화면이 닫혀도 이 화면이 있는 동안은 그 주소가 산다.
    await tester.runAsync(() async {
      final s = await Socket.connect(InternetAddress.loopbackIPv4, opened.port);
      s.destroy();
    });

    nav.currentState!.pop();
    await tester.pumpAndSettle();
    await opening;
    expect(find.byType(DevServerScreen), findsNothing);
    await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 50)));
    await tester.pump();
    await tester.runAsync(() async {
      await expectLater(
        Socket.connect(InternetAddress.loopbackIPv4, opened.port),
        throwsA(isA<SocketException>()),
      );
    });
  });
}
