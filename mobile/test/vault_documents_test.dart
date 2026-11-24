import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:fsrs/fsrs.dart' as fsrs;
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/documents/vault_api.dart';
import 'package:kasaterm_mobile/documents/vault_reader.dart';
import 'package:kasaterm_mobile/documents/vault_study.dart';
import 'package:kasaterm_mobile/kasanet.dart';
import 'package:kasaterm_mobile/relay_account.dart';
import 'package:kasaterm_mobile/server.dart';

http.Response _response(Map<String, Object?> body, [int status = 200]) =>
    http.Response.bytes(utf8.encode(jsonEncode(body)), status);

class _DirectNative implements KasanetNative {
  @override
  String get id => 'phone';

  @override
  String? get lastError => null;

  @override
  void networkChanged() {}

  @override
  int open(String peerJson) => 34567;

  @override
  KasanetPath state(int port) => const KasanetPath(direct: true);
}

void main() {
  test(
    'regular tables become cards, including escaped pipes and extra columns',
    () {
      final doc = VaultMarkdown(r'''
# 언어

| 단어 | 뜻 | 예시 |
| --- | --- | --- |
| **調べる** | 알아보다 | 조사하다 |
| a\|b | 선택 | 셸 |

```markdown
| 무시 | 설명 |
| --- | --- |
| 예시 | 코드 |
```
''');
      expect(doc.cards, [
        (front: '調べる', back: '알아보다\n조사하다'),
        (front: 'a|b', back: '선택\n셸'),
      ]);
      expect(doc.body, startsWith('# 언어'));
    },
  );

  test('deck front and back may reorder or select columns', () {
    final doc = VaultMarkdown('''---
deck: true
front: "일본어"
back: 뜻
---
| 뜻 | 읽기 | 일본어 |
| --- | --- | --- |
| 알아보다 | しらべる | 調べる |
''');
    expect(doc.cards, [(front: '調べる', back: '알아보다')]);
    expect(doc.body, startsWith('| 뜻'));
  });

  test('review scheduling persists per scope, deck and front', () async {
    FlutterSecureStorage.setMockInitialValues({});
    const store = VaultStudyStore('account|device');
    const other = VaultStudyStore('other account|device');
    const card = (front: '調べる', back: '알아보다');
    final now = DateTime.utc(2026, 11, 24, 12);
    final scheduled = fsrs.Scheduler(enableFuzzing: false)
        .reviewCard(
          fsrs.Card(cardId: 1, due: now),
          fsrs.Rating.good,
          reviewDateTime: now,
        )
        .card;
    const path = '일본어/어휘.md';

    expect(await store.read(path, card), isNull);
    await store.write(path, card, scheduled);
    final saved = await store.read(path, (front: card.front, back: '조사하다'));
    expect(saved?.due, now.add(const Duration(minutes: 10)));
    expect(saved?.lastReview, now);
    expect(await store.read('다른 덱.md', card), isNull);
    expect(await other.read(path, card), isNull);
  });

  test(
    'vault requests use a stable device and a failed read never uses the gateway',
    () async {
      final calls = <Uri>[];
      final client = MockClient((req) async {
        calls.add(req.url);
        if (req.url.path.endsWith('/version')) {
          return _response({
            'machine_id': 'mini',
            'kasanet': {'id': 'mini'},
          });
        }
        if (req.url.path.endsWith('/kasanet/phone')) {
          return _response({'ttl_secs': 60});
        }
        expect(req.url.host, '127.0.0.1');
        expect(req.url.port, 34567);
        expect(req.url.queryParameters['path'], '리서치/용어.md');
        expect(req.headers['authorization'], isNull);
        expect(req.followRedirects, isFalse);
        throw http.ClientException('disconnected', req.url);
      });
      final server = Server.account(
        AccountSession(
          origin: Uri.parse('https://gateway.invalid'),
          account: 'account',
          deviceId: 'device',
          token: 'session',
        ),
        client: client,
        kasanet: _DirectNative(),
      );
      addTearDown(server.close);
      final api = VaultApi.kasanet(server, '~mini');
      await expectLater(
        api.file('리서치/용어.md'),
        throwsA(isA<http.ClientException>()),
      );
      expect(calls.map((call) => call.path), [
        '/relay/account/m/~mini/version',
        '/relay/account/m/~mini/kasanet/phone',
        '/vault/file',
      ]);
    },
  );

  test(
    'the vault device generates and serves a deck without gateway fallback',
    () async {
      final sent = <({Uri uri, String method, String body})>[];
      final client = MockClient((req) async {
        sent.add((uri: req.url, method: req.method, body: req.body));
        final path = req.url.path;
        if (path.endsWith('/version')) {
          final machine = req.url.pathSegments[3].substring(1);
          return _response({
            'machine_id': machine,
            'kasanet': {'id': machine},
          });
        }
        if (path.endsWith('/kasanet/phone')) {
          return _response({'ttl_secs': 60});
        }
        if (path == '/vault/quiz') {
          expect(req.url.port, 34567);
          expect(jsonDecode(req.body), {
            'path': '메모리.md',
            'kind': 'terms',
            'n': 30,
          });
          return _response({
            'deck_path': '_study/퀴즈.md',
            'cards': 3,
            'title': '퀴즈',
          });
        }
        expect(path, '/vault/file');
        expect(req.url.port, 34567);
        return _response({
          'path': req.url.queryParameters['path'],
          'text': '# 문서',
          'sha': 'abc',
        });
      });
      final server = Server.account(
        AccountSession(
          origin: Uri.parse('https://gateway.invalid'),
          account: 'account',
          deviceId: 'device',
          token: 'session',
        ),
        client: client,
        kasanet: _DirectNative(),
      );
      addTearDown(server.close);
      final api = VaultApi.kasanet(server, '~mini');
      final path = await api.createQuiz('메모리.md', VaultQuizKind.terms);
      expect((await api.file(path)).path, '_study/퀴즈.md');
      expect(sent.map((call) => call.uri.path), [
        '/relay/account/m/~mini/version',
        '/relay/account/m/~mini/kasanet/phone',
        '/vault/quiz',
        '/vault/file',
      ]);
      expect(
        sent
            .where((call) => call.uri.host == 'gateway.invalid')
            .every(
              (call) => !call.body.contains('메모리.md') && !call.uri.hasQuery,
            ),
        isTrue,
      );
    },
  );

  testWidgets('a git-busy response retains the edit and its original SHA', (
    tester,
  ) async {
    final sent = <Map<String, dynamic>>[];
    final api = VaultApi((method, path, {body, query, required timeout}) async {
      sent.add(jsonDecode(body!) as Map<String, dynamic>);
      if (sent.length == 1) return _response({'error': 'git busy, retry'}, 503);
      return _response({
        'path': sent.last['path'],
        'sha': 'saved',
        'commit': 'commit',
      });
    });
    await _openEditor(tester, api);
    await tester.enterText(find.byType(TextField), '저장 대기 중인 글');
    await tester.pump();
    await tester.tap(find.text('저장'));
    await tester.pumpAndSettle();
    expect(find.text('볼트가 다른 변경을 저장 중이에요. 잠시 뒤 다시 저장해 주세요.'), findsOneWidget);
    expect(find.text('저장 대기 중인 글'), findsOneWidget);
    await tester.tap(find.text('저장'));
    await tester.pumpAndSettle();
    expect(sent.map((req) => req['base_sha']), ['original', 'original']);
    expect(sent.map((req) => req['text']).toSet(), {'저장 대기 중인 글'});
  });

  testWidgets(
    'retry opens the document and a generated deck opens by its returned path',
    (tester) async {
      var reads = 0;
      var changes = 0;
      final api = VaultApi((
        method,
        path, {
        body,
        query,
        required timeout,
      }) async {
        if (method == 'POST') {
          expect(path, 'quiz');
          expect(jsonDecode(body!), {
            'path': '리서치/용어.md',
            'kind': 'terms',
            'n': 30,
          });
          return _response({'deck_path': '_study/용어 덱.md', 'cards': 1});
        }
        final file = query!['path']!;
        if (reads++ == 0) return _response({'error': 'unavailable'}, 503);
        return _response({'path': file, 'text': '# 문서 본문', 'sha': 'current'});
      });
      await tester.pumpWidget(
        MaterialApp(
          home: VaultReader(
            api: api,
            path: '리서치/용어.md',
            studyScope: 'test',
            onChanged: () {
              changes++;
            },
          ),
        ),
      );
      await tester.pumpAndSettle();
      await tester.tap(find.text('다시 시도'));
      await tester.pumpAndSettle();
      expect(find.text('용어'), findsOneWidget);
      await tester.tap(find.byTooltip('덱 만들기'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('용어 덱 만들기'));
      await tester.pumpAndSettle();
      expect(find.text('용어 덱'), findsOneWidget);
      expect(reads, 3);
      expect(changes, 1);
    },
  );

  testWidgets('reload discards the draft and adopts the latest SHA', (
    tester,
  ) async {
    final sent = <Map<String, dynamic>>[];
    final api = VaultApi((method, path, {body, query, required timeout}) async {
      final draft = jsonDecode(body!) as Map<String, dynamic>;
      sent.add(draft);
      return sent.length == 1
          ? _response({
              'error': 'conflict',
              'sha': 'remote',
              'text': '다른 기기 문서',
            }, 409)
          : _response({
              'path': draft['path'],
              'sha': 'saved',
              'commit': 'commit',
            });
    });
    await _openEditor(tester, api);
    await tester.enterText(find.byType(TextField), '폰에서 고친 초안');
    await tester.pump();
    await tester.tap(find.text('저장'));
    await tester.pumpAndSettle();
    expect(find.text('다른 곳에서 바뀜'), findsOneWidget);
    await tester.tap(find.text('다시 불러오기'));
    await tester.pumpAndSettle();
    expect(find.text('다른 기기 문서'), findsOneWidget);
    await tester.enterText(find.byType(TextField), '최신 문서 위에 편집');
    await tester.pump();
    await tester.tap(find.text('저장'));
    await tester.pumpAndSettle();
    expect(sent.map((req) => req['base_sha']), ['original', 'remote']);
    expect(sent.last['text'], '최신 문서 위에 편집');
  });

  testWidgets(
    'a deleted file is restored using a null base SHA only after confirmation',
    (tester) async {
      final sent = <Map<String, dynamic>>[];
      final api = VaultApi((
        method,
        path, {
        body,
        query,
        required timeout,
      }) async {
        final draft = jsonDecode(body!) as Map<String, dynamic>;
        sent.add(draft);
        return sent.length == 1
            ? _response({'error': 'conflict', 'sha': null, 'text': null}, 409)
            : _response({
                'path': draft['path'],
                'sha': 'restored',
                'commit': 'commit',
              });
      });
      await _openEditor(tester, api);
      await tester.enterText(find.byType(TextField), '복원할 초안');
      await tester.pump();
      await tester.tap(find.text('저장'));
      await tester.pumpAndSettle();
      expect(sent.length, 1);
      await tester.tap(find.text('초안으로 복원'));
      await tester.pumpAndSettle();
      expect(sent.map((req) => req['base_sha']), ['original', null]);
      expect(sent.last['text'], '복원할 초안');
    },
  );

  testWidgets(
    'overwrite asks again if another edit arrives during the decision',
    (tester) async {
      final sent = <Map<String, dynamic>>[];
      final api = VaultApi((
        method,
        path, {
        body,
        query,
        required timeout,
      }) async {
        final draft = jsonDecode(body!) as Map<String, dynamic>;
        sent.add(draft);
        return sent.length <= 2
            ? _response({
                'error': 'conflict',
                'sha': 'remote${sent.length}',
                'text': '다른 기기 문서',
              }, 409)
            : _response({
                'path': draft['path'],
                'sha': 'saved',
                'commit': 'commit',
              });
      });
      await _openEditor(tester, api);
      await tester.enterText(find.byType(TextField), '같은 폰 초안');
      await tester.pump();
      await tester.tap(find.text('저장'));
      await tester.pumpAndSettle();
      for (var i = 0; i < 2; i++) {
        expect(find.text('다른 곳에서 바뀜'), findsOneWidget);
        expect(sent.length, i + 1);
        await tester.tap(find.text('내 것으로 덮기'));
        await tester.pumpAndSettle();
      }
      expect(sent.map((req) => req['base_sha']), [
        'original',
        'remote1',
        'remote2',
      ]);
      expect(sent.map((req) => req['text']).toSet(), {'같은 폰 초안'});
      expect(find.text('편집 종료'), findsOneWidget);
    },
  );
}

Future<void> _openEditor(WidgetTester tester, VaultApi api) async {
  await tester.pumpWidget(
    MaterialApp(
      home: Builder(
        builder: (context) => Scaffold(
          body: TextButton(
            onPressed: () => Navigator.of(context).push<VaultFile>(
              MaterialPageRoute(
                builder: (_) => VaultEditor(
                  api: api,
                  file: (path: '리서치/용어.md', text: '원본', sha: 'original'),
                ),
              ),
            ),
            child: const Text('편집 종료'),
          ),
        ),
      ),
    ),
  );
  await tester.tap(find.text('편집 종료'));
  await tester.pumpAndSettle();
}
