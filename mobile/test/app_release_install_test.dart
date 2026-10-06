import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/app_release.dart';

void main() {
  const channel = MethodChannel('kasaterm/release');
  final calls = <MethodCall>[];
  final release = AppRelease('1.0.0', '2610061200', Uri.parse('itms-services://?action=download-manifest'));
  TestWidgetsFlutterBinding.ensureInitialized();
  final messenger = TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
  setUp(() {
    calls.clear();
    messenger.setMockMethodCallHandler(channel, (call) async {
      calls.add(call);
      return null;
    });
  });
  tearDown(() => messenger.setMockMethodCallHandler(channel, null));

  Future<void> start(WidgetTester tester, {bool opened = true}) async {
    await tester.pumpWidget(MaterialApp(
      home: Scaffold(
        body: Builder(
          builder: (context) => TextButton(
            onPressed: () => installRelease(context, release, open: () async => opened, wait: const Duration(seconds: 30)),
            child: const Text('설치'),
          ),
        ),
      ),
    ));
    await tester.tap(find.text('설치'));
    await tester.pump();
  }

  void confirmDialog(WidgetTester tester) {
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
  }

  testWidgets('설치 확인 창이 닫혀 돌아오면 잠깐 알린 뒤 「열기」 알림을 걸고 비켜선다', (tester) async {
    await start(tester);
    expect(find.textContaining('앱을 닫아요'), findsNothing, reason: '확인 창이 뜨기 전에는 알리지 않는다');
    confirmDialog(tester);
    await tester.pump();
    expect(find.textContaining('앱을 닫아요'), findsOneWidget);
    await tester.pumpAndSettle();
    expect(calls, isEmpty);
    await tester.pump(releaseStepAsideDelay);
    await tester.pumpAndSettle();
    expect(calls.single.method, 'stepAside');
    expect(calls.single.arguments, {
      'title': 'KASATERM 새 판 1.0.0 (2610061200)',
      'body': '설치가 끝나면 눌러서 열어요',
      'after': releaseOpenNoticeAfter.inSeconds,
    });
  });

  testWidgets('알림 띠의 [취소]를 누르면 비켜서지 않는다', (tester) async {
    await start(tester);
    confirmDialog(tester);
    await tester.pumpAndSettle(const Duration(milliseconds: 100));
    await tester.tap(find.text('취소'));
    await tester.pumpAndSettle();
    await tester.pump(releaseStepAsideDelay);
    expect(calls, isEmpty);
  });

  testWidgets('설치 확인 창이 안 뜨면(열기 실패·돌아오지 않음) 아무것도 안 한다', (tester) async {
    await start(tester, opened: false);
    confirmDialog(tester);
    await tester.pumpAndSettle();
    expect(find.textContaining('앱을 닫아요'), findsNothing);

    await start(tester);
    await tester.pump(const Duration(seconds: 31));
    confirmDialog(tester);
    await tester.pumpAndSettle();
    expect(find.textContaining('앱을 닫아요'), findsNothing);
    expect(calls, isEmpty);
  });
}
