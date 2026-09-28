import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';

import 'relay_account.dart';

/// 폰 자체 밝기. 「데스크톱 따라감」이 기본 — 붙은 기계의 색을 그대로 입는다.
/// 밝게·어둡게를 고르면 데스크톱 색을 접고 폰의 기본 얼굴을 그 밝기로 입는다
/// (데스크톱 팔레트는 한 벌뿐이라 밝기를 뒤집을 수가 없다).
final phoneThemeMode = ValueNotifier<ThemeMode>(ThemeMode.system);
final phoneThemeSync = PhoneThemeSync();

class ThemePrefs {
  const ThemePrefs({this.scope = ''});

  final String scope;
  String get storageKey => scope.isEmpty ? _key : '$_key/$scope';

  static const _key = 'theme.mode';
  static const _storage = FlutterSecureStorage();

  Future<ThemeMode> load() async {
    try {
      final v = await _storage.read(key: storageKey);
      return ThemeMode.values.firstWhere(
        (m) => m.name == v,
        orElse: () => ThemeMode.system,
      );
    } catch (_) {
      return ThemeMode.system;
    }
  }

  Future<void> save(ThemeMode m) async {
    try {
      await _storage.write(key: storageKey, value: m.name);
    } catch (_) {
      // 저장소가 막혀도 이번 실행은 고른 대로 보인다.
    }
  }
}

class PhoneThemeSync {
  PhoneThemeSync({RelayAccountApi Function(AccountSession)? apiFactory})
    : _factory =
          apiFactory ??
          ((session) => RelayAccountApi(session.origin, session: session));
  final RelayAccountApi Function(AccountSession) _factory;
  RelayAccountApi? _api;
  AccountSession? _account;
  ThemePrefs _prefs = const ThemePrefs();
  int _generation = 0;
  int _edit = 0;
  int _refreshSequence = 0;
  final Map<int, int> _pendingWrites = {};
  AccountSyncSnapshot? _snapshot;
  Future<void> _writes = Future.value();
  void Function()? onUnauthorized;

  Future<void> bind(AccountSession? account) async {
    final generation = ++_generation;
    _api?.close();
    _account = account;
    _api = account == null ? null : _factory(account);
    _snapshot = null;
    _prefs = ThemePrefs(
      scope: account == null ? '' : '${account.origin}|${account.account}',
    );
    phoneThemeMode.value = ThemeMode.system;
    final edit = _edit;
    final local = await _prefs.load();
    if (generation != _generation || edit != _edit) return;
    phoneThemeMode.value = local;
    await refresh();
  }

  Future<void> refresh() async {
    final api = _api;
    if (api == null || (_pendingWrites[_generation] ?? 0) > 0) return;
    final generation = _generation;
    final edit = _edit;
    final request = ++_refreshSequence;
    try {
      final snapshot = await api.readSync();
      if (generation != _generation ||
          edit != _edit ||
          request != _refreshSequence ||
          (_snapshot != null && snapshot.revision < _snapshot!.revision)) {
        return;
      }
      _snapshot = snapshot;
      final mode = ThemeMode.values
          .where((m) => m.name == snapshot.settings['mobile_theme_mode'])
          .firstOrNull;
      if (mode == null) return;
      phoneThemeMode.value = mode;
      await _prefs.save(mode);
    } on AccountException catch (e) {
      if (generation == _generation && e.status == 401) onUnauthorized?.call();
    }
  }

  Future<String?> setMode(ThemeMode mode) {
    final generation = _generation;
    final api = _api;
    final prefs = _prefs;
    _edit++;
    _pendingWrites[generation] = (_pendingWrites[generation] ?? 0) + 1;
    phoneThemeMode.value = mode;
    final completer = Completer<String?>();
    _writes = _writes
        .catchError((Object _) {})
        .then((_) async {
          if (generation != _generation) {
            completer.complete();
            return;
          }
          await prefs.save(mode);
          if (api == null) {
            completer.complete();
            return;
          }
          try {
            var snapshot = _snapshot ?? await api.readSync();
            for (var attempt = 0; attempt < 3; attempt++) {
              if (generation != _generation) {
                completer.complete();
                return;
              }
              try {
                snapshot = await api.setMobileTheme(
                  snapshot.revision,
                  mode.name,
                );
                if (generation == _generation) _snapshot = snapshot;
                completer.complete();
                return;
              } on AccountSyncConflict catch (e) {
                snapshot = e.current;
              }
            }
            throw const AccountException('다른 기기에서 설정이 바뀌었어요. 다시 선택해 주세요.');
          } on AccountException catch (e) {
            if (generation == _generation && e.status == 401) {
              onUnauthorized?.call();
            }
            completer.complete(
              generation == _generation
                  ? '이 폰에는 적용했지만 계정에 저장하지 못했어요. ${e.message}'
                  : null,
            );
          }
        })
        .whenComplete(() {
          final remaining = (_pendingWrites[generation] ?? 1) - 1;
          if (remaining == 0) {
            _pendingWrites.remove(generation);
          } else {
            _pendingWrites[generation] = remaining;
          }
        });
    return completer.future;
  }

  void unbind() {
    _generation++;
    _api?.close();
    _api = null;
    _account = null;
    _snapshot = null;
    onUnauthorized = null;
  }

  AccountSession? get account => _account;
}
