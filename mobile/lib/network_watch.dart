import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

/// 앱을 켠 채 망이 바뀐 순간(와이파이↔셀룰러, 다른 와이파이) — iOS `NWPathMonitor`(`AppDelegate.swift`)가 알린다.
///
/// 길은 앱으로 돌아올 때([BackgroundGrace.onReturn])만 다시 찾았다. 켜 둔 채 밖으로 나가면 옛 망의 직통 입구가
/// 「직통」이라 답하며 요청과 소켓을 삼켰다(10-31 15:30, 사진 보내기·화면 보기). 듣는 쪽은 묶어 둔 연결을 버리고
/// 소켓을 관문으로 곧바로 다시 붙인다.
class NetworkWatch extends ChangeNotifier {
  NetworkWatch._();

  static final NetworkWatch instance = NetworkWatch._();
  static const _channel = MethodChannel('kasaterm/network');
  bool _attached = false;

  void attach() {
    if (_attached) return;
    _attached = true;
    _channel.setMethodCallHandler((call) async {
      if (call.method == 'changed') changed();
    });
  }

  /// 뒤에 있는 동안 바뀐 것은 돌아올 때 [BackgroundGrace.onReturn] 이 다룬다.
  @visibleForTesting
  void changed() {
    if (WidgetsBinding.instance.lifecycleState != AppLifecycleState.resumed) return;
    notifyListeners();
  }
}
