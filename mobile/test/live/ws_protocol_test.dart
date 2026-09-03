// 실서버 상대 규약 검증. KASA_ROOT 가 없으면 통째로 건너뛴다.
//
//   KASA_ROOT=http://127.0.0.1:8765/ flutter test test/live/
//
// 사용자의 pane 을 미러하지 않는다 — 실행 중인 claude 입력창에 글자가 들어간다.
// 대신 새 웹 셸을 만들어 쓰고 끝나면 지운다.
@Tags(['live'])
library;

import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:kasaterm_mobile/grid.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:web_socket_channel/io.dart';

Future<void> waitFor(bool Function() done, {Duration timeout = const Duration(seconds: 6)}) async {
  final until = DateTime.now().add(timeout);
  while (!done()) {
    if (DateTime.now().isAfter(until)) fail('시간 안에 조건이 안 됐다');
    await Future<void>.delayed(const Duration(milliseconds: 100));
  }
}

void main() {
  final rootText = Platform.environment['KASA_ROOT'];
  if (rootText == null || rootText.isEmpty) {
    test('KASA_ROOT 없음', () {}, skip: 'KASA_ROOT=http://127.0.0.1:8765/ 로 실서버를 가리켜라');
    return;
  }
  final server = Server(Uri.parse(rootText));

  test('mobile/me · term/panes · sessions · machines 가 모양대로 온다', () async {
    final me = await server.me();
    expect(me.name, isA<String>());
    final panes = await server.panes();
    for (final p in panes) {
      expect(p.id, isNotEmpty);
      expect(p.status, isNotEmpty);
    }
    expect(await server.sessions(), isA<List<String>>());
    expect(await server.machines(), isA<List<Machine>>());
  });

  test('웹 셸 왕복: size → grid → binary 입력이 화면에 → text 프레임은 버려짐 → 정리', () async {
    final ch = IOWebSocketChannel.connect(server.wsUri(
      'term/ws',
      query: {'grid': '1', 'cwd': '/tmp'},
    ));
    final grid = Grid();
    String? id;
    bool? mirror;
    var frames = 0;
    final sub = ch.stream.listen((data) {
      if (data is! String) return;
      final m = jsonDecode(data);
      if (m is! Map) return;
      final msg = m.cast<String, Object?>();
      switch (msg['t']) {
        case 'size':
          id = msg['id'] as String?;
          mirror = msg['mirror'] as bool?;
          grid.apply({'cols': msg['cols'], 'rows': msg['rows']});
        case 'grid':
          grid.apply(msg);
          frames++;
      }
    });
    try {
      await waitFor(() => id != null);
      expect(mirror, isFalse);
      expect(id, startsWith('web-'));
      await waitFor(() => frames > 0);
      expect(grid.cols, greaterThan(0));

      bool seen(String s) => List.generate(grid.rows, grid.rowText).any((row) => row.contains(s));

      ch.sink.add(Uint8List.fromList(utf8.encode('echo 가나다\r')));
      await waitFor(() => seen('가나다'));

      // 같은 글을 text 프레임으로 보내면 서버가 제어 JSON 으로 읽고 버린다 —
      // 화면에 절대 나오면 안 된다.
      ch.sink.add('echo ZZZTEXT\r');
      await Future<void>.delayed(const Duration(seconds: 1));
      expect(seen('ZZZTEXT'), isFalse);
    } finally {
      await sub.cancel();
      await ch.sink.close();
      if (id != null) {
        final res = await http.delete(server.uri('term/session', query: {'pane': id!}));
        expect(res.statusCode, lessThan(500));
      }
    }
  });
}
