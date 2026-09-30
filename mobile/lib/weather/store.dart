import 'dart:async';
import 'dart:convert';

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';

import '../relay_account.dart';
import 'model.dart';

/// 날씨 한 벌 — 설정(계정으로 기기끼리, 「날씨는 기기마다」면 이 폰만), 카드별 덮어쓰기(이 폰만),
/// 지금 초점 카드, OS 접근성. 화면은 이 값들을 듣고 그린다.
final weather = WeatherStore();

class WeatherStore with WidgetsBindingObserver {
  WeatherStore({RelayAccountApi Function(AccountSession)? apiFactory})
    : _factory = apiFactory ?? ((session) => RelayAccountApi(session.origin, session: session));

  final RelayAccountApi Function(AccountSession) _factory;

  final settings = ValueNotifier<WeatherSettings>(const WeatherSettings());

  /// 켜면 설정을 계정에 올리지도 받지도 않는다 — 배터리 따라 폰만 다르게.
  final perDevice = ValueNotifier<bool>(false);
  final cards = ValueNotifier<Map<String, CardWeather>>(const {});

  /// 방금 만지거나 스크롤한 카드.
  final focused = ValueNotifier<String?>(null);
  final reduceTransparency = ValueNotifier<bool>(false);

  /// iOS 「동작 줄이기」는 MediaQuery.disableAnimations 가 아니라 이 표시로 온다(엔진이 reduceMotion 만 켠다).
  final reduceMotion = ValueNotifier<bool>(false);

  /// 계정에 못 올린 까닭 — 설정 시트가 한 줄로 보인다.
  final syncNote = ValueNotifier<String?>(null);

  late final Listenable changes = Listenable.merge([settings, cards, focused, reduceTransparency, reduceMotion]);

  static const _storage = FlutterSecureStorage();
  static const _keys = ['weather'];
  static const _channel = MethodChannel('kasaterm/a11y');

  /// 검증용: 빌드 때 `KASA_WEATHER`(설정 JSON)를 주면 그 값으로 켜고 계정과 주고받지 않는다.
  static const _forced = String.fromEnvironment('KASA_WEATHER');

  String _scope = '';
  RelayAccountApi? _api;
  int _generation = 0;
  AccountSyncSnapshot? _snapshot;
  Future<void> _writes = Future.value();

  String _key(String name) => _scope.isEmpty ? 'weather.$name' : 'weather.$name/$_scope';

  CardWeather cardOf(String id) => cards.value[id] ?? CardWeather.follow;

  void focus(String id) {
    if (focused.value != id) focused.value = id;
  }

  Future<void> bind(AccountSession? account) async {
    final generation = ++_generation;
    _api?.close();
    _api = account == null || _forced.isNotEmpty ? null : _factory(account);
    _snapshot = null;
    _scope = account == null ? '' : '${account.origin}|${account.account}';
    unawaited(_watchOs());
    if (_forced.isNotEmpty) {
      settings.value = WeatherSettings.fromJson(jsonDecode(_forced));
      return;
    }
    final local = await _read('settings');
    final device = await _read('per_device');
    final overrides = await _read('cards');
    if (generation != _generation) return;
    settings.value = WeatherSettings.fromJson(local);
    perDevice.value = device == true;
    cards.value = {
      if (overrides is Map)
        for (final e in overrides.entries)
          if (CardWeather.fromJson(e.value) is! FollowWeather) '${e.key}': CardWeather.fromJson(e.value),
    };
    await refresh();
  }

  void unbind() {
    _generation++;
    _api?.close();
    _api = null;
    _snapshot = null;
  }

  /// 계정의 값을 받아 온다. 옛 관문은 `weather` 키를 모른다 — 그러면 이 폰 값 그대로.
  Future<void> refresh() async {
    final api = _api;
    if (api == null || perDevice.value) return;
    final generation = _generation;
    try {
      final snap = await api.readSyncWith(_keys);
      if (generation != _generation) return;
      _snapshot = snap;
      final raw = snap.settings['weather'];
      if (raw is Map) {
        settings.value = WeatherSettings.fromJson(raw);
        await _write('settings', settings.value.toJson());
      }
      syncNote.value = null;
    } on AccountException catch (e) {
      if (generation == _generation) syncNote.value = e.message;
    }
  }

  /// [commit] 가 거짓이면 화면만 바꾼다 — 끌고 있는 막대가 한 칸마다 계정에 쓰지 않게.
  Future<void> set(WeatherSettings next, {bool commit = true}) {
    settings.value = next;
    if (!commit) return _writes;
    final generation = _generation;
    final api = _api;
    _writes = _writes.catchError((Object _) {}).then((_) async {
      await _write('settings', next.toJson());
      if (api == null || perDevice.value || generation != _generation) return;
      try {
        var snap = _snapshot ?? await api.readSyncWith(_keys);
        for (var attempt = 0; attempt < 3; attempt++) {
          try {
            snap = await api.patchSync(snap.revision, {'weather': next.toJson()}, keys: _keys);
            if (generation == _generation) _snapshot = snap;
            syncNote.value = null;
            return;
          } on AccountSyncConflict catch (e) {
            snap = e.current;
          }
        }
        syncNote.value = '다른 기기에서 설정이 바뀌었어요. 다시 골라 주세요.';
      } on AccountException catch (e) {
        if (generation == _generation) syncNote.value = '이 폰에는 적용했지만 계정에 저장하지 못했어요. ${e.message}';
      }
    });
    return _writes;
  }

  Future<void> setPerDevice(bool on) async {
    perDevice.value = on;
    await _write('per_device', on);
    if (!on) await set(settings.value);
  }

  Future<void> setCard(String id, CardWeather w) async {
    cards.value = {...cards.value}..remove(id);
    if (w is! FollowWeather) cards.value = {...cards.value, id: w};
    await _write('cards', {for (final e in cards.value.entries) e.key: e.value.toJson()});
  }

  // iOS 「투명도 줄이기」는 플러터가 안 알려 준다 — 앱 쪽 다리로 묻고, 바뀌면 다리가 알린다.
  // 「동작 줄이기」는 MediaQuery.disableAnimations 로 온다.
  bool _observing = false;

  @override
  void didChangeAccessibilityFeatures() {
    final f = WidgetsBinding.instance.platformDispatcher.accessibilityFeatures;
    reduceMotion.value = f.reduceMotion || f.disableAnimations;
  }

  Future<void> _watchOs() async {
    if (!_observing) {
      _observing = true;
      WidgetsBinding.instance.addObserver(this);
    }
    didChangeAccessibilityFeatures();
    _channel.setMethodCallHandler((call) async {
      if (call.method == 'reduceTransparency') reduceTransparency.value = call.arguments == true;
    });
    try {
      reduceTransparency.value = await _channel.invokeMethod<bool>('reduceTransparency') ?? false;
    } catch (_) {
      // 다리가 없는 판(웹·시험)은 꺼진 것으로 본다.
    }
  }

  Future<Object?> _read(String name) async {
    try {
      final v = await _storage.read(key: _key(name));
      return v == null ? null : jsonDecode(v);
    } catch (_) {
      return null;
    }
  }

  Future<void> _write(String name, Object? value) async {
    try {
      await _storage.write(key: _key(name), value: jsonEncode(value));
    } catch (_) {
      // 저장소가 막혀도 이번 실행은 고른 대로 보인다.
    }
  }
}
