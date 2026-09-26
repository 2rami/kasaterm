/// 작업 모드(정리·조율)와 권한 표. 정본은 나쵸다 — `GET/POST /api/app/work-mode`·
/// `GET /api/app/capabilities`(나쵸 desk-api.md). 폰은 허브 중계(`nacho/app/…`)로 PC 와 같은
/// 창구를 부르고, 모드 설명·권한 표 문구도 나쵸가 준 것을 그대로 싣는다.
///
/// 모드는 권한이 아니다. 바꿔도 확인 규칙·위임 범위·도는 일은 그대로다. 여기서 모드를 짐작하거나
/// 저절로 바꾸지 않는다 — 쓰기는 사람이 고를 때 한 번, 그때 본 rev 와 새 nonce 로만 한다.
library;

import 'dart:async';

import 'package:flutter/foundation.dart';

import 'nacho.dart';
import 'server.dart';

enum WorkMode {
  organize('정리', '터미널을 직접 볼 때. 지금 창의 일을 정리해 보여 주고, 나쵸는 창에 먼저 끼어들지 않아요.'),
  coordinate('조율', '나쵸에게 맡길 때. 모든 기기의 일을 한 판에, 사람 차례가 맨 위예요.');

  const WorkMode(this.label, this.blurb);

  final String label;

  /// 나쵸가 기능 안내를 안 줄 때만 쓰는 한 줄.
  final String blurb;

  /// 나쵸가 쓰는 두 이름만 받는다. 모르는 이름을 가까운 모드로 짐작하지 않는다.
  static WorkMode? parse(Object? wire) => switch (wire) {
    'organize' => WorkMode.organize,
    'coordinate' => WorkMode.coordinate,
    _ => null,
  };
}

/// 나쵸가 적은 모드 한 판. `rev` 는 나쵸의 정수 — 쓸 때 본 그대로 돌려준다.
class ModeState {
  const ModeState({
    required this.mode,
    required this.rev,
    this.changedAtMs,
    this.changedBy,
  });

  final WorkMode mode;
  final int rev;
  final int? changedAtMs;
  final String? changedBy;

  static ModeState? fromJson(Object? j) {
    if (j is! Map) return null;
    final mode = WorkMode.parse(j['mode']);
    final rev = j['rev'];
    if (mode == null || rev is! num) return null;
    return ModeState(
      mode: mode,
      rev: rev.toInt(),
      changedAtMs: (j['changed_at_ms'] as num?)?.toInt(),
      changedBy: j['changed_by'] as String?,
    );
  }
}

class ModeInfo {
  const ModeInfo({
    required this.mode,
    required this.label,
    required this.summary,
    this.effects = const [],
  });

  final WorkMode mode;
  final String label;
  final String summary;
  final List<String> effects;
}

/// 권한 표의 동작 하나와 그 확인 방식(`how`: none·button·card·approval).
class TierAction {
  const TierAction({required this.id, required this.label, required this.how});

  final String id;
  final String label;
  final String how;

  /// PC 작업 탭과 같은 말.
  String get howWord => switch (how) {
    'none' => '묻지 않음',
    'button' => '확인 단추',
    'card' => '갈림길 카드',
    'approval' => '승인(범위·해시·만료·1회)',
    _ => '확인 방식 미확인',
  };
}

class Tier {
  const Tier({
    required this.tier,
    required this.label,
    required this.rule,
    this.actions = const [],
  });

  final String tier;
  final String label;
  final String rule;
  final List<TierAction> actions;
}

class ApprovalCaps {
  const ApprovalCaps({
    required this.enabled,
    this.httpActions = const [],
    this.decideInApp = false,
    this.decideVia = '',
  });

  final bool enabled;
  final List<String> httpActions;
  final bool decideInApp;
  final String decideVia;
}

/// 나쵸 기능 안내. 모르는 모드는 버리고, 무제한 모드 여부는 나쵸가 말한 것만 싣는다.
class Capabilities {
  const Capabilities({
    this.approvals,
    this.modes = const [],
    this.tiers = const [],
    this.unlimitedMode,
    this.notes = const [],
  });

  final ApprovalCaps? approvals;
  final List<ModeInfo> modes;
  final List<Tier> tiers;
  final bool? unlimitedMode;
  final List<String> notes;

  /// 동작 id 의 사람 이름 — 권한 표에 있으면 그 이름, 없으면 id 그대로.
  String actionLabel(String id) {
    for (final t in tiers) {
      for (final a in t.actions) {
        if (a.id == id && a.label.isNotEmpty) return a.label;
      }
    }
    return id;
  }

  ModeInfo? info(WorkMode mode) {
    for (final m in modes) {
      if (m.mode == mode) return m;
    }
    return null;
  }

  static Capabilities fromJson(Map<String, Object?> j) {
    List<String> strings(Object? v) => [
      for (final s in (v as List? ?? const []))
        if (s is String && s.trim().isNotEmpty) s,
    ];
    final a = j['approvals'];
    final wm = j['work_mode'];
    final policy = j['policy'] is Map ? j['policy'] as Map : const {};
    return Capabilities(
      approvals: a is Map
          ? ApprovalCaps(
              enabled: a['enabled'] == true,
              httpActions: strings(a['http_actions']),
              decideInApp: a['decide_in_app'] == true,
              decideVia: a['decide_via'] as String? ?? '',
            )
          : null,
      modes: [
        for (final m in (wm is Map ? wm['modes'] as List? : null) ?? const [])
          if (m is Map && WorkMode.parse(m['mode']) != null)
            ModeInfo(
              mode: WorkMode.parse(m['mode'])!,
              label: m['label'] as String? ?? '',
              summary: m['summary'] as String? ?? '',
              effects: strings(m['effects']),
            ),
      ],
      tiers: [
        for (final t in (policy['tiers'] as List? ?? const []))
          if (t is Map)
            Tier(
              tier: t['tier'] as String? ?? '',
              label: t['label'] as String? ?? '',
              rule: t['rule'] as String? ?? '',
              actions: [
                for (final x in (t['actions'] as List? ?? const []))
                  if (x is Map)
                    TierAction(
                      id: x['id'] as String? ?? '',
                      label: x['label'] as String? ?? '',
                      how: x['how'] as String? ?? '',
                    )
                  else if (x is String)
                    TierAction(id: x, label: x, how: ''),
              ],
            ),
      ],
      unlimitedMode: policy['unlimited_mode'] as bool?,
      notes: strings(policy['notes']),
    );
  }
}

/// 나쵸가 이 경로를 모르는 판(aiohttp 기본 404) — 오류어가 없거나 not_found 뿐이다.
bool isOldNacho(int status, Map<String, Object?> j) {
  final error = j['error'];
  return status == 404 && (error == null || error == 'not_found');
}

String newNonce() => newMessageId();

/// 폰의 작업 모드 창구. 읽기는 작업판을 열거나 당겨 새로 할 때, 쓰기는 사람이 모드를 고를 때 한 번.
class WorkModeDesk extends ChangeNotifier {
  WorkModeDesk(this.server) : demo = false;

  /// 검사·시뮬레이터용 예시. 나쵸에 묻지도 쓰지도 않고, 화면이 예시라고 밝힌다.
  WorkModeDesk.demo({this.state, this.caps}) : server = null, demo = true;

  final Server? server;
  final bool demo;

  ModeState? state;

  /// 나쵸가 작업 모드를 모른다(옛 나쵸). 모드를 지어내 싣지 않는다.
  bool oldNacho = false;

  /// 모드를 읽지 못한 까닭.
  String? problem;
  Capabilities? caps;
  String? capsProblem;
  bool writing = false;

  /// 방금 쓰기의 결과 — 그사이 바뀜·실패.
  String? notice;
  bool _disposed = false;

  /// 지금 고를 수 없으면 그 까닭. null 이면 고를 수 있다.
  String? get writeBlock {
    if (demo) return '예시 데이터라 바꿀 수 없어요';
    if (writing) return '나쵸에 적는 중이에요';
    if (oldNacho) return '이 나쵸는 아직 작업 모드를 몰라요 — 나쵸를 새 판으로 올려야 바꿀 수 있어요';
    if (state == null) return problem ?? '나쵸가 지금 모드를 아직 알려 주지 않았어요';
    return null;
  }

  /// 모드 이름 — 나쵸가 준 것, 없으면 앱의 이름.
  String labelOf(WorkMode mode) {
    final label = caps?.info(mode)?.label ?? '';
    return label.isEmpty ? mode.label : label;
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  Future<void> load() async {
    final s = server;
    if (s == null) return;
    await Future.wait([_loadMode(s), _loadCaps(s)]);
  }

  Future<void> _loadMode(Server s) async {
    try {
      final (status, j) = await s.nacho('work-mode');
      if (status == 200 && ModeState.fromJson(j['work_mode']) != null) {
        state = ModeState.fromJson(j['work_mode']);
        oldNacho = false;
        problem = null;
      } else {
        state = null;
        oldNacho = isOldNacho(status, j);
        problem = oldNacho
            ? '이 나쵸는 아직 작업 모드를 몰라요'
            : nachoWhy(j['error'] as String? ?? '$status', j);
      }
    } on ServerException catch (e) {
      state = null;
      problem = e.message;
    }
    _notify();
  }

  Future<void> _loadCaps(Server s) async {
    try {
      final (status, j) = await s.nacho('capabilities');
      if (status == 200) {
        caps = Capabilities.fromJson(j);
        capsProblem = null;
      } else {
        caps = null;
        capsProblem = isOldNacho(status, j)
            ? '이 나쵸는 아직 기능 안내를 주지 않아요 — 권한 표를 모릅니다'
            : nachoWhy(j['error'] as String? ?? '$status', j);
      }
    } on ServerException catch (e) {
      caps = null;
      capsProblem = e.message;
    }
    _notify();
  }

  /// 사람이 고른 모드를 나쵸에 적는다. 같은 모드이거나 고를 수 없으면 아무것도 보내지 않는다.
  Future<void> choose(WorkMode mode) async {
    final s = server;
    final seen = state;
    if (s == null || writeBlock != null || seen == null || seen.mode == mode) {
      return;
    }
    writing = true;
    notice = null;
    _notify();
    try {
      final (status, j) = await s.nacho(
        'work-mode',
        body: {'mode': mode.name, 'rev': seen.rev, 'nonce': newNonce()},
      );
      final next = ModeState.fromJson(j['work_mode']);
      if (status == 200 && next != null) {
        state = next;
      } else if (status == 409 && j['error'] == 'stale_rev') {
        if (next != null) state = next;
        notice = '그사이 다른 곳에서 모드가 바뀌었어요 — 지금 모드를 보고 다시 골라 주세요';
      } else {
        notice =
            '모드를 바꾸지 못했어요: ${nachoWhy(j['error'] as String? ?? '$status', j)}';
      }
    } on ServerException catch (e) {
      // 적혔는지 모른다 — 다시 보내지 않고 나쵸가 적은 판을 다시 읽는다.
      notice = '결과를 확인하지 못했어요(${e.message}) — 나쵸가 적은 모드를 다시 읽어요';
      writing = false;
      await _loadMode(s);
      return;
    }
    writing = false;
    _notify();
  }

  @override
  void dispose() {
    _disposed = true;
    super.dispose();
  }
}

/// 예시 판의 모드·권한 — 나쵸 고정 자료(capabilities.implemented.json)의 모양을 따른다.
WorkModeDesk demoModes() => WorkModeDesk.demo(
  state: const ModeState(
    mode: WorkMode.coordinate,
    rev: 3,
    changedAtMs: 1790449200000,
    changedBy: 'app:desktop',
  ),
  caps: const Capabilities(
    approvals: ApprovalCaps(
      enabled: true,
      httpActions: ['kasaterm_restart'],
      decideVia: 'owner_dm_button',
    ),
    modes: [
      ModeInfo(
        mode: WorkMode.organize,
        label: '정리',
        summary: '사람이 터미널에서 직접 일한다 — 나쵸는 정리만 한다',
        effects: [
          '지금 작업·바뀐 것·다음 일·막힘·검증을 정리해 보여 준다',
          '자동으로 도는 턴은 학생 창에 글을 넣거나 새 학생을 띄우지 않는다',
          '도는 학생을 멈추거나 끊지 않는다',
        ],
      ),
      ModeInfo(
        mode: WorkMode.coordinate,
        label: '조율',
        summary: '나쵸에게 맡긴다 — 학생·기기에 나누고 검증·보고한다',
        effects: ['맡긴 일을 학생과 기기에 나눈다', '사람 차례(승인·질문)가 맨 위에 선다'],
      ),
    ],
    tiers: [
      Tier(
        tier: 'delegated',
        label: '맡긴 범위 안 — 묻지 않는다',
        rule: '조회와 되돌릴 수 있는 작업은 맡긴 범위 안에서 묻지 않고 한다.',
        actions: [
          TierAction(id: 'read', label: '조회·읽기·로그 확인', how: 'none'),
          TierAction(id: 'code_edit', label: '코드 고치기·검사 돌리기', how: 'none'),
          TierAction(id: 'local_commit', label: '로컬 커밋', how: 'none'),
        ],
      ),
      Tier(
        tier: 'confirm_once',
        label: '되돌리기 어려운 일 — 범위를 묶어 한 번 확인',
        rule: '대상·범위·만료를 묶은 확인을 한 번 받는다. 받은 확인은 그 범위에 한 번만 쓴다.',
        actions: [
          TierAction(id: 'merge_pr', label: '머지', how: 'button'),
          TierAction(
            id: 'kasaterm_restart',
            label: '카사텀 앱 재시작',
            how: 'approval',
          ),
          TierAction(id: 'install', label: '설치', how: 'card'),
          TierAction(id: 'delete', label: '삭제', how: 'card'),
          TierAction(id: 'deploy', label: '배포', how: 'card'),
          TierAction(id: 'permission', label: '권한 넓히기', how: 'card'),
        ],
      ),
    ],
    unlimitedMode: false,
    notes: ['확인 없이 모든 도구를 쓰는 모드는 없다', '모드를 바꿔도 이 표는 그대로다'],
  ),
);
