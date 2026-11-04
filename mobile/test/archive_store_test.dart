import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/archive_model.dart';
import 'package:kasaterm_mobile/archive_store_io.dart';

import 'archive_fixtures.dart';

void main() {
  late Directory dir;
  late IoArchiveStore store;
  const scope = 'https://archive.invalid|alice';
  setUp(() async {
    dir = await Directory.systemTemp.createTemp('offline-chat-store-');
    store = IoArchiveStore(dir);
  });
  tearDown(() async {
    await dir.delete(recursive: true);
  });

  test(
    'a new store instance reads the complete account-scoped conversation offline',
    () async {
      final raw = message('user', '인터넷 없이 읽어요.');
      final entry = entryFor(raw);
      await store.save(
        scope,
        SavedConversation(entry: entry, raw: raw, savedAt: DateTime.utc(2026)),
        current: () => true,
      );
      final reopened = IoArchiveStore(dir);
      await reopened.recover();
      expect((await reopened.read(scope, entry.id))!.raw, raw);
      expect(await reopened.list('https://archive.invalid|bob'), isEmpty);
      expect(
        await reopened.read('https://archive.invalid|bob', entry.id),
        isNull,
      );
      expect((await dir.list().toList()).every((entry) => !entry.path.contains('alice')), isTrue);
    },
  );

  test(
    'canceling after writing new bytes retains the previously committed download',
    () async {
      final raw = message('user', '기존 저장본');
      final first = entryFor(raw);
      await store.save(
        scope,
        SavedConversation(entry: first, raw: raw, savedAt: DateTime.utc(2026)),
        current: () => true,
      );
      final newer = '$raw${message('assistant', '추가 답변')}';
      var checks = 0;
      await expectLater(
        store.save(
          scope,
          SavedConversation(
            entry: entryFor(newer, revision: 2),
            raw: newer,
            savedAt: DateTime.utc(2026),
          ),
          current: () => ++checks < 3,
        ),
        throwsA(isA<ArchiveException>()),
      );
      expect((await store.read(scope, first.id))!.raw, raw);
      expect(
        (await dir.list(recursive: true).toList()).where(
          (f) => f.path.endsWith('.part'),
        ),
        isEmpty,
      );
    },
  );

  test(
    'an incomplete or corrupt body is never reported as a complete downloaded record',
    () async {
      final raw = message('user', '확인할 기록');
      final entry = entryFor(raw);
      await expectLater(
        store.save(
          scope,
          SavedConversation(
            entry: entry,
            raw: 'short',
            savedAt: DateTime.utc(2026),
          ),
          current: () => true,
        ),
        throwsA(isA<ArchiveException>()),
      );
      expect(await store.list(scope), isEmpty);
      await store.save(
        scope,
        SavedConversation(entry: entry, raw: raw, savedAt: DateTime.utc(2026)),
        current: () => true,
      );
      final body = (await dir.list(recursive: true).toList())
          .whereType<File>()
          .singleWhere((f) => f.path.endsWith('.jsonl'));
      await body.writeAsString(repeated('x', entry.bytes));
      await expectLater(
        store.read(scope, entry.id),
        throwsA(isA<ArchiveException>()),
      );
    },
  );

  test(
    'a damaged manifest does not erase other records and can be explicitly replaced',
    () async {
      final raw = message('user', '다시 받은 기록');
      final entry = entryFor(raw);
      await store.save(
        scope,
        SavedConversation(entry: entry, raw: raw, savedAt: DateTime.utc(2026)),
        current: () => true,
      );
      final meta = (await dir.list(recursive: true).toList())
          .whereType<File>()
          .singleWhere((f) => f.path.endsWith('.json'));
      await meta.writeAsString('{');
      expect(await store.list(scope), isEmpty);
      await store.save(
        scope,
        SavedConversation(entry: entry, raw: raw, savedAt: DateTime.utc(2026)),
        current: () => true,
      );
      expect((await store.read(scope, entry.id))!.raw, raw);
    },
  );

  test(
    'recovery removes uncommitted files but preserves the published snapshot',
    () async {
      final raw = message('assistant', '저장 완료');
      final entry = entryFor(raw);
      await store.save(
        scope,
        SavedConversation(entry: entry, raw: raw, savedAt: DateTime.utc(2026)),
        current: () => true,
      );
      final parent = Directory(
        '${dir.path}/${sha256.convert(utf8.encode(scope))}',
      );
      final part = File('${parent.path}/${entry.id}.json.failed.part');
      await part.writeAsString('half');
      final orphan = File(
        '${parent.path}/${repeated('c', 64)}-${repeated('d', 64)}.jsonl',
      );
      await orphan.writeAsString('orphan');
      final reopened = IoArchiveStore(dir);
      await reopened.recover();
      expect(await part.exists(), isFalse);
      expect(await orphan.exists(), isFalse);
      expect((await reopened.read(scope, entry.id))!.raw, raw);
      await reopened.remove(scope, entry.id);
      expect(await reopened.list(scope), isEmpty);
    },
  );

  test(
    'traversal and linked account folders cannot reach another location',
    () async {
      await expectLater(
        store.read(scope, '../../elsewhere'),
        throwsA(isA<ArchiveException>()),
      );
      final outside = await Directory.systemTemp.createTemp(
        'offline-chat-outside-',
      );
      addTearDown(() => outside.delete(recursive: true));
      await Link(
        '${dir.path}/${sha256.convert(utf8.encode(scope))}',
      ).create(outside.path);
      await expectLater(store.list(scope), throwsA(isA<ArchiveException>()));
      expect(await outside.list().toList(), isEmpty);
    },
  );
}
