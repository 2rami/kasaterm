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
    this.target,
    this.reply = 0,
    this.turn,
    this.mirror = false,
    this.notify = true,
    this.askedMs,
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
    target: _str(j['target']),
    reply: (j['reply'] as num?)?.toInt() ?? 0,
    turn: _str(j['turn']),
    mirror: j['mirror'] == true,
    notify: j['notify'] != false,
    askedMs: (j['asked_ms'] as num?)?.toInt(),
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

  /// 어느 창구에서 오간 말인가 — `app`(카사모바일) · `pet`(바탕화면 펫) · `discord`·`slack`(거노 DM).
  /// 거노 창구는 한 대화라, 다른 창구의 한 번도 이 원장에 옮겨 적힌다.
  final String surface;

  /// 사람이 읽는 창구 이름 — `디스코드 DM`·`슬랙 DM`, 펫은 기계 이름.
  final String? place;

  /// 펫 전달·연결 줄의 대상 펫(`kasapet:<기계>`). `reply` 는 전달 줄이 가리키는 답의 순번.
  final String? target;
  final int reply;

  /// 그 한 번의 이름표 — 재시도해도 같다.
  final String? turn;

  /// 다른 창구에서 끝난 한 번을 옮겨 적은 줄. 답이 끝난 뒤에 한꺼번에 적힌다.
  final bool mirror;

  /// false 면 알림(소리·배너·햅틱)을 내지 않는다 — 알림은 거노가 지금 쓰는 창구 하나에서만.
  final bool notify;

  /// 원래 창구에서 거노가 말한 시각. 원장은 답이 끝난 순서라, 두 창구에서 겹쳐 말하면 뒤바뀐다.
  final int? askedMs;

  /// 앱에서 한 말이 아니면 「디코 DM에서」처럼 출처를 단다.
  String? get origin => switch (surface) {
    'app' => null,
    'pet' => '펫${place == null ? '' : '($place)'}에서',
    'discord' => '디코 DM에서',
    'slack' => '슬랙 DM에서',
    _ => place == null ? null : '$place에서',
  };
}

/// 말풍선 순서. 옮겨 적힌 한 번은 답이 끝난 때 원장에 들어오므로, 거노가 말한 시각(`asked_ms`)
/// 자리로 당겨 그 앞뒤 대화 사이에 선다. 그 한 번의 답·상태 줄은 말 바로 뒤에 붙는다.
/// `asked_ms` 가 없는 원장은 순번 그대로다.
List<NachoEvent> timeline(List<NachoEvent> events) {
  if (!events.any((e) => e.askedMs != null)) return events;
  final asked = <String, int>{
    for (final e in events)
      if (e.kind == 'message' && e.id != null && e.askedMs != null)
        e.id!: e.askedMs!,
  };
  int keyOf(NachoEvent e) {
    if (e.kind == 'message') return e.askedMs ?? e.atMs;
    // 옮겨 적힌 한 번의 답·상태만 그 말 자리로 — 앱에서 한 말의 답은 제 시각에 선다.
    if (e.mirror && e.message != null) return asked[e.message] ?? e.atMs;
    return e.atMs;
  }

  final keyed = [for (final e in events) (keyOf(e), e)];
  keyed.sort((a, b) {
    final k = a.$1.compareTo(b.$1);
    return k != 0 ? k : a.$2.seq.compareTo(b.$2.seq);
  });
  return [for (final k in keyed) k.$2];
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

/// 펫 한 대 — 이어 볼 수 있는 바탕화면.
class NachoPet {
  const NachoPet(this.raw);

  final Map<String, Object?> raw;

  String get conv => raw['conv'] as String? ?? '';
  String get place => raw['place'] as String? ?? '펫';
  bool get alive => raw['alive'] == true;
  bool get linked => raw['linked'] == true;

  /// 기계 번호. 우편함 이름은 판의 라벨이라 바뀌어, 한 기계가 옛·새 이름 여럿으로 갈린다.
  String? get machine => _str(raw['machine']);

  /// 우편함을 끌어간 적이 있나 — 묻기만 하는 옛 판 펫은 떠 있어도 말풍선으로 못 받는다.
  bool get canReceive => raw['can_receive'] == true;
  int? get seenAgoS => (raw['seen_ago_s'] as num?)?.toInt();

  /// 떠 있는지를 짐작하지 않는다 — 마지막으로 다녀간 때를 그대로.
  String get status {
    final head = _presence;
    return canReceive ? head : '$head · 말풍선 받기 확인 안 됨(우편함을 끌어가는 펫 판 필요)';
  }

  String get _presence {
    if (alive) return '켜져 있음';
    final s = seenAgoS;
    if (s == null) return '다녀간 기록 없음';
    if (s < 3600) return '꺼져 있음 · ${(s / 60).ceil()}분 전까지';
    if (s < 86400) return '꺼져 있음 · ${(s / 3600).floor()}시간 전까지';
    return '꺼져 있음 · ${(s / 86400).floor()}일 전까지';
  }
}

/// 같은 기계의 옛·새 이름을 한 줄로. 남기는 것은 연결된 것 → 켜진 것 → 말풍선을 받는 것 →
/// 가장 최근에 다녀간 것 순. 기계 번호를 모르는 펫은 그대로 둔다(짐작해 묶지 않는다).
List<NachoPet> groupPets(List<NachoPet> pets) {
  int rank(NachoPet p) =>
      (p.linked ? 8 : 0) + (p.alive ? 4 : 0) + (p.canReceive ? 2 : 0);
  bool better(NachoPet a, NachoPet b) {
    final r = rank(a).compareTo(rank(b));
    if (r != 0) return r > 0;
    return (a.seenAgoS ?? 1 << 30) < (b.seenAgoS ?? 1 << 30);
  }

  final out = <NachoPet>[];
  final at = <String, int>{};
  for (final p in pets) {
    final m = p.machine;
    if (m == null) {
      out.add(p);
      continue;
    }
    final i = at[m];
    if (i == null) {
      at[m] = out.length;
      out.add(p);
    } else if (better(p, out[i])) {
      out[i] = p;
    }
  }
  return out;
}

/// 답 하나가 연결된 펫에 어떻게 갔나. 펫이 받아 간 영수증이 있을 때만 「표시됨」이다.
String deliveryLabel(String state, String? target) {
  final pet = target == null ? '펫' : '펫(${target.replaceFirst('kasapet:', '')})';
  return switch (state) {
    'delivered' => '$pet에도 표시됨',
    'queued' => '$pet 우편함 대기 — 펫이 받아 가면 표시',
    'expired' => '$pet이 받아 가기 전에 만료',
    'no_target' => '연결된 펫 없음 — 폰에만',
    'failed' => '$pet에 못 넣음',
    _ => '',
  };
}

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
  'pet_cannot_receive' => '이 펫은 아직 말풍선을 받을 수 없어요 — 우편함을 끌어가는 판의 펫을 골라 주세요',
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
  static const pageRows = 500;
  static const tailRows = 200;

  /// 한 번에 넘길 페이지 상한 — 끝없이 도는 서버 오류를 막는 안전판. 넘으면 다음 바퀴가 이어 받는다.
  static const maxDrainPages = 40;

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

  /// 검사용 — 그 커서에서 한 번 따라잡는다.
  @visibleForTesting
  Future<void> catchUpForTest({required int after}) {
    lastSeq = after;
    loaded = true;
    return _catchUp();
  }

  /// 처음 붙을 때만 최근 [tailRows] 줄(그 앞은 일부러 안 받는다). 그 뒤로는 커서 뒤를 **다 받을 때까지**
  /// 페이지를 넘긴다 — 끊겨 있던 동안 쌓인 줄이 한 페이지를 넘어도 빠지지 않게.
  Future<void> _catchUp() async {
    try {
      var first = !loaded && lastSeq == 0;
      for (var page = 0; page < maxDrainPages; page++) {
        final (status, j) = first
            ? await server.nacho('events', query: {'tail': '$tailRows'})
            : await server.nacho(
                'events',
                query: {'after': '$lastSeq', 'limit': '$pageRows'},
              );
        first = false;
        if (status != 200) {
          _fail(_why(j['error'] as String? ?? '$status', j));
          return;
        }
        if (!_absorb(j).more) return;
      }
    } on ServerException catch (e) {
      _fail(e.message);
    }
  }

  Future<void> _loop(int gen) async {
    var backoff = 1;
    var more = false;
    while (_running && gen == _gen && !_disposed) {
      try {
        // 더 받을 것이 남았으면 기다리지 않고 바로 다음 페이지를 받는다.
        final (status, j) = await server.nacho(
          'events',
          query: {'after': '$lastSeq', 'wait': more ? '0' : '$pollWait', 'limit': '$pageRows'},
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
        final got = _absorb(j);
        more = got.more;
        if (got.added) unawaited(loadTasks());
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

  /// 한 페이지를 넣는다. 같은 순번은 두 번 안 넣는다(재접속이 겹쳐도).
  ///
  /// ★커서는 **이 페이지에서 실제로 받은 마지막 순번**까지만 전진한다. 서버가 알려 주는 원장 끝
  /// (`head_seq`, 옛 서버의 `last_seq`)으로 뛰면 한 번에 못 받은 나머지가 통째로 빠진다(2026-09-25 검수:
  /// 500개를 넘게 밀린 채 다시 붙으면 첫 500개 뒤로 끝까지 건너뛰었다).
  ({bool added, bool more}) _absorb(Map<String, Object?> j) {
    var added = false;
    var pageLast = 0;
    final page = j['events'] as List? ?? const [];
    for (final raw in page) {
      if (raw is! Map) continue;
      final e = NachoEvent.fromJson(raw.cast<String, Object?>());
      pageLast = math.max(pageLast, e.seq);
      if (e.seq <= 0 || !_seen.add(e.seq)) continue;
      events.add(e);
      added = true;
      if (e.kind == 'message' && e.id != null) _outbox.remove(e.id);
    }
    if (added) events.sort((a, b) => a.seq.compareTo(b.seq));
    lastSeq = math.max(lastSeq, pageLast);
    // 더 받을 것이 있나 — 서버가 말해 주면 그대로, 말이 없는 옛 서버면 페이지가 꽉 찼는지로.
    final hasMore = j['has_more'];
    final more = hasMore is bool ? hasMore : page.length >= pageRows;
    online = true;
    problem = null;
    loaded = true;
    _notify();
    return (added: added, more: more);
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

  /// 서버가 받아 두었고 아직 끝나지 않은 상태 — 이 동안 나쵸 자리에 「답하는 중」 점을 띄운다.
  static const answeringStates = {'accepted', 'queued', 'running'};

  /// 나쵸가 지금 답을 만들고 있는 말의 id. 없으면 null — 점은 이 하나로만 띄운다.
  ///
  /// 첫 답(`reply`)이 오거나 끝 상태(답함·실패·거절·끊김·재시작)가 되면 내린다. 나쵸는 말을 받은
  /// 차례대로 하나씩 도므로, 뒤에 보낸 말이 이미 답을 받았으면 그 앞의 말도 끝난 것이다 — 서버가
  /// 끝 상태를 못 적고 죽은 옛 말이 점을 영영 붙잡지 않게 한다.
  String? get awaiting {
    for (final o in _outbox.values) {
      if (answeringStates.contains(o.state)) return o.id;
    }
    final replied = <String>{
      for (final e in events)
        if (e.kind == 'reply' && e.message != null) e.message!,
    };
    for (final e in events.reversed) {
      // 옮겨 적힌 줄은 다른 창구에서 이미 끝난 한 번이라 이 폰의 기다림과 무관하다.
      if (e.kind != 'message' || e.id == null || e.mirror) continue;
      final id = e.id!;
      if (replied.contains(id)) return null;
      return answeringStates.contains(stateOf(id)) ? id : null;
    }
    return null;
  }

  /// 그 말이 도는 동안의 마지막 진행 한 줄.
  String? progressOf(String id) {
    for (final e in events.reversed) {
      if (e.kind == 'progress' && e.message == id) return e.text;
    }
    return null;
  }

  /// 그 답(순번)의 펫 전달 — 원장의 마지막 deliver 줄. 없으면 null.
  ({String state, String? target})? deliveryOf(int replySeq) {
    for (final e in events.reversed) {
      if (e.kind == 'deliver' && e.reply == replySeq) {
        return (state: e.state ?? '', target: e.target);
      }
    }
    return null;
  }

  /// 지금 연결된 펫(원장의 마지막 연결 줄). 서버가 정본이라 목록은 `pets()` 로 다시 읽는다.
  String? get linkedPet {
    for (final e in events.reversed) {
      if (e.kind == 'notice' && e.notice == 'link') return e.target;
    }
    return null;
  }

  Future<(String?, List<NachoPet>)> pets() async {
    final (status, j) = await server.nacho('pets');
    if (status != 200) {
      final code = j['error'] as String? ?? '$status';
      throw NachoError(code, _why(code, j), status: status);
    }
    return (
      _str(j['linked']),
      groupPets([
        for (final p in (j['pets'] as List? ?? const []))
          if (p is Map) NachoPet(p.cast<String, Object?>()),
      ]),
    );
  }

  /// 이 펫 한 대를 잇는다. null 이면 끊는다.
  Future<void> linkPet(String? conv) async {
    final (status, j) = await server.nacho('pets/link', body: {'conv': conv ?? ''});
    if (status != 200) {
      final code = j['error'] as String? ?? '$status';
      throw NachoError(code, code == 'no_pet' ? '그 펫은 다녀간 기록이 없어 이을 수 없다' : _why(code, j), status: status);
    }
    unawaited(_catchUp());
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
