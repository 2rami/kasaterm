import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/claude_style.dart';
import 'package:kasaterm_mobile/grid_canvas.dart';
import 'package:kasaterm_mobile/main.dart';
import 'package:kasaterm_mobile/screens/connect.dart';
import 'package:kasaterm_mobile/screens/conversation_view.dart';
import 'package:kasaterm_mobile/screens/terminal.dart';
import 'package:kasaterm_mobile/server.dart';
import 'package:kasaterm_mobile/sprite_cache.dart';
import 'package:kasaterm_mobile/term_session.dart';
import 'package:kasaterm_mobile/theme_prefs.dart';

class FixtureServer extends Server {
  FixtureServer() : super(Uri.parse('https://fixture.invalid/'));
  @override
  Future<List<Pane>> panes({String? machine}) async => [];
}

Pane fixturePane(String harness) => Pane(
  id: '%7',
  name: '개발 도우미',
  title: '',
  status: 'idle',
  window: 0,
  cwd: '/workspace/kasa',
  harness: harness,
  session: 'mobile-qa',
);

class FixtureSession extends TermSession {
  FixtureSession(super.server, super.pane) {
    state = TermState.connected;
    final rows = [
      '${pane.harness == 'codex' ? 'OpenAI Codex' : 'Claude Code'} · sample fixture',
      '한글과 English가 섞인 문장도 폰 화면에서 자연스럽게 이어집니다.',
      '수정한 파일: connection.dart · 로그인과 기기 대기를 구분했습니다.',
      '검사 결과: 인증 경계 / 계정 전환 / 테마 복원 모두 통과',
      '',
      for (var i = 1; i <= 16; i++) '검증 $i  가나 가나다 한국어와 줄바꿈 · narrow viewport',
      '────────────────────────────────────────────────────',
      '❯ 다음 작업을 입력해 주세요',
      '────────────────────────────────────────────────────',
      '${String.fromCharCode(pane.harness == 'codex' ? statusModelGpt : statusModelClaude)}  /workspace/kasa',
    ];
    grid.apply({
      'cols': 96,
      'rows': rows.length,
      'dirty': [
        for (final (i, row) in rows.indexed)
          [
            i,
            [
              [row, null, null, 0],
            ],
          ],
      ],
      'cursor': [rows.length - 3, 2],
      'cursorVisible': false,
    });
  }
  final sent = <String>[];
  @override
  void connect() {}
  @override
  bool get canSend => !server.isClosed;
  @override
  void sendText(String text) {
    if (!server.isClosed) sent.add(text);
  }

  @override
  void sendBytes(List<int> bytes) {
    sent.add(String.fromCharCodes(bytes));
  }
}

Future<void> loadFonts() async {
  for (final (family, files) in [
    ('MaterialIcons', ['fonts/MaterialIcons-Regular.otf']),
    ('Pretendard', ['Pretendard-Regular.otf', 'Pretendard-SemiBold.otf']),
    (
      'TermMono',
      [
        'JetBrainsMonoNerdFontMono-Regular.ttf',
        'JetBrainsMonoNerdFontMono-Bold.ttf',
      ],
    ),
    (
      'TermHangul',
      [
        'D2CodingLigatureNerdFontMono-Regular.ttf',
        'D2CodingLigatureNerdFontMono-Bold.ttf',
      ],
    ),
    ('TermSymbol', ['STIXTwoMath.otf']),
  ]) {
    final loader = FontLoader(family);
    for (final file in files) {
      loader.addFont(
        rootBundle.load(
          file.startsWith('fonts/') ? file : 'assets/fonts/$file',
        ),
      );
    }
    await loader.load();
  }
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(loadFonts);

  for (final width in [320.0, 390.0, 430.0]) {
    for (final harness in ['codex', 'claude']) {
      testWidgets(
        '$harness $width: Korean, wrap, scroll, select, IME, targets',
        (tester) async {
          tester.view.physicalSize = Size(width, 844);
          tester.view.devicePixelRatio = 1;
          addTearDown(tester.view.resetPhysicalSize);
          addTearDown(tester.view.resetDevicePixelRatio);
          addTearDown(tester.view.resetViewInsets);
          paneView.value = PaneView.terminal;
          phoneThemeMode.value = harness == 'codex'
              ? ThemeMode.light
              : ThemeMode.dark;
          final brightness = harness == 'codex'
              ? Brightness.light
              : Brightness.dark;
          final server = FixtureServer();
          final pane = fixturePane(harness);
          final session = FixtureSession(server, pane);
          await tester.pumpWidget(
            MaterialApp(
              debugShowCheckedModeBanner: false,
              theme: buildTheme(brightness),
              home: TerminalScreen(
                server: server,
                pane: pane,
                session: session,
              ),
            ),
          );
          await tester.pumpAndSettle();
          expect(tester.takeException(), isNull);
          expect(find.byType(WrappedCanvas), findsOneWidget);
          final canvas = tester.getRect(find.byType(WrappedCanvas));
          expect(canvas.left, greaterThanOrEqualTo(0));
          expect(canvas.right, lessThanOrEqualTo(width));
          final esc = tester.getRect(find.text('esc'));
          final target = tester.getRect(
            find
                .ancestor(of: find.text('esc'), matching: find.byType(InkWell))
                .first,
          );
          expect(target.height, greaterThanOrEqualTo(44));
          expect(target.width, greaterThanOrEqualTo(44));
          expect(target.contains(esc.center), isTrue);
          await tester.runAsync(() async {
            spriteCache.icon(harness);
            for (
              var attempt = 0;
              attempt < 30 && spriteCache.icon(harness) == null;
              attempt++
            ) {
              await Future<void>.delayed(const Duration(milliseconds: 10));
            }
          });
          await tester.pumpAndSettle();
          expect(spriteCache.icon(harness), isNotNull);
          if (width == 390) {
            await expectLater(
              find.byType(MaterialApp),
              matchesGoldenFile('goldens/account_${harness}_390.png'),
            );
          }
          final scroll = find
              .descendant(
                of: find.byType(WrappedCanvas),
                matching: find.byType(SingleChildScrollView),
              )
              .first;
          await tester.drag(scroll, const Offset(0, 160));
          await tester.pump();
          expect(tester.takeException(), isNull);
          await tester.tap(find.byTooltip('글자 선택·복사'));
          await tester.pumpAndSettle();
          final selected = tester.widget<SelectableText>(
            find.byType(SelectableText),
          );
          expect(selected.data, contains('한글과 English'));
          expect(selected.data, contains('검증 16'));
          Navigator.of(tester.element(find.byType(SelectableText))).pop();
          await tester.pumpAndSettle();
          final field = find.byType(TextField).last;
          await tester.tap(field);
          tester.testTextInput.updateEditingValue(
            const TextEditingValue(
              text: '한',
              selection: TextSelection.collapsed(offset: 1),
              composing: TextRange(start: 0, end: 1),
            ),
          );
          await tester.pump();
          expect(session.sent, isEmpty);
          tester.testTextInput.updateEditingValue(
            const TextEditingValue(
              text: '한글',
              selection: TextSelection.collapsed(offset: 2),
            ),
          );
          await tester.pump();
          expect(session.sent, isNotEmpty);
          tester.view.viewInsets = const FakeViewPadding(bottom: 336);
          await tester.pumpAndSettle();
          expect(tester.takeException(), isNull);
          expect(tester.getRect(field).bottom, lessThanOrEqualTo(844 - 336));
          await tester.pumpWidget(const SizedBox());
          server.close();
        },
      );
    }
  }

  for (final brightness in Brightness.values) {
    testWidgets('login $brightness golden and measured position', (
      tester,
    ) async {
      tester.view.physicalSize = const Size(390, 844);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      await tester.pumpWidget(
        MaterialApp(
          debugShowCheckedModeBanner: false,
          theme: buildTheme(brightness),
          home: ConnectScreen(
            onConnected: (_) async {},
            onLogin: (_, _, _) async {},
          ),
        ),
      );
      await tester.pumpAndSettle();
      // 위에 쌍둥이 그림(96)이 선다 — 그래도 자판(844 화면에서 약 508 위)보다 로그인 단추까지 위다.
      expect(
        tester.getRect(find.byKey(const Key('account-input'))).top,
        closeTo(263.5, 0.5),
      );
      await expectLater(
        find.byType(MaterialApp),
        matchesGoldenFile('goldens/account_login_${brightness.name}.png'),
      );
    });
  }
}
