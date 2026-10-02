import 'dart:convert';
import 'dart:ffi';
import 'dart:io';

import 'package:ffi/ffi.dart';

import 'kasanet.dart';

typedef _StartC = Int32 Function(Pointer<Utf8>);
typedef _StartD = int Function(Pointer<Utf8>);
typedef _StrC = Pointer<Utf8> Function();
typedef _OpenC = Int32 Function(Pointer<Utf8>);
typedef _OpenD = int Function(Pointer<Utf8>);
typedef _StateC = Pointer<Utf8> Function(Uint16);
typedef _StateD = Pointer<Utf8> Function(int);
typedef _VoidC = Void Function();
typedef _VoidD = void Function();
typedef _FreeC = Void Function(Pointer<Utf8>);
typedef _FreeD = void Function(Pointer<Utf8>);

KasanetNative? _node;
bool _tried = false;

/// 앱에 링크된 카사넷(ios/KasaNet)을 띄운다. 기호가 없거나(시험·맥) 못 뜨면 null — 늘 관문으로 간다.
/// 키는 앱 컨테이너(`Library/Application Support/kasanet/`)에만 둔다.
KasanetNative? startKasanet() {
  if (_tried) return _node;
  _tried = true;
  if (!Platform.isIOS) return null;
  try {
    final lib = DynamicLibrary.process();
    // iOS 앱 환경에는 HOME 이 없다. 임시 폴더(NSTemporaryDirectory)가 컨테이너 바로 아래 `tmp` 라 그 위가 컨테이너다.
    final container = Directory.systemTemp.parent.path;
    final dir = Directory('$container/Library/Application Support/kasanet')
      ..createSync(recursive: true);
    final node = _Ffi(lib);
    if (!node.start('${dir.path}/kasanet.key')) return null;
    _node = node;
  } catch (_) {
    _node = null;
  }
  return _node;
}

class _Ffi implements KasanetNative {
  _Ffi(DynamicLibrary lib)
    : _start = lib.lookupFunction<_StartC, _StartD>('kasanet_start'),
      _id = lib.lookupFunction<_StrC, _StrC>('kasanet_id'),
      _open = lib.lookupFunction<_OpenC, _OpenD>('kasanet_open'),
      _state = lib.lookupFunction<_StateC, _StateD>('kasanet_state'),
      _changed = lib.lookupFunction<_VoidC, _VoidD>('kasanet_network_changed'),
      _lastError = lib.lookupFunction<_StrC, _StrC>('kasanet_last_error'),
      _free = lib.lookupFunction<_FreeC, _FreeD>('kasanet_free_string');

  final _StartD _start;
  final _StrC _id;
  final _OpenD _open;
  final _StateD _state;
  final _VoidD _changed;
  final _StrC _lastError;
  final _FreeD _free;

  String? _take(Pointer<Utf8> p) {
    if (p == nullptr) return null;
    try {
      return p.toDartString();
    } finally {
      _free(p);
    }
  }

  bool start(String keyPath) {
    final p = keyPath.toNativeUtf8();
    try {
      return _start(p) == 0;
    } finally {
      malloc.free(p);
    }
  }

  @override
  String? get id => _take(_id());

  @override
  int open(String peerJson) {
    final p = peerJson.toNativeUtf8();
    try {
      return _open(p);
    } finally {
      malloc.free(p);
    }
  }

  @override
  KasanetPath? state(int port) {
    final s = _take(_state(port));
    if (s == null) return null;
    final Object? j;
    try {
      j = jsonDecode(s);
    } catch (_) {
      return null;
    }
    if (j is! Map) return null;
    return KasanetPath(
      direct: j['path'] == 'direct' || j['path'] == 'kasa_relay',
      relayed: j['path'] == 'kasa_relay',
      rttMs: (j['rtt_ms'] as num?)?.toInt(),
      error: j['error'] as String?,
    );
  }

  @override
  void networkChanged() => _changed();

  @override
  String? get lastError => _take(_lastError());
}
