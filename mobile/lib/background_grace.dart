import 'dart:async';

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

/// 다른 앱에 다녀오는 동안 화면을 살려 두는 시간.
///
/// 예전에는 앱이 뒤로 가는 순간 허브 폴링·롱폴·터미널 소켓을 전부 닫았다. 그래서 메시지 하나 보고 돌아와도
/// 관문을 다시 건너 붙는 동안(소켓 0.3초 + 첫 화면 0.3초, 폰 망에서는 1초 남짓) 낡은 화면을 봤다.
/// 이제는 iOS 가 뒤로 간 앱에 주는 시간(`beginBackgroundTask`, 보통 30초) 동안 그대로 두고, 그 안에
/// 돌아오면 다시 붙을 일이 없다. 시간이 다 되면 예전처럼 닫고, 돌아오면 다시 붙는다.
///
/// 화면들은 [live] 하나만 본다 — 참이면 붙어 있고, 거짓이 되면 닫고, 다시 참이 되면 다시 붙는다.
class BackgroundGrace extends ChangeNotifier with WidgetsBindingObserver {
  BackgroundGrace._();

  static final BackgroundGrace instance = BackgroundGrace._();
  static const _channel = MethodChannel('kasaterm/background');

  /// iOS 가 시간을 더 줘도 이만큼에서 스스로 닫는다 — 끝 무렵에 강제로 멈추면 닫는 인사도 못 한다.
  static const cap = Duration(seconds: 25);

  bool _live = true;
  bool get live => _live;

  /// 뒤에 갔다 돌아온 순간 — 화면들이 다시 붙기 전에 먼저 부른다. 유예 안에 돌아와도 부른다: 묶어 둔 HTTP 연결은
  /// 그 사이 끊겼을 수 있다.
  VoidCallback? onReturn;
  bool _away = false;
  bool _attached = false;
  Timer? _cap;

  void attach() {
    if (_attached) return;
    _attached = true;
    WidgetsBinding.instance.addObserver(this);
    _channel.setMethodCallHandler((call) async {
      // 돌아온 뒤에 늦게 온 만료는 앞에 있는 화면을 닫으면 안 된다.
      if (call.method == 'expired' && _away) _expire();
    });
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    switch (state) {
      case AppLifecycleState.resumed:
        if (_away) onReturn?.call();
        _away = false;
        _cap?.cancel();
        _cap = null;
        unawaited(_call('end'));
        if (!_live) {
          _live = true;
          notifyListeners();
        }
      case AppLifecycleState.hidden:
      case AppLifecycleState.paused:
        if (_away) return;
        _away = true;
        unawaited(_leave());
      case AppLifecycleState.detached:
        _expire();
      case AppLifecycleState.inactive:
        break;
    }
  }

  Future<void> _leave() async {
    final granted = await _call('begin') == true;
    if (!_away) {
      // 받기 전에 이미 돌아왔다 — 돌아올 때 보낸 `end` 가 앞질렀을 수 있어 한 번 더 놓는다.
      if (granted) unawaited(_call('end'));
      return;
    }
    if (!granted) {
      _expire();
      return;
    }
    _cap ??= Timer(cap, _expire);
  }

  void _expire() {
    _cap?.cancel();
    _cap = null;
    if (_live) {
      _live = false;
      notifyListeners();
    }
    unawaited(_call('end'));
  }

  /// 네이티브 쪽이 없으면(시험·iOS 밖) 시간을 못 받은 것으로 친다 — 예전처럼 곧바로 닫는다.
  Future<Object?> _call(String method) async {
    try {
      return await _channel.invokeMethod<Object?>(method);
    } on MissingPluginException {
      return null;
    } on PlatformException {
      return null;
    }
  }

  @visibleForTesting
  void resetForTest() {
    _cap?.cancel();
    _cap = null;
    _live = true;
    _away = false;
  }
}
