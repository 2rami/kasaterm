import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/screens/share_screen.dart';
import 'package:kasaterm_mobile/server.dart';

/// KASA-share 첫 화면 — 날짜 폴더(그림 표지·파일 수)와 폴더 밖 낱장, 32MB 넘는 파일의 까닭.
void main() {
  testWidgets('KASA-share 목록', (tester) async {
    tester.view.physicalSize = const Size(390, 640);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    final now = DateTime.now().millisecondsSinceEpoch;
    Map<String, Object> file(String path, String kind, int size, int ago) => {
      'path': path,
      'name': path.contains('/') ? path.substring(path.indexOf('/') + 1) : path,
      'size': size,
      'modified_ms': now - ago,
      'kind': kind,
      'origin': 'MacBook Pro',
    };
    const h = 3600 * 1000;
    final body = {
      'ok': true,
      'name': 'KASA-share',
      'folders': [
        {
          'name': '2026-09-28-카사텀-쌍둥이-시안',
          'modified_ms': now - 20 * 60 * 1000,
          'files': [
            file('2026-09-28-카사텀-쌍둥이-시안/1-짝눈.png', 'image', 1205779, h),
            file('2026-09-28-카사텀-쌍둥이-시안/2-차분.png', 'image', 998000, h),
            file('2026-09-28-카사텀-쌍둥이-시안/고른 까닭.md', 'markdown', 812, h),
          ],
        },
        {
          'name': '2026-09-27-관문-로그인-흐름',
          'modified_ms': now - 26 * h,
          'files': [
            file('2026-09-27-관문-로그인-흐름/흐름.html', 'html', 18400, 26 * h),
            file('2026-09-27-관문-로그인-흐름/sub/녹화.mp4', 'video', 9800000, 26 * h),
          ],
        },
      ],
      'files': [
        file('회의 메모.md', 'markdown', 2300, 3 * h),
        file('빌드 로그.txt', 'text', 15, 5 * h),
        file('덤프.bin', 'other', 40000000, 50 * h),
      ],
    };
    final server = Server(
      Uri.parse('http://127.0.0.1:1/'),
      client: MockClient(
        (_) async => http.Response(
          jsonEncode(body),
          200,
          headers: {'content-type': 'application/json; charset=utf-8'},
        ),
      ),
    );
    await tester.pumpWidget(
      MaterialApp(
        theme: ThemeData(colorSchemeSeed: const Color(0xff4c6ef5)),
        home: ShareScreen(server: server),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    expect(find.text('카사텀-쌍둥이-시안'), findsOneWidget);
    expect(find.text('폴더 밖'), findsOneWidget);
    await expectLater(
      find.byType(ShareScreen),
      matchesGoldenFile('goldens/share_screen.png'),
    );
  });
}
