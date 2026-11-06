import 'dart:convert';
import 'dart:io';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:kasaterm_mobile/main.dart' show buildTheme;
import 'package:kasaterm_mobile/screens/desktop_settings.dart';
import 'package:kasaterm_mobile/server.dart';

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(() => Future<void>.delayed(const Duration(seconds: 1)));
  testWidgets('native lid switches, registration and background connection result', (tester) async {
    var on = false;
    var hotspot = false;
    var name = '검증용 폰';
    var status = '';
    final received = <Map<String, dynamic>>[];
    final http = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    http.listen((request) async {
      request.response.headers.contentType = ContentType.json;
      Object body;
      if (request.method == 'POST') {
        final args = jsonDecode(await utf8.decoder.bind(request).join()) as Map<String, dynamic>;
        received.add(args);
        switch (args['action']) {
          case 'lid-away-enabled': on = args['label'] == 'true';
          case 'lid-away-hotspot': hotspot = args['label'] == 'true';
          case 'lid-away-hotspot-ssid': name = args['label'] as String;
          case 'lid-away-hotspot-test': status = '연결 요청은 끝났지만 연결 확인은 안 됐어요';
        }
        body = {'ok': true};
      } else {
        body = {'lid_away': {'supported': true, 'on': on, 'minutes': 30, 'ready': true,
          'hotspot_on': hotspot, 'hotspot_ssid': name,
          'hotspot': {'busy': false, 'status': status}}};
      }
      request.response.write(jsonEncode(body));
      await request.response.close();
    });
    final server = Server(Uri.parse('http://127.0.0.1:${http.port}/'));
    Future<void> settle(Finder found) async {
      for (var i = 0; i < 80; i++) {
        await tester.pump(const Duration(milliseconds: 50));
        await Future<void>.delayed(const Duration(milliseconds: 100));
        if (found.evaluate().isNotEmpty && find.byType(LinearProgressIndicator).evaluate().isEmpty) break;
      }
      await tester.pumpAndSettle();
      expect(found, findsWidgets);
    }
    try {
      await tester.pumpWidget(MaterialApp(theme: buildTheme(Brightness.light),
        home: DesktopSettingsScreen(server: server, title: '검증용 맥북', machine: '~fixture-mac')));
      await settle(find.text('덮어도 마저 하고 잠자기'));
      await tester.tap(find.text('덮어도 마저 하고 잠자기'));
      await settle(find.text('30분'));
      await tester.tap(find.text('덮으면 폰 핫스팟 연결'));
      await settle(find.text('등록할 핫스팟 이름'));
      await binding.takeScreenshot('lid-hotspot-switches');
      await tester.tap(find.text('등록할 핫스팟 이름'));
      await tester.pumpAndSettle();
      await tester.enterText(find.byType(TextField).last, '새 폰 / 이름');
      await tester.pumpAndSettle();
      await binding.takeScreenshot('lid-hotspot-registration-keyboard');
      await tester.tap(find.text('저장'));
      await settle(find.text('새 폰 / 이름'));
      await tester.ensureVisible(find.text('지금 연결 확인'));
      await tester.tap(find.text('지금 연결 확인'));
      await settle(find.text('연결 요청은 끝났지만 연결 확인은 안 됐어요'));
      await binding.takeScreenshot('lid-hotspot-unverified');
      expect(received.map((r) => r['action']), ['lid-away-enabled', 'lid-away-hotspot', 'lid-away-hotspot-ssid', 'lid-away-hotspot-test']);
      expect(on && hotspot, isTrue);
      expect(name, '새 폰 / 이름');
      tester.platformDispatcher.textScaleFactorTestValue = 2;
      addTearDown(tester.platformDispatcher.clearTextScaleFactorTestValue);
      await tester.pumpAndSettle();
      await tester.ensureVisible(find.text('지금 연결 확인'));
      await binding.takeScreenshot('lid-hotspot-large-text');
      expect(tester.takeException(), isNull);
    } finally {
      await tester.pumpWidget(const SizedBox());
      await tester.pumpAndSettle();
      server.close();
      await http.close(force: true);
    }
  });
}
