import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/approvals.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/screens/approval_screen.dart';
import 'package:kasaterm_mobile/secure_key.dart';

import 'relay_account_test.dart' show session;

/// 관문·맥 시험(`approval_text::key_id`)과 같은 벡터 — 공개키 0x04,1..64.
const vectorPublic = 'BAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0+P0A=';

String challenge({String nonce = '00112233445566778899aabbccddeeff'}) => jsonEncode({
  'v': 1,
  'kind': 'op.read',
  'nonce': nonce,
  'account': 'geno',
  'device': 'dev_book',
  'student': '유우카',
  'pane': '%3',
  'cwd': '/Users/kasa/repo',
  'command': 'kasaterm-cli op run -e DB=op://kasaterm-agents/db/password -- psql',
  'refs': ['op://kasaterm-agents/db/password'],
  'exp': 120000,
});

Map<String, Object?> secretPending({String? text}) => {
  'id': 'apv_0123456789abcdef0123456789abcdef',
  'kind': 'secret',
  'challenge': text ?? challenge(),
  'state': 'pending',
  'machine': '건호의 MacBook Pro',
  'device': 'dev_book',
  'student': '다른 이름',
  'pane': '%9',
  'cwd': '/elsewhere',
  'tool': '1Password',
  'fields': [
    {'name': 'refs', 'label': '참조', 'text': 'op://somewhere/else/x'},
  ],
  'truncated': false,
  'created': 0,
  'expires': 120000,
  'now': 0,
  'digest': 'd1',
  'by': null,
};

class FakeSigner implements ApprovalSigner {
  FakeSigner({this.public = vectorPublic, this.answer = 'MEUCIQ=='});
  String? public;
  String? answer;
  final signed = <String>[];

  @override
  Future<String?> publicKey() async => public;
  @override
  Future<String> create() async => public = vectorPublic;
  @override
  Future<void> delete() async => public = null;
  @override
  Future<String?> sign(String challenge, String reason) async {
    signed.add(challenge);
    if (answer == 'throw') throw PlatformException(code: 'sign_failed', message: 'biometry changed');
    return answer;
  }
}

void main() {
  test('키 id·지문은 관문·맥과 같은 값이다', () {
    final id = approvalKeyId(base64.decode(vectorPublic));
    expect(id, '0ed3a6ab957ff6f59a9630a473d31a7d');
    expect(approvalFingerprint(id), '0ED3-A6AB-957F');
  });

  test('비밀 요청은 도전값에서 편 것만 보인다 — 관문이 실은 다른 칸·학생은 쓰지 않는다', () {
    final a = Approval.fromJson(secretPending())!;
    expect(a.isSecret, isTrue);
    expect(a.secretValid, isTrue);
    expect(a.student, '유우카');
    expect(a.pane, '%3');
    expect(a.cwd, '/Users/kasa/repo');
    expect(a.refs, ['op://kasaterm-agents/db/password']);
    expect(a.fields.first.text, 'op://kasaterm-agents/db/password');
    expect(a.fields.last.text, contains('op run'));
    final broken = Approval.fromJson(secretPending(text: '{"v":2}'))!;
    expect(broken.secretValid, isFalse);
  });

  ApprovalCenter centerWith(http.Client client, FakeSigner signer) {
    final c = ApprovalCenter(
      api: (s) => RelayAccountApi(s.origin, session: s, client: client),
      clock: () => DateTime.fromMillisecondsSinceEpoch(0),
      signer: signer,
    );
    c.bind(session());
    c.setForeground(false);
    c.debugSeed([Approval.fromJson(secretPending())!]);
    return c;
  }

  test('Face ID 로 서명한 허락은 도전값 그대로에 서명하고 키 id·서명을 함께 보낸다', () async {
    final sent = <Map<String, Object?>>[];
    final client = MockClient((req) async {
      if (req.url.path.endsWith('/approver')) return http.Response('{"ok":true}', 200);
      sent.add(jsonDecode(req.body) as Map<String, Object?>);
      return http.Response(jsonEncode({'ok': true, 'approval': {...secretPending(), 'state': 'allowed'}}), 200,
          headers: {'content-type': 'application/json; charset=utf-8'});
    });
    final signer = FakeSigner();
    final center = centerWith(client, signer);
    final a = center.pending.single;
    await center.decide(a, true);
    expect(signer.signed, [challenge()]);
    expect(sent.single['key'], '0ed3a6ab957ff6f59a9630a473d31a7d');
    expect(sent.single['sig'], 'MEUCIQ==');
    expect(sent.single['decision'], 'allow');
  });

  test('Face ID 취소·실패·열쇠 없음이면 관문에 아무것도 보내지 않는다. 거절은 서명 없이 나간다', () async {
    final sent = <String>[];
    final client = MockClient((req) async {
      sent.add(req.url.path);
      return http.Response(jsonEncode({'ok': true, 'approval': {...secretPending(), 'state': 'denied'}}), 200,
          headers: {'content-type': 'application/json; charset=utf-8'});
    });
    for (final (signer, code) in [
      (FakeSigner(answer: null), 'cancelled'),
      (FakeSigner(answer: 'throw'), 'sign_failed'),
      (FakeSigner(public: null), 'no_key'),
    ]) {
      final center = centerWith(client, signer);
      await expectLater(
        center.decide(center.pending.single, true),
        throwsA(isA<AccountException>().having((e) => e.code, 'code', code)),
      );
    }
    // 목록 받기(긴 폴링)만 있고 결정·승인 열쇠 등록은 한 번도 없다.
    expect(sent.where((p) => p.endsWith('/decide') || p.endsWith('/approver')), isEmpty);
    final signer = FakeSigner();
    final center = centerWith(client, signer);
    await center.decide(center.pending.single, false);
    expect(signer.signed, isEmpty);
    expect(sent.where((p) => p.endsWith('/decide')), hasLength(1));
  });

  testWidgets('비밀 요청 화면은 참조·명령과 「Face ID 로 허락」을 보이고, 모양이 틀린 요청은 허락을 막는다', (tester) async {
    final client = MockClient((req) async => http.Response('{"ok":true,"approvals":[]}', 200));
    final center = centerWith(client, FakeSigner());
    await tester.pumpWidget(MaterialApp(theme: buildTheme(Brightness.light), home: ApprovalScreen(center: center, id: 'apv_0123456789abcdef0123456789abcdef')));
    await tester.pump();
    expect(find.text('1Password 승인 요청'), findsOneWidget);
    expect(find.text('Face ID 로 허락'), findsOneWidget);
    expect(find.byKey(const Key('approval-secret-note')), findsOneWidget);
    expect(find.text('op://kasaterm-agents/db/password'), findsOneWidget);

    center.debugSeed([Approval.fromJson(secretPending(text: 'not json'))!]);
    await tester.pump();
    final allow = tester.widget<InkWell>(
      find.descendant(of: find.byKey(const Key('approval-allow')), matching: find.byType(InkWell)),
    );
    expect(allow.onTap, isNull);
  });
}
