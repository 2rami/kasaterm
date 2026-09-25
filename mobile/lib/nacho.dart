/// 카사모바일의 나쵸 창구 — 대화 원장을 이어 받고, 말을 보내고, 작업 장부를 읽는다.
///
/// 정본은 나쵸의 원장과 작업 장부다. 여기서 상태를 짐작해 앞서 그리지 않는다 — 보낸 말도
/// 서버가 영수증을 주기 전까지는 「보내는 중」이고, 답은 원장에 적힌 것만 보인다.
///
/// 끊겨도 빠지지 않게: 원장의 순번(seq)을 따라 「마지막으로 본 순번 뒤」를 다시 받는다.
/// 두 번 보내지 않게: 말마다 폰이 id 를 한 번 짓고, 재시도는 **같은 id·같은 내용**으로만 한다
/// — 서버는 그걸 처음 영수증으로 돌려준다.
library;

import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/foundation.dart';

import 'server.dart';

String newMessageId() {
  final r = math.Random.secure();
  return List.generate(24, (_) => r.nextInt(16).toRadixString(16)).join();
}

class NachoEvent {
  const NachoEvent({
    required this.seq,
    required this.kind,
    required this.atMs,
    this.id,
    this.text = '',
    this.task,
    this.message,
    this.state,
    this.note = '',
    this.notice,
    this.files = const [],
    this.surface = 'app',
    this.place,
  });

  factory NachoEvent.fromJson(Map<String, Object?> j) => NachoEvent(
    seq: (j['seq'] as num?)?.toInt() ?? 0,
    kind: j['kind'] as String? ?? '',
    atMs: (j['at_ms'] as num?)?.toInt() ?? 0,
    id: _str(j['id']),
    text: j['text'] as String? ?? '',
    task: _str(j['task']),
    message: _str(j['message']),
    state: _str(j['state']),
    note: j['note'] as String? ?? '',
    notice: _str(j['notice']),
    files: [
      for (final f in (j['files'] as List? ?? const [])) f.toString(),
    ],
    surface: j['surface'] as String? ?? 'app',
    place: _str(j['place']),
  );

  final int seq;

  /// message · status · progress · reply · notice
  final String kind;
  final int atMs;
  final String? id;
  final String text;
  final String? task;
  final String? message;
  final String? state;
  final String note;
  final String? notice;
  final List<String> files;

  /// 어느 화면에서 오간 말인가 — `app`(카사모바일) · `pet`(바탕화면 펫). 같은 나쵸의 두 화면이다.
  final String surface;
  final String? place;

  /// 앱에서 한 말이 아니면 「펫(미니)에서」처럼 출처를 단다.
  String? get origin => surface == 'pet' ? '펫${place == null ? '' : '($place)'}에서' : null;
}

String? _str(Object? v) {
  final s = v?.toString();
  return s == null || s.isEmpty ? null : s;
}

/// 접수 상태를 사람 말로. 서버가 준 값만 옮긴다.
String receiptLabel(String state) => switch (state) {
  'accepted' => '접수됨',
  'queued' => '앞 턴이 끝나면 이어서',
  'running' => '나쵸가 답하는 중',
  'answered' => '답함',
  'failed' => '실패',
  'refused' => '받지 않음',
  'interrupted' => '끊김 — 다시 보내 주세요',
  'restart' => '재시작 중 — 뜨면 장부에서 이어감',
  'sending' => '보내는 중',
  'unsent' => '보내지 못함',
  _ => state,
};

class NachoTaskCard {
  const NachoTaskCard(this.raw);

  final Map<String, Object?> raw;

  String get id => raw['id'] as String? ?? '';
  String get goal => raw['goal'] as String? ?? '';
  String get state => raw['state'] as String? ?? '';
  String get stateLabel => raw['state_label'] as String? ?? state;
  String get group => raw['group'] as String? ?? 'active';
  String get project => raw['project'] as String? ?? '기타';
  String get place => raw['place'] as String? ?? '';
  String get step => raw['step'] as String? ?? '';
  String get attention => raw['attention'] as String? ?? '';
  bool get paused => raw['paused'] == true;
  String get rev => raw['rev']?.toString() ?? '';
  int get updatedMs => (raw['updated_ms'] as num?)?.toInt() ?? 0;
  Map<String, Object?>? get student =>
      (raw['student'] as Map?)?.cast<String, Object?>();
}

class NachoTaskDetail extends NachoTaskCard {
  const NachoTaskDetail(super.raw);

  String get request => raw['request'] as String? ?? goal;
  String get result => raw['result'] as String? ?? '';
  String get blocked => raw['blocked'] as String? ?? '';
  List<String> get remaining => [
    for (final r in (raw['remaining'] as List? ?? const [])) r.toString(),
  ];
  Map<String, Object?>? get verify =>
      (raw['verify'] as Map?)?.cast<String, Object?>();
  Map<String, Object?>? get report =>
      (raw['report'] as Map?)?.cast<String, Object?>();
  Map<String, Object?>? get approval =>
      (raw['approval'] as Map?)?.cast<String, Object?>();
  List<Map<String, Object?>> get history => _maps(raw['history']);
  List<Map<String, Object?>> get hops => _maps(raw['hops']);
  String? get previewUrl =>
      _str(((raw['preview'] as Map?) ?? const {})['url']);
  bool get hasShot => ((raw['preview'] as Map?) ?? const {})['shot'] == true;
  bool get canDirect => raw['can_direct'] == true;
  List<NachoEvent> get events => [
    for (final e in _maps(raw['events'])) NachoEvent.fromJson(e),
  ];
}

List<Map<String, Object?>> _maps(Object? v) => [
  for (final m in (v as List? ?? const []))
    if (m is Map) m.cast<String, Object?>(),
];

/// 서버가 거절한 까닭. `message` 는 사람에게 그대로 보여도 되는 말이다.
class NachoError implements Exception {
  const NachoError(this.code, this.message, {this.status = 0, this.task});

  final String code;
  final String message;
  final int status;
  final NachoTaskCard? task;

  @override
  String toString() => message;
}

String _why(String code, Map<String, Object?> j) => switch (code) {
  'app_key_missing' => '나쵸 앱 창구가 아직 열리지 않았다(나쵸 쪽 키 설정 필요)',
  'nacho_key_missing' => '이 허브에 나쵸 키가 없다 — 허브 설정이 필요하다',
  'nacho_unconfigured' => '이 허브가 나쵸 자리를 모른다 — 허브 설정이 필요하다',
  'nacho_unreachable' => '허브가 나쵸에 닿지 못했다',
  'owner_only' || 'not_owner' => '주인 주소에서만 나쵸와 얘기할 수 있다',
  'stale_rev' => '그사이 작업이 바뀌었다 — 새 상태를 보고 다시 보내 주세요',
  'task_closed' => '이미 닫힌 작업이다',
  'task_elsewhere' => '이 작업은 지금 ${j['place'] ?? '다른 창구'}에 있다',
  'no_task' => '작업을 찾지 못했다',
  'id_conflict' => '같은 번호로 다른 말이 이미 접수됐다',
  'secret_like' => '비밀처럼 보이는 글은 앱 기록에 남기지 않는다',
  'too_long' => '글이 너무 길다',
  _ => '나쵸가 받지 않았다 ($code)',
};

class _Outgoing {
  _Outgoing(this.id, this.text, this.task, this.rev);

  final String id;
  final String text;
  final String? task;
  final String? rev;
  String state = 'sending';
  String error = '';
}

/// 나쵸 창구 하나(주인 주소 하나). 화면이 떠 있는 동안 원장을 이어 받는다.
class NachoDesk extends ChangeNotifier {
  NachoDesk(this.server);

  final Server server;
  final List<NachoEvent> events = [];
  final Set<int> _seen = {};
  final Map<String, _Outgoing> _outbox = {};
  int lastSeq = 0;
  bool online = false;
  String? problem;
  bool loaded = false;

  List<NachoTaskCard> tasks = const [];
  Map<String, int> groups = const {};
  String? tasksProblem;

  bool _running = false;
  int _gen = 0;
  bool _disposed = false;

  static const pollWait = 25;

  Future<void> start() async {
    if (_running) return;
    _running = true;
    final gen = ++_gen;
    await _catchUp();
    unawaited(loadTasks());
    unawaited(_loop(gen));
  }

  /// 앱이 뒤로 가면 멈춘다. 다시 오면 `start` 가 마지막 순번 뒤부터 잇는다.
  void stop() {
    _running = false;
    _gen++;
  }

  Future<void> _catchUp() async {
    try {
      final (status, j) = lastSeq == 0
          ? await server.nacho('events', query: {'tail': '200'})
          : await server.nacho(
              'events',
              query: {'after': '$lastSeq', 'limit': '500'},
            );
      if (status != 200) {
        _fail(_why(j['error'] as String? ?? '$status', j));
        return;
      }
      _absorb(j);
    } on ServerException catch (e) {
      _fail(e.message);
    }
  }

  Future<void> _loop(int gen) async {
    var backoff = 1;
    while (_running && gen == _gen && !_disposed) {
      try {
        final (status, j) = await server.nacho(
          'events',
          query: {'after': '$lastSeq', 'wait': '$pollWait', 'limit': '500'},
          timeout: const Duration(seconds: pollWait + 15),
        );
        if (gen != _gen) return;
        if (status != 200) {
          _fail(_why(j['error'] as String? ?? '$status', j));
          await Future<void>.delayed(Duration(seconds: backoff));
          backoff = math.min(backoff * 2, 30);
          continue;
        }
        backoff = 1;
        if (_absorb(j)) unawaited(loadTasks());
      } on ServerException catch (e) {
        if (gen != _gen) return;
        _fail(e.message);
        await Future<void>.delayed(Duration(seconds: backoff));
        backoff = math.min(backoff * 2, 30);
      }
    }
  }

  void _fail(String why) {
    online = false;
    problem = why;
    _notify();
  }

  /// 새 줄을 받았으면 true. 같은 순번은 두 번 안 넣는다(재접속이 겹쳐도).
  bool _absorb(Map<String, Object?> j) {
    var added = false;
    for (final raw in (j['events'] as List? ?? const [])) {
      if (raw is! Map) continue;
      final e = NachoEvent.fromJson(raw.cast<String, Object?>());
      if (e.seq <= 0 || !_seen.add(e.seq)) continue;
      events.add(e);
      added = true;
      if (e.kind == 'message' && e.id != null) _outbox.remove(e.id);
    }
    if (added) events.sort((a, b) => a.seq.compareTo(b.seq));
    final last = (j['last_seq'] as num?)?.toInt() ?? 0;
    lastSeq = math.max(lastSeq, math.max(last, events.isEmpty ? 0 : events.last.seq));
    online = true;
    problem = null;
    loaded = true;
    _notify();
    return added;
  }

  /// 그 말의 지금 상태 — 원장의 마지막 status, 아직 원장에 없으면 보내는 쪽 상태.
  String stateOf(String id) {
    final out = _outbox[id];
    if (out != null) return out.state;
    for (final e in events.reversed) {
      if (e.kind == 'status' && e.message == id) return e.state ?? 'accepted';
    }
    return 'accepted';
  }

  String noteOf(String id) {
    final out = _outbox[id];
    if (out != null) return out.error;
    for (final e in events.reversed) {
      if (e.kind == 'status' && e.message == id) return e.note;
    }
    return '';
  }

  /// 그 말이 도는 동안의 마지막 진행 한 줄.
  String? progressOf(String id) {
    for (final e in events.reversed) {
      if (e.kind == 'progress' && e.message == id) return e.text;
    }
    return null;
  }

  /// 아직 원장에 안 적힌 보낸 말(보내는 중·못 보냄).
  List<({String id, String text, String state, String error})> get outgoing => [
    for (final o in _outbox.values)
      (id: o.id, text: o.text, state: o.state, error: o.error),
  ];

  /// 말 하나를 보낸다. 끊기면 **같은 id** 로 다시 시도한다 — 서버가 처음 영수증을 돌려주니
  /// 두 번 접수되지 않는다. 서버가 거절하면(4xx) 그 까닭을 `NachoError` 로 올린다.
  Future<String> send(String text, {String? task, String? rev, String? id}) async {
    final mid = id ?? newMessageId();
    final out = _outbox[mid] ?? _Outgoing(mid, text, task, rev);
    out
      ..state = 'sending'
      ..error = '';
    _outbox[mid] = out;
    _notify();
    var wait = 1;
    for (var attempt = 0; attempt < 4; attempt++) {
      try {
        final (status, j) = await server.nacho(
          'messages',
          body: {
            'id': mid,
            'text': out.text,
            if (out.task != null) 'task': out.task,
            if (out.rev != null) 'rev': out.rev,
          },
        );
        if (status == 200 && j['ok'] == true) {
          // 원장에서 그 줄을 받을 때까지는 영수증 상태로 보인다.
          final rec = (j['receipt'] as Map?)?.cast<String, Object?>() ?? {};
          out.state = rec['state'] as String? ?? 'accepted';
          _notify();
          unawaited(_catchUp());
          return mid;
        }
        final code = j['error'] as String? ?? '$status';
        final card = (j['task'] as Map?)?.cast<String, Object?>();
        _outbox.remove(mid);
        _notify();
        throw NachoError(
          code,
          _why(code, j),
          status: status,
          task: card == null ? null : NachoTaskCard(card),
        );
      } on ServerException {
        await Future<void>.delayed(Duration(seconds: wait));
        wait *= 2;
      }
    }
    out
      ..state = 'unsent'
      ..error = '보내지 못했다 — 눌러서 다시 보내기(같은 말로 한 번만 접수된다)';
    _notify();
    return mid;
  }

  Future<void> retry(String id) async {
    final out = _outbox[id];
    if (out == null) return;
    await send(out.text, task: out.task, rev: out.rev, id: id);
  }

  Future<void> loadTasks() async {
    try {
      final (status, j) = await server.nacho('tasks');
      if (status != 200) {
        tasksProblem = _why(j['error'] as String? ?? '$status', j);
        _notify();
        return;
      }
      tasks = [
        for (final t in (j['tasks'] as List? ?? const []))
          if (t is Map) NachoTaskCard(t.cast<String, Object?>()),
      ];
      groups = {
        for (final e in ((j['groups'] as Map?) ?? const {}).entries)
          e.key.toString(): (e.value as num?)?.toInt() ?? 0,
      };
      tasksProblem = null;
      _notify();
    } on ServerException catch (e) {
      tasksProblem = e.message;
      _notify();
    }
  }

  Future<NachoTaskDetail> task(String id) async {
    final (status, j) = await server.nacho('tasks/$id');
    if (status != 200) {
      final code = j['error'] as String? ?? '$status';
      throw NachoError(code, _why(code, j), status: status);
    }
    return NachoTaskDetail(
      ((j['task'] as Map?) ?? const {}).cast<String, Object?>(),
    );
  }

  /// 작업 목록에 있는(대화 한 마디가 아닌) 일인가 — 답 아래 「작업 보기」를 달 때.
  NachoTaskCard? workOf(String? taskId) {
    if (taskId == null) return null;
    for (final t in tasks) {
      if (t.id == taskId) return t;
    }
    return null;
  }

  Uri shotUri(String taskId) => server.nachoUri('tasks/$taskId/shot');

  Uri fileUri(int seq, int index, {String? task}) => server.nachoUri(
    'files/$seq/$index',
    query: task == null ? null : {'task': task},
  );

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    stop();
    super.dispose();
  }
}
