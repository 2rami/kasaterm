import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/screens/shared_rooms.dart';
import 'package:kasaterm_mobile/shared_rooms_repository.dart';

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(() => Future<void>.delayed(const Duration(seconds: 1)));

  Future<void> until(WidgetTester tester, bool Function() ready) async {
    final deadline = DateTime.now().add(const Duration(seconds: 40));
    while (!ready()) {
      if (DateTime.now().isAfter(deadline)) fail('Shared room did not become ready');
      await tester.pump(const Duration(milliseconds: 100));
      await Future<void>.delayed(const Duration(milliseconds: 100));
    }
    await tester.pump(const Duration(milliseconds: 500));
  }

  testWidgets('native shared reader without a paired desktop and account revocation', (tester) async {
    final raw = '${jsonEncode({'type': 'user', 'message': {'content': '이 대화를 함께 볼 수 있나요?'}})}\n'
        '${jsonEncode({'type': 'assistant', 'message': {'content': '초대받은 대화는 폰에서도 읽을 수 있어요.\n\n원본 작업은 계속돼요.'}})}\n';
    final session = {
      'id': 'session1', 'title': '함께 보는 대화', 'harness': 'claude', 'revision': 1, 'generation': 'generation1',
      'bytes': utf8.encode(raw).length, 'cursor': 0, 'updated_at_ms': DateTime.now().millisecondsSinceEpoch,
      'caught_up': true, 'checkpoint': null, 'source_key': '', 'publisher_machine_id': '',
    };
    final room = {
      'id': 'room1', 'title': '프로젝트 공유방', 'owner_account': 'alice', 'owner_name': 'Alice', 'my_role': 'member',
      'allow_fork': false, 'revision': 1, 'updated_at_ms': DateTime.now().millisecondsSinceEpoch,
      'members': [
        {'account': 'alice', 'display_name': 'Alice', 'role': 'owner'},
        {'account': 'bob', 'display_name': 'Bob', 'role': 'member'},
      ],
      'sessions': [session], 'invites': <Object>[],
    };
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final requests = <String>[];
    server.listen((request) async {
      requests.add('${request.method} ${request.uri.path}');
      Object body = {'ok': false, 'error': 'shared_missing'};
      var status = 404;
      if (request.headers.value(HttpHeaders.authorizationHeader) != 'Bearer shared-fixture') {
        status = 401;
      } else if (request.uri.path == '/relay/shared-rooms') {
        status = 200;
        body = {'ok': true, 'rooms': [room]};
      } else if (request.uri.path == '/relay/shared-rooms/room1') {
        status = 200;
        body = {'ok': true, 'room': room};
      } else if (request.uri.path.endsWith('/content')) {
        final offset = int.parse(request.uri.queryParameters['offset']!);
        final end = int.parse(request.uri.queryParameters['end']!);
        status = 200;
        body = {'ok': true, 'content': {'generation': 'generation1', 'revision': 1, 'offset': offset,
          'next': end, 'end': end, 'raw': utf8.decode(utf8.encode(raw).sublist(offset, end))}};
      }
      await request.drain<void>();
      request.response.statusCode = status;
      request.response.headers.contentType = ContentType.json;
      request.response.headers.set(HttpHeaders.cacheControlHeader, 'no-store');
      request.response.write(jsonEncode(body));
      await request.response.close();
    });
    final repo = SharedRoomsRepository(AccountSession(origin: Uri.parse('http://127.0.0.1:${server.port}'),
      account: 'bob', deviceId: 'fixture-phone', token: 'shared-fixture'));
    try {
      await repo.load();
      expect(repo.desktop, isNull);
      await tester.pumpWidget(MaterialApp(theme: buildTheme(Brightness.light), home: SharedRoomsScreen(repository: repo)));
      await until(tester, () => find.text('프로젝트 공유방').evaluate().isNotEmpty);
      await binding.takeScreenshot('shared-rooms-native-list');
      await tester.tap(find.text('프로젝트 공유방'));
      await until(tester, () => find.byType(SharedRoomScreen).evaluate().isNotEmpty);
      await until(tester, () => find.text('함께 보는 대화').evaluate().isNotEmpty);
      await tester.tap(find.text('함께 보는 대화'));
      await until(tester, () => find.textContaining('초대받은 대화는', findRichText: true).evaluate().isNotEmpty);
      expect(find.byType(TextField), findsNothing);
      await binding.takeScreenshot('shared-rooms-native-reader');
      expect(requests.any((path) => path.contains('/content')), isTrue);
      expect(requests.every((path) => path.startsWith('GET /relay/shared-rooms')), isTrue);
      repo.close();
      await until(tester, () => find.text('현재 계정의 연결이 종료됐어요.').evaluate().isNotEmpty);
      expect(find.textContaining('초대받은 대화는', findRichText: true), findsNothing);
      await binding.takeScreenshot('shared-rooms-native-revoked');
      expect(tester.takeException(), isNull);
    } finally {
      await tester.pumpWidget(const SizedBox.shrink());
      repo.close();
      await server.close(force: true);
    }
  });
}
