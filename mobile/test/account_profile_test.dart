import 'dart:convert';
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/screens/account_profile.dart';

final origin = Uri.parse('https://gateway.invalid');

Map<String, Object?> profileJson({String? login = 'kasa', Object? avatar}) => {
  'ok': true,
  'account': 'one',
  'login': login,
  'has_password': login != null,
  'nickname': '건호',
  'display_name': '건호',
  'avatar': avatar,
  'identities': [
    {'provider': 'google', 'display': 'me@example.com', 'picture': 'https://lh3.googleusercontent.com/a/face'},
    {'provider': 'github', 'display': 'octo', 'picture': 'https://evil.example/pixel.png'},
  ],
};

/// 한글이 든 JSON 답 — `http.Response(String)` 은 latin1 로만 싣는다.
http.Response json(Object? body, [int status = 200]) =>
    http.Response.bytes(utf8.encode(jsonEncode(body)), status, headers: {'content-type': 'application/json; charset=utf-8'});

/// 관문 흉내 — 받은 요청을 남기고 [answer] 가 정한 답을 준다.
class FakeProfileGateway {
  FakeProfileGateway(this.answer);

  final http.Response Function(http.Request req) answer;
  final requests = <http.Request>[];

  RelayAccountApi api() => RelayAccountApi(
    origin,
    client: MockClient((req) async {
      requests.add(req);
      return answer(req);
    }),
  );
}

Future<Uint8List> png(int w, int h) async {
  final recorder = ui.PictureRecorder();
  Canvas(recorder).drawRect(Rect.fromLTWH(0, 0, w.toDouble(), h.toDouble()), Paint()..color = const Color(0xff336699));
  final image = await recorder.endRecording().toImage(w, h);
  final data = await image.toByteData(format: ui.ImageByteFormat.png);
  return data!.buffer.asUint8List();
}

void main() {
  test('profile keeps linked logins and drops pictures from other hosts', () {
    final profile = AccountProfile.fromJson(profileJson(avatar: {'source': 'google', 'url': 'https://lh3.googleusercontent.com/a/face'}));
    expect(profile.login, 'kasa');
    expect(profile.hasPassword, isTrue);
    expect(profile.name, '건호');
    expect(profile.linked(OAuthProvider.google)?.display, 'me@example.com');
    expect(profile.linked(OAuthProvider.github)?.picture, isNull, reason: 'a picture outside the provider image hosts');
    expect(profile.avatar?.source, 'google');
    final tampered = AccountProfile.fromJson(profileJson(avatar: {'source': 'github', 'url': 'http://avatars.githubusercontent.com/u/1'}));
    expect(tampered.avatar, isNull, reason: 'plain http picture');
    final oauth = AccountProfile.fromJson({'login': null, 'display_name': 'me@example.com', 'identities': []});
    expect(oauth.hasPassword, isFalse);
    expect(oauth.name, 'me@example.com');
  });

  test('profile calls send JSON, raw image bytes and the current password', () async {
    final gate = FakeProfileGateway((req) => json(profileJson()));
    final api = gate.api();
    await api.updateProfile(nickname: '건호');
    await api.uploadAvatar([0x89, 0x50, 0x4e, 0x47]);
    await api.changeLogin('current-pass', ' Kasa ');
    expect(gate.requests[0].method, 'PATCH');
    expect(jsonDecode(gate.requests[0].body), {'nickname': '건호'});
    expect(gate.requests[1].method, 'PUT');
    expect(gate.requests[1].headers['content-type'], 'application/octet-stream');
    expect(gate.requests[1].bodyBytes, [0x89, 0x50, 0x4e, 0x47]);
    expect(gate.requests[2].url.path, '/relay/profile/login');
    expect(jsonDecode(gate.requests[2].body), {'password': 'current-pass', 'login': 'kasa'});
  });

  test('gateway refusals become plain sentences', () async {
    final gate = FakeProfileGateway((req) => http.Response('{"ok":false,"error":"login_taken"}', 409));
    await expectLater(
      gate.api().changeLogin('pass', 'taken'),
      throwsA(isA<AccountException>().having((e) => e.message, 'message', contains('이미 누가 쓰는 아이디'))),
    );
    final old = FakeProfileGateway((req) => http.Response('', 404));
    await expectLater(
      old.api().profile(),
      throwsA(isA<AccountException>().having((e) => e.message, 'message', contains('관문 업데이트'))),
    );
  });

  test('a picked photo becomes a 256 px square PNG', () async {
    final out = await squareAvatar(await png(400, 300));
    final codec = await ui.instantiateImageCodec(out);
    final frame = await codec.getNextFrame();
    expect((frame.image.width, frame.image.height), (256, 256));
  });

  testWidgets('password change checks both new fields before asking the gateway', (tester) async {
    final gate = FakeProfileGateway((req) => http.Response('{"ok":true}', 200));
    AccountProfile? result;
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => TextButton(
            onPressed: () async => result = await showSecretSheet(
              context,
              profile: AccountProfile.fromJson(profileJson()),
              api: gate.api,
              password: true,
            ),
            child: const Text('open'),
          ),
        ),
      ),
    );
    await tester.tap(find.text('open'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('secret-current')), 'old-password');
    await tester.enterText(find.byKey(const Key('secret-next')), 'new-password');
    await tester.enterText(find.byKey(const Key('secret-again')), 'new-passwork');
    await tester.tap(find.byKey(const Key('secret-submit')));
    await tester.pumpAndSettle();
    expect(find.text('새 비밀번호 두 칸이 달라요.'), findsOneWidget);
    expect(gate.requests, isEmpty);

    await tester.enterText(find.byKey(const Key('secret-again')), 'new-password');
    await tester.tap(find.byKey(const Key('secret-submit')));
    await tester.pumpAndSettle();
    expect(gate.requests.single.url.path, '/relay/profile/password');
    expect(jsonDecode(gate.requests.single.body), {'password': 'old-password', 'new_password': 'new-password'});
    expect(result?.login, 'kasa');
  });

  testWidgets('choosing a provider picture switches the avatar without closing', (tester) async {
    final gate = FakeProfileGateway(
      (req) => json(profileJson(avatar: {'source': 'google', 'url': 'https://lh3.googleusercontent.com/a/face'})),
    );
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => TextButton(
            onPressed: () => showProfileSheet(
              context,
              profile: AccountProfile.fromJson(profileJson()),
              api: gate.api,
              pick: () async => null,
            ),
            child: const Text('open'),
          ),
        ),
      ),
    );
    await tester.tap(find.text('open'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('profile-photo-github')), findsNothing, reason: 'its picture is from another host');
    await tester.tap(find.byKey(const Key('profile-photo-google')));
    await tester.pump();
    await tester.pump();
    expect(jsonDecode(gate.requests.single.body), {'avatar': 'google'});
    expect(find.byKey(const Key('profile-save')), findsOneWidget, reason: 'the sheet stays open');
  });
}
