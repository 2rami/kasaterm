import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/app_release.dart';

void main() {
  const channel = MethodChannel('kasaterm/release');
  final calls = <MethodCall>[];
  final release = AppRelease(
    '1.0.0',
    '2610061200',
    Uri.parse(
      'itms-services://?action=download-manifest&url=https%3A%2F%2Fk.example%2Frelay%2Finstall%2Ftok%2Fmanifest.plist',
    ),
  );
  TestWidgetsFlutterBinding.ensureInitialized();
  final messenger =
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
  setUp(() {
    calls.clear();
    messenger.setMockMethodCallHandler(channel, (call) async {
      calls.add(call);
      return null;
    });
  });
  tearDown(() => messenger.setMockMethodCallHandler(channel, null));

  List<MethodCall> steps() =>
      calls.where((c) => c.method == 'stepAside').toList();
  List<String> logs() => [
    for (final c in calls)
      if (c.method == 'log') (c.arguments as Map)['line'] as String,
  ];

  Future<void> start(WidgetTester tester, {bool opened = true}) async {
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: Builder(
            builder: (context) => TextButton(
              onPressed: () => installRelease(
                context,
                release,
                open: () async => opened,
                wait: const Duration(seconds: 30),
              ),
              child: const Text('설치'),
            ),
          ),
        ),
      ),
    );
    await tester.tap(find.text('설치'));
    await tester.pump();
  }

  void confirmDialog(WidgetTester tester) {
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
  }

  testWidgets('설치 확인 창이 닫혀 돌아오면 잠깐 알린 뒤 「열기」 알림을 걸고 비켜선다', (tester) async {
    await start(tester);
    expect(
      find.textContaining('앱을 닫아요'),
      findsNothing,
      reason: '확인 창이 뜨기 전에는 알리지 않는다',
    );
    confirmDialog(tester);
    await tester.pump();
    await tester.pump();
    expect(find.textContaining('앱을 닫아요'), findsOneWidget);
    await tester.pumpAndSettle();
    expect(steps(), isEmpty);
    await tester.pump(releaseStepAsideDelay);
    await tester.pumpAndSettle();
    expect(steps().single.arguments, {
      'title': 'KASATERM 새 판 1.0.0 (2610061200)',
      'body': '설치가 끝나면 눌러서 열어요',
      'after': releaseOpenNoticeAfter.inSeconds,
      'page': 'https://k.example/relay/install/tok/',
    });
    expect(calls.first.method, 'arm');
    expect(
      logs(),
      containsAllInOrder([
        contains('[설치] 누름'),
        '설치 창 열기 → true',
        '앱 상태 inactive',
        '앱 상태 resumed',
        '기다림 끝 · back',
        '띠 닫힘 · timeout',
      ]),
    );
  });

  testWidgets('알림 띠의 [취소]를 누르면 비켜서지 않는다', (tester) async {
    await start(tester);
    confirmDialog(tester);
    await tester.pumpAndSettle(const Duration(milliseconds: 100));
    await tester.tap(find.text('취소'));
    await tester.pumpAndSettle();
    await tester.pump(releaseStepAsideDelay);
    expect(steps(), isEmpty);
    expect(logs(), contains('띠 닫힘 · action'));
  });

  testWidgets('확인 창이 앱을 비활성으로 만들지 않으면 [앱 닫기] 띠로 비켜선다', (tester) async {
    await start(tester);
    await tester.pump(releaseLeaveGrace);
    await tester.pump();
    await tester.pumpAndSettle();
    expect(find.text('앱 닫기'), findsOneWidget);
    expect(steps(), isEmpty);
    await tester.tap(find.text('앱 닫기'));
    await tester.pumpAndSettle();
    expect(steps(), hasLength(1));
    expect(logs(), contains('기다림 끝 · pressed'));
  });

  testWidgets('열기가 실패로 답해도 확인 창이 떴다 돌아오면 비켜선다', (tester) async {
    await start(tester, opened: false);
    confirmDialog(tester);
    await tester.pump();
    await tester.pump();
    expect(find.textContaining('앱을 닫아요'), findsOneWidget);
    await tester.pumpAndSettle();
    await tester.pump(releaseStepAsideDelay);
    await tester.pumpAndSettle();
    expect(steps(), hasLength(1));
    expect(logs(), contains('설치 창 열기 → false'));
  });

  testWidgets('[앱 닫기] 띠가 떠 있어도 확인 창이 늦게 떴다 돌아오면 3초 띠로 바꾼다', (tester) async {
    await start(tester);
    await tester.pump(releaseLeaveGrace);
    await tester.pumpAndSettle();
    expect(find.text('앱 닫기'), findsOneWidget);
    confirmDialog(tester);
    await tester.pump();
    await tester.pumpAndSettle();
    expect(find.text('앱 닫기'), findsNothing);
    expect(find.textContaining('앱을 닫아요'), findsOneWidget);
    await tester.pump(releaseStepAsideDelay);
    await tester.pumpAndSettle();
    expect(steps(), hasLength(1));
  });

  testWidgets('사람이 먼저 홈으로 가거나 시간이 다 되면 아무것도 안 한다', (tester) async {
    await start(tester);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.hidden);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.paused);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.hidden);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pumpAndSettle();
    expect(find.textContaining('앱을 닫아요'), findsNothing);
    expect(logs(), contains('기다림 끝 · gone'));

    calls.clear();
    await start(tester);
    await tester.pump(const Duration(seconds: 31));
    await tester.pumpAndSettle();
    confirmDialog(tester);
    await tester.pumpAndSettle();
    expect(find.textContaining('앱을 닫아요'), findsNothing);
    expect(steps(), isEmpty);
    expect(logs(), contains('기다림 끝 · timeout'));
  });

  test('사파리 설치 화면은 manifest 의 디렉터리', () {
    expect(release.page.toString(), 'https://k.example/relay/install/tok/');
    expect(
      AppRelease(
        '1',
        '2',
        Uri.parse('itms-services://?action=download-manifest'),
      ).page,
      isNull,
    );
  });
}
