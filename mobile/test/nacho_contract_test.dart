import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/nacho.dart';
import 'package:kasaterm_mobile/server.dart';

/// 나쵸의 「한 대화·여러 창구」 계약(nacho-neko docs/development/one-conversation.md, 2026-09-25)
/// 중 앱이 지킬 몫 — 출처·알림 끔·말한 시각 순서·펫 기계 묶기·409.
class _PetServer extends Server {
  _PetServer(this.reply) : super(Uri.parse('http://127.0.0.1:1/u/slug/'));

  final (int, Map<String, Object?>) reply;

  @override
  Future<(int, Map<String, Object?>)> nacho(
    String path, {
    Map<String, String>? query,
    Map<String, Object?>? body,
    Duration timeout = const Duration(seconds: 20),
  }) async => reply;
}

NachoEvent ev(Map<String, Object?> j) => NachoEvent.fromJson(j);

void main() {
  test('디코에서 오간 한 번 — 새 필드와 출처', () {
    final m = ev({
      'seq': 812,
      'kind': 'message',
      'id': 'discord-1',
      'text': '펫 요약 모양 바꿔줘',
      'surface': 'discord',
      'place': '디스코드 DM',
      'turn': 'discord:discord:777:1420',
      'mirror': true,
      'asked_ms': 1000,
      'at_ms': 5000,
    });
    final r = ev({
      'seq': 813,
      'kind': 'reply',
      'message': 'discord-1',
      'text': '바꿨어',
      'surface': 'discord',
      'mirror': true,
      'notify': false,
      'at_ms': 5001,
    });
    expect(m.origin, '디코 DM에서');
    expect(m.mirror, isTrue);
    expect(m.askedMs, 1000);
    expect(m.turn, 'discord:discord:777:1420');
    expect(r.notify, isFalse);
    expect(
      ev({'seq': 1, 'kind': 'reply', 'surface': 'slack'}).origin,
      '슬랙 DM에서',
    );
    expect(
      ev({'seq': 1, 'kind': 'reply', 'surface': 'pet', 'place': '미니'}).origin,
      '펫(미니)에서',
    );
    // 옛 서버 줄 — 새 필드가 없어도 전과 같다.
    final old = ev({'seq': 2, 'kind': 'reply', 'text': 'x'});
    expect(old.origin, isNull);
    expect(old.notify, isTrue);
    expect(old.mirror, isFalse);
    expect(old.askedMs, isNull);
  });

  test('옮겨 적힌 한 번은 말한 시각 자리로, 앱 대화는 그대로', () {
    final events = [
      ev({
        'seq': 1,
        'kind': 'message',
        'id': 'a1',
        'at_ms': 2000,
        'asked_ms': 2000,
      }),
      ev({'seq': 2, 'kind': 'reply', 'message': 'a1', 'at_ms': 3000}),
      // 디코에서 1000 에 물었고 4000 에 끝나 원장엔 뒤에 적혔다.
      ev({
        'seq': 3,
        'kind': 'message',
        'id': 'd1',
        'surface': 'discord',
        'mirror': true,
        'asked_ms': 1000,
        'at_ms': 4000,
      }),
      ev({
        'seq': 4,
        'kind': 'reply',
        'message': 'd1',
        'surface': 'discord',
        'mirror': true,
        'at_ms': 4000,
      }),
      ev({
        'seq': 5,
        'kind': 'status',
        'message': 'd1',
        'state': 'answered',
        'mirror': true,
        'at_ms': 4000,
      }),
    ];
    expect([for (final e in timeline(events)) e.seq], [3, 4, 5, 1, 2]);

    final plain = [
      ev({'seq': 1, 'kind': 'message', 'id': 'a', 'at_ms': 9}),
      ev({'seq': 2, 'kind': 'reply', 'message': 'a', 'at_ms': 1}),
    ];
    expect(
      identical(timeline(plain), plain),
      isTrue,
      reason: 'asked_ms 가 없으면 순번 그대로',
    );
  });

  test('옮겨 적힌 말은 이 폰의 「답하는 중」을 끄지 않는다', () {
    final desk = NachoDesk(_PetServer((200, {})))
      ..events.addAll([
        ev({'seq': 1, 'kind': 'message', 'id': 'a1', 'at_ms': 1}),
        ev({
          'seq': 2,
          'kind': 'status',
          'message': 'a1',
          'state': 'running',
          'at_ms': 2,
        }),
        ev({
          'seq': 3,
          'kind': 'message',
          'id': 'd1',
          'mirror': true,
          'surface': 'discord',
          'at_ms': 3,
        }),
        ev({
          'seq': 4,
          'kind': 'reply',
          'message': 'd1',
          'mirror': true,
          'at_ms': 3,
        }),
      ]);
    expect(desk.awaiting, 'a1');
    desk.dispose();
  });

  test('같은 기계의 옛·새 펫 이름은 한 줄로', () {
    final pets = groupPets([
      NachoPet({'conv': 'kasapet:맥북', 'machine': 'm1', 'seen_ago_s': 90000}),
      NachoPet({
        'conv': 'kasapet:건호의 MacBook Pro',
        'machine': 'm1',
        'alive': true,
        'can_receive': true,
      }),
      NachoPet({'conv': 'kasapet:미니', 'machine': null}),
      NachoPet({'conv': 'kasapet:미니2'}),
    ]);
    expect(
      [for (final p in pets) p.conv],
      ['kasapet:건호의 MacBook Pro', 'kasapet:미니', 'kasapet:미니2'],
    );
  });

  test('말풍선을 못 받는 펫을 이으면 409 를 사람 말로', () async {
    final desk = NachoDesk(
      _PetServer((409, {'error': 'pet_cannot_receive', 'conv': 'kasapet:맥북'})),
    );
    await expectLater(
      desk.linkPet('kasapet:맥북'),
      throwsA(
        isA<NachoError>()
            .having((e) => e.code, 'code', 'pet_cannot_receive')
            .having((e) => e.message, 'message', contains('말풍선을 받을 수 없어요')),
      ),
    );
    desk.dispose();
  });
}
