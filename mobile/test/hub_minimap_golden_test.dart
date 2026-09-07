import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/screens/hub.dart';
import 'package:kasaterm_mobile/server.dart';

/// 허브가 서버에 묻는 것마다 한 방 세 학생을 답한다 — 미니맵이 방 아래 붙는 모양을 굳힌다.
http.Response _answer(http.Request req) {
  final path = req.url.path;
  Object body;
  if (path.endsWith('term/panes')) {
    body = [
      {
        'id': '%1',
        'name': '아리스',
        'title': '폰 미니맵',
        'status': 'busy',
        'window': 0,
        'cwd': '/w',
        'color': '#7c9cff',
      },
      {
        'id': '%2',
        'name': '모모이',
        'title': '',
        'status': 'waiting',
        'kind': 'permission',
        'window': 0,
        'cwd': '/w',
        'color': '#ff9c7c',
      },
      {
        'id': '%3',
        'name': '',
        'title': '',
        'status': '',
        'window': 0,
        'cwd': '/w/shell',
      },
      // 탭 안에 숨은 학생 — 옛 서버는 배치에 탭을 안 실어 어느 칸에도 없다.
      {
        'id': '%4',
        'name': '세이아',
        'title': '',
        'status': 'waiting',
        'kind': 'question',
        'window': 0,
        'cwd': '/w',
        'color': '#b48cff',
      },
    ];
  } else if (path.endsWith('sessions')) {
    body = {
      'labels': ['게임개발부'],
    };
  } else if (path.endsWith('machines')) {
    body = {'machines': []};
  } else if (path.endsWith('windows')) {
    body = {
      'ok': true,
      'windows': [
        {
          'idx': 0,
          'active': true,
          'aspect': 1.9,
          'panes': [
            {'surface_id': '%1', 'x': 0, 'y': 0, 'w': 60, 'h': 100},
            {'surface_id': '%2', 'x': 60, 'y': 0, 'w': 40, 'h': 55},
            {'surface_id': '%3', 'x': 60, 'y': 55, 'w': 20, 'h': 45},
            // 목록에 없는 pane(맨 셸) — 빈 상자가 아니라 터미널 글리프로.
            {'surface_id': '%5', 'x': 80, 'y': 55, 'w': 20, 'h': 45},
          ],
        },
      ],
    };
  } else {
    return http.Response('', 404);
  }
  return http.Response(
    jsonEncode(body),
    200,
    headers: {'content-type': 'application/json'},
  );
}

void main() {
  testWidgets('허브 미니맵', (tester) async {
    tester.view.physicalSize = const Size(390 * 3, 520 * 3);
    tester.view.devicePixelRatio = 3;
    addTearDown(tester.view.reset);
    final server = Server(
      Uri.parse('http://127.0.0.1:1/'),
      client: MockClient((r) async => _answer(r)),
    );
    await tester.pumpWidget(
      MaterialApp(
        theme: ThemeData(colorSchemeSeed: const Color(0xff3b82f6)),
        home: HubScreen(server: server, onChangeAddress: () async {}),
      ),
    );
    await tester.pump();
    // 떠오르기(Appear)가 끝난 뒤를 찍는다 — 중간 프레임을 골든으로 굳히면 안 된다.
    await tester.pump(const Duration(milliseconds: 1200));
    expect(find.text('게임개발부'), findsOneWidget);
    expect(find.text('아리스'), findsNWidgets(2));
    // 숨은 탭 학생은 지도 밑 「탭 안」 줄에 서고, 목록에는 그대로 있다.
    expect(find.text('탭 안'), findsOneWidget);
    expect(find.text('세이아'), findsOneWidget);
    await expectLater(
      find.byType(HubScreen),
      matchesGoldenFile('goldens/hub_minimap.png'),
    );
  });
}
