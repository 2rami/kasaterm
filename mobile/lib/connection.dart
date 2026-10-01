import 'dart:async';

import 'package:flutter/foundation.dart';

import 'connection_store.dart';
import 'relay_account.dart';
import 'server.dart';

enum ConnectionPhase { restoring, signedOut, checking, waiting, ready }

typedef RelayFactory =
    RelayAccountApi Function(Uri origin, AccountSession? session);

class ConnectionController extends ChangeNotifier {
  ConnectionController({
    ConnectionStore? store,
    RelayFactory? relayFactory,
    Server Function(AccountSession)? serverFactory,
  }) : _store = store ?? const ConnectionStore(),
       _relay =
           relayFactory ??
           ((origin, session) => RelayAccountApi(origin, session: session)),
       _serverFactory = serverFactory ?? Server.account;

  final ConnectionStore _store;
  final RelayFactory _relay;
  final Server Function(AccountSession) _serverFactory;
  ConnectionPhase phase = ConnectionPhase.restoring;
  AccountSession? account;
  Server? server;
  String? message;
  Future<bool> Function()? beforeDisconnect;
  List<Map<String, dynamic>> devices = [];
  int _generation = 0;
  bool _disposed = false;
  Future<void> _storageQueue = Future.value();

  bool _current(int generation) => !_disposed && generation == _generation;
  void _notify() {
    if (!_disposed) notifyListeners();
  }

  Future<void> _save(SavedConnection value, int generation) {
    final write = _storageQueue.catchError((Object _) {}).then((_) async {
      if (_current(generation)) await _store.save(value);
    });
    _storageQueue = write;
    return write;
  }

  int _reset() {
    _generation++;
    server?.close();
    server = null;
    account = null;
    devices = [];
    message = null;
    return _generation;
  }

  /// `preferBaked` 는 실기 설치(`tool/phone.sh`)가 구운 주소로 옛 주소 연결을 갈아 끼울 때만 켠다 — 저장된
  /// 옛 주소는 앱을 다시 깔아도 키체인에 남아서, 그 기기가 관문에서 사라지면 구운 새 주소가 영영 안 쓰였다.
  /// 계정 로그인은 건드리지 않는다.
  Future<void> restore({String bakedRoot = '', bool preferBaked = false}) async {
    final generation = _reset();
    phase = ConnectionPhase.restoring;
    _notify();
    try {
      final saved = await _store.load();
      if (!_current(generation)) return;
      account = saved?.account;
      if (account != null) {
        await _check(generation);
        return;
      }
      final baked = Server.parse(bakedRoot);
      final replace = preferBaked && baked != null && saved?.legacyRoot != baked;
      final root = saved == null || replace ? baked : saved.legacyRoot;
      if (replace) await _save(SavedConnection(legacyRoot: baked), generation);
      if (root != null && Server.parse(root.toString()) != null) {
        server = Server(root);
        phase = ConnectionPhase.ready;
      } else {
        phase = ConnectionPhase.signedOut;
      }
    } catch (_) {
      if (!_current(generation)) return;
      phase = ConnectionPhase.signedOut;
      message = '저장된 로그인을 읽지 못했어요. 다시 로그인해 주세요.';
    }
    _notify();
  }

  Future<void> login(Uri origin, String name, String password) async {
    final api = _relay(origin, null);
    try {
      await _signIn(() => api.login(name, password));
    } finally {
      api.close();
    }
  }

  /// Google·GitHub 로그인이 받아 온 세션으로 들어간다(`screens/oauth_sheet.dart`).
  Future<void> adopt(AccountSession session) => _signIn(() async => session);

  /// 이 설치의 고정 id(`ConnectionStore.installId`).
  Future<String> installId() => _store.installId();

  RelayAccountApi relay(Uri origin, AccountSession? session) => _relay(origin, session);

  Future<void> _signIn(Future<AccountSession> Function() obtain) async {
    final generation = _reset();
    phase = ConnectionPhase.checking;
    _notify();
    try {
      final session = await obtain();
      if (!_current(generation)) return;
      await _save(SavedConnection(account: session), generation);
      if (!_current(generation)) return;
      account = session;
      await _check(generation);
    } on AccountException catch (e) {
      if (!_current(generation)) return;
      phase = ConnectionPhase.signedOut;
      message = e.message;
      _notify();
      rethrow;
    } catch (_) {
      if (!_current(generation)) return;
      phase = ConnectionPhase.signedOut;
      message = '로그인을 안전하게 저장하지 못했어요. 다시 시도해 주세요.';
      _notify();
      throw AccountException(message!);
    }
  }

  Future<void> retry() async {
    if (account == null || phase == ConnectionPhase.checking) return;
    await _check(_generation);
  }

  Future<void> _check(int generation) async {
    final session = account!;
    phase = ConnectionPhase.checking;
    message = null;
    _notify();
    final api = _relay(session.origin, session);
    Server? candidate;
    try {
      await api.verify(session);
      if (!_current(generation)) return;
      devices = await api.devices();
      if (!_current(generation)) return;
      candidate = _serverFactory(session);
      await candidate.me().timeout(const Duration(seconds: 15));
      if (!_current(generation)) return;
      server?.close();
      server = candidate;
      candidate = null;
      server!.onUnauthorized = () {
        unawaited(_expired(generation));
      };
      phase = ConnectionPhase.ready;
    } on AccountException catch (e) {
      if (!_current(generation)) return;
      if (e.status == 401) {
        await _expired(generation);
        return;
      }
      phase = ConnectionPhase.waiting;
      message = e.message;
    } on ServerException catch (e) {
      if (!_current(generation)) return;
      if (e.status == 401) {
        await _expired(generation);
        return;
      }
      phase = ConnectionPhase.waiting;
      message = e.status == 503
          ? accountError(503)
          : e.status == 404
          ? '로그인했어요. 데스크톱과 서버를 최신 버전으로 업데이트해 주세요.'
          : '로그인했어요. 데스크톱에 연결하지 못했어요. 켜져 있는지 확인해 주세요.';
    } catch (_) {
      if (!_current(generation)) return;
      phase = ConnectionPhase.waiting;
      message = '연결 확인이 지연되고 있어요. 잠시 뒤 다시 시도해 주세요.';
    } finally {
      candidate?.close();
      api.close();
    }
    if (_current(generation)) _notify();
  }

  Future<void> _expired(int generation) async {
    if (!_current(generation)) return;
    await logout(revoke: false);
    if (!_current(generation + 1)) return;
    message = '로그인이 만료되었어요. 다시 로그인해 주세요.';
    _notify();
  }

  Future<void> connectLegacy(Server candidate) async {
    final generation = _reset();
    phase = ConnectionPhase.checking;
    _notify();
    try {
      await _save(SavedConnection(legacyRoot: candidate.root), generation);
      if (!_current(generation)) {
        candidate.close();
        return;
      }
      server = candidate;
      phase = ConnectionPhase.ready;
      _notify();
    } catch (_) {
      candidate.close();
      if (!_current(generation)) return;
      phase = ConnectionPhase.signedOut;
      _notify();
      throw const ServerException('연결을 안전하게 저장하지 못했어요.');
    }
  }

  Future<void> logout({bool revoke = true}) async {
    final previous = account;
    final cleanup = beforeDisconnect?.call() ?? Future.value(true);
    final generation = _reset();
    phase = ConnectionPhase.signedOut;
    _notify();
    unawaited(() async {
      final cleaned = await cleanup.timeout(
        const Duration(seconds: 3),
        onTimeout: () => false,
      );
      if (!cleaned && _current(generation)) {
        message = '이 폰의 연결은 종료했어요. 이전 기기의 알림 해제는 연결이 돌아오면 다시 확인해야 해요.';
        _notify();
      }
      if (revoke && previous != null) {
        final api = _relay(previous.origin, previous);
        try {
          await api.logout().timeout(const Duration(seconds: 3));
        } catch (_) {
          if (_current(generation)) {
            message = '이 폰의 로그인은 지웠어요. 서버 연결이 없어 원격 로그인 해제는 확인하지 못했어요.';
            _notify();
          }
        } finally {
          api.close();
        }
      }
    }());
    try {
      await _save(const SavedConnection(), generation);
    } catch (_) {
      if (_current(generation)) {
        message = '이 기기의 로그아웃 저장에 실패했어요. 다시 눌러 주세요.';
        _notify();
      }
    }
  }

  @override
  void dispose() {
    _disposed = true;
    _reset();
    super.dispose();
  }
}
