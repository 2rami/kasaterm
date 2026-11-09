import 'dart:async';
import 'dart:math';

import 'package:flutter/foundation.dart';

import 'archive_model.dart' show maxArchiveBytes;
import 'conversation.dart';
import 'relay_account.dart';
import 'server.dart';
import 'shared_rooms_model.dart';

class RelaySharedRoomsApi implements SharedRoomsApi {
  RelaySharedRoomsApi(AccountSession session) : _api = RelayAccountApi(session.origin, session: session);
  final RelayAccountApi _api;
  @override
  Future<List<SharedRoom>> rooms() => _api.sharedRooms();
  @override
  Future<SharedRoom> room(String id) => _api.sharedRoom(id);
  @override
  Future<SharedRoom> create(String title, {required bool allowFork, required String requestId}) =>
      _api.createSharedRoom(title, allowFork: allowFork, requestId: requestId);
  @override
  Future<SharedRoom> update(String id, int expectedRevision, {String? title, bool? allowFork}) =>
      _api.updateSharedRoom(id, expectedRevision, title: title, allowFork: allowFork);
  @override
  Future<void> delete(String id) => _api.deleteSharedRoom(id);
  @override
  Future<SharedInvite> invite(String roomId) => _api.createSharedInvite(roomId);
  @override
  Future<void> revokeInvite(String roomId, String inviteId) => _api.revokeSharedInvite(roomId, inviteId);
  @override
  Future<SharedRoom?> accept(String token) => _api.acceptSharedInvite(token);
  @override
  Future<void> removeMember(String roomId, String account) => _api.removeSharedMember(roomId, account);
  @override
  Future<SharedContent> content(String roomId, String sessionId, {required String generation, required int offset, int? end}) =>
      _api.sharedContent(roomId, sessionId, generation: generation, offset: offset, end: end);
  @override
  Future<SharedForkInfo> fork(String roomId, String sessionId, {required String checkpointId, required String requestId}) =>
      _api.createSharedFork(roomId, sessionId, checkpointId: checkpointId, requestId: requestId);
  @override
  void close() => _api.close();
}

String sharedRequestId() {
  final rng = Random.secure();
  return List.generate(16, (_) => rng.nextInt(256).toRadixString(16).padLeft(2, '0')).join();
}

/// 계정 하나의 공유방. 계정이 바뀌거나 로그아웃하면 [close] — 그 뒤 도착한 응답은 버리고 받은 내용·초대 코드를 걷는다.
/// 공유방은 기기 연결과 별개라 데스크톱이 없어도 목록·참여·읽기가 된다. [desktop] 은 포크 목적지를 고를 때만 쓴다.
class SharedRoomsRepository extends ChangeNotifier {
  SharedRoomsRepository(this.account, {SharedRoomsApi? api}) : _api = api ?? RelaySharedRoomsApi(account);

  final AccountSession account;
  final SharedRoomsApi _api;
  void Function()? onUnauthorized;

  Server? _desktop;
  Server? get desktop => _desktop == null || _desktop!.isClosed ? null : _desktop;
  set desktop(Server? value) {
    if (identical(value, _desktop)) return;
    _desktop = value;
    _notify();
  }

  bool _closed = false;
  int _epoch = 0;
  bool get locked => _closed;

  List<SharedRoom> rooms = const [];
  final Map<String, SharedRoom> _details = {};
  bool loading = true, refreshing = false;
  String? error;

  /// 방에서 쫓겨났거나 방이 사라졌을 때 그 방 화면에 남길 말.
  final Map<String, String> gone = {};

  final Map<String, Future<SharedRoom?>> _roomReads = {};
  final Map<String, String> _forkRequests = {};
  bool _loadingList = false, _again = false;

  void _notify() {
    if (!_closed) notifyListeners();
  }

  SharedRoom? room(String id) => _details[id] ?? rooms.where((r) => r.id == id).firstOrNull;

  /// 늦게 온 응답이 다른 계정 화면에 내용을 남기지 않게 — 요청 전 번호와 같을 때만 받는다.
  bool _current(int epoch) => !_closed && epoch == _epoch;

  String _message(Object e, {String? roomId}) {
    if (e is AccountException) {
      if (e.status == 401) {
        final expired = onUnauthorized;
        close();
        expired?.call();
        return sharedError('unauthorized', 401);
      }
      if (roomId != null && sharedAccessLost(e.code, e.status)) _forget(roomId, e.message);
      return e.message;
    }
    if (e is SharedRoomsException) return e.message;
    return '공유방에 연결하지 못했어요. 네트워크를 확인해 주세요.';
  }

  SharedRoomsException _fail(Object e, {String? roomId}) {
    final message = _message(e, roomId: roomId);
    return SharedRoomsException(message, code: e is AccountException ? e.code : null, status: e is AccountException ? e.status : null);
  }

  void _forget(String roomId, String why) {
    rooms = [for (final r in rooms) if (r.id != roomId) r];
    _details.remove(roomId);
    _forkRequests.removeWhere((k, _) => k.startsWith('$roomId\u0000'));
    gone[roomId] = why;
    _notify();
  }

  void _store(SharedRoom room) {
    gone.remove(room.id);
    _details[room.id] = room;
    final i = rooms.indexWhere((r) => r.id == room.id);
    rooms = i < 0 ? [...rooms, room] : ([...rooms]..[i] = room);
  }

  Future<void> load() async {
    if (_closed) return;
    if (_loadingList) {
      _again = true;
      return;
    }
    _loadingList = true;
    final epoch = _epoch;
    refreshing = true;
    _notify();
    try {
      final list = await _api.rooms();
      if (!_current(epoch)) return;
      final ids = {for (final r in list) r.id};
      for (final id in _details.keys.where((id) => !ids.contains(id)).toList()) {
        _details.remove(id);
        gone[id] = '공유가 끝났거나 이 방에서 나왔어요.';
      }
      for (final r in list) {
        _details[r.id] = r;
        gone.remove(r.id);
      }
      rooms = list;
      error = null;
    } catch (e) {
      if (_current(epoch)) error = _message(e);
    } finally {
      _loadingList = false;
      if (_current(epoch)) {
        loading = false;
        refreshing = false;
        _notify();
        if (_again) {
          _again = false;
          unawaited(load());
        }
      }
    }
  }

  /// 방 상세를 새로 받는다. 같은 방을 여러 화면이 동시에 물어도 한 번만 간다. 권한이 없어졌으면 null.
  Future<SharedRoom?> refreshRoom(String id) {
    if (_closed) return Future.value(null);
    return _roomReads[id] ??= () async {
      final epoch = _epoch;
      try {
        final room = await _api.room(id);
        if (!_current(epoch)) return null;
        if (room.id != id) throw const SharedRoomsException('공유방 응답을 확인하지 못했어요.');
        _store(room);
        _notify();
        return room;
      } catch (e) {
        if (!_current(epoch)) return null;
        final failure = _fail(e, roomId: id);
        if (_closed || gone.containsKey(id)) return null;
        throw failure;
      } finally {
        _roomReads.remove(id);
      }
    }();
  }

  Future<T> _act<T>(Future<T> Function() action, {String? roomId}) async {
    if (_closed) throw const SharedRoomsException('현재 계정의 연결이 종료됐어요.');
    final epoch = _epoch;
    try {
      final result = await action();
      if (!_current(epoch)) throw const SharedRoomsException('현재 계정의 연결이 종료됐어요.');
      return result;
    } on SharedRoomsException {
      rethrow;
    } catch (e) {
      if (!_current(epoch)) throw const SharedRoomsException('현재 계정의 연결이 종료됐어요.');
      if (e is AccountException && e.code == 'shared_conflict' && roomId != null) unawaited(refreshRoom(roomId).catchError((_) => null));
      throw _fail(e, roomId: roomId);
    }
  }

  Future<SharedRoom> create(String title, {required bool allowFork, required String requestId}) async {
    final room = await _act(() => _api.create(title.trim(), allowFork: allowFork, requestId: requestId));
    _store(room);
    _notify();
    return room;
  }

  Future<SharedRoom> _update(SharedRoom room, {String? title, bool? allowFork}) async {
    final fresh = await _act(() => _api.update(room.id, room.revision, title: title, allowFork: allowFork), roomId: room.id);
    _store(fresh);
    _notify();
    return fresh;
  }

  Future<SharedRoom> rename(SharedRoom room, String title) => _update(room, title: title.trim());
  Future<SharedRoom> setAllowFork(SharedRoom room, bool allow) => _update(room, allowFork: allow);

  /// 원문 코드는 돌려줄 뿐 저장하지 않는다 — 다시 볼 수 없으니 화면이 바로 보여 주고 복사하게 한다.
  Future<SharedInvite> issueInvite(String roomId) async {
    final invite = await _act(() => _api.invite(roomId), roomId: roomId);
    if (invite.roomId != roomId) throw const SharedRoomsException('공유방 응답을 확인하지 못했어요.');
    unawaited(refreshRoom(roomId).catchError((_) => null));
    return invite;
  }

  Future<void> revokeInvite(String roomId, String inviteId) async {
    await _act(() => _api.revokeInvite(roomId, inviteId), roomId: roomId);
    await refreshRoom(roomId).catchError((_) => null);
  }

  Future<void> removeMember(String roomId, String account) async {
    await _act(() => _api.removeMember(roomId, account), roomId: roomId);
    await refreshRoom(roomId).catchError((_) => null);
  }

  Future<void> leave(String roomId) async {
    await _act(() => _api.removeMember(roomId, account.account), roomId: roomId);
    _forget(roomId, '이 방에서 나왔어요.');
  }

  Future<void> deleteRoom(String roomId) async {
    await _act(() => _api.delete(roomId), roomId: roomId);
    _forget(roomId, '공유를 끝냈어요.');
  }

  /// 초대를 지금 계정으로 받는다. 방에 들어가도 셸·창은 만들지 않는다.
  Future<SharedRoom?> accept(String token) async {
    final room = await _act(() => _api.accept(token));
    if (room != null) {
      _store(room);
      _notify();
    }
    unawaited(load());
    return room;
  }

  /// 수신자 소유 사본을 고정한다. 같은 방·대화·체크포인트의 재시도는 같은 request_id 라 사본이 하나만 생긴다.
  Future<SharedForkInfo> fixFork(SharedRoom room, SharedSession session) async {
    final checkpoint = session.checkpoint;
    if (!room.canFork(session) || checkpoint == null) {
      throw const SharedRoomsException('이 대화는 지금 포크할 수 없어요.');
    }
    final requestId = stableRequestId(room.id, 'fork\u0000${session.id}\u0000${checkpoint.id}');
    final info = await _act(
      () => _api.fork(room.id, session.id, checkpointId: checkpoint.id, requestId: requestId),
      roomId: room.id,
    );
    if (info.roomId != room.id || info.sessionId != session.id || info.checkpointId != checkpoint.id) {
      throw const SharedRoomsException('포크 응답이 요청과 맞지 않아요.');
    }
    return info;
  }

  /// 같은 일의 재시도에 같은 표식을 준다. 계정이 바뀌거나 방을 잃으면 잊는다.
  String stableRequestId(String roomId, String key) => _forkRequests['$roomId\u0000$key'] ??= sharedRequestId();

  SharedSessionReader reader(String roomId, String sessionId) => SharedSessionReader._(this, roomId, sessionId);

  Future<SharedContent> _content(String roomId, String sessionId, String generation, int offset, int end) =>
      _api.content(roomId, sessionId, generation: generation, offset: offset, end: end);

  void close() {
    if (_closed) return;
    _closed = true;
    _epoch++;
    _desktop = null;
    rooms = const [];
    _details.clear();
    _roomReads.clear();
    _forkRequests.clear();
    gone.clear();
    error = null;
    _api.close();
    notifyListeners();
  }

  @override
  void dispose() {
    close();
    super.dispose();
  }
}

/// 공유된 대화 하나의 읽기. PTY 도 원본 칸도 없다 — 관문의 본문만 이어 받아 [conversation] 에 편다.
///
/// 위치는 관문이 준 UTF-8 바이트다. 256KiB 로 잘린 조각은 줄 중간에서 끝날 수 있어 마지막 줄은 다음 조각까지 들고 있는다.
/// 세대가 바뀌면 처음부터 다시 받는다. [hold] 동안(사람이 위로 올려 읽는 중) 받은 것은 쌓아만 두고 [fresh] 로 알린다 —
/// 새 줄을 끼워 넣으면 읽던 자리가 밀린다.
class SharedSessionReader extends ChangeNotifier {
  SharedSessionReader._(this._repo, this.roomId, this.sessionId);

  final SharedRoomsRepository _repo;
  final String roomId, sessionId;
  final conversation = Conversation(archived: true);

  /// 한 번 받기에 이어 읽는 조각 수 — 큰 대화도 몇 번의 폴링에 걸쳐 다 온다.
  static const chunksPerPoll = 8;

  SharedSession? session;
  String _generation = '';
  int offset = 0;
  String _carry = '';
  final StringBuffer _all = StringBuffer(), _unapplied = StringBuffer();
  bool _resetPending = false, _shown = false;
  bool _busy = false, _disposed = false;

  bool loading = true, fresh = false;
  String? error;

  /// 방이나 대화가 사라졌다 — 받은 내용은 이미 걷었다.
  String? gone;

  bool _hold = false;
  bool get hold => _hold;
  set hold(bool value) {
    if (_hold == value) return;
    _hold = value;
    if (!value) _sync();
  }

  bool get _live => !_disposed && !_repo.locked;

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  void _drop(String why) {
    gone = why;
    session = null;
    _restart('');
    conversation.clear();
    _shown = false;
    fresh = false;
    loading = false;
    _notify();
  }

  void _restart(String generation) {
    _generation = generation;
    offset = 0;
    _carry = '';
    _all.clear();
    _unapplied.clear();
    _resetPending = true;
  }

  /// 받는 중인 대화의 끝(바이트). 첫 화면의 진행 막대에 쓴다.
  int end = 0;
  double? get progress => end == 0 ? null : offset / end;

  Future<void> poll() async {
    if (_busy || !_live) return;
    _busy = true;
    final repo = _repo;
    final epoch = repo._epoch;
    bool stale() => !_live || repo._epoch != epoch;
    try {
      final room = await repo.refreshRoom(roomId);
      if (stale()) return;
      final latest = room?.session(sessionId);
      if (latest == null) {
        _drop(room == null ? repo.gone[roomId] ?? '이 공유방을 볼 수 없어요.' : '게시자가 이 대화를 내렸어요.');
        return;
      }
      session = latest;
      gone = null;
      if (latest.bytes > maxArchiveBytes) {
        throw const SharedRoomsException('대화가 너무 길어 폰에서 열 수 없어요. 데스크톱에서 열어 주세요.');
      }
      // 같은 세대가 줄어들 일은 없다 — 세대가 바뀌었거나 그렇게 보이면 처음부터 다시 받는다.
      if (latest.generation != _generation || offset > latest.bytes) _restart(latest.generation);
      end = latest.bytes;
      for (var i = 0; i < chunksPerPoll && offset < end; i++) {
        final chunk = await repo._content(roomId, sessionId, _generation, offset, end);
        if (stale()) return;
        if (chunk.generation != _generation || chunk.offset != offset || chunk.end != end || chunk.next <= offset) {
          _restart('');
          throw const SharedRoomsException('대화 조각이 달라졌어요. 다시 받을게요.');
        }
        offset = chunk.next;
        final text = _carry + chunk.raw;
        final cut = text.lastIndexOf('\n') + 1;
        _all.write(text.substring(0, cut));
        _unapplied.write(text.substring(0, cut));
        _carry = text.substring(cut);
      }
      error = null;
      loading = offset < end && !_shown;
      _sync();
    } catch (e) {
      if (stale()) return;
      final failure = e is SharedRoomsException ? e : repo._fail(e, roomId: roomId);
      if (repo.locked) return;
      if (repo.gone.containsKey(roomId)) {
        _drop(repo.gone[roomId]!);
        return;
      }
      if (failure.code == 'shared_conflict') _restart('');
      error = failure.message;
      loading = false;
      _notify();
    } finally {
      _busy = false;
    }
  }

  void _sync() {
    final pending = _resetPending || _unapplied.isNotEmpty;
    // 첫 화면은 끝까지 받은 뒤 한 번에 편다. 그 뒤로는 읽는 자리를 지킨다.
    if (loading || (_hold && _shown)) {
      fresh = _shown && pending;
      _notify();
      return;
    }
    if (_resetPending) {
      conversation.clear();
      conversation.apply(_all.toString(), reset: true, next: offset);
    } else if (_unapplied.isNotEmpty &&
        !conversation.apply(_unapplied.toString(), reset: false, next: offset)) {
      // 같은 세대 안에서 세션이 이어진 기록(/clear 등) — 다시 펴면 둘 다 보인다.
      conversation.apply(_all.toString(), reset: true, next: offset);
    }
    _unapplied.clear();
    _resetPending = false;
    _shown = true;
    fresh = false;
    _notify();
  }

  /// 「새 내용 보기」 — 쌓아 둔 것을 지금 편다.
  void showFresh() {
    final held = _hold;
    _hold = false;
    _sync();
    _hold = held;
  }

  @override
  void dispose() {
    _disposed = true;
    super.dispose();
  }
}
