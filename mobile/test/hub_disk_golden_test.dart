import 'dart:convert';

import 'inline_result_fonts.dart';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/screens/hub.dart';
import 'package:kasaterm_mobile/server.dart';

const _gib = 1 << 30;

/// 학생 하나가 도는 기기의 디스크가 위험 문턱 아래다 — 기기 머리 밑에 경고 카드가 선다.
http.Response _answer(http.Request req) {
  final path = req.url.path;
  Object body;
  if (path.endsWith('term/panes')) {
    body = [
      {
        'id': '%1',
        'name': '아리스',
        'title': '디스크 지키기',
        'status': 'working',
        'window': 0,
        'cwd': '/w',
        'color': '#7c9cff',
      },
    ];
  } else if (path.endsWith('term/disk')) {
    body = {
      'free': 9 * _gib ~/ 2,
      'total': 926 * _gib,
      'level': 'critical',
      'warn_free': 20 * _gib,
      'critical_free': 5 * _gib,
      'auto_prune': true,
      'big': [
        {'label': '/var/folders 임시', 'path': '/private/var/folders/zf/x', 'bytes': 74 * _gib},
        {'label': '~/.local/state/kasaterm-preview', 'path': '/Users/x/.local/state/kasaterm-preview', 'bytes': 57 * _gib},
      ],
      'last_pass': {'at': 0, 'reason': 'pressure', 'freed': 12 * _gib, 'removed': 3, 'skipped': 1},
    };
  } else if (path.endsWith('sessions')) {
    body = {
      'labels': ['맥미니'],
    };
  } else if (path.endsWith('machines')) {
    body = {'machines': []};
  } else if (path.endsWith('windows')) {
    body = {'ok': true, 'windows': []};
  } else {
    return http.Response('', 404);
  }
  return http.Response(jsonEncode(body), 200, headers: {'content-type': 'application/json'});
}

void main() {
  setUpAll(loadInlineResultFonts);
  testWidgets('허브 디스크 경고', (tester) async {
    tester.view.physicalSize = const Size(390 * 3, 520 * 3);
    tester.view.devicePixelRatio = 3;
    addTearDown(tester.view.reset);
    final server = Server(Uri.parse('http://127.0.0.1:1/'), client: MockClient((r) async => _answer(r)));
    await tester.pumpWidget(
      MaterialApp(
        theme: buildTheme(Brightness.light),
        home: HubScreen(server: server, onChangeAddress: () async {}),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 1200));
    expect(find.text('디스크 4.5GB 남음'), findsOneWidget);
    expect(
      find.text('큰 곳 · /var/folders 임시 74GB · ~/.local/state/kasaterm-preview 57GB\n빌드 산출물 12GB 지움'),
      findsOneWidget,
    );
    await expectLater(find.byType(HubScreen), matchesGoldenFile('goldens/hub_disk.png'));
  });
}
