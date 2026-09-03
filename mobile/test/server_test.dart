import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/server.dart';

const slug = 'abcdefghij0123456789abcde';
const publicRoot = 'https://kasaterm.debimarlene.com/u/$slug/';
// http.Response 는 charset 이 없으면 본문을 latin1 로 인코딩해 한글에서 죽는다.
const _json = {'content-type': 'application/json; charset=utf-8'};

void main() {
  group('Server.parse', () {
    test('스킴이 없으면 https 를 붙이고 뒤 / 를 보장한다', () {
      final u = Server.parse('kasaterm.debimarlene.com/u/$slug');
      expect(u.toString(), publicRoot);
    });

    test('쿼리·조각은 버리고 로컬 주소는 http 그대로', () {
      expect(Server.parse('http://127.0.0.1:8765?x=1#y').toString(), 'http://127.0.0.1:8765/');
    });

    test('주소가 아니면 null', () {
      expect(Server.parse(''), isNull);
      expect(Server.parse('   '), isNull);
      expect(Server.parse('://'), isNull);
    });
  });

  group('Server.uri', () {
    final s = Server(Uri.parse(publicRoot));

    test('pane id 의 % 는 %25 로 나가고 slug 아래 상대경로다', () {
      final u = s.uri('term/ws', query: {'pane': '%3', 'grid': '1'});
      expect(u.path, '/u/$slug/term/ws');
      expect(u.query, 'pane=%253&grid=1');
    });

    test('ws 스킴은 http/https 를 따라간다', () {
      expect(s.wsUri('term/ws', query: {'grid': '1'}).scheme, 'wss');
      final local = Server(Uri.parse('http://127.0.0.1:8765/'));
      expect(local.wsUri('term/ws', query: {'grid': '1'}).scheme, 'ws');
    });

    test('다른 기계는 m/<이름>/ 접두가 붙는다', () {
      final u = s.uri('term/panes', machine: '맥미니');
      expect(u.path, '/u/$slug/m/${Uri.encodeComponent('맥미니')}/term/panes');
    });

    test('describe 는 slug 를 가린다', () {
      expect(s.describe(), 'kasaterm.debimarlene.com/u/•••/');
      expect(s.describe().contains(slug), isFalse);
      expect(Server(Uri.parse('http://127.0.0.1:8765/')).describe(), '127.0.0.1:8765/');
    });
  });

  group('요청', () {
    test('실패 문구에 slug 가 없다', () async {
      final s = Server(Uri.parse(publicRoot), client: MockClient((_) async => http.Response('nope', 500)));
      try {
        await s.me();
        fail('예외가 나야 한다');
      } on ServerException catch (e) {
        expect(e.message.contains(slug), isFalse);
        expect(e.message, contains('500'));
      }
    });

    test('연결 자체가 안 될 때도 문구에 slug 가 없다', () async {
      final s = Server(Uri.parse(publicRoot), client: MockClient((_) async => throw http.ClientException('boom', Uri.parse(publicRoot))));
      try {
        await s.panes();
        fail('예외가 나야 한다');
      } on ServerException catch (e) {
        expect(e.message.contains(slug), isFalse);
      }
    });

    test('panes·sessions·machines 를 모양대로 읽는다', () async {
      final s = Server(Uri.parse(publicRoot), client: MockClient((req) async {
        final path = req.url.path;
        if (path.endsWith('/term/panes')) {
          return http.Response(
            jsonEncode([
              {'id': '%3', 'name': '아리스', 'title': '앱', 'status': 'waiting', 'slug': 'arisu', 'window': 1, 'cwd': '/x', 'color': '#4a90e2'},
            ]),
            200,
            headers: _json,
          );
        }
        if (path.endsWith('/sessions')) {
          return http.Response(jsonEncode({'labels': ['', '아이폰']}), 200, headers: _json);
        }
        if (path.endsWith('/machines')) {
          return http.Response(
            jsonEncode({
              'machines': [
                {
                  'label': '맥미니',
                  'online': true,
                  'panes': [
                    {'id': '%1', 'name': '유즈', 'status': 'idle', 'window': 0},
                  ],
                },
                {'label': '집', 'online': false, 'panes': []},
              ],
            }),
            200,
            headers: _json,
          );
        }
        return http.Response('?', 404);
      }));
      final panes = await s.panes();
      expect(panes.single.isWaiting, isTrue);
      expect(panes.single.machine, isNull);
      expect(await s.sessions(), ['', '아이폰']);
      final machines = await s.machines();
      expect(machines.length, 2);
      expect(machines.first.panes.single.machine, '맥미니');
      expect(machines.last.online, isFalse);
    });

    test('send 는 JSON 으로 글과 submit 을 보낸다', () async {
      http.Request? seen;
      final s = Server(Uri.parse(publicRoot), client: MockClient((req) async {
        seen = req;
        return http.Response('{"ok":true}', 200);
      }));
      await s.send('%3', '안녕', machine: '맥미니');
      expect(seen!.url.path, '/u/$slug/m/${Uri.encodeComponent('맥미니')}/send');
      expect(seen!.url.query, 'surface=%253');
      expect(jsonDecode(seen!.body), {'text': '안녕', 'submit': true});
    });

    test('shot 은 503 이면 null', () async {
      final s = Server(Uri.parse(publicRoot), client: MockClient((_) async => http.Response('', 503)));
      expect(await s.shot('%3'), isNull);
    });
  });
}
