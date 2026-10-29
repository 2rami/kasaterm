import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/screen_session.dart';
import 'package:kasaterm_mobile/screens/screen_mirror.dart';
import 'package:kasaterm_mobile/server.dart';

/// 2×1 PNG — 가로가 두 배인 그림이라 상자 비율이 드러난다.
final _picture = base64Decode(
  'iVBORw0KGgoAAAANSUhEUgAAAAIAAAABCAYAAAD0In+KAAAADklEQVR4nGP4z8AAQv8BD/kD/YURmXYAAAAASUVORK5CYII=',
);

class _NativeHttp extends HttpOverrides {}

Uint8List _frame(int seq) {
  final meta = utf8.encode(jsonEncode({'w': 2, 'h': 1, 'd': 1}));
  final head = ByteData(6)
    ..setUint32(0, seq)
    ..setUint16(4, meta.length);
  return Uint8List.fromList([...head.buffer.asUint8List(), ...meta, ..._picture]);
}

/// 받은 화면 그림 — 다른 RawImage(쌍둥이 그림 등)와 가른다.
final _screen = find.byWidgetPredicate((w) => w is RawImage && w.image?.width == 2);

void main() {
  testWidgets('조작에서 탭은 그 자리 클릭, 끌기는 휠 — 보기에서는 아무것도 안 보낸다', (tester) async {
    tester.view.physicalSize = const Size(390, 844);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    final heard = <Map<String, Object?>>[];
    final http = (await tester.runAsync(() => HttpOverrides.runWithHttpOverrides(() async {
      final http = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
      http.listen((request) async {
        final ws = await WebSocketTransformer.upgrade(request);
        ws.listen((data) {
          if (data is! String) return;
          final m = (jsonDecode(data) as Map).cast<String, Object?>();
          if (m['t'] != 'ack') heard.add(m);
        });
        ws.add(jsonEncode({
          't': 'hello',
          'display': 1,
          'displays': [
            {'id': 1, 'width': 2000, 'height': 1000, 'main': true},
          ],
        }));
        ws.add(_frame(1));
      });
      return http;
    }, _NativeHttp())))!;
    addTearDown(() => http.close(force: true));

    // 소켓·그림 풀기는 진짜 시간에서 — 시험 시계 안에서는 악수가 끝나지 않는다.
    final server = Server(Uri.parse('http://127.0.0.1:${http.port}/'));
    final session = (await tester.runAsync(() => HttpOverrides.runWithHttpOverrides(() async {
      final session = ScreenSession(server);
      session.connect();
      for (var i = 0; i < 100 && session.image == null; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 20));
      }
      return session;
    }, _NativeHttp())))!;
    expect(session.image, isNotNull, reason: '그림이 와야 시험이 된다');
    await tester.pumpWidget(MaterialApp(home: ScreenMirrorScreen(server: server, title: '맥북', session: session)));
    await tester.pump();
    final picture = _screen;
    expect(picture, findsOneWidget, reason: '그림이 와야 시험이 된다');

    final box = tester.getRect(picture);
    expect(box.width / box.height, closeTo(2, 0.01));
    await tester.tapAt(box.topLeft + Offset(box.width * 0.25, box.height * 0.5));
    await tester.pump(const Duration(milliseconds: 500));
    expect(heard, isEmpty, reason: '보기에서 누른 것이 그 기기로 갔다');

    await tester.tap(find.byTooltip('조작하기'));
    await tester.pump();
    final controlBox = tester.getRect(_screen);
    await tester.tapAt(controlBox.topLeft + Offset(controlBox.width * 0.25, controlBox.height * 0.5));
    await tester.pump(const Duration(milliseconds: 100));
    // 위로 50px 끌기 — 그림 상자 폭 390 이 화면 2000 포인트라 약 256 포인트 굴린다.
    await tester.dragFrom(controlBox.center, const Offset(0, -50));
    await tester.pump(const Duration(milliseconds: 100));
    await tester.tap(find.byTooltip('보기로'));
    await tester.pump();
    for (var i = 0; i < 20; i++) {
      await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 20)));
    }

    final pointers = heard.where((m) => m['t'] == 'pointer').toList();
    expect(pointers.map((m) => m['b']), [1, 0]);
    expect(pointers.first['x'] as double, closeTo(0.25, 0.01));
    expect(pointers.first['y'] as double, closeTo(0.5, 0.01));
    final wheel = heard.where((m) => m['t'] == 'scroll').fold<int>(0, (sum, m) => sum + (m['dy'] as int));
    expect(wheel, inInclusiveRange(-260, -200));
    await tester.pumpWidget(const SizedBox());
  });
}
