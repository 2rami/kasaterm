import 'dart:async';
import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/archive_model.dart';
import 'package:kasaterm_mobile/archive_repository.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/conversation.dart';

import 'archive_fixtures.dart';

Server account([String name = 'alice']) => Server.account(
  archiveSession(name),
  connectDirect: false,
  client: MockClient(
    (_) async => throw StateError('desktop must not be reached'),
  ),
);
SavedConversation saved(String raw, {int revision = 1}) => SavedConversation(
  entry: entryFor(raw, revision: revision),
  raw: raw,
  savedAt: DateTime.utc(2026),
);

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  test('older cloud snapshots cannot replace a newer direct download', () {
    final remote = entryFor(message('assistant', 'saved'));
    final direct = ArchiveEntry.fromJson({
      ...remote.toJson(),
      'revision': 0,
      'updated_at': remote.updatedAt.millisecondsSinceEpoch ~/ 1000 + 30,
    });
    expect(remote.newerThan(direct), isFalse);
    final caughtUp = ArchiveEntry.fromJson({
      ...remote.toJson(),
      'revision': 2,
      'updated_at': direct.updatedAt.millisecondsSinceEpoch ~/ 1000 + 1,
    });
    expect(caughtUp.newerThan(direct), isTrue);
    final appended = ArchiveEntry.fromJson({
      ...remote.toJson(),
      'cursor': remote.cursor + 1,
    });
    expect(appended.newerThan(direct), isTrue);
  });
  test(
    'omitted photo-only turns remain visible in archived Claude and Codex conversations',
    () {
      final claude =
          '${jsonEncode({
            'type': 'user',
            'message': {
              'content': [
                {'type': 'image', 'source': {}},
              ],
            },
          })}\n';
      final parsed = Conversation(archived: true)
        ..apply(claude, reset: true, next: 1);
      expect(parsed.items.whereType<ChatBubble>().single.omittedImages, 1);
      final codex = [
        {
          'type': 'event_msg',
          'payload': {
            'type': 'user_message',
            'message': '',
            'images': [null],
          },
        },
        {
          'type': 'event_msg',
          'payload': {
            'type': 'item_completed',
            'item': {
              'type': 'UserMessage',
              'content': [
                {'type': 'input_image'},
              ],
            },
          },
        },
        {
          'type': 'event_msg',
          'payload': {'type': 'agent_message', 'message': '이미지를 확인했어요.'},
        },
        {
          'type': 'event_msg',
          'payload': {
            'type': 'user_message',
            'message': '',
            'images': [null],
          },
        },
      ].map(jsonEncode).join('\n');
      final turns = Conversation(archived: true)
        ..apply(codex, reset: true, next: 2);
      expect(
        turns.items.whereType<ChatBubble>().where((b) => b.user).length,
        2,
      );
      expect(
        turns.items
            .whereType<ChatBubble>()
            .where((b) => b.user)
            .every((b) => b.omittedImages == 1),
        isTrue,
      );
    },
  );
  test(
    'downloaded records load before a failed network request and survive a new repository',
    () async {
      final raw = message('user', '오프라인 기록');
      final server = account();
      final store = MemoryArchiveStore();
      final first = ArchiveRepository(
        server,
        store: store,
        api: FakeArchiveApi(entryFor(raw), raw),
      );
      await first.save(entryFor(raw));
      first.dispose();
      final api = FakeArchiveApi(entryFor(raw), raw)..failCatalog = true;
      final restarted = ArchiveRepository(server, store: store, api: api);
      await restarted.load();
      expect(restarted.saved.single.id, entryFor(raw).id);
      expect((await restarted.open(entryFor(raw).id))!.raw, raw);
      expect(restarted.error, isNotNull);
      final other = ArchiveRepository(
        account('bob'),
        store: store,
        api: FakeArchiveApi(entryFor(raw), raw)..failCatalog = true,
      );
      await other.load();
      expect(other.saved, isEmpty);
      expect(await other.open(entryFor(raw).id), isNull);
      other.server.close();
      other.dispose();
      restarted.dispose();
      server.close();
    },
  );

  test(
    'reconnection appends only new archive bytes without a desktop request',
    () async {
      final first = message('user', '첫 질문');
      final all = '$first${message('assistant', '새 답변')}';
      final server = account();
      final store = MemoryArchiveStore();
      await store.save(server.resumeScope, saved(first), current: () => true);
      final api = FakeArchiveApi(entryFor(all, revision: 2), all);
      final repo = ArchiveRepository(server, store: store, api: api);
      await repo.load(updateSaved: true);
      expect(api.requested, [utf8.encode(first).length]);
      expect((await repo.open(entryFor(all).id))!.raw, all);
      repo.dispose();
      server.close();
    },
  );

  test(
    'a failed refresh and a canceled download keep the old saved version',
    () async {
      final first = message('user', '저장한 질문');
      final all = '$first${message('assistant', '새 답변')}';
      final server = account();
      final store = MemoryArchiveStore();
      await store.save(server.resumeScope, saved(first), current: () => true);
      final api = FakeArchiveApi(entryFor(all, revision: 2), all)
        ..failRead = true;
      final repo = ArchiveRepository(server, store: store, api: api);
      await expectLater(repo.save(api.entry), throwsA(isA<ArchiveException>()));
      expect((await repo.open(api.entry.id))!.raw, first);
      api.failRead = false;
      api.readGate = Completer<void>();
      final download = repo.save(api.entry);
      await Future<void>.delayed(Duration.zero);
      repo.cancel();
      api.readGate!.complete();
      await expectLater(download, throwsA(isA<ArchiveException>()));
      expect((await repo.open(api.entry.id))!.raw, first);
      repo.dispose();
      server.close();
    },
  );

  test(
    'deleting during another repository download cannot resurrect the local record',
    () async {
      final raw = message('assistant', '사라져야 할 저장본');
      final server = account();
      final store = MemoryArchiveStore();
      final api = FakeArchiveApi(entryFor(raw), raw)
        ..readGate = Completer<void>();
      final downloading = ArchiveRepository(server, store: store, api: api);
      final manager = ArchiveRepository(
        server,
        store: store,
        api: FakeArchiveApi(entryFor(raw), raw),
      );
      final work = downloading.save(api.entry);
      await api.started.future;
      await manager.remove(api.entry.id);
      api.readGate!.complete();
      await expectLater(work, throwsA(isA<ArchiveException>()));
      expect(await store.read(server.resumeScope, api.entry.id), isNull);
      manager.dispose();
      downloading.dispose();
      server.close();
    },
  );

  test(
    'account closure during download rejects late content and locks the reader',
    () async {
      final raw = message('user', '이전 계정 기록');
      final server = account();
      final store = MemoryArchiveStore();
      final api = FakeArchiveApi(entryFor(raw), raw)
        ..readGate = Completer<void>();
      final repo = ArchiveRepository(server, store: store, api: api);
      final work = repo.save(api.entry);
      await api.started.future;
      server.close();
      api.readGate!.complete();
      await expectLater(work, throwsA(isA<ArchiveException>()));
      expect(repo.locked, isTrue);
      expect(await store.read(server.resumeScope, api.entry.id), isNull);
      await expectLater(
        repo.open(api.entry.id),
        throwsA(isA<ArchiveException>()),
      );
      repo.dispose();
    },
  );

  test(
    'changed generations and incomplete chunks never replace a complete saved record',
    () async {
      final raw = message('user', '저장된 질문');
      final more = '$raw${message('assistant', '추가 답')}';
      for (final alter in <ArchiveChunk Function(ArchiveChunk)>[
        (c) => ArchiveChunk(
          raw: c.raw,
          next: c.next,
          end: c.end,
          generation: repeated('c', 32),
        ),
        (c) => ArchiveChunk(
          raw: '',
          next: utf8.encode(raw).length,
          end: c.end,
          generation: c.generation,
        ),
        (c) => ArchiveChunk(
          raw: c.raw,
          next: c.next - 1,
          end: c.end,
          generation: c.generation,
        ),
      ]) {
        final server = account();
        final store = MemoryArchiveStore();
        await store.save(server.resumeScope, saved(raw), current: () => true);
        final api = FakeArchiveApi(entryFor(more, revision: 2), more)
          ..alter = alter;
        final repo = ArchiveRepository(server, store: store, api: api);
        await expectLater(
          repo.save(api.entry),
          throwsA(isA<ArchiveException>()),
        );
        expect((await repo.open(api.entry.id))!.raw, raw);
        repo.dispose();
        server.close();
      }
    },
  );

  test(
    'source downloads start at zero and distinguish source cursors from stored byte lengths',
    () async {
      final one = message('user', '맨 처음'), two = message('assistant', '마지막');
      final calls = <int>[];
      final server = Server(
        Uri.parse('http://127.0.0.1:1/'),
        client: MockClient((request) async {
          expect(request.url.path, '/conversation-archive');
          final from = int.parse(request.url.queryParameters['offset']!);
          calls.add(from);
          return http.Response(
            jsonEncode({
              'ok': true,
              'archive_version': 1,
              'raw': from == 0
                  ? one
                  : from == 1000
                  ? two
                  : '',
              'offset': from == 0 ? 1000 : 2000,
              'reset': from == 0,
              'source': 'fixture-session.jsonl',
              'address': {
                'surface_id': '%1',
                'machine_id': 'machine-a',
                'surface_key': 'fixture-key',
              },
            }),
            200,
            headers: {'content-type': 'application/json'},
          );
        }),
      );
      final store = MemoryArchiveStore();
      final repo = ArchiveRepository(server, store: store);
      const pane = Pane(
        id: '%1',
        name: 'Assistant',
        title: '대화',
        status: 'idle',
        window: 0,
        cwd: '/project',
        harness: 'claude',
        surfaceKey: 'fixture-key',
      );
      final result = await repo.savePane(pane, source: 'fixture-session.jsonl');
      expect(calls, [0, 1000, 2000]);
      expect(result.raw, '$one$two');
      expect(result.entry.bytes, utf8.encode('$one$two').length);
      expect(result.entry.cursor, 2000);
      repo.dispose();
      server.close();
    },
  );
}
