import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/kasanet.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/server.dart';

class FakeNative implements KasanetNative {
  bool direct = true;
  bool relayed = false;
  String? error;
  final opened = <String>[];
  int changes = 0;

  @override
  String? get id => 'phone-id';

  @override
  int open(String peerJson) {
    opened.add(peerJson);
    return 40001;
  }

  @override
  KasanetPath? state(int port) => port == 40001
      ? KasanetPath(direct: direct, relayed: relayed, rttMs: direct ? 7 : null, error: error)
      : null;

  @override
  void networkChanged() => changes++;

  @override
  String? get lastError => null;
}

final session = AccountSession(
  origin: Uri.parse('https://gw.example'),
  account: 'me',
  deviceId: 'dev1',
  token: 'tok',
);

/// 관문과 데스크톱 입구를 흉내 낸다. 받은 요청을 적어 두고, [directDown] 이면 입구 요청을 연결 실패로 끊는다.
class Wire {
  final requests = <http.Request>[];
  bool kasanet = true;
  int desktopPort = 8765;
  int registerStatus = 200;
  bool directDown = false;

  late final client = MockClient((req) async {
    requests.add(req);
    final u = req.url;
    if (u.host == '127.0.0.1') {
      if (directDown) throw http.ClientException('입구가 끊겼다', u);
      return http.Response('direct ${u.path}', 200);
    }
    switch (u.path) {
      case '/relay/account/version':
      case '/relay/account/m/~mini/version':
        return http.Response(
          jsonEncode({
            'machine_id': u.path.contains('mini') ? 'mini' : 'mac1',
            if (kasanet)
              'kasanet': {'id': 'desk', 'port': desktopPort, 'addrs': []},
          }),
          200,
        );
      case '/relay/account/kasanet/phone':
      case '/relay/account/m/~mini/kasanet/phone':
        return http.Response(
          jsonEncode({'ok': registerStatus == 200, 'ttl_secs': 900}),
          registerStatus,
        );
    }
    return http.Response('gateway ${u.path}', 200);
  });
}

Future<void> settle() => Future<void>.delayed(const Duration(milliseconds: 20));

void main() {
  test('직통이 서면 입구로, 관문이 확인할 값과 배우는 길은 관문으로', () async {
    final wire = Wire();
    final native = FakeNative();
    final server = Server.account(
      session,
      client: wire.client,
      kasanet: native,
    );
    addTearDown(server.close);

    // 처음은 관문으로 가며 뒤에서 배운다.
    expect(server.uri('term/panes').host, 'gw.example');
    await settle();
    expect(native.opened, hasLength(1));
    final reg = wire.requests.firstWhere(
      (r) => r.url.path.endsWith('kasanet/phone'),
    );
    expect(reg.headers['authorization'], 'Bearer tok', reason: '등록은 관문 자격으로');
    expect(jsonDecode(reg.body), {'id': 'phone-id'});

    expect(
      server.uri('term/panes').toString(),
      'http://127.0.0.1:40001/term/panes',
    );
    expect(
      server.uri('term/panes', machine: '~mac1').toString(),
      'http://127.0.0.1:40001/term/panes',
      reason: '기본 기계의 id 로 불러도 같은 입구',
    );
    expect(server.uri('machines').host, 'gw.example');
    expect(server.uri('kasanet/phone').host, 'gw.example');
    expect(server.pathOf(null), (true, 7));

    final ws = server.wsUri('term/ws', query: {'pane': '%3'});
    expect(ws.toString(), 'ws://127.0.0.1:40001/term/ws?pane=%253');
    expect(server.wsProtocolsFor(ws), isNull, reason: '입구 소켓에는 자격을 싣지 않는다');
    expect(
      server.wsProtocolsFor(server.wsUri('machines', query: {})),
      session.protocols,
    );

    await server.panesOrEmpty();
    final direct = wire.requests.lastWhere((r) => r.url.host == '127.0.0.1');
    expect(
      direct.headers.containsKey('authorization'),
      isFalse,
      reason: '입구로는 관문 토큰을 안 보낸다',
    );
  });

  test('직통을 잃으면 관문으로 돌아가고 길 바뀜을 알린다', () async {
    final wire = Wire();
    final native = FakeNative();
    final server = Server.account(
      session,
      client: wire.client,
      kasanet: native,
    );
    addTearDown(server.close);
    server.uri('term/panes');
    await settle();
    var woke = 0;
    server.routeChanges!.addListener(() => woke++);
    native.direct = false;
    await Future<void>.delayed(const Duration(milliseconds: 1200));
    expect(woke, 1);
    expect(server.uri('term/panes').host, 'gw.example');
    expect(server.pathOf(null), (false, null));
  });

  test('국내 중계로 가도 입구로 싣고, 직통↔중계가 바뀌면 알린다', () async {
    final wire = Wire();
    final native = FakeNative()..relayed = true;
    final server = Server.account(
      session,
      client: wire.client,
      kasanet: native,
    );
    addTearDown(server.close);
    server.uri('term/panes');
    await settle();
    expect(server.uri('term/panes').host, '127.0.0.1');
    expect(server.relayedOf(null), isTrue);
    var woke = 0;
    server.routeChanges!.addListener(() => woke++);
    native.relayed = false;
    await Future<void>.delayed(const Duration(milliseconds: 1200));
    expect(woke, 1, reason: '중계 → 직통도 길 바뀜');
    expect(server.relayedOf(null), isFalse);
    expect(server.uri('term/panes').host, '127.0.0.1');
  });

  test('입구로 간 GET 이 끊기면 관문으로 한 번 더, POST 는 다시 안 보낸다', () async {
    final wire = Wire();
    final native = FakeNative();
    final server = Server.account(
      session,
      client: wire.client,
      kasanet: native,
    );
    addTearDown(server.close);
    server.uri('term/panes');
    await settle();
    wire.directDown = true;
    wire.requests.clear();
    await server.panesOrEmpty();
    expect(wire.requests.map((r) => r.url.host), ['127.0.0.1', 'gw.example']);
    expect(wire.requests.last.url.path, '/relay/account/term/panes');
    expect(wire.requests.last.headers['authorization'], 'Bearer tok');

    wire.requests.clear();
    await expectLater(server.send('%1', 'hi'), throwsA(anything));
    expect(wire.requests.map((r) => r.url.host), [
      '127.0.0.1',
    ], reason: '키 입력이 두 번 가면 안 된다');
  });

  test('카사넷이 없는 옛 데스크톱·거절하는 데스크톱은 관문 그대로', () async {
    for (final setup in [
      (Wire w) => w.kasanet = false,
      (Wire w) => w.registerStatus = 403,
    ]) {
      final wire = Wire();
      setup(wire);
      final native = FakeNative();
      final server = Server.account(
        session,
        client: wire.client,
        kasanet: native,
      );
      server.uri('term/panes');
      await settle();
      expect(native.opened, isEmpty);
      expect(server.uri('term/panes').host, 'gw.example');
      server.close();
    }
  });

  test('다른 기계는 그 기계 길로 따로 배운다', () async {
    final wire = Wire();
    final native = FakeNative();
    final server = Server.account(
      session,
      client: wire.client,
      kasanet: native,
    );
    addTearDown(server.close);
    expect(
      server.uri('term/panes', machine: '~mini').path,
      '/relay/account/m/~mini/term/panes',
    );
    await settle();
    expect(
      wire.requests.map((r) => r.url.path),
      containsAll([
        '/relay/account/m/~mini/version',
        '/relay/account/m/~mini/kasanet/phone',
      ]),
    );
    expect(
      server.uri('term/panes', machine: '~mini').toString(),
      'http://127.0.0.1:40001/term/panes',
    );
    // 표시 이름뿐인 옛 route 는 다루지 않는다.
    expect(server.uri('term/panes', machine: 'old-name').host, 'gw.example');
  });

  test('다시 등록할 때 데스크톱이 새 포트로 떴으면 입구를 새로 잇고, 거절하면 관문으로 돌아간다', () async {
    final wire = Wire();
    final native = FakeNative();
    final server = Server.account(
      session,
      client: wire.client,
      kasanet: native,
    );
    addTearDown(server.close);
    server.uri('term/panes');
    await settle();
    wire.desktopPort = 54002;
    KasanetRouter.resumed();
    await settle();
    expect(native.opened, hasLength(2));
    expect(jsonDecode(native.opened.last)['port'], 54002);
    expect(server.uri('term/panes').host, '127.0.0.1');

    wire.registerStatus = 403;
    KasanetRouter.resumed();
    await settle();
    expect(
      server.uri('term/panes').host,
      'gw.example',
      reason: '관문에서 폐기된 폰은 직통을 놓는다',
    );
  });

  test('앱이 깨어나면 망 바뀜을 알리고 등록을 새로 한다', () async {
    final wire = Wire();
    final native = FakeNative();
    final server = Server.account(
      session,
      client: wire.client,
      kasanet: native,
    );
    addTearDown(server.close);
    server.uri('term/panes');
    await settle();
    wire.requests.clear();
    KasanetRouter.resumed();
    await settle();
    expect(native.changes, 1);
    expect(
      wire.requests.where((r) => r.url.path.endsWith('kasanet/phone')),
      hasLength(1),
    );
  });
}

extension on Server {
  Future<void> panesOrEmpty() async {
    try {
      await panes();
    } catch (_) {}
  }
}
