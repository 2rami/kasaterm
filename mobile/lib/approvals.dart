import 'dart:async';
import 'dart:convert';
import 'dart:math';

import 'package:flutter/foundation.dart';

import 'relay_account.dart';

/// 이 앱 실행 동안만 메모리에 있는 원격 승인 열쇠. 키체인에도 디스크에도 두지 않는다
/// (docs/remote-approval.md — 결정은 사람이 누른 화면에서만 나간다).
final String approvalKey = () {
  final rng = Random.secure();
  return base64Url.encode(List.generate(32, (_) => rng.nextInt(256))).replaceAll('=', '');
}();

class ApprovalField {
  const ApprovalField(this.name, this.label, this.text);
  final String name;
  final String label;
  final String text;
}

/// 관문이 준 원격 승인 요청 하나(`/relay/approvals`). 글은 요청한 기기가 비밀만 가린 원문이다.
class Approval {
  const Approval({
    required this.id,
    required this.state,
    required this.machine,
    required this.device,
    required this.student,
    required this.pane,
    required this.cwd,
    required this.tool,
    required this.fields,
    required this.truncated,
    required this.created,
    required this.expires,
    required this.digest,
    this.byLabel,
    this.byKind,
    this.reason,
  });

  final String id;
  final String state;
  final String machine;
  final String device;
  final String student;
  final String pane;
  final String cwd;
  final String tool;
  final List<ApprovalField> fields;
  final bool truncated;
  final int created;
  final int expires;
  final String digest;
  final String? byLabel;
  final String? byKind;
  final String? reason;

  bool get pending => state == 'pending';

  /// 한 줄 — 알림 띠·목록. 첫 칸(명령·파일)의 첫 줄.
  String get headline {
    final first = fields.isEmpty ? '' : fields.first.text;
    final line = first.split('\n').first;
    return line.isEmpty ? tool : line;
  }

  /// 닫힌 요청이 어떻게 닫혔나.
  String get closedLine => switch (state) {
    'allowed' => '${byLabel ?? '다른 기기'}에서 허락했어요',
    'denied' => '${byLabel ?? '다른 기기'}에서 거절했어요',
    'expired' => '2분이 지나 원래 창으로 돌아갔어요',
    'cancelled' => reason == 'gone' ? '그 창이 닫혔어요' : '원래 창에서 답했어요',
    _ => '',
  };

  static Approval? fromJson(Object? raw) {
    if (raw is! Map) return null;
    String s(String k) => raw[k] is String ? raw[k] as String : '';
    int n(String k) => raw[k] is num ? (raw[k] as num).toInt() : 0;
    final id = s('id');
    final digest = s('digest');
    if (id.isEmpty || digest.isEmpty) return null;
    final by = raw['by'] is Map ? raw['by'] as Map : const {};
    return Approval(
      id: id,
      state: s('state'),
      machine: s('machine'),
      device: s('device'),
      student: s('student'),
      pane: s('pane'),
      cwd: s('cwd'),
      tool: s('tool'),
      fields: [
        for (final f in raw['fields'] is List ? raw['fields'] as List : const [])
          if (f is Map && f['text'] is String)
            ApprovalField('${f['name'] ?? ''}', '${f['label'] ?? f['name'] ?? ''}', f['text'] as String),
      ],
      truncated: raw['truncated'] == true,
      created: n('created'),
      expires: n('expires'),
      digest: digest,
      byLabel: by['label'] is String ? by['label'] as String : null,
      byKind: by['kind'] is String ? by['kind'] as String : null,
      reason: raw['reason'] is String ? raw['reason'] as String : null,
    );
  }
}

/// 계정의 원격 승인 요청을 앱이 앞에 있는 동안 긴 폴링으로 지켜본다. 다른 곳에서 닫히면 목록이 그 상태를 말한다.
class ApprovalCenter extends ChangeNotifier {
  ApprovalCenter({RelayAccountApi Function(AccountSession)? api, DateTime Function()? clock})
    : _api = api ?? ((s) => RelayAccountApi(s.origin, session: s)),
      _clock = clock ?? DateTime.now;

  static final ApprovalCenter instance = ApprovalCenter();
  static const _minRound = Duration(milliseconds: 300);

  final RelayAccountApi Function(AccountSession) _api;
  final DateTime Function() _clock;
  AccountSession? _session;
  bool _foreground = true;
  int _gen = 0;
  int? _rev;
  int _skew = 0;
  final Map<String, Approval> _items = {};
  final Set<String> _announced = {};

  /// 앱이 앞에 있을 때 새 요청이 오면 — 알림 띠를 세우는 자리.
  void Function(Approval)? onNew;

  AccountSession? get session => _session;
  List<Approval> get pending =>
      _items.values.where((a) => a.pending).toList()..sort((a, b) => a.created.compareTo(b.created));
  Approval? byId(String id) => _items[id];

  /// 관문 시계로 남은 시간 — 폰 시계가 틀려도 만료를 같은 순간에 본다.
  int remainingMs(Approval a) => a.expires - (_clock().millisecondsSinceEpoch + _skew);

  void bind(AccountSession? session) {
    if (identical(session, _session)) return;
    _session = session;
    _items.clear();
    _announced.clear();
    _rev = null;
    notifyListeners();
    _restart();
  }

  void setForeground(bool foreground) {
    if (_foreground == foreground) return;
    _foreground = foreground;
    _restart();
  }

  void _restart() {
    final gen = ++_gen;
    if (_session == null || !_foreground) return;
    unawaited(_loop(gen));
  }

  Future<void> _loop(int gen) async {
    final session = _session;
    if (session == null) return;
    final api = _api(session);
    var backoff = 2;
    try {
      var first = true;
      while (gen == _gen) {
        try {
          final asked = DateTime.now();
          final json = await api.approvals(since: first ? null : _rev, wait: first ? 0 : 25);
          if (gen != _gen) return;
          _apply(json, announce: true);
          // 관문이 붙들지 않고 바로 답하면(다른 계정의 변화로 판이 넘어갔을 때) 잠깐 쉰다 — 헛도는 고리를 막는다.
          if (!first && DateTime.now().difference(asked) < _minRound) {
            await Future<void>.delayed(_minRound);
          }
          first = false;
          backoff = 2;
        } on AccountException catch (e) {
          if (gen != _gen) return;
          if (e.status == 401 || e.status == 404) return;
          await Future<void>.delayed(Duration(seconds: backoff));
          backoff = min(backoff * 2, 15);
        }
      }
    } finally {
      api.close();
    }
  }

  /// 지금 목록을 한 번 받는다(알림을 누르고 들어왔을 때).
  Future<void> refresh() async {
    final session = _session;
    if (session == null) return;
    final api = _api(session);
    try {
      _apply(await api.approvals(), announce: false);
    } on AccountException {
      // 다음 긴 폴링이 다시 받는다.
    } finally {
      api.close();
    }
  }

  void _apply(Map<String, dynamic> json, {required bool announce}) {
    if (json['rev'] is num) _rev = (json['rev'] as num).toInt();
    if (json['now'] is num) _skew = (json['now'] as num).toInt() - _clock().millisecondsSinceEpoch;
    final fresh = <String, Approval>{};
    for (final raw in json['approvals'] is List ? json['approvals'] as List : const []) {
      final a = Approval.fromJson(raw);
      if (a != null) fresh[a.id] = a;
    }
    // 닫혀 목록에서 걷힌 요청은 마지막 모습을 남겨 둔다 — 열린 화면이 「어디서 닫혔나」를 말하게.
    for (final old in _items.values) {
      if (!fresh.containsKey(old.id) && old.pending) {
        fresh[old.id] = Approval.fromJson({..._raw(old), 'state': 'expired'})!;
      } else if (!fresh.containsKey(old.id)) {
        fresh[old.id] = old;
      }
    }
    _items
      ..clear()
      ..addAll(fresh);
    for (final a in pending) {
      if (_announced.add(a.id) && announce) onNew?.call(a);
    }
    notifyListeners();
  }

  /// 사람이 이 화면에서 누른 결정. 실패하면 [AccountException] — 이미 닫힌 요청이면 목록을 다시 받아 그 상태를 보인다.
  Future<Approval> decide(Approval a, bool allow) async {
    final session = _session;
    if (session == null) throw const AccountException('로그인이 풀렸어요.');
    final api = _api(session);
    try {
      final json = await api.decideApproval(a.id, a.digest, allow, approvalKey);
      final done = Approval.fromJson(json['approval']);
      if (done != null) {
        _items[done.id] = done;
        notifyListeners();
      }
      return done ?? a;
    } on AccountException catch (e) {
      if (e.code == 'already_closed' || e.code == 'expired' || e.code == 'not_found') {
        try {
          _apply(await api.approvals(), announce: false);
        } on AccountException {
          // 그대로 오류를 보인다.
        }
      }
      rethrow;
    } finally {
      api.close();
    }
  }

  @visibleForTesting
  void debugSeed(List<Approval> items, {int skew = 0}) {
    _items
      ..clear()
      ..addEntries(items.map((a) => MapEntry(a.id, a)));
    _skew = skew;
    notifyListeners();
  }
}

Map<String, Object?> _raw(Approval a) => {
  'id': a.id,
  'state': a.state,
  'machine': a.machine,
  'device': a.device,
  'student': a.student,
  'pane': a.pane,
  'cwd': a.cwd,
  'tool': a.tool,
  'fields': [
    for (final f in a.fields) {'name': f.name, 'label': f.label, 'text': f.text},
  ],
  'truncated': a.truncated,
  'created': a.created,
  'expires': a.expires,
  'digest': a.digest,
  'by': a.byLabel == null ? null : {'label': a.byLabel, 'kind': a.byKind},
  'reason': a.reason,
};
