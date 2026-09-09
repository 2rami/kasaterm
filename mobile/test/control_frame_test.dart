import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/control_session.dart';
import 'package:kasaterm_mobile/server.dart';

const slug = 'abcdefghij0123456789abcde';

/// 소켓 없이 프레임만 넣어 보는 세션 — 열기 콜백의 결과가 답장으로 나가는지.
class FakeControl extends ControlSession {
  FakeControl(super.server, {required super.onOpenUrl});
  @override
  void connect() {}
}

void main() {
  group('ControlFrame.parse', () {
    test('hello 는 이름과 id', () {
      final f = ControlFrame.parse(
        jsonEncode({'t': 'hello', 'name': 'geono', 'id': 'phone:geono'}),
      );
      expect(f, isA<HelloFrame>());
      f as HelloFrame;
      expect(f.name, 'geono');
      expect(f.id, 'phone:geono');
    });

    test('open-url 은 주소·mode·req 를 읽고 req 는 수라도 글자로 되돌린다', () {
      final f = ControlFrame.parse(
        jsonEncode({
          't': 'open-url',
          'url': 'https://example.com/a?b=1',
          'mode': 'web',
          'req': 7,
        }),
      );
      expect(f, isA<OpenUrlFrame>());
      f as OpenUrlFrame;
      expect(f.url.toString(), 'https://example.com/a?b=1');
      expect(f.web, isTrue);
      expect(f.req, '7');
      final reply = jsonDecode(f.opened(ok: true)) as Map;
      expect(reply, {'t': 'opened', 'req': '7', 'ok': true});
      final failed = jsonDecode(f.opened(ok: false, error: '못 열었다')) as Map;
      expect(failed['ok'], isFalse);
      expect(failed['error'], '못 열었다');
      expect(failed['req'], '7');
    });

    test('mode 가 없거나 chrome 이면 브라우저', () {
      final f = ControlFrame.parse(
        jsonEncode({'t': 'open-url', 'url': 'https://x.dev/', 'req': 'r1'}),
      );
      expect((f as OpenUrlFrame).web, isFalse);
      final c = ControlFrame.parse(
        jsonEncode({
          't': 'open-url',
          'url': 'https://x.dev/',
          'mode': 'chrome',
          'req': 'r2',
        }),
      );
      expect((c as OpenUrlFrame).web, isFalse);
    });

    test('스킴 없는 주소·모르는 t·JSON 아님은 null', () {
      expect(
        ControlFrame.parse(
          jsonEncode({'t': 'open-url', 'url': 'example.com', 'req': '1'}),
        ),
        isNull,
      );
      expect(ControlFrame.parse(jsonEncode({'t': 'grid'})), isNull);
      expect(ControlFrame.parse('not json'), isNull);
      expect(ControlFrame.parse('[1,2]'), isNull);
    });
  });

  group('ControlSession.handle', () {
    final server = Server(Uri.parse('https://example.com/u/$slug/'));

    test('hello 로 connected 가 되고 이름·id 를 기억한다', () {
      final s = FakeControl(server, onOpenUrl: (_) async {});
      expect(s.state, ControlState.connecting);
      s.handle(jsonEncode({'t': 'hello', 'name': 'geono', 'id': 'phone:geono'}));
      expect(s.state, ControlState.connected);
      expect(s.name, 'geono');
      expect(s.id, 'phone:geono');
      s.dispose();
    });

    test('open-url 은 콜백으로 간다 — 실패해도 세션은 산다', () async {
      final opened = <String>[];
      final s = FakeControl(
        server,
        onOpenUrl: (f) async {
          opened.add('${f.web ? 'web' : 'chrome'} ${f.url}');
          if (f.req == 'bad') throw Exception('안 열림');
        },
      );
      s.handle(
        jsonEncode({
          't': 'open-url',
          'url': 'https://a.dev/',
          'mode': 'web',
          'req': 'ok',
        }),
      );
      s.handle(
        jsonEncode({'t': 'open-url', 'url': 'https://b.dev/', 'req': 'bad'}),
      );
      await Future<void>.delayed(Duration.zero);
      expect(opened, ['web https://a.dev/', 'chrome https://b.dev/']);
      expect(s.state, ControlState.connected);
      s.dispose();
    });

    test('pause 는 paused, resume 은 다시 connecting', () {
      final s = FakeControl(server, onOpenUrl: (_) async {});
      s.pause();
      expect(s.state, ControlState.paused);
      s.resume();
      expect(s.state, ControlState.connecting);
      s.dispose();
    });
  });
}
