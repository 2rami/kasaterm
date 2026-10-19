import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/screens/approval_key.dart';
import 'package:kasaterm_mobile/server.dart';

import 'approval_secret_test.dart' show FakeSigner;
import 'relay_account_test.dart' show session;

const utf8Json = {'content-type': 'application/json; charset=utf-8'};

/// `vectorPublic` 의 지문 — 맥이 믿으면 상태의 trusted 에 이 값이 든다.
const mine = '0ED3-A6AB-957F';

/// 관문 열쇠 창구와 맥 한 대(`mini`)를 흉내 낸다. 맥은 열쇠가 맡겨진 뒤 `op-link` 를 받으면 그 폰을 믿는다.
class Rig {
  Rig({this.keychain = true, this.token = false});

  /// 키체인 판 맥이면 참, 아니면 1Password 로 읽던 옛 판(토큰 여부는 [token]).
  final bool keychain;
  final bool token;
  final registered = <String>[];
  final links = <String>[];

  http.Client get gateway => MockClient((req) async {
        expect(req.url.path, '/relay/approvals/keys');
        registered.add((jsonDecode(req.body) as Map)['public'] as String);
        return http.Response(jsonEncode({'ok': true, 'key': {'id': '0ed3a6ab957ff6f59a9630a473d31a7d'}}), 200);
      });

  Server get server => Server(
        Uri.parse('https://hub.test/u/slug/'),
        client: MockClient((req) async {
          if (req.url.path == '/u/slug/machines') {
            return http.Response(jsonEncode({'machines': [{'label': '맥미니', 'route': 'mini', 'online': true}]}), 200, headers: utf8Json);
          }
          expect(req.url.path, '/u/slug/m/mini/settings/action');
          expect(jsonDecode(req.body), {'action': 'op-link'});
          links.add(req.url.path);
          return http.Response(
            jsonEncode({
              'ok': true,
              'op': {
                if (keychain) 'source': 'keychain',
                'token': keychain || token,
                if (!keychain) 'vault': token ? 'KASA 학생' : null,
                'trusted': [if (registered.isNotEmpty) {'label': '폰', 'fingerprint': mine}],
              },
            }),
            200,
            headers: utf8Json,
          );
        }),
      );
}

Widget host(Rig rig, FakeSigner signer) => MaterialApp(
      theme: buildTheme(Brightness.light),
      home: ApprovalKeyScreen(
        api: () => RelayAccountApi(session().origin, session: session(), client: rig.gateway),
        server: rig.server,
        signer: signer,
      ),
    );

void main() {
  testWidgets('Face ID 한 번이 열쇠를 맡기고 맥이 그 폰을 믿게 한다', (tester) async {
    final rig = Rig();
    final signer = FakeSigner(public: null);
    await tester.pumpWidget(host(rig, signer));
    await tester.pumpAndSettle();
    expect(find.text('Face ID 로 연결'), findsOneWidget);
    expect(find.text('이 폰은 아직이에요'), findsOneWidget);

    await tester.tap(find.byKey(const Key('op-link')));
    await tester.pumpAndSettle();
    expect(rig.registered, hasLength(1));
    expect(find.text('연결됐어요'), findsOneWidget);
    expect(find.text('연결됨 · 이 맥 키체인'), findsOneWidget);
    expect(find.text('Face ID 로 연결'), findsNothing);
  });

  testWidgets('Face ID 를 취소하면 아무것도 맡기지 않고, 토큰 없는 옛 판 맥은 업데이트하라고 적는다', (tester) async {
    final rig = Rig(keychain: false);
    final signer = FakeSigner(public: null, answer: null);
    await tester.pumpWidget(host(rig, signer));
    await tester.pumpAndSettle();
    expect(find.textContaining('새 판으로 업데이트해 주세요'), findsOneWidget);

    await tester.tap(find.byKey(const Key('op-link')));
    await tester.pumpAndSettle();
    expect(rig.registered, isEmpty);
    expect(find.text('Face ID 로 연결'), findsOneWidget);

    // Face ID 를 마치면 폰은 이어지지만 그 맥이 아직 쓸 수 없으니 「연결됐어요」가 아니다.
    signer.answer = 'MEUCIQ==';
    await tester.tap(find.byKey(const Key('op-link')));
    await tester.pumpAndSettle();
    expect(rig.registered, hasLength(1));
    expect(find.text('맥에서 한 번만 더'), findsOneWidget);
    expect(find.text('연결됐어요'), findsNothing);
    expect(find.textContaining('새 판으로 업데이트해 주세요'), findsOneWidget);
  });

  testWidgets('토큰을 넣어 둔 옛 판 맥도 허락할 수 있는 맥으로 센다', (tester) async {
    final rig = Rig(keychain: false, token: true);
    await tester.pumpWidget(host(rig, FakeSigner(public: null)));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('op-link')));
    await tester.pumpAndSettle();
    expect(find.text('연결됐어요'), findsOneWidget);
    expect(find.textContaining('옛 판(1Password)'), findsOneWidget);
  });

  testWidgets('맥 목록 없이(승인 화면에서) 들어와도 맡기고 나면 연결로 보인다', (tester) async {
    final rig = Rig();
    await tester.pumpWidget(MaterialApp(
      theme: buildTheme(Brightness.light),
      home: ApprovalKeyScreen(
        api: () => RelayAccountApi(session().origin, session: session(), client: rig.gateway),
        signer: FakeSigner(public: null),
      ),
    ));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('op-link')));
    await tester.pumpAndSettle();
    expect(rig.registered, hasLength(1));
    expect(find.text('연결됐어요'), findsOneWidget);
    expect(find.byKey(const Key('op-link')), findsNothing);
  });

  test('옛 판 맥의 답(op 없음)은 업데이트 안내가 된다', () {
    expect(OpLink.parse({'ok': false, 'error': 'unknown action'}), isNull);
    final link = OpLink.parse({'ok': true, 'op': {'token': true, 'vault': 'KASA 학생', 'trusted': [{'fingerprint': mine}]}})!;
    expect(link.trusted, [mine]);
    expect(link.keychain, isFalse);
    final now = OpLink.parse({'ok': true, 'op': {'source': 'keychain', 'token': true, 'trusted': []}})!;
    expect(now.keychain, isTrue);
    expect(now.ready, isTrue);
  });
}
