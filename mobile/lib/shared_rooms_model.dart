import 'dart:convert';

/// 공유방 관문 응답의 모양(docs/shared-rooms.md, `crates/kasa-socket/src/shared_rooms.rs`).
/// 관문이 보낸 값이라도 화면·주소에 쓰기 전에 길이·제어 문자를 확인한다.
class SharedRoomsException implements Exception {
  const SharedRoomsException(this.message, {this.code, this.status});
  final String message;
  final String? code;
  final int? status;
  @override
  String toString() => message;
}

const _malformed = SharedRoomsException('공유방 응답을 확인하지 못했어요. 앱이나 관문 업데이트가 필요할 수 있어요.');

String sharedError(String? code, int status) => switch (code) {
  'shared_invalid' => '요청 모양이 맞지 않아요. 입력한 값을 확인해 주세요.',
  'unauthorized' => '로그인이 만료됐어요. 다시 로그인해 주세요.',
  'shared_forbidden' => '이 공유방에서 할 수 없는 일이에요.',
  'desktop_only' => '이 일은 데스크톱 카사텀에서 해 주세요.',
  'fork_disabled' => '이 공유방은 포크를 허용하지 않아요.',
  'shared_missing' => '공유방이나 대화를 찾지 못했어요. 공유가 끝났거나 권한이 바뀌었을 수 있어요.',
  'shared_conflict' => '그사이 공유방이 바뀌었어요. 새로 불러온 뒤 다시 해 주세요.',
  'invite_unavailable' => '만료됐거나 이미 쓴 초대 코드예요. 새 코드를 받아 주세요.',
  'shared_limit' => '공유방 한도를 넘었어요.',
  'shared_busy' => '요청이 많아요. 잠시 뒤 다시 해 주세요.',
  'shared_storage' => '공유방 저장소가 잠시 응답하지 않아요. 잠시 뒤 다시 해 주세요.',
  _ when status == 401 => '로그인이 만료됐어요. 다시 로그인해 주세요.',
  _ when status == 404 => '관문이 공유방을 아직 몰라요. 관문 업데이트가 필요해요.',
  _ when status == 429 => '요청이 많아요. 잠시 뒤 다시 해 주세요.',
  _ => '공유방 요청을 처리하지 못했어요 ($status).',
};

/// 권한이 사라졌다는 답 — 그 방의 내용을 화면과 메모리에서 걷는다.
bool sharedAccessLost(String? code, int? status) =>
    code == 'shared_missing' || code == 'shared_forbidden' || (code == null && status == 403);

final _control = RegExp(r'[\x00-\x1f\x7f]');

class _Json {
  _Json(Object? value) : map = value is Map ? value : throw _malformed;
  final Map map;

  String text(String key, {int max = 512, bool empty = true, bool optional = false}) {
    final v = map[key];
    if (v == null && optional) return '';
    if (v is! String || (!empty && v.isEmpty) || utf8.encode(v).length > max || _control.hasMatch(v)) {
      throw _malformed;
    }
    return v;
  }

  String id(String key, {bool optional = false}) => text(key, max: 128, empty: optional, optional: optional);

  int number(String key, {bool optional = false}) {
    final v = map[key];
    if (v == null && optional) return 0;
    if (v is! int || v < 0 || v > 1 << 53) throw _malformed;
    return v;
  }

  bool flag(String key) {
    final v = map[key];
    if (v is! bool) throw _malformed;
    return v;
  }

  DateTime time(String key) => DateTime.fromMillisecondsSinceEpoch(number(key), isUtc: true);

  List<T> list<T>(String key, T Function(Object?) item, {int max = 4096}) {
    final v = map[key];
    if (v == null) return const [];
    if (v is! List || v.length > max) throw _malformed;
    return [for (final e in v) item(e)];
  }
}

class SharedMember {
  const SharedMember({required this.account, required this.displayName, required this.role});
  final String account, displayName, role;
  bool get owner => role == 'owner';
  String get label => displayName.isNotEmpty ? displayName : account;

  factory SharedMember.fromJson(Object? value) {
    final j = _Json(value);
    return SharedMember(account: j.id('account'), displayName: j.text('display_name', max: 256), role: j.text('role', max: 32));
  }
}

class SharedCheckpoint {
  const SharedCheckpoint({
    required this.id,
    required this.harness,
    required this.boundaryLabel,
    required this.capturedAt,
    required this.hasProject,
    this.omissions = const [],
  });
  final String id, harness, boundaryLabel;
  final DateTime capturedAt;
  final bool hasProject;
  final List<String> omissions;

  factory SharedCheckpoint.fromJson(Object? value) {
    final j = _Json(value);
    // 원본 세션 id·경계 id 는 데스크톱이 포크 자료에서 쓴다 — 폰 화면엔 남기지 않는다.
    return SharedCheckpoint(
      id: j.id('id'),
      harness: j.text('harness', max: 32),
      boundaryLabel: j.text('boundary_label', max: 512),
      capturedAt: j.time('captured_at_ms'),
      hasProject: j.flag('has_project'),
      omissions: j.list('omissions', (e) => e is String && e.length <= 512 && !_control.hasMatch(e) ? e : throw _malformed, max: 256),
    );
  }
}

class SharedSession {
  const SharedSession({
    required this.id,
    required this.title,
    required this.harness,
    required this.revision,
    required this.generation,
    required this.bytes,
    required this.updatedAt,
    required this.caughtUp,
    this.sourceKey = '',
    this.publisherMachineId = '',
    this.checkpoint,
  });
  final String id, title, harness, generation;

  /// 게시자 본인에게만 온다 — 참여자에게는 빈 값이다.
  final String sourceKey, publisherMachineId;
  final int revision, bytes;
  final DateTime updatedAt;
  final bool caughtUp;
  final SharedCheckpoint? checkpoint;

  String get displayTitle => title.isNotEmpty
      ? title
      : harness == 'codex'
      ? 'Codex 대화'
      : 'Claude 대화';

  factory SharedSession.fromJson(Object? value) {
    final j = _Json(value);
    final checkpoint = j.map['checkpoint'];
    return SharedSession(
      id: j.id('id'),
      sourceKey: j.text('source_key', optional: true),
      title: j.text('title'),
      harness: j.text('harness', max: 32),
      publisherMachineId: j.text('publisher_machine_id', max: 128, optional: true),
      revision: j.number('revision'),
      generation: j.text('generation', max: 64),
      bytes: j.number('bytes'),
      updatedAt: j.time('updated_at_ms'),
      caughtUp: j.flag('caught_up'),
      checkpoint: checkpoint == null ? null : SharedCheckpoint.fromJson(checkpoint),
    );
  }
}

class SharedInviteSummary {
  const SharedInviteSummary({required this.id, required this.expiresAt, required this.maxUses, required this.uses});
  final String id;
  final DateTime expiresAt;
  final int maxUses, uses;
  bool usable(DateTime now) => expiresAt.isAfter(now) && (maxUses == 0 || uses < maxUses);

  factory SharedInviteSummary.fromJson(Object? value) {
    final j = _Json(value);
    return SharedInviteSummary(id: j.id('id'), expiresAt: j.time('expires_at_ms'), maxUses: j.number('max_uses'), uses: j.number('uses'));
  }
}

class SharedRoom {
  const SharedRoom({
    required this.id,
    required this.title,
    required this.ownerAccount,
    required this.ownerName,
    required this.myRole,
    required this.allowFork,
    required this.revision,
    required this.updatedAt,
    this.members = const [],
    this.sessions = const [],
    this.invites = const [],
  });
  final String id, title, ownerAccount, ownerName, myRole;
  final bool allowFork;
  final int revision;
  final DateTime updatedAt;
  final List<SharedMember> members;
  final List<SharedSession> sessions;

  /// 방 주인에게만 온다.
  final List<SharedInviteSummary> invites;

  bool get owner => myRole == 'owner';
  String get displayTitle => title.isNotEmpty ? title : '이름 없는 공유방';
  bool canFork(SharedSession s) => allowFork && s.checkpoint != null;
  SharedSession? session(String id) => sessions.where((s) => s.id == id).firstOrNull;

  factory SharedRoom.fromJson(Object? value) {
    final j = _Json(value);
    final room = SharedRoom(
      id: j.id('id'),
      title: j.text('title'),
      ownerAccount: j.id('owner_account'),
      ownerName: j.text('owner_name', max: 256),
      myRole: j.text('my_role', max: 32),
      allowFork: j.flag('allow_fork'),
      revision: j.number('revision'),
      updatedAt: j.time('updated_at_ms'),
      members: j.list('members', SharedMember.fromJson),
      sessions: j.list('sessions', SharedSession.fromJson),
      invites: j.list('invites', SharedInviteSummary.fromJson),
    );
    if (room.sessions.map((s) => s.id).toSet().length != room.sessions.length) throw _malformed;
    return room;
  }
}

List<SharedRoom> sharedRoomsFrom(Map<String, dynamic> json) {
  final rooms = _Json(json).list('rooms', SharedRoom.fromJson, max: 1024);
  if (json['rooms'] is! List || rooms.map((r) => r.id).toSet().length != rooms.length) throw _malformed;
  return rooms;
}

/// 발급 응답에만 오는 초대 원문. 화면 메모리에만 두고 저장하지 않는다.
class SharedInvite {
  const SharedInvite({required this.id, required this.roomId, required this.token, required this.expiresAt, required this.maxUses});
  final String id, roomId, token;
  final DateTime expiresAt;
  final int maxUses;

  factory SharedInvite.fromJson(Object? value) {
    final j = _Json(value);
    final token = j.text('token', max: 512, empty: false);
    if (parseInviteToken(token) != token) throw _malformed;
    return SharedInvite(id: j.id('id'), roomId: j.id('room_id'), token: token, expiresAt: j.time('expires_at_ms'), maxUses: j.number('max_uses'));
  }
}

/// 사람이 붙여 넣은 초대 — 코드 자체이거나 코드를 실은 링크. 공백·줄바꿈은 걷는다.
String? parseInviteToken(String text) {
  var t = text.replaceAll(RegExp(r'\s'), '');
  if (t.contains('://')) {
    final u = Uri.tryParse(t);
    t = u?.queryParameters['token'] ?? u?.queryParameters['invite'] ?? (u?.pathSegments.lastOrNull ?? '');
  }
  return RegExp(r'^[A-Za-z0-9._~-]{8,512}$').hasMatch(t) ? t : null;
}

/// 한 세대 안의 본문 조각. 위치는 모두 UTF-8 바이트다.
class SharedContent {
  const SharedContent({
    required this.generation,
    required this.revision,
    required this.offset,
    required this.next,
    required this.end,
    required this.raw,
  });
  final String generation, raw;
  final int revision, offset, next, end;

  static const maxChunk = 256 << 10;

  factory SharedContent.fromJson(Object? value) {
    final j = _Json(value);
    final raw = j.map['raw'];
    if (raw is! String) throw _malformed;
    final content = SharedContent(
      generation: j.text('generation', max: 64, empty: false),
      revision: j.number('revision'),
      offset: j.number('offset'),
      next: j.number('next'),
      end: j.number('end'),
      raw: raw,
    );
    final size = utf8.encode(raw).length;
    if (size > maxChunk || content.next != content.offset + size || content.next > content.end) throw _malformed;
    return content;
  }
}

class SharedForkInfo {
  const SharedForkInfo({required this.id, required this.roomId, required this.sessionId, required this.checkpointId});
  final String id, roomId, sessionId, checkpointId;

  factory SharedForkInfo.fromJson(Object? value) {
    final j = _Json(value);
    return SharedForkInfo(id: j.id('id'), roomId: j.id('room_id'), sessionId: j.id('session_id'), checkpointId: j.id('checkpoint_id'));
  }
}

/// 공유방 관문 창구 — 시험은 가짜로 바꿔 끼운다.
abstract class SharedRoomsApi {
  Future<List<SharedRoom>> rooms();
  Future<SharedRoom> room(String id);
  Future<SharedRoom> create(String title, {required bool allowFork, required String requestId});
  Future<SharedRoom> update(String id, int expectedRevision, {String? title, bool? allowFork});
  Future<void> delete(String id);
  Future<SharedInvite> invite(String roomId);
  Future<void> revokeInvite(String roomId, String inviteId);
  Future<SharedRoom?> accept(String token);
  Future<void> removeMember(String roomId, String account);
  Future<SharedContent> content(String roomId, String sessionId, {required String generation, required int offset, int? end});
  Future<SharedForkInfo> fork(String roomId, String sessionId, {required String checkpointId, required String requestId});
  void close();
}

/// 포크 목적지(내 데스크톱)의 공유방 창구 상태 — `GET /shared-rooms/status`.
class ForkDestinationStatus {
  const ForkDestinationStatus({required this.available, required this.authenticated, required this.machineId, this.defaultCwd = ''});
  final bool available, authenticated;
  final String machineId, defaultCwd;

  factory ForkDestinationStatus.fromJson(Object? value) {
    final j = _Json(value);
    return ForkDestinationStatus(
      available: j.flag('available'),
      authenticated: j.flag('authenticated'),
      machineId: j.text('machine_id', max: 128, empty: false),
      defaultCwd: j.text('default_cwd', max: 4096, optional: true),
    );
  }
}

/// 게시할 수 있는 내 PC 의 대화 하나(`GET /shared-rooms/sources`). [address] 는 PC 가 준 완전한 주소 그대로 보낸다 —
/// 일부만 보내 PC 가 짐작하게 하면 그사이 같은 칸의 대화가 바뀌었을 때 다른 대화가 게시된다.
class PublishSource {
  const PublishSource({required this.address, required this.title, required this.harness, required this.roomLabel, required this.ready});
  final Map<String, Object?> address;
  final String title, harness, roomLabel;
  final bool ready;

  String get key => '${address['surface_key']}\u0000${address['session_id']}';
  String get displayTitle => title.isNotEmpty ? title : harness == 'codex' ? 'Codex 대화' : 'Claude 대화';

  factory PublishSource.fromJson(Object? value, String machineId) {
    final j = _Json(value);
    final a = _Json(j.map['address']);
    for (final key in const ['machine_id', 'surface_key', 'surface_id', 'session_id', 'instance_id']) {
      a.text(key, max: 128, empty: false);
    }
    if (a.map['machine_id'] != machineId) throw _malformed;
    return PublishSource(
      address: Map<String, Object?>.unmodifiable(a.map.cast<String, Object?>()),
      title: j.text('title', optional: true),
      harness: j.text('harness', max: 32, optional: true),
      roomLabel: j.text('room_label', max: 1024, optional: true),
      ready: j.flag('ready'),
    );
  }
}

List<PublishSource> publishSourcesFrom(Object? value) {
  final j = _Json(value);
  final machineId = j.text('machine_id', max: 128, empty: false);
  return j.list('sources', (e) => PublishSource.fromJson(e, machineId), max: 512);
}

enum ForkStage { preparing, starting, ready, needsInput, failed }

/// 목적지가 만든 내 새 대화의 주소. 원본 주소가 아니라 포크 결과다.
class ForkAddress {
  const ForkAddress({required this.machineId, required this.surfaceId, required this.surfaceKey});
  final String machineId, surfaceId, surfaceKey;

  static ForkAddress? fromJson(Object? value) {
    if (value == null) return null;
    final j = _Json(value);
    return ForkAddress(
      machineId: j.text('machine_id', max: 128, empty: false),
      surfaceId: j.text('surface_id', max: 64, empty: false),
      surfaceKey: j.text('surface_key', max: 128, optional: true),
    );
  }
}

class ForkOperation {
  const ForkOperation({required this.stage, this.address, this.childSessionId = '', this.error = ''});
  final ForkStage stage;
  final ForkAddress? address;
  final String childSessionId, error;

  bool get done => stage == ForkStage.ready || stage == ForkStage.failed;

  factory ForkOperation.fromJson(Object? value) {
    final j = _Json(value);
    final stage = switch (j.map['status']) {
      'preparing' => ForkStage.preparing,
      'starting' => ForkStage.starting,
      'ready' => ForkStage.ready,
      'needs_input' => ForkStage.needsInput,
      'failed' => ForkStage.failed,
      _ => throw _malformed,
    };
    final op = ForkOperation(
      stage: stage,
      address: ForkAddress.fromJson(j.map['address']),
      childSessionId: j.text('child_session_id', max: 128, optional: true),
      error: j.text('error', max: 1024, optional: true),
    );
    if (stage == ForkStage.ready && op.address == null) throw _malformed;
    return op;
  }
}
