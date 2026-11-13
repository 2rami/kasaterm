import 'dart:convert';
import 'dart:io';

import 'inline_result_fonts.dart';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/screens/conversation_view.dart';
import 'package:kasaterm_mobile/screens/hub.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/student_walk.dart';
import 'package:kasaterm_mobile/term_session.dart';

/// 데스크톱 `/term/walk` 가 주는 꼴 그대로 — 사이드바 프레임(합집합 크롭)을 가로로 이은 여섯 장.
final _sheet = File('test/fixtures/walk_arisu.png').readAsBytesSync();

http.Response? _walk(http.Request req) {
  final path = req.url.path;
  if (path.endsWith('term/walk/arisu.png')) {
    return http.Response.bytes(_sheet, 200, headers: {
      'content-type': 'image/png',
      'x-kasa-walk-frames': '6',
      'x-kasa-walk-ms': '140',
    });
  }
  // 프레임 수를 안 실은 답 — 시트를 몇 장으로 자를지 모르니 걷지 않는다.
  if (path.endsWith('term/walk/bare.png')) {
    return http.Response.bytes(_sheet, 200, headers: {'content-type': 'image/png'});
  }
  return null;
}

Server _server([http.Response Function(http.Request)? rest]) => Server(
  Uri.parse('http://127.0.0.1:1/'),
  client: MockClient((r) async => _walk(r) ?? rest?.call(r) ?? http.Response('', 404)),
);

http.Response _json(Object body) =>
    http.Response(jsonEncode(body), 200, headers: {'content-type': 'application/json'});

void main() {
  setUpAll(loadInlineResultFonts);

  test('걷기 시트는 프레임 수·간격과 함께 오고, 옛 서버·헤더 없는 답은 얼굴로 돌린다', () async {
    final server = _server();
    final got = await server.walkSheet('arisu');
    expect(got, isNotNull);
    expect((got!.frames, got.frameMs), (6, 140));
    expect(got.png, _sheet);
    expect(await server.walkSheet('old'), isNull, reason: '길이 없는 옛 서버는 404');
    expect(await server.walkSheet('bare'), isNull);
    // 다른 기기 칸은 그 기기로 건너가는 문으로 묻는다 — 얼굴(`avatar`)과 같은 길.
    expect(server.walk('arisu', machine: 'mini').path, '/m/mini/term/walk/arisu.png');
  });

  testWidgets('받기 전엔 얼굴, 받으면 사이드바 박자로 걷고 움직임 줄이기에선 첫 걸음에 선다', (tester) async {
    final server = _server();
    final sheets = WalkSheets();
    Widget walker(String slug, {bool still = false}) => MaterialApp(
      home: MediaQuery(
        data: MediaQueryData(disableAnimations: still),
        child: Center(
          child: StudentWalk(
            server: server,
            slug: slug,
            size: 30,
            fallback: const Text('얼굴'),
            sheets: sheets,
          ),
        ),
      ),
    );
    // 그림 풀기는 가짜 시계로는 안 끝난다 — 받기를 실제 시간 쪽에서 시작만 해 두고, 위젯은 그 받기를 기다린다.
    late Future<WalkSheet?> arisu, old;
    await tester.runAsync(() async {
      arisu = sheets.load(server, 'arisu', null);
      old = sheets.load(server, 'old', null);
    });
    await tester.pumpWidget(walker('arisu'));
    expect(find.text('얼굴'), findsOneWidget);
    final sheet = (await tester.runAsync(() => arisu))!;
    expect(sheet.frames, hasLength(6));
    expect({for (final f in sheet.frames) (f.width, f.height)}, {(86, 208)});
    await tester.pump();
    expect(find.text('얼굴'), findsNothing);
    final frames = sheet.frames;
    RawImage shown() => tester.widget<RawImage>(find.byType(RawImage));
    expect(shown().image, frames[0]);
    expect(shown().alignment, Alignment.bottomCenter);
    expect(tester.getSize(find.byType(RawImage)), const Size(30, 30));
    await tester.pump(const Duration(milliseconds: 150));
    expect(shown().image, frames[1]);
    await tester.pump(const Duration(milliseconds: 140 * 5));
    expect(shown().image, frames[0], reason: '여섯 걸음이 한 바퀴');

    await tester.pumpWidget(walker('arisu', still: true));
    await tester.pump(const Duration(milliseconds: 500));
    expect(shown().image, frames[0]);

    // 걷기 그림이 없는 학생·옛 서버 — 받기를 끝내도 얼굴 그대로다.
    expect(await tester.runAsync(() => old), isNull);
    await tester.pumpWidget(walker('old'));
    await tester.pump();
    expect(find.text('얼굴'), findsOneWidget);
    expect(find.byType(RawImage), findsNothing);
  });

  testWidgets('허브 미니맵 — 하는 중인 학생만 얼굴 상자에서 걷는다', (tester) async {
    tester.view.physicalSize = const Size(390 * 3, 360 * 3);
    tester.view.devicePixelRatio = 3;
    addTearDown(tester.view.reset);
    final server = _server((req) {
      final path = req.url.path;
      if (path.endsWith('term/panes')) {
        return _json([
          {'id': '%1', 'name': '아리스', 'slug': 'arisu', 'title': '걷는 칸', 'status': 'working',
            'window': 0, 'cwd': '/w', 'color': '#7c9cff'},
          {'id': '%2', 'name': '아리스', 'slug': 'arisu', 'title': '쉬는 칸', 'status': 'idle',
            'window': 0, 'cwd': '/w', 'color': '#7c9cff'},
        ]);
      }
      if (path.endsWith('sessions')) return _json({'labels': ['게임개발부']});
      if (path.endsWith('machines')) return _json({'machines': []});
      if (path.endsWith('windows')) {
        return _json({
          'ok': true,
          'windows': [
            {
              'idx': 0, 'active': true, 'aspect': 1.9,
              'panes': [
                {'surface_id': '%1', 'x': 0, 'y': 0, 'w': 50, 'h': 100},
                {'surface_id': '%2', 'x': 50, 'y': 0, 'w': 50, 'h': 100},
              ],
            },
          ],
        });
      }
      return http.Response('', 404);
    });
    // 앱이 함께 쓰는 캐시에 미리 받아 둔다 — 실제로도 한 번 받은 학생은 다시 설 때 첫 그림부터 걷는다.
    await tester.runAsync(() => walkSheets.load(server, 'arisu', null));
    await tester.pumpWidget(
      MaterialApp(
        theme: buildTheme(Brightness.light),
        home: HubScreen(server: server, onChangeAddress: () async {}),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 1200));
    expect(find.text('걷는 칸'), findsOneWidget);
    expect(find.byType(StudentWalk), findsOneWidget, reason: '쉬는 칸은 얼굴');
    expect(find.descendant(of: find.byType(StudentWalk), matching: find.byType(RawImage)), findsOneWidget);
    await expectLater(find.byType(HubScreen), matchesGoldenFile('goldens/hub_walk.png'));
  });

  testWidgets('대화 보기 — 하는 중 줄의 학생이 걷는다', (tester) async {
    tester.view.physicalSize = const Size(390 * 3, 360 * 3);
    tester.view.devicePixelRatio = 3;
    addTearDown(tester.view.reset);
    final raw = [
      {
        'type': 'user', 'sessionId': 's1', 'timestamp': '2026-11-13T03:10:00Z',
        'message': {'role': 'user', 'content': '폰에서도 걷게 해 줘'},
      },
    ].map(jsonEncode).join('\n');
    final server = _server((req) => req.url.path.endsWith('transcript-raw')
        ? _json({'ok': true, 'raw': raw, 'offset': 10, 'reset': true})
        : http.Response('', 404));
    const pane = Pane(
      id: '%1',
      name: '아리스',
      slug: 'arisu',
      title: '',
      status: 'working',
      window: 0,
      cwd: '/m',
      doing: 'Bash flutter test',
    );
    final session = TermSession(server, pane)..state = TermState.connected;
    await tester.runAsync(() => walkSheets.load(server, 'arisu', null));
    await tester.pumpWidget(
      MaterialApp(
        theme: buildTheme(Brightness.light),
        home: Scaffold(
          appBar: AppBar(title: const Text('아리스')),
          body: ConversationView(
            server: server,
            pane: pane,
            session: session,
            accent: const Color(0xff7c9cff),
            onTerminal: () {},
          ),
        ),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));
    expect(find.text('하는 중'), findsOneWidget);
    expect(find.byType(StudentWalk), findsOneWidget);
    expect(find.descendant(of: find.byType(StudentWalk), matching: find.byType(RawImage)), findsOneWidget);
    await expectLater(find.byType(Scaffold), matchesGoldenFile('goldens/conversation_walk.png'));
    await tester.pumpWidget(const SizedBox());
    session.dispose();
  });
}
