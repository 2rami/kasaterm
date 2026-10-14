import 'dart:convert';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:kasaterm_mobile/main.dart' show buildTheme;
import 'package:kasaterm_mobile/screens/share_browser.dart';
import 'package:kasaterm_mobile/screens/share_screen.dart';
import 'package:kasaterm_mobile/server.dart';
import 'share_test_fonts.dart';

ShareListing fixture() => ShareListing.fromJson({
  'name': 'KASA-share',
  'folders': [
    {
      'name': '2026-10-14-검토자료',
      'modified_ms': 2000,
      'files': [
        {
          'path': '2026-10-14-검토자료/설계.md',
          'name': '설계.md',
          'kind': 'markdown',
          'origin': 'Mac mini',
          'modified_ms': 2000,
          'size': 240,
        },
        {
          'path': '2026-10-14-검토자료/하위/그림.png',
          'name': '하위/그림.png',
          'kind': 'image',
          'origin': 'Windows PC',
          'modified_ms': 1000,
          'size': 100,
        },
      ],
    },
  ],
  'files': [
    {
      'path': '최신.txt',
      'name': '최신.txt',
      'kind': 'text',
      'origin': 'MacBook',
      'modified_ms': 3000,
      'size': 20,
    },
  ],
});

void main() {
  setUpAll(loadShareFonts);
  test(
    'recent search spans nested folders and preserves full file identity',
    () {
      final listing = fixture();
      expect(
        matchingShareFiles(
          listing,
          '',
          ShareFilter.all,
        ).map((file) => file.name),
        ['최신.txt', '설계.md', '하위/그림.png'],
      );
      expect(
        matchingShareFiles(listing, 'mini 문서', ShareFilter.all).single.path,
        '2026-10-14-검토자료/설계.md',
      );
      expect(
        matchingShareFiles(listing, 'windows', ShareFilter.images).single.kind,
        ShareKind.image,
      );
      expect(matchingShareFiles(listing, '그림', ShareFilter.documents), isEmpty);
      expect(matchingShareFiles(listing, '없음', ShareFilter.all), isEmpty);
    },
  );

  test(
    'old servers remain valid and sync labels never claim cloud storage',
    () {
      expect(fixture().sync, isNull);
      final current = ShareSync.fromJson({
        'updated_ms': DateTime.now().millisecondsSinceEpoch,
        'peers': [
          {'label': 'mini', 'ok': true},
        ],
      });
      expect(current.summary, contains('받기 확인'));
      expect(current.summary, contains('클라우드 보관은 아님'));
      expect(
        ShareSync.fromJson({'updated_ms': 1, 'peers': []}).summary,
        contains('늦어지고'),
      );
      expect(
        ShareSync.fromJson({'paused_deletes': 20}).summary,
        contains('삭제 동기화를 멈췄'),
      );
    },
  );

  for (final width in [320.0, 390.0, 430.0]) {
    for (final brightness in Brightness.values) {
      testWidgets(
        'share $width $brightness exposes data and keeps search in bounds',
        (tester) async {
          tester.view.physicalSize = Size(width, 844);
          tester.view.devicePixelRatio = 1;
          addTearDown(tester.view.reset);
          final server = Server(
            Uri.parse('http://127.0.0.1:1/'),
            client: MockClient(
              (_) async => http.Response(jsonEncode({'ok': true}), 200),
            ),
          );
          await tester.pumpWidget(
            MaterialApp(
              theme: buildTheme(brightness),
              home: ShareScreen(server: server, initial: fixture()),
            ),
          );
          await tester.pump(const Duration(milliseconds: 300));
          final first = tester.getRect(
            find.byKey(const ValueKey('share-file-최신.txt')),
          );
          expect(first.top, lessThanOrEqualTo(300));
          expect(first.left, greaterThanOrEqualTo(0));
          expect(first.right, lessThanOrEqualTo(width));
          final input = find.byKey(const ValueKey('share-search'));
          expect(
            tester.widget<TextField>(input).style!.fontSize,
            greaterThanOrEqualTo(16),
          );
          expect(tester.getSize(input).height, greaterThanOrEqualTo(44));
          expect(
            tester.getSize(find.byTooltip('파일 종류')).height,
            greaterThanOrEqualTo(44),
          );
          await tester.enterText(input, 'mini 설계');
          await tester.pump();
          expect(find.text('설계.md'), findsOneWidget);
          expect(find.text('최신.txt'), findsNothing);
          await tester.enterText(input, '없는파일');
          await tester.pump();
          expect(find.text('맞는 파일이 없어요. 검색어와 파일 종류를 바꿔 주세요'), findsOneWidget);
          await tester.tap(find.byTooltip('검색 지우기'));
          await tester.pump();
          FocusManager.instance.primaryFocus?.unfocus();
          await tester.tap(find.text('폴더'));
          await tester.pump();
          expect(find.text('검토자료'), findsOneWidget);
          expect(tester.takeException(), isNull);
          await tester.pumpWidget(const SizedBox());
        },
      );
    }
  }
}
