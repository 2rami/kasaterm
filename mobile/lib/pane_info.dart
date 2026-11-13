/// Info 「지금 보는 칸」 카드 — 데스크톱 `info_focus.rs` 의 `FocusCard`·`card_lines` 폰판. 사실은 원본 앱이
/// `/term/pane-info` 로 짓고(거울 기기가 받는 것과 같은 답), 여기서는 줄만 같은 규칙으로 세운다.
library;

import 'shell_blocks.dart' show tildePath;

const paneInfoSchema = 'kasa.pane-info.v1';

/// 최근 도구는 셋까지(데스크톱 `RECENT_SHOWN`).
const _recentShown = 3;

String _s(Map j, String k) => j[k] is String ? j[k] as String : '';
int _i(Map j, String k) => j[k] is num ? (j[k] as num).toInt() : 0;
List<Map> _list(Map j, String k) => [for (final v in (j[k] is List ? j[k] as List : const [])) if (v is Map) v];

class FocusTool {
  const FocusTool({required this.tool, this.label = '', this.agent = false, this.ageMs = 0, this.tookMs, this.error = false});
  factory FocusTool.fromJson(Map j) => FocusTool(
        tool: _s(j, 'tool'),
        label: _s(j, 'label'),
        agent: j['agent'] == true,
        ageMs: _i(j, 'age_ms'),
        tookMs: j['took_ms'] is num ? (j['took_ms'] as num).toInt() : null,
        error: j['error'] == true,
      );
  final String tool;
  final String label;
  final bool agent;
  final int ageMs;
  final int? tookMs;
  final bool error;
}

class FocusTask {
  const FocusTask({required this.kind, this.label = '', this.status = '', this.ageMs = 0});
  factory FocusTask.fromJson(Map j) =>
      FocusTask(kind: _s(j, 'kind'), label: _s(j, 'label'), status: _s(j, 'status'), ageMs: _i(j, 'age_ms'));
  final String kind;
  final String label;
  final String status;
  final int ageMs;
}

class FocusFacts {
  const FocusFacts({
    this.state = '',
    this.tools = const [],
    this.recent = const [],
    this.background = const [],
    this.contextTokens,
    this.contextWindow,
    this.contextPercent,
    this.askTool,
    this.askPreview = '',
    this.askAgeMs = 0,
  });
  factory FocusFacts.fromJson(Map j) {
    final c = j['context'] is Map ? j['context'] as Map : null;
    final a = j['ask'] is Map ? j['ask'] as Map : null;
    int? n(Object? v) => v is num ? v.toInt() : null;
    return FocusFacts(
      state: _s(j, 'state'),
      tools: [for (final t in _list(j, 'tools')) FocusTool.fromJson(t)],
      recent: [for (final t in _list(j, 'recent')) FocusTool.fromJson(t)],
      background: [for (final t in _list(j, 'background')) FocusTask.fromJson(t)],
      contextTokens: n(c?['tokens']),
      contextWindow: n(c?['window']),
      contextPercent: c?['percent'] is num ? (c!['percent'] as num).toDouble() : null,
      askTool: a == null ? null : _s(a, 'tool'),
      askPreview: a == null ? '' : _s(a, 'preview'),
      askAgeMs: a == null ? 0 : _i(a, 'age_ms'),
    );
  }

  /// `working`·`resting`·`permission`·`question`·`compacting`.
  final String state;
  final List<FocusTool> tools;

  /// 끝난 도구, 최근 것부터.
  final List<FocusTool> recent;
  final List<FocusTask> background;
  final int? contextTokens;
  final int? contextWindow;
  final double? contextPercent;

  /// 승인 요청이 없으면 null.
  final String? askTool;
  final String askPreview;
  final int askAgeMs;
}

class FocusPort {
  const FocusPort({required this.port, this.name = '', this.site = ''});
  final int port;

  /// 그 포트를 쥔 프로세스 이름.
  final String name;

  /// 표준 서비스 이름이나 그 서버가 응답한 `<title>`.
  final String site;
}

class FocusCard {
  const FocusCard({
    required this.pane,
    this.harness = '',
    this.shell = '',
    this.program = '',
    this.cwd = '',
    this.model = '',
    this.effort = '',
    this.facts,
    this.ports = const [],
  });
  factory FocusCard.fromJson(Map j) => FocusCard(
        pane: _s(j, 'pane'),
        harness: _s(j, 'harness'),
        shell: _s(j, 'shell'),
        program: _s(j, 'program'),
        cwd: _s(j, 'cwd'),
        model: _s(j, 'model'),
        effort: _s(j, 'effort'),
        facts: j['facts'] is Map ? FocusFacts.fromJson(j['facts'] as Map) : null,
        ports: [
          for (final p in _list(j, 'ports'))
            if (p['port'] is num) FocusPort(port: (p['port'] as num).toInt(), name: _s(p, 'name'), site: _s(p, 'site')),
        ],
      );

  final String pane;

  /// `claude`·`codex`·`agy` — 셸뿐이면 빈 값.
  final String harness;
  final String shell;

  /// 에이전트가 아닌 칸에서 셸 위에 지금 도는 프로그램.
  final String program;
  final String cwd;
  final String model;
  final String effort;

  /// 연결 mod 가 알린 지금(claude 칸만).
  final FocusFacts? facts;
  final List<FocusPort> ports;
}

/// `/term/pane-info` 한 번의 답. 신원이 안 맞거나 옛 데스크톱이면 [error].
class PaneInfoAnswer {
  const PaneInfoAnswer({this.card, this.seq, this.error});
  factory PaneInfoAnswer.fromJson(Object? body) {
    if (body is! Map || body['schema'] != paneInfoSchema) return const PaneInfoAnswer(error: 'update_needed');
    if (body['ok'] != true) return PaneInfoAnswer(error: _s(body, 'error').isEmpty ? 'unknown' : _s(body, 'error'));
    final card = body['card'];
    return PaneInfoAnswer(card: card is Map ? FocusCard.fromJson(card) : null, seq: _s(body, 'seq'));
  }
  final FocusCard? card;
  final String? seq;
  final String? error;
}

/// 못 읽은 까닭을 사람 말로 — 데스크톱 거울 카드(`info_focus.rs remote_answer`)와 같은 말. 폰에겐 원본이 데스크톱이다.
String paneInfoIssue(String code) => switch (code) {
      'update_needed' => '데스크톱 앱이 옛 판이라 이 칸의 정보를 못 받아요',
      'source_changed' || 'pane_gone' => '칸이 바뀌었어요 — 다시 읽는 중이에요',
      'unreachable' => '데스크톱에 닿지 않아요 — 다시 잇는 중이에요',
      _ => '데스크톱이 이 칸의 정보를 못 내줬어요',
    };

sealed class FocusLine {
  const FocusLine();
}

/// 카드 머리 — 칸 종류와 오른쪽의 칸 id.
class FocusHead extends FocusLine {
  const FocusHead(this.title, this.note);
  final String title;
  final String note;
}

class FocusText extends FocusLine {
  const FocusText(this.text, {this.warn = false});
  final String text;
  final bool warn;
}

/// 「이름 · 값」 한 줄. 여러 줄인 칸은 첫 줄만 이름을 단다(나머지는 빈 이름). 값이 null 이면 흐린 「—」.
class FocusKv extends FocusLine {
  const FocusKv(this.name, this.value, {this.tip = '', this.warn = false});
  final String name;
  final String? value;
  final String tip;
  final bool warn;
}

/// 누르는 줄 — 경로(폰은 그 기기 폴더를 못 여니 경로를 복사)·포트(개발 서버 화면으로 연다).
class FocusLink extends FocusLine {
  const FocusLink(this.name, this.value, {this.path, this.port});
  final String name;
  final String value;
  final String? path;
  final int? port;
}

String focusSeconds(int s) {
  if (s < 60) return '$s초';
  if (s < 3600) return '${s ~/ 60}분';
  final h = s ~/ 3600, m = (s % 3600) ~/ 60;
  return m == 0 ? '$h시간' : '$h시간 $m분';
}

String _took(int ms) => ms < 1000 ? '${(ms / 1000).toStringAsFixed(1)}초' : focusSeconds(ms ~/ 1000);

String _tokens(int n) {
  if (n >= 1000000) return n % 1000000 == 0 ? '${n ~/ 1000000}M' : '${(n / 1000000).toStringAsFixed(1)}M';
  if (n >= 1000) return '${n ~/ 1000}k';
  return '$n';
}

String _harnessLabel(String h) => switch (h) {
      'claude' => 'Claude',
      'codex' => 'Codex',
      'agy' => 'Antigravity',
      _ => h,
    };

String _taskKind(String k) => switch (k) {
      'subagent' => '서브에이전트',
      'shell' => '셸',
      'monitor' => '모니터',
      'workflow' => '워크플로',
      _ => k,
    };

String _toolLine(FocusTool t, String elapsed) {
  final head = t.agent ? '서브 · ${t.tool}' : t.tool;
  return t.label.isEmpty ? '$head · $elapsed' : '$head · ${t.label} · $elapsed';
}

/// 카드의 줄들 — 맨 위 머리, 그 아래 「이름 · 값」 표. [elapsedMs] 는 답을 받은 뒤 흐른 시간(나이에 더한다),
/// [device] 는 다른 기기 칸이면 그 기기 이름.
List<FocusLine> focusLines(
  FocusCard? card, {
  String? issue,
  String? device,
  int elapsedMs = 0,
  bool altScreen = false,
}) {
  int age(int ms) => (ms + elapsedMs) ~/ 1000;
  if (card == null) {
    return [
      FocusHead('지금 보는 칸', device ?? ''),
      FocusText(issue ?? '보고 있는 칸을 읽는 중이에요', warn: issue != null),
    ];
  }
  final facts = card.facts;
  final String kind;
  if (card.harness.isNotEmpty) {
    kind = _harnessLabel(card.harness);
  } else if (altScreen && card.program.isNotEmpty) {
    kind = '전체 화면 · ${card.program}';
  } else if (card.program.isNotEmpty) {
    kind = '${card.shell} · ${card.program} 실행 중';
  } else {
    kind = card.shell.isEmpty ? '셸' : '셸 · ${card.shell}';
  }
  final lines = <FocusLine>[FocusHead(kind, device == null ? card.pane : '$device · ${card.pane}')];
  if (issue != null) lines.add(FocusText(issue, warn: true));
  if (facts != null) {
    final (state, warn) = switch (facts.state) {
      'permission' => ('승인 기다림', true),
      'question' => ('답 기다림', true),
      'compacting' => ('압축 중', false),
      'working' => ('일하는 중', false),
      _ => ('쉼', false),
    };
    lines.add(FocusKv('상태', state, warn: warn));
  }
  if (card.cwd.isNotEmpty) lines.add(FocusLink('경로', tildePath(card.cwd), path: card.cwd));
  if (card.model.isNotEmpty) {
    lines.add(FocusKv('모델', card.effort.isEmpty ? card.model : '${card.model} · ${card.effort}',
        tip: card.effort.isEmpty ? '' : '추론 강도 ${card.effort}'));
  }
  if (facts != null) {
    final parts = [
      if (facts.contextPercent != null) '${facts.contextPercent!.toStringAsFixed(0)}%',
      if (facts.contextTokens != null && facts.contextWindow != null)
        '${_tokens(facts.contextTokens!)} / ${_tokens(facts.contextWindow!)}',
    ];
    if (parts.isNotEmpty) {
      lines.add(FocusKv('문맥', parts.join(' · '),
          tip: '이 대화가 쓴 문맥 창', warn: (facts.contextPercent ?? 0) >= 80));
    }
    if (facts.askTool != null) {
      final value = facts.askPreview.isEmpty ? facts.askTool! : '${facts.askTool} · ${facts.askPreview}';
      lines.add(FocusKv('승인', '$value · ${focusSeconds(age(facts.askAgeMs))}', tip: facts.askPreview, warn: true));
    }
    for (final (i, t) in facts.tools.indexed) {
      lines.add(FocusKv(i == 0 ? '도구' : '', _toolLine(t, focusSeconds(age(t.ageMs))), tip: t.label));
    }
    for (final (i, t) in facts.recent.take(_recentShown).indexed) {
      final line = _toolLine(t, _took(t.tookMs ?? 0)) + (t.error ? ' · 실패' : '');
      lines.add(FocusKv(i == 0 ? '최근' : '', line, tip: '${focusSeconds(age(t.ageMs))} 전에 끝남', warn: t.error));
    }
    for (final (i, t) in facts.background.indexed) {
      final elapsed = focusSeconds(age(t.ageMs));
      final value = t.label.isEmpty ? '${_taskKind(t.kind)} · $elapsed' : '${_taskKind(t.kind)} · ${t.label} · $elapsed';
      lines.add(FocusKv(i == 0 ? '백그라운드' : '', value, tip: t.label));
    }
  } else if (card.harness == 'claude') {
    lines.add(const FocusKv('도구', null,
        tip: '이 칸의 claude 는 연결 mod 없이 떠 있어요 — 도구·백그라운드는 mod 가 실린 칸만 보여요'));
  }
  for (final (i, p) in card.ports.indexed) {
    final value = [p.port.toString(), if (p.name.isNotEmpty) p.name, if (p.site.isNotEmpty) p.site].join(' · ');
    lines.add(FocusLink(i == 0 ? '포트' : '', value, port: p.port));
  }
  return lines;
}
