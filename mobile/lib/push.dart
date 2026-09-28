import 'dart:async';

import 'package:flutter/services.dart';
import 'package:flutter/foundation.dart' show visibleForTesting;

import 'app_link.dart';
import 'server.dart';

/// 푸시 다리 — 네이티브(AppDelegate)가 애플에서 받은 기기 토큰을 카사텀 서버에
/// 맡기고, 알림을 누르면 그 학생 화면으로 보낸다. 서버가 바뀌면 새 서버에도 맡긴다.
class PushBridge {
  PushBridge._({Server Function(Server)? cleanupFactory})
    : _cleanupFactory =
          cleanupFactory ??
          ((server) => server.account == null
              ? Server(server.root)
              : Server.account(server.account!)) {
    _ch.setMethodCallHandler(_fromNative);
  }

  @visibleForTesting
  PushBridge.forTesting({Server Function(Server)? cleanupFactory})
    : this._(cleanupFactory: cleanupFactory ?? ((server) => server));

  static final PushBridge instance = PushBridge._();

  static const _ch = MethodChannel('kasaterm/push');
  final Server Function(Server) _cleanupFactory;
  bool cleanupPending = false;

  Server? _server;
  String? _token;
  String _env = 'prod';
  Future<void> Function(AppLink)? _onTap;
  bool _requested = false;
  Server? _registeredServer;
  String? _registeredToken;
  Future<void> _registration = Future.value();

  /// 연결된 서버가 정해질 때마다 부른다 — 토큰이 이미 있으면 바로 맡긴다.
  Future<void> bind(Server server, Future<void> Function(AppLink) onTap) async {
    if (server.account != null) {
      // Desktop APNs subscriptions do not yet have an account-device lease.
      await unbind();
      return;
    }
    _server = server;
    _onTap = onTap;
    _registration = _registration.then((_) async {
      if (_registeredServer != null && _registeredServer != server) {
        await _removePrevious();
      }
    });
    await _registration;
    if (_server != server || server.isClosed || cleanupPending) return;
    if (!_requested) {
      _requested = true;
      try {
        await _ch.invokeMethod<void>('request');
        // 네이티브 등록이 Dart 연결보다 먼저 끝나거나 엔진이 다시 붙은 경우.
        final saved = await _ch.invokeMapMethod<String, Object?>('token');
        if (saved != null) {
          _token = saved['token'] as String?;
          _env = (saved['env'] as String?) ?? 'prod';
        }
        final pending = await _ch.invokeMapMethod<String, Object?>('pending');
        if (pending != null && pending.isNotEmpty) _tap(pending);
      } on MissingPluginException {
        // 웹·데스크톱 개발 실행 — 푸시 없음.
        return;
      } on PlatformException {
        _requested = false;
        return;
      }
    }
    await _register();
  }

  Future<bool> unbind() {
    final oldServer = _server;
    final oldToken = _token;
    _server = null;
    _onTap = null;
    _requested = false;
    unawaited(_suspendNative());
    _registration = _registration.then((_) async {
      // 앞선 등록 요청이 아직 진행 중일 수 있어 큐 안에서 정본을 읽는다.
      final registeredServer = _registeredServer ?? oldServer;
      final registeredToken = _registeredToken ?? oldToken;
      if (registeredServer == null || registeredToken == null) return;
      // 실패하면 다음 bind가 먼저 재시도할 수 있도록 이전 발신자를 기억한다.
      _registeredServer = registeredServer;
      _registeredToken = registeredToken;
      await _removePrevious();
    });
    return _registration.then((_) => !cleanupPending);
  }

  Future<void> _suspendNative() async {
    try {
      await _ch.invokeMethod<void>('suspend');
    } on MissingPluginException {
      // Non-iOS test/development runs have no native registration.
    } on PlatformException {
      cleanupPending = true;
    }
  }

  Future<void> _removePrevious() async {
    final previous = _registeredServer;
    final token = _registeredToken;
    if (previous == null || token == null) return;
    final cleanup = _cleanupFactory(previous);
    try {
      await cleanup
          .unregisterPushToken(token)
          .timeout(const Duration(seconds: 3));
      _registeredServer = null;
      _registeredToken = null;
      cleanupPending = false;
    } catch (_) {
      cleanupPending = true;
    } finally {
      if (!identical(cleanup, previous)) cleanup.close();
    }
  }

  Future<void> retryCleanup() {
    _registration = _registration.then((_) async {
      if (cleanupPending) await _removePrevious();
    });
    return _registration;
  }

  Future<void> _register() {
    _registration = _registration.then((_) => _registerCurrent());
    return _registration;
  }

  Future<void> _registerCurrent() async {
    final s = _server;
    final t = _token;
    if (s == null || s.isClosed || s.account != null || t == null) return;
    try {
      final previousServer = _registeredServer;
      final previousToken = _registeredToken;
      // 한 서버만 폰에 발신한다. 옛 서버가 살아 있는데 등록 해제가 실패하면
      // 다음 재연결까지 이전 등록을 유지해 두 발신자를 동시에 만들지 않는다.
      if (previousServer != null &&
          previousToken != null &&
          (previousServer != s || previousToken != t)) {
        await _removePrevious();
        if (cleanupPending || _server != s || s.isClosed) return;
      }
      await s.registerPushToken(t, _env);
      // ready=false여도 서버에는 토큰이 저장된다. 키가 나중에 연결되면
      // 발신할 수 있으므로 준비 상태와 무관하게 등록 소유자를 추적한다.
      _registeredServer = s;
      _registeredToken = t;
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
    final url = m['url'] as String?;
    if (url != null && url.isNotEmpty) {
      _onTap?.call(AppLink(url: url));
      return;
    }
    final pane = m['pane'] as String?;
    if (pane == null || pane.isEmpty) return;
    final machine = m['machine'] as String?;
    _onTap?.call(
      AppLink(
        machine: (machine == null || machine.isEmpty) ? null : machine,
        pane: pane,
      ),
    );
  }
}
