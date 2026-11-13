import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/archive_model.dart';
import 'package:kasaterm_mobile/archive_repository.dart';
import 'package:kasaterm_mobile/approvals.dart';
import 'package:kasaterm_mobile/connection_store.dart';
import 'package:kasaterm_mobile/hub_snapshot.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/screens/saved_conversations.dart';
import 'package:kasaterm_mobile/server.dart';

import 'archive_fixtures.dart';
import 'inline_result_fonts.dart';

void main() {
  setUpAll(() async {
    await loadInlineResultFonts();
    await (FontLoader('TermMono')..addFont(
          rootBundle.load('assets/fonts/JetBrainsMonoNerdFontMono-Regular.ttf'),
        ))
        .load();
    await (FontLoader('TermHangul')..addFont(
          rootBundle.load(
            'assets/fonts/D2CodingLigatureNerdFontMono-Regular.ttf',
          ),
        ))
        .load();
  });

  testWidgets(
    'saved chat renders messages and code offline without source requests or live controls',
    (tester) async {
      tester.view.physicalSize = const Size(390 * 3, 844 * 3);
      tester.view.devicePixelRatio = 3;
      addTearDown(tester.view.reset);
      var requests = 0;
      final server = Server.account(
        archiveSession(),
        connectDirect: false,
        client: MockClient((_) async {
          requests++;
          throw StateError('saved reader must not contact the desktop');
        }),
      );
      final raw =
          '${message('user', '인터넷이 끊겨도 읽을 수 있나요?')}${message('assistant', '네. 저장한 텍스트와 코드는 계속 읽을 수 있어요.\n\n```dart\nfinal saved = true;\n```\n\n```text\n[[ki:0123456789abcdef:00/01]]\n```')}';
      final saved = SavedConversation(
        entry: entryFor(raw),
        raw: raw,
        savedAt: DateTime.utc(2026),
      );
      final repo = ArchiveRepository(
        server,
        store: MemoryArchiveStore(),
        api: FakeArchiveApi(saved.entry, raw)..failCatalog = true,
      );
      await tester.pumpWidget(
        MaterialApp(
          theme: buildTheme(Brightness.light),
          home: SavedConversationScreen(repository: repo, conversation: saved),
        ),
      );
      await tester.pumpAndSettle();
      expect(
        find.textContaining('저장한 텍스트', findRichText: true),
        findsOneWidget,
      );
      expect(find.textContaining('읽기 전용'), findsOneWidget);
      expect(find.textContaining('이미지 첨부'), findsOneWidget);
      expect(find.byType(TextField), findsNothing);
      expect(find.text('허락'), findsNothing);
      expect(requests, 0);
      expect(tester.takeException(), isNull);
      await expectLater(
        find.byType(Scaffold),
        matchesGoldenFile('goldens/saved_conversation_offline.png'),
      );
      server.close();
      await tester.pump();
      expect(find.textContaining('저장한 텍스트', findRichText: true), findsNothing);
      expect(find.text('현재 계정의 연결이 종료됐어요.'), findsOneWidget);
      await tester.pumpWidget(const SizedBox());
      repo.dispose();
    },
  );

  for (final width in [320.0, 390.0, 430.0]) {
    testWidgets(
      'offline library at $width exposes cached history when the gateway is unavailable',
      (tester) async {
        tester.view.physicalSize = Size(width, 844);
        tester.view.devicePixelRatio = 1;
        addTearDown(tester.view.reset);
        final server = Server.account(
          archiveSession(),
          connectDirect: false,
          client: MockClient((_) async => throw StateError('no desktop')),
        );
        final raw = message('assistant', '연결이 없어도 남아 있는 답변');
        final entry = entryFor(raw, title: '연결이 끊긴 뒤에도 읽을 수 있는 길이가 긴 대화 제목');
        final store = MemoryArchiveStore();
        await store.save(
          server.resumeScope,
          SavedConversation(
            entry: entry,
            raw: raw,
            savedAt: DateTime.utc(2026),
          ),
          current: () => true,
        );
        final repo = ArchiveRepository(
          server,
          store: store,
          api: FakeArchiveApi(entry, raw)..failCatalog = true,
        );
        await tester.pumpWidget(
          MaterialApp(
            theme: buildTheme(Brightness.dark),
            home: SavedConversationsScreen(server: server, repository: repo),
          ),
        );
        await tester.pumpAndSettle();
        expect(find.text(entry.title), findsOneWidget);
        expect(find.textContaining('오프라인 저장됨'), findsOneWidget);
        expect(tester.takeException(), isNull);
        if (width == 390) {
          await expectLater(
            find.byType(Scaffold),
            matchesGoldenFile('goldens/saved_conversations_offline.png'),
          );
        }
        await tester.tap(find.text(entry.title));
        await tester.pumpAndSettle();
        expect(
          find.textContaining('남아 있는 답변', findRichText: true),
          findsOneWidget,
        );
        expect(tester.takeException(), isNull);
        await tester.pumpWidget(const SizedBox());
        repo.dispose();
        server.close();
      },
    );
  }

  testWidgets(
    'cached account exposes saved conversations while initial connection verification is offline',
    (tester) async {
      FlutterSecureStorage.setMockInitialValues({
        ConnectionStore.key: jsonEncode(archiveSession().toJson()),
      });
      // 본 허브가 없는 계정 — 지난 허브 대신 대기 화면이 저장한 대화 입구를 연다. 파일 읽기는 가짜 시간에서 안 끝난다.
      HubSnapshots.shared = HubSnapshots.memory();
      await tester.pumpWidget(
        MaterialApp(
          navigatorKey: navigatorKey,
          theme: buildTheme(Brightness.light),
          home: const RootScreen(),
        ),
      );
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));
      expect(find.text('저장한 대화'), findsOneWidget);
      expect(tester.takeException(), isNull);
      ApprovalCenter.instance.bind(null);
      await tester.pumpWidget(const SizedBox());
      await tester.pump(const Duration(seconds: 3));
    },
  );

  testWidgets(
    'deleted cloud record exposes restore inside its management menu',
    (tester) async {
      final server = Server.account(archiveSession(), connectDirect: false);
      final raw = message('assistant', '남아 있는 폰 사본');
      final entry = entryFor(raw);
      final deleted = ArchiveEntry.fromJson({
        ...entry.toJson(),
        'deleted': true,
      });
      final store = MemoryArchiveStore();
      await store.save(
        server.resumeScope,
        SavedConversation(entry: entry, raw: raw, savedAt: DateTime.utc(2026)),
        current: () => true,
      );
      final repo = ArchiveRepository(
        server,
        store: store,
        api: FakeArchiveApi(deleted, raw),
      );
      await tester.pumpWidget(
        MaterialApp(
          theme: buildTheme(Brightness.light),
          home: SavedConversationsScreen(server: server, repository: repo),
        ),
      );
      await tester.pumpAndSettle();
      expect(find.text('계정에 다시 동기화'), findsNothing);
      await tester.tap(find.byTooltip('대화 관리'));
      await tester.pumpAndSettle();
      expect(find.text('계정에 다시 동기화'), findsOneWidget);
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox());
      repo.dispose();
      server.close();
    },
  );
}
