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
  kindTests();
  group('Server.parse', () {
    test('스킴이 없으면 https 를 붙이고 뒤 / 를 보장한다', () {
      final u = Server.parse('kasaterm.debimarlene.com/u/$slug');
      expect(u.toString(), publicRoot);
    });

    test('쿼리·조각은 버리고 로컬 주소는 http 그대로', () {
      expect(
        Server.parse('http://127.0.0.1:8765?x=1#y').toString(),
        'http://127.0.0.1:8765/',
      );
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
      expect(
        Server(Uri.parse('http://127.0.0.1:8765/')).describe(),
        '127.0.0.1:8765/',
      );
    });
  });

  group('브라우징 기기', () {
    test('registerDevice 는 mobile/device 에 논리 크기·배율·플랫폼을 싣고 id 를 받는다', () async {
      http.Request? seen;
      final s = Server(
        Uri.parse(publicRoot),
        client: MockClient((req) async {
          seen = req;
          return http.Response(
            jsonEncode({'ok': true, 'id': 'phone:geono'}),
            200,
            headers: _json,
          );
        }),
      );
      final id = await s.registerDevice(
        const DeviceInfo(
          width: 393,
          height: 852,
          dpr: 3,
          platform: 'ios',
          model: 'iPhone 15',
        ),
      );
      expect(id, 'phone:geono');
      expect(seen!.method, 'POST');
      expect(seen!.url.path, '/u/$slug/mobile/device');
      expect(jsonDecode(seen!.body), {
        'width': 393,
        'height': 852,
        'dpr': 3.0,
        'platform': 'ios',
        'model': 'iPhone 15',
      });
    });

    test('registerDevice 는 ok 가 아니면 예외(slug 없이)', () async {
      final s = Server(
        Uri.parse(publicRoot),
        client: MockClient(
          (_) async => http.Response('{"ok":false}', 200, headers: _json),
        ),
      );
      try {
        await s.registerDevice(
          const DeviceInfo(width: 1, height: 1, dpr: 1, platform: 'web'),
        );
        fail('예외가 나야 한다');
      } on ServerException catch (e) {
        expect(e.message.contains(slug), isFalse);
      }
    });

    test('browseDevices 는 목록·고른 것·자동 여부를 모양대로 읽는다', () async {
      final s = Server(
        Uri.parse(publicRoot),
        client: MockClient((req) async {
          expect(req.url.path, '/u/$slug/browse/devices');
          return http.Response(
            jsonEncode({
              'ok': true,
              'open': 'web',
              'selected': 'phone:geono',
              'auto': false,
              'devices': [
                {'id': '', 'label': '이 기기', 'kind': 'desktop', 'online': true},
                {
                  'id': '~1a2b',
                  'label': 'MacBook',
                  'kind': 'desktop',
                  'online': true,
                },
                {
                  'id': 'phone:geono',
                  'label': 'geono 폰',
                  'kind': 'phone',
                  'online': true,
                  'model': 'iPhone',
                  'viewport': {'width': 393, 'height': 852, 'dpr': 3},
                },
                {'label': 'id 없음'},
              ],
            }),
            200,
            headers: _json,
          );
        }),
      );
      final b = await s.browseDevices();
      expect(b.open, 'web');
      expect(b.selected, 'phone:geono');
      expect(b.auto, isFalse);
      expect(b.selectedItem, 'phone:geono');
      expect(b.devices.map((d) => d.id), ['', '~1a2b', 'phone:geono']);
      final phone = b.devices.last;
      expect(phone.isPhone, isTrue);
      expect(phone.model, 'iPhone');
      expect(phone.viewport!.width, 393);
      expect(phone.viewport!.height, 852);
      expect(phone.viewport!.dpr, 3);
      expect(b.devices.first.isPhone, isFalse);
      expect(b.devices.first.viewport, isNull);
    });

    test('auto 면 selectedItem 이 auto', () {
      final b = BrowseDevices.fromJson({
        'ok': true,
        'open': 'chrome',
        'selected': '',
        'auto': true,
        'devices': [],
      });
      expect(b!.selectedItem, BrowseDevices.autoId);
      expect(BrowseDevices.fromJson('x'), isNull);
    });

    test('setBrowseDevice·setBrowseOpen 은 settings/action 으로 간다', () async {
      final bodies = <Map<String, Object?>>[];
      final s = Server(
        Uri.parse(publicRoot),
        client: MockClient((req) async {
          expect(req.url.path, '/u/$slug/settings/action');
          bodies.add((jsonDecode(req.body) as Map).cast<String, Object?>());
          return http.Response('{"ok":true}', 200, headers: _json);
        }),
      );
      await s.setBrowseDevice('auto');
      await s.setBrowseOpen('web');
      expect(bodies, [
        {'action': 'browse-device', 'id': 'auto'},
        {'action': 'browse-open', 'id': 'web'},
      ]);
    });

    test('controlUri 는 mobile/ws 이고 ws 스킴', () {
      final u = Server(Uri.parse(publicRoot)).controlUri();
      expect(u.scheme, 'wss');
      expect(u.path, '/u/$slug/mobile/ws');
    });
  });

  group('요청', () {
    test('실패 문구에 slug 가 없다', () async {
      final s = Server(
        Uri.parse(publicRoot),
        client: MockClient((_) async => http.Response('nope', 500)),
      );
      try {
        await s.me();
        fail('예외가 나야 한다');
      } on ServerException catch (e) {
        expect(e.message.contains(slug), isFalse);
        expect(e.message, contains('500'));
      }
    });

    test('연결 자체가 안 될 때도 문구에 slug 가 없다', () async {
      final s = Server(
        Uri.parse(publicRoot),
        client: MockClient(
          (_) async =>
              throw http.ClientException('boom', Uri.parse(publicRoot)),
        ),
      );
      try {
        await s.panes();
        fail('예외가 나야 한다');
      } on ServerException catch (e) {
        expect(e.message.contains(slug), isFalse);
      }
    });

    test('panes·sessions·machines 를 모양대로 읽는다', () async {
      final s = Server(
        Uri.parse(publicRoot),
        client: MockClient((req) async {
          final path = req.url.path;
          if (path.endsWith('/term/panes')) {
            return http.Response(
              jsonEncode([
                {
                  'id': '%3',
                  'name': '아리스',
                  'title': '앱',
                  'status': 'waiting',
                  'slug': 'arisu',
                  'window': 1,
                  'cwd': '/x',
                  'color': '#4a90e2',
                },
              ]),
              200,
              headers: _json,
            );
          }
          if (path.endsWith('/sessions')) {
            return http.Response(
              jsonEncode({
                'labels': ['', '아이폰'],
              }),
              200,
              headers: _json,
            );
          }
          if (path.endsWith('/machines')) {
            return http.Response(
              jsonEncode({
                'machines': [
                  {
                    'label': '맥미니',
                    'route': '~mini-stable',
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
        }),
      );
      final panes = await s.panes();
      expect(panes.single.isWaiting, isTrue);
      expect(panes.single.machine, isNull);
      expect(await s.sessions(), ['', '아이폰']);
      final machines = await s.machines();
      expect(machines.length, 2);
      expect(machines.first.route, '~mini-stable');
      expect(machines.first.panes.single.machine, '~mini-stable');
      expect(machines.last.route, '집', reason: '옛 payload는 label로 폴백');
      expect(machines.last.online, isFalse);
    });

    test('토큰에 대비 바닥이 없으면 settings/values 에서 꺼낸다', () async {
      final s = Server(
        Uri.parse(publicRoot),
        client: MockClient((req) async {
          final path = req.url.path;
          if (path.endsWith('/design-tokens')) {
            return http.Response(
              jsonEncode({
                'theme': 'dark',
                'palette': {'bg': '#252c35', 'fg': '#ffffff'},
                'ansi': List.filled(16, '#123456'),
              }),
              200,
              headers: _json,
            );
          }
          if (path.endsWith('/settings/values')) {
            return http.Response(
              jsonEncode({
                'appearance': {'min_contrast': 3.5},
              }),
              200,
              headers: _json,
            );
          }
          return http.Response('?', 404);
        }),
      );
      expect((await s.designTokens())!.minContrast, 3.5);
    });

    test('send 는 JSON 으로 글과 submit 을 보낸다', () async {
      http.Request? seen;
      final s = Server(
        Uri.parse(publicRoot),
        client: MockClient((req) async {
          seen = req;
          return http.Response('{"ok":true}', 200);
        }),
      );
      await s.send('%3', '안녕', machine: '맥미니');
      expect(seen!.url.path, '/u/$slug/m/${Uri.encodeComponent('맥미니')}/send');
      expect(seen!.url.query, 'surface=%253');
      expect(jsonDecode(seen!.body), {'text': '안녕', 'submit': true});
    });
  });
  _designTokensTests();
}

void _designTokensTests() {
  group('DesignTokens', () {
    test('design-tokens 응답을 팔레트로 — 알파는 버리고 theme 이 light 가 아니면 다크', () {
      final t = DesignTokens.fromJson({
        'theme': 'dark',
        'character_accents': {'아로나': '#4a90e2', '깨진것': 'zz'},
        'palette': {
          'bg': '#252c35',
          'fg': '#ffffff',
          'accent': '#5a8ce6',
          'border': '#505c6e6e',
        },
        'ansi': List.generate(
          16,
          (i) => '#${i.toRadixString(16).padLeft(2, '0')}0000',
        ),
      });
      expect(t, isNotNull);
      expect(t!.dark, isTrue);
      expect(t.bg, 0xff252c35);
      expect(t.fg, 0xffffffff);
      expect(t.accent, 0xff5a8ce6);
      expect(t.ansi[1], 0xff010000);
      expect(t.characterAccents['아로나'], 0xff4a90e2);
      expect(t.minContrast, DesignTokens.defaultMinContrast);
      expect(
        DesignTokens.fromJson({
          'theme': 'dark',
          'min_contrast': 3.5,
          'palette': {'bg': '#252c35', 'fg': '#ffffff'},
          'ansi': List.filled(16, '#123456'),
        })!.minContrast,
        3.5,
      );
      expect(DesignTokens.parseHex('#505c6e6e'), 0xff505c6e);
    });

    test('모양이 어긋나면 null — 색은 장식이라 앱 기본색으로 간다', () {
      expect(DesignTokens.fromJson({'theme': 'light'}), isNull);
      expect(
        DesignTokens.fromJson({
          'palette': {'bg': '#000000', 'fg': '#ffffff'},
          'ansi': ['#000'],
        }),
        isNull,
      );
      expect(
        DesignTokens.fromJson({
          'theme': 'light',
          'palette': {'bg': '#ffffff', 'fg': '#000000'},
          'ansi': List.filled(16, '#123456'),
        })!.dark,
        isFalse,
      );
    });
  });
}

void kindTests() {
  Pane pane(Map<String, Object?> extra) =>
      Pane.fromJson({'id': '%1', 'name': '아리스', ...extra});

  test('기다리는 종류가 칩 글귀가 된다', () {
    expect(
      pane({'status': 'waiting', 'kind': 'permission'}).kindLabel,
      '승인 기다림',
    );
    expect(pane({'status': 'waiting', 'kind': 'question'}).kindLabel, '질문 기다림');
    expect(pane({'status': 'waiting', 'kind': 'idle'}).kindLabel, '오래 기다림');
    expect(pane({'status': 'waiting'}).kindLabel, '답 기다림');
    expect(pane({'status': 'blocked'}).kindLabel, '답 기다림');
  });

  test('작업 중이면 무엇을 하는지 — 백그라운드 설명 > 서브에이전트 > 도구 라벨', () {
    expect(pane({'status': 'working'}).kindLabel, '작업 중');
    expect(
      pane({'status': 'working', 'doing': 'Bash cargo check'}).kindLabel,
      'Bash cargo check',
    );
    expect(
      pane({
        'status': 'working',
        'doing': 'Edit a.dart',
        'background': ['애플 메일 감시'],
      }).kindLabel,
      '백그라운드 · 애플 메일 감시',
    );
    expect(
      pane({
        'status': 'working',
        'background': ['a', 'b'],
      }).kindLabel,
      '백그라운드 2개 · a',
    );
    expect(
      pane({
        'status': 'working',
        'subagents': ['정찰'],
      }).kindLabel,
      '서브에이전트 1 · 정찰',
    );
  });

  test('쉰 지 10분 안이면 「방금 끝냄」, 넘으면 「쉼」', () {
    expect(pane({'status': 'idle', 'idle_secs': 30}).kindLabel, '방금 끝냄');
    expect(pane({'status': 'idle', 'idle_secs': 3600}).kindLabel, '쉼');
    expect(pane({'status': 'idle'}).kindLabel, '쉼');
    expect(pane({'status': 'working'}).kindLabel, '작업 중');
  });

  test('blocked 도 사람 손이 필요한 것으로 센다', () {
    expect(pane({'status': 'blocked'}).isWaiting, isTrue);
  });
}
