import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/term_session.dart';

class _NativeHttp extends HttpOverrides {}

/// 원본 칸 흉내 — 악수 `size` 를 보내고, 폰이 보낸 제어(JSON)를 쌓는다.
class _Host {
  _Host(this.http);
  final HttpServer http;
  final control = <Map<String, Object?>>[];
  final asks = <String>[];
  final keys = <String>[];

  /// 제어와 키가 온 순서 그대로 — 쥐는 요청이 그 키보다 먼저 가는지 본다.
  final log = <String>[];
  WebSocket? socket;

  static Future<_Host> start({required bool latest}) async {
    final host = _Host(await HttpServer.bind(InternetAddress.loopbackIPv4, 0));
    host.http.listen((request) async {
      if (!WebSocketTransformer.isUpgradeRequest(request)) {
        request.response.statusCode = 404;
        await request.response.close();
        return;
      }
      final socket = await WebSocketTransformer.upgrade(
        request,
        protocolSelector: (p) =>
            p.contains('kasa-relay-account') ? 'kasa-relay-account' : null,
      );
      host.socket = socket;
      socket.add(
        jsonEncode({
          't': 'size',
          'cols': 24,
          'rows': 15,
          'mirror': true,
          'id': '%3',
          if (latest)
            'capabilities': {'mirror_viewport': 1, 'viewport_latest': 1},
        }),
      );
      socket.listen((m) {
        if (m is List<int>) {
          host.keys.add(utf8.decode(m));
          host.log.add('key');
        }
        if (m is String) {
          final v = (jsonDecode(m) as Map).cast<String, Object?>();
          host.asks.add(v['t'] as String);
          if (v['t'] == 'viewport') {
            host.control.add(v);
            host.log.add('viewport ${v['op']}');
          }
        }
      });
    });
    return host;
  }

  void send(Map<String, Object?> m) => socket!.add(jsonEncode(m));
}

Future<void> _until(bool Function() done) async {
  for (var i = 0; i < 80 && !done(); i++) {
    await Future<void>.delayed(const Duration(milliseconds: 25));
  }
}

Future<void> _quiet() => Future<void>.delayed(const Duration(milliseconds: 400));

/// 시험용 조용한 시간 — 실제(1분)보다 짧게.
const idle = Duration(milliseconds: 1200);

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  final pane = Pane(
    id: '%3',
    name: '유우카',
    title: '',
    status: 'active',
    window: 0,
    cwd: '/',
  );

  Server serverFor(_Host host) => Server.account(
    AccountSession(
      origin: Uri.parse('http://127.0.0.1:${host.http.port}'),
      account: 'fixture',
      deviceId: 'phone-fixture',
      token: 'test-device-token',
    ),
  );

  test(
    '보기만 하면 원본 크기 그대로, 폰에서 치면 쥐고, 조용해지면 놓는다',
    () => HttpOverrides.runWithHttpOverrides(() async {
      final host = await _Host.start(latest: true);
      final s = TermSession(serverFor(host), pane, holdIdle: idle)
        ..holdViewport = true
        ..setViewport(42, 30)
        ..connect();
      await _until(() => s.state == TermState.connected);
      await _quiet();
      expect(host.control, isEmpty, reason: '열어 보기만 해서는 데스크톱 칸 크기를 안 바꾼다');

      s.sendText('a');
      await _until(() => host.keys.isNotEmpty);
      expect(host.control, [
        {'t': 'viewport', 'op': 'acquire', 'cols': 42, 'rows': 30},
      ]);
      expect(host.log, ['viewport acquire', 'key'], reason: '친 글은 폰 크기 격자에 들어간다');

      // 자판이 오르내리는 동안의 중간 크기는 모아서 마지막 하나만.
      s
        ..setViewport(42, 24)
        ..setViewport(42, 18);
      await _until(() => host.control.length > 1);
      await _quiet();
      expect(host.control.skip(1), [
        {'t': 'viewport', 'op': 'resize', 'cols': 42, 'rows': 18},
      ]);

      // 치는 동안은 쥔 채로 — 마지막 키부터 다시 센다.
      s.sendText('b');
      await Future<void>.delayed(idle * 0.6);
      s.sendText('c');
      await Future<void>.delayed(idle * 0.6);
      expect(host.control, hasLength(2), reason: '마지막 키에서 아직 조용한 시간이 안 지났다');
      await _until(() => host.control.length > 2);
      expect(host.control.last, {'t': 'viewport', 'op': 'release'});

      s.setViewport(42, 30);
      await _quiet();
      expect(host.control, hasLength(3), reason: '놓은 뒤엔 다시 보기만 한다');

      s.ctrl('c');
      await _until(() => host.control.length > 3);
      expect(host.control.last, {
        't': 'viewport',
        'op': 'acquire',
        'cols': 42,
        'rows': 30,
      });

      // 자판이 움직이는 중에 친 첫 키는 크기가 멈춘 뒤에 쥔다 — 중간 크기(42×1)를 원본에 주지 않는다.
      host.send({'t': 'viewport', 'granted': false, 'lost': true});
      await _quiet();
      s
        ..setViewport(42, 1)
        ..sendText('d')
        ..setViewport(42, 33);
      await _until(() => host.control.length > 4);
      await _quiet();
      expect(host.control.skip(4), [
        {'t': 'viewport', 'op': 'acquire', 'cols': 42, 'rows': 33},
      ]);
      s.setViewport(42, 30);
      await _until(() => host.control.length > 5);
      expect(host.control.last['op'], 'resize');
      host.control.removeRange(4, host.control.length);

      // 데스크톱에서 그 칸에 키를 치면 원본이 되찾는다 — 보기만으로는 도로 빼앗지 않는다.
      host.send({'t': 'viewport', 'granted': false, 'lost': true});
      await _quiet();
      s.setViewport(42, 24);
      await _quiet();
      expect(host.control, hasLength(4));

      await s.replyAfterAttachment('답장');
      await _until(() => host.control.length > 4);
      expect(host.control.last, {
        't': 'viewport',
        'op': 'acquire',
        'cols': 42,
        'rows': 24,
      });

      // 대화 보기·격자 그대로 보기로 가면 놓고, 거기서 쳐도 쥐지 않는다.
      s.holdViewport = false;
      await _until(() => host.control.length > 5);
      expect(host.control.last, {'t': 'viewport', 'op': 'release'});
      s.sendText('c');
      await _quiet();
      expect(host.control, hasLength(6));

      s.dispose();
      await host.http.close(force: true);
    }, _NativeHttp()),
  );

  test(
    '폰을 떠났다 돌아오면 보기만 하고, 치던 중 끊겼다 다시 붙으면 다시 쥔다',
    () => HttpOverrides.runWithHttpOverrides(() async {
      final host = await _Host.start(latest: true);
      final s = TermSession(serverFor(host), pane)
        ..holdViewport = true
        ..setViewport(42, 30)
        ..connect();
      await _until(() => s.state == TermState.connected);
      s.sendText('a');
      await _until(() => host.control.isNotEmpty);

      final first = host.socket;
      await first!.close();
      await _until(() => host.socket != first && host.control.length > 1);
      expect(host.control.last['op'], 'acquire', reason: '치던 중 망이 끊긴 것은 떠난 것이 아니다');

      s.pause();
      final second = host.socket;
      s.resume();
      await _until(() => host.socket != second);
      await _until(() => s.state == TermState.connected);
      await _quiet();
      expect(host.control, hasLength(2), reason: '앱을 다시 열어 보는 것은 보기다');

      s.dispose();
      await host.http.close(force: true);
    }, _NativeHttp()),
  );

  test(
    '크기 계약을 모르는 옛 호스트엔 크기를 보내지 않는다',
    () => HttpOverrides.runWithHttpOverrides(() async {
      final host = await _Host.start(latest: false);
      final s = TermSession(serverFor(host), pane)
        ..holdViewport = true
        ..setViewport(42, 30)
        ..connect();
      await _until(() => s.state == TermState.connected);
      s.sendText('a');
      await _until(() => host.keys.isNotEmpty);
      await _quiet();
      expect(host.control, isEmpty);
      s.dispose();
      await host.http.close(force: true);
    }, _NativeHttp()),
  );

  test(
    '앱이 굴리는 화면에 휠을 SGR 로 넘기되 원본 크기를 쥐는 입력으로 치지 않는다',
    () => HttpOverrides.runWithHttpOverrides(() async {
      final host = await _Host.start(latest: true);
      final s = TermSession(serverFor(host), pane)
        ..holdViewport = true
        ..setViewport(42, 30)
        ..connect();
      await _until(() => s.state == TermState.connected);
      host.send({
        't': 'grid',
        'cols': 24,
        'rows': 15,
        'dirty': [],
        'alt': true,
        'mouse': true,
        'mouseSgr': true,
      });
      await _until(() => s.scrollsApp);
      s
        ..wheel(3)
        ..wheel(-1);
      await _until(() => host.keys.length >= 2);
      expect(host.keys, [
        '\x1b[<64;13;8M' * 3,
        '\x1b[<65;13;8M',
      ]);
      await _quiet();
      expect(host.control, isEmpty, reason: '휠은 보기다 — 원본 크기를 쥐지 않는다');

      host.send({'t': 'grid', 'alt': false, 'dirty': []});
      await _until(() => !s.scrollsApp);
      s.wheel(2);
      await _quiet();
      expect(host.keys, hasLength(2), reason: '스크롤백이 있는 화면은 폰이 굴린다');

      s.dispose();
      await host.http.close(force: true);
    }, _NativeHttp()),
  );

  test(
    '전체 화면(대체 화면) 동안 지난 줄을 비우고, 나오면 원본의 지난 줄을 다시 받는다',
    () => HttpOverrides.runWithHttpOverrides(() async {
      final host = await _Host.start(latest: false);
      final s = TermSession(serverFor(host), pane)..connect();
      List<List<Object?>> rows(String prefix, int n) => [
        for (var i = 0; i < n; i++)
          [
            ['$prefix $i', null, null, 0],
          ],
      ];
      String last() => s.history.last.map((r) => r.text).join();
      await _until(() => host.asks.contains('history'));
      host.send({'t': 'history', 'rows': rows('셸 줄', 5)});
      host.send({'t': 'grid', 'cols': 24, 'rows': 15, 'dirty': [], 'alt': false});
      await _until(() => s.history.length == 5);

      host.send({'t': 'grid', 'dirty': [], 'alt': true});
      await _until(() => s.grid.alt);
      expect(s.history, isEmpty, reason: '전체 화면 위에 셸 지난 줄이 비치면 안 된다');
      // 원본이 전체 화면을 줄이면 밀려 올라간 윗줄이 지난 줄로 온다 — 옛 머리말이 겹쳐 보였다.
      host
        ..send({'t': 'scrolled', 'rows': rows('Claude Code 머리말', 3)})
        ..send({'t': 'history', 'rows': rows('Claude Code 머리말', 3)});
      await _quiet();
      expect(s.history, isEmpty);

      final before = host.asks.where((t) => t == 'history').length;
      host.send({'t': 'grid', 'dirty': [], 'alt': false});
      await _until(
        () => host.asks.where((t) => t == 'history').length > before,
      );
      host.send({'t': 'history', 'rows': rows('셸 줄', 6)});
      await _until(() => s.history.length == 6);
      expect(last(), '셸 줄 5');

      s.dispose();
      await host.http.close(force: true);
    }, _NativeHttp()),
  );
}
