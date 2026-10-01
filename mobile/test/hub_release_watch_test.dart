import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/app_release.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/screens/hub.dart';
import 'package:kasaterm_mobile/server.dart';

class _CountingServer extends Server {
  _CountingServer() : super(Uri.parse('http://127.0.0.1:1/'), client: MockClient((_) async => http.Response('', 404)));

  int asked = 0;

  @override
  Future<AppRelease?> latestRelease() async {
    asked++;
    return null;
  }
}

void main() {
  testWidgets('앱을 켜 둔 채 쓰는 동안에도 새 판을 다시 묻고, 뒤로 가면 멈춘다', (tester) async {
    final server = _CountingServer();
    await tester.pumpWidget(MaterialApp(
      theme: buildTheme(Brightness.light),
      home: HubScreen(server: server, onChangeAddress: () async {}),
    ));
    await tester.pump();
    expect(server.asked, 1);
    await tester.pump(const Duration(seconds: 61));
    expect(server.asked, 3);

    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.hidden);
    await tester.pump(const Duration(seconds: 120));
    expect(server.asked, 3);

    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pump();
    expect(server.asked, 4);
    await tester.pumpWidget(const SizedBox());
  });
}
