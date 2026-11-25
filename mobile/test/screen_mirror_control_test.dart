import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/screen_session.dart';
import 'package:kasaterm_mobile/screens/controls.dart' show KeyRow, SendButton;
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
  testWidgets('보기는 아무것도 안 보내고, 터치는 그 자리·트랙패드는 커서 자리를 누르며 두 손가락이 휠이다', (tester) async {
    tester.view.physicalSize = const Size(390, 844);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    final heard = <Map<String, Object?>>[];
    final http = (await tester.runAsync(() => HttpOverrides.runWithHttpOverrides(() async {
      final http = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
      http.listen((request) async {
        if (!WebSocketTransformer.isUpgradeRequest(request)) {
          request.response.statusCode = HttpStatus.notFound;
          await request.response.close();
          return;
        }
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

    Future<void> settle() async {
      await tester.pump(const Duration(milliseconds: 100));
      for (var i = 0; i < 10; i++) {
        await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 20)));
      }
    }

    final box = tester.getRect(picture);
    expect(box.width / box.height, closeTo(2, 0.01));
    await tester.tapAt(box.topLeft + Offset(box.width * 0.25, box.height * 0.5));
    await tester.pump(const Duration(milliseconds: 500));
    expect(heard, isEmpty, reason: '보기에서 누른 것이 그 기기로 갔다');

    // 터치 — 누른 자리 클릭, 두 손가락 끌기는 휠.
    await tester.tap(find.text('터치'));
    await tester.pump();
    final touchBox = tester.getRect(_screen);
    await tester.tapAt(touchBox.topLeft + Offset(touchBox.width * 0.25, touchBox.height * 0.5));
    await settle();
    expect(find.text('클릭'), findsOneWidget, reason: '입력 표시가 떠야 한다');
    var pointers = heard.where((m) => m['t'] == 'pointer').toList();
    expect(pointers.map((m) => m['b']), [1, 0]);
    expect(pointers.first['x'] as double, closeTo(0.25, 0.01));
    expect(pointers.first['y'] as double, closeTo(0.5, 0.01));
    heard.clear();
    // 두 손가락을 위로 50px — 그림 상자 폭 390 이 화면 2000 포인트라 약 256 포인트 굴린다.
    final a = await tester.startGesture(touchBox.center - const Offset(30, 0));
    final b = await tester.startGesture(touchBox.center + const Offset(30, 0), pointer: 7);
    for (var i = 0; i < 5; i++) {
      await a.moveBy(const Offset(0, -10));
      await b.moveBy(const Offset(0, -10));
    }
    await a.up();
    await b.up();
    await settle();
    final wheel = heard.where((m) => m['t'] == 'scroll').fold<int>(0, (sum, m) => sum + (m['dy'] as int));
    expect(wheel, inInclusiveRange(-260, -250));
    expect(heard.where((m) => m['t'] == 'pointer'), isEmpty, reason: '움직인 두 손가락은 누르지 않는다');
    heard.clear();

    // 트랙패드 — 손가락이 어디 있든 커서는 옮긴 만큼만 간다.
    await tester.tap(find.text('트랙패드'));
    await tester.pump();
    final padBox = tester.getRect(_screen);
    final slide = await tester.startGesture(padBox.bottomRight - const Offset(5, 5));
    await slide.moveBy(Offset(padBox.width * 0.05, 0));
    await slide.moveBy(Offset(padBox.width * 0.05, 0));
    await slide.up();
    await tester.tapAt(padBox.topLeft + const Offset(5, 5));
    await settle();
    pointers = heard.where((m) => m['t'] == 'pointer').toList();
    // 커서는 두 손가락의 첫 손가락 자리(가운데에서 왼쪽 30px)에 있었고 트랙패드가 폭의 10% 옮겼다.
    expect(pointers.last['x'] as double, closeTo(0.5 - 30 / touchBox.width + 0.1, 0.01),
      reason: '탭은 누른 자리가 아니라 커서 자리를 누른다');
    expect(pointers.last['y'] as double, closeTo(0.5, 0.01));
    expect(pointers.reversed.take(2).map((m) => m['b']), [0, 1]);
    heard.clear();

    await tester.tap(find.text('드래그'));
    await tester.tap(find.text('shift'));
    final drag = await tester.startGesture(padBox.center);
    await drag.moveBy(const Offset(60, 10));
    await tester.pump();
    await drag.cancel();
    await tester.pump();
    await tester.tap(find.text('shift'));
    await tester.ensureVisible(find.text('ctrl'));
    await tester.tap(find.text('ctrl'));
    await tester.scrollUntilVisible(find.byIcon(Icons.keyboard_arrow_left), 120,
      scrollable: find.descendant(of: find.byType(KeyRow), matching: find.byType(Scrollable)));
    await tester.tap(find.byIcon(Icons.keyboard_arrow_left));
    await settle();
    final dragged = heard.where((m) => m['t'] == 'pointer').toList();
    expect(dragged.first['b'], 1);
    expect(dragged.first['m'], 1);
    expect(dragged.last['b'], 0, reason: '손짓이 취소되어도 PC 마우스를 놓아야 한다');
    expect(dragged.last['m'], 1, reason: '놓는 순간에도 끌기의 수정키를 유지한다');
    expect(heard.where((m) => m['t'] == 'scroll'), isEmpty);
    expect(heard.where((m) => m['t'] == 'key').single, {'t': 'key', 'k': 'ArrowLeft', 'm': 3});
    heard.clear();

    // 글 넣기 — 폰 자판이 조합을 끝낸 글을 통째로.
    await tester.scrollUntilVisible(find.byIcon(Icons.keyboard_alt_outlined), -120,
      scrollable: find.descendant(of: find.byType(KeyRow), matching: find.byType(Scrollable)));
    await tester.tap(find.byIcon(Icons.keyboard_alt_outlined));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField).last, '안녕 hello');
    await tester.pump();
    await tester.tap(find.byType(SendButton).last);
    await tester.pumpAndSettle();
    await settle();
    expect(heard.where((m) => m['t'] == 'text').single, {'t': 'text', 's': '안녕 hello'});
    heard.clear();

    await tester.tap(find.text('보기'));
    await tester.pump();
    await tester.tapAt(tester.getRect(_screen).center);
    await settle();
    expect(heard, isEmpty, reason: '보기로 돌아오면 다시 아무것도 안 보낸다');
    await tester.pumpWidget(const SizedBox());
  });
}
