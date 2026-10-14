import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kasaterm_mobile/background_grace.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  const channel = MethodChannel('kasaterm/background');
  final messenger = TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
  final grace = BackgroundGrace.instance;
  late List<String> calls;
  var granted = true;
  var changes = 0;
  void count() => changes++;

  setUp(() {
    calls = [];
    granted = true;
    changes = 0;
    grace
      ..attach()
      ..resetForTest()
      ..addListener(count);
    messenger.setMockMethodCallHandler(channel, (call) async {
      calls.add(call.method);
      return call.method == 'begin' ? granted : null;
    });
  });

  tearDown(() {
    grace.removeListener(count);
    messenger.setMockMethodCallHandler(channel, null);
  });

  Future<void> expiredFromIos() => messenger.handlePlatformMessage(
    channel.name,
    const StandardMethodCodec().encodeMethodCall(const MethodCall('expired')),
    (_) {},
  );

  testWidgets('유예 중에는 유지하고 돌아오면 반쯤 열린 연결도 새로 잇게 알린다', (tester) async {
    grace.didChangeAppLifecycleState(AppLifecycleState.hidden);
    grace.didChangeAppLifecycleState(AppLifecycleState.paused);
    await tester.pump(const Duration(seconds: 10));
    expect(grace.live, isTrue);
    expect(calls, ['begin']);
    grace.didChangeAppLifecycleState(AppLifecycleState.resumed);
    await tester.pump();
    expect(grace.live, isTrue);
    expect(changes, 1);
    expect(calls, ['begin', 'end']);
    await tester.pump(BackgroundGrace.cap);
    expect(grace.live, isTrue);
    grace.didChangeAppLifecycleState(AppLifecycleState.resumed);
    expect(changes, 1, reason: '같은 복귀 통지를 두 번 받아도 다시 열지 않는다');
  });

  testWidgets('유예 안에 돌아와도 돌아온 순간을 알린다 — 묶어 둔 연결은 그 사이 끊겼을 수 있다', (tester) async {
    var returns = 0;
    grace.onReturn = () => returns++;
    addTearDown(() => grace.onReturn = null);
    grace.didChangeAppLifecycleState(AppLifecycleState.inactive);
    grace.didChangeAppLifecycleState(AppLifecycleState.resumed);
    expect(returns, 0);
    grace.didChangeAppLifecycleState(AppLifecycleState.hidden);
    await tester.pump(const Duration(seconds: 3));
    grace.didChangeAppLifecycleState(AppLifecycleState.resumed);
    expect(returns, 1);
    expect(grace.live, isTrue);
    await tester.pump();
  });

  testWidgets('받은 시간이 다 되면 닫고, 돌아오면 다시 연다', (tester) async {
    grace.didChangeAppLifecycleState(AppLifecycleState.hidden);
    await tester.pump(BackgroundGrace.cap);
    expect(grace.live, isFalse);
    expect(changes, 1);
    expect(calls, ['begin', 'end']);
    grace.didChangeAppLifecycleState(AppLifecycleState.resumed);
    await tester.pump();
    expect(grace.live, isTrue);
    expect(changes, 2);
  });

  testWidgets('iOS 가 먼저 거두면 그때 닫고, 시간을 못 받으면 곧바로 닫는다', (tester) async {
    grace.didChangeAppLifecycleState(AppLifecycleState.hidden);
    await tester.pump(const Duration(seconds: 3));
    await expiredFromIos();
    expect(grace.live, isFalse);
    grace.didChangeAppLifecycleState(AppLifecycleState.resumed);
    await tester.pump();

    granted = false;
    grace.didChangeAppLifecycleState(AppLifecycleState.hidden);
    await tester.pump();
    expect(grace.live, isFalse);
  });

  testWidgets('돌아온 뒤 늦게 온 만료는 앞의 화면을 닫지 않는다', (tester) async {
    grace.didChangeAppLifecycleState(AppLifecycleState.hidden);
    await tester.pump();
    grace.didChangeAppLifecycleState(AppLifecycleState.resumed);
    await tester.pump();
    await expiredFromIos();
    expect(grace.live, isTrue);
    expect(changes, 1);
  });

  testWidgets('짧거나 긴 외출 모두 HTTP 정리를 화면 재연결보다 먼저 한다', (tester) async {
    final order = <String>[];
    void changed() { if (grace.live) order.add('screen'); }
    grace.onReturn = () => order.add('http');
    grace.addListener(changed);
    addTearDown(() {
      grace.onReturn = null;
      grace.removeListener(changed);
    });
    for (final away in [const Duration(seconds: 2), BackgroundGrace.cap]) {
      grace.didChangeAppLifecycleState(AppLifecycleState.hidden);
      await tester.pump(away);
      grace.didChangeAppLifecycleState(AppLifecycleState.resumed);
      await tester.pump();
    }
    expect(order, ['http', 'screen', 'http', 'screen']);
  });
}
