import 'package:flutter/services.dart';

import 'app_link.dart';
import 'server.dart';

/// 푸시 다리 — 네이티브(AppDelegate)가 애플에서 받은 기기 토큰을 카사텀 서버에
/// 맡기고, 알림을 누르면 그 학생 화면으로 보낸다. 서버가 바뀌면 새 서버에도 맡긴다.
class PushBridge {
  PushBridge._() {
    _ch.setMethodCallHandler(_fromNative);
  }

  static final PushBridge instance = PushBridge._();

  static const _ch = MethodChannel('kasaterm/push');

  Server? _server;
  String? _token;
  String _env = 'prod';
  Future<void> Function(AppLink)? _onTap;
  bool _requested = false;

  /// 연결된 서버가 정해질 때마다 부른다 — 토큰이 이미 있으면 바로 맡긴다.
  Future<void> bind(Server server, Future<void> Function(AppLink) onTap) async {
    _server = server;
    _onTap = onTap;
    if (!_requested) {
      _requested = true;
      try {
        await _ch.invokeMethod<void>('request');
        final pending = await _ch.invokeMapMethod<String, Object?>('pending');
        if (pending != null && pending.isNotEmpty) _tap(pending);
      } on MissingPluginException {
        // 웹·데스크톱 개발 실행 — 푸시 없음.
        return;
      } on PlatformException {
        return;
      }
    }
    await _register();
  }

  void unbind() {
    _server = null;
  }

  Future<void> _register() async {
    final s = _server;
    final t = _token;
    if (s == null || t == null) return;
    try {
      await s.registerPushToken(t, _env);
    } on ServerException {
      // 다음 연결·다음 토큰 때 다시.
    }
  }

  Future<Object?> _fromNative(MethodCall call) async {
    switch (call.method) {
      case 'onToken':
        final m = (call.arguments as Map?)?.cast<String, Object?>();
        _token = m?['token'] as String?;
        _env = (m?['env'] as String?) ?? 'prod';
        await _register();
      case 'onTap':
        final m = (call.arguments as Map?)?.cast<String, Object?>();
        if (m != null) _tap(m);
      case 'onTokenError':
        break;
    }
    return null;
  }

  void _tap(Map<String, Object?> m) {
    final pane = m['pane'] as String?;
    if (pane == null || pane.isEmpty) return;
    final machine = m['machine'] as String?;
    _onTap?.call(
      AppLink(machine: (machine == null || machine.isEmpty) ? null : machine, pane: pane),
    );
  }
}
