import 'dart:convert';
import 'package:http/http.dart' as http;

/// 사용자에게 보여도 되는 오류 — 주소(slug)가 들어 있지 않다.
class ServerException implements Exception {
  ServerException(this.message);
  final String message;

  @override
  String toString() => message;
}

class Pane {
  const Pane({
    required this.id,
    required this.name,
    required this.title,
    required this.status,
    required this.window,
    required this.cwd,
    this.slug,
    this.color,
    this.model,
    this.effort,
    this.machine,
    this.kind,
    this.waitingFor,
    this.idleSecs,
    this.closed = false,
    this.undocked = false,
    this.session,
    this.harness,
    this.mirrorOf,
    this.contextPct,
    this.branch,
    this.modelLabel,
    this.effortLabel,
    this.doing,
    this.background = const [],
    this.subagents = const [],
  });

  final String id;
  final String name;
  final String title;
  final String status;
  final int window;
  final String cwd;
  final String? slug;
  final String? color;
  final String? model;
  final String? effort;

  /// 다른 기계의 pane 이면 그 기계 route — 요청마다 `m/<route>/` 접두가 붙는다.
  /// 새 서버는 `~<stable id>`, 옛 서버는 표시 이름이다.
  final String? machine;

  /// 무엇을 기다리나 — permission(승인) · question(질문·선택) · idle(답 없이 방치).
  /// 없으면 화면 글자로만 잡은 「답 기다림」.
  final String? kind;
  final String? waitingFor;

  /// 쉬기 시작한 지 몇 초 — 「방금 끝냄」과 「쉬는 중」을 가른다.
  final int? idleSecs;

  /// 닫았지만 살아 있는 pane(데스크톱의 되살리기 목록) — 방에 없다.
  final bool closed;

  /// 별도 OS 창으로 뗀 pane — 방은 떠나온 방 그대로지만 배치도 칸엔 없다.
  final bool undocked;

  /// `/rename` 으로 붙인 세션 이름 — 데스크톱 pane 머리의 그것. codex 는 없다.
  final String? session;

  /// claude·codex — 상태줄 앞의 작은 로고.
  final String? harness;

  /// 이 자리가 다른 기계 pane 의 거울이면 그 기계 이름. 몸통은 저쪽에 있다.
  final String? mirrorOf;
  final int? contextPct;
  final String? branch;

  /// 사람 말로 다듬은 모델 이름(「Fable 5.1 1M」)과 effort — 상태줄 재료.
  final String? modelLabel;
  final String? effortLabel;

  /// 지금 하는 일 — 최신 도구 라벨(「Bash cargo check」·「Edit hub.dart」). 작업 중일 때만 뜻이 있다.
  final String? doing;

  /// 아직 안 끝난 백그라운드 셸·감시의 설명. 작업 중 표시가 이것 때문일 때가 많다 —
  /// 감시 알림이 대화에 들어와 「움직이는 중」으로 보인다(2026-09-08 지시).
  final List<String> background;

  /// 도는 서브에이전트의 설명.
  final List<String> subagents;

  /// 작업 중일 때 정확한 한 마디. 백그라운드가 있으면 그 설명이 먼저다 — 최신 도구
  /// 라벨은 턴이 끝난 뒤에도 남아, 감시만 도는 pane 에 옛 「Edit …」가 붙는다.
  String get busyLabel {
    if (background.isNotEmpty) {
      final n = background.length;
      return n == 1
          ? '백그라운드 · ${background.first}'
          : '백그라운드 $n개 · ${background.first}';
    }
    if (subagents.isNotEmpty) {
      return '서브에이전트 ${subagents.length} · ${subagents.first}';
    }
    final d = doing ?? '';
    return d.isEmpty ? '작업 중' : d;
  }

  /// PC 상태줄의 조각들 — 모델 · 브랜치 · 컨텍스트% · effort. 빈 것은 뺀다.
  List<String> get statusParts => [
    if ((modelLabel ?? '').isNotEmpty)
      modelLabel!
    else if ((model ?? '').isNotEmpty)
      model!,
    if ((branch ?? '').isNotEmpty) branch!,
    if (contextPct != null) '$contextPct%',
    if ((effortLabel ?? effort ?? '').isNotEmpty) (effortLabel ?? effort)!,
  ];

  /// 목록 한 줄용 — 모델과 effort 만.
  List<String> get briefStatusParts => [
    if ((modelLabel ?? '').isNotEmpty)
      modelLabel!
    else if ((model ?? '').isNotEmpty)
      model!,
    if ((effortLabel ?? effort ?? '').isNotEmpty) (effortLabel ?? effort)!,
  ];

  /// 사람 손이 필요한가 — 답·승인·질문 어느 쪽이든.
  bool get isWaiting => status == 'waiting' || status == 'blocked';
  bool get isIdle => status == 'idle';

  /// 끝낸 지 얼마 안 됐다 — 마지막 답을 읽을 차례.
  bool get justDone => isIdle && idleSecs != null && idleSecs! < 600;

  /// 목록 칩에 쓰는 한 마디.
  String get kindLabel {
    if (isWaiting) {
      return switch (kind) {
        'permission' => '승인 기다림',
        'question' => '질문 기다림',
        'idle' => '오래 기다림',
        _ => '답 기다림',
      };
    }
    if (isBusy) return busyLabel;
    if (justDone) return '방금 끝냄';
    return '쉼';
  }

  /// 상태가 비면(학생이 안 도는 셸) 바쁜 것이 아니다 — 웹 허브와 같은 규칙.
  bool get isBusy => status.isNotEmpty && !isIdle && !isWaiting;
  bool get isWebShell => id.startsWith('web-');

  /// 학생이 없는 pane 은 「셸」, 둘째 줄은 제목이나 폴더 이름 — 웹 허브와 같다.
  bool get isShell => name.isEmpty;
  String get displayName => isShell ? '셸' : name;
  String get subtitle {
    // 목록엔 /rename 으로 붙인 이름(session)만 — 에이전트가 제 대화를 요약한 제목은
    // 안 보인다(2026-09-08 지시). 이름 없는 셸만 어느 폴더인지 한 마디.
    if (!isShell) return '';
    final parts = cwd.split('/').where((s) => s.isNotEmpty).toList();
    return parts.isEmpty ? '' : parts.last;
  }

  static Pane fromJson(Map<String, Object?> j, {String? machine}) => Pane(
    id: j['id'] as String? ?? '',
    name: j['name'] as String? ?? '',
    title: j['title'] as String? ?? '',
    status: j['status'] as String? ?? '',
    window: (j['window'] as num?)?.toInt() ?? 0,
    cwd: j['cwd'] as String? ?? '',
    slug: j['slug'] as String?,
    color: j['color'] as String?,
    model: j['model'] as String?,
    effort: j['effort'] as String?,
    machine: machine,
    kind: j['kind'] as String?,
    waitingFor: j['waiting_for'] as String?,
    idleSecs: (j['idle_secs'] as num?)?.toInt(),
    closed: j['closed'] == true,
    undocked: j['undocked'] == true,
    session: j['session'] as String?,
    harness: j['harness'] as String?,
    mirrorOf: j['mirror_of'] as String?,
    contextPct: (j['context_pct'] as num?)?.toInt(),
    branch: j['branch'] as String?,
    modelLabel: j['model_label'] as String?,
    effortLabel: j['effort_label'] as String?,
    doing: j['doing'] as String?,
    background: [for (final s in (j['background'] as List?) ?? const []) '$s'],
    subagents: [for (final s in (j['subagents'] as List?) ?? const []) '$s'],
  );
}

/// 데스크톱 창 안에서 pane 이 차지하는 자리 — 창 대비 백분율.
class PaneRect {
  const PaneRect({
    required this.surface,
    required this.x,
    required this.y,
    required this.w,
    required this.h,
    this.tabs = const [],
    this.tabActive,
  });

  final String surface;
  final int x;
  final int y;
  final int w;
  final int h;

  /// 이 자리에 앉은 탭들의 pane id(순서대로). 탭이 둘 이상일 때만 온다 — 탭 하나는
  /// 곧 `surface` 라 따로 싣지 않는다.
  final List<String> tabs;

  /// `tabs` 중 지금 앞에 나온 탭의 자리.
  final int? tabActive;

  static PaneRect? fromJson(Map<String, Object?> j) {
    final id = j['surface_id'] as String?;
    if (id == null) return null;
    int n(String k) => (j[k] as num?)?.toInt() ?? 0;
    final tabs = j['tabs'];
    return PaneRect(
      surface: id,
      x: n('x'),
      y: n('y'),
      w: n('w'),
      h: n('h'),
      tabs: tabs is List ? tabs.whereType<String>().toList() : const [],
      tabActive: (j['tab_active'] as num?)?.toInt(),
    );
  }
}

/// 방(윈도우) 하나의 배치 — 미니맵 재료. `idx` 가 pane 의 `window` 와 맞는다.
class WindowLayout {
  const WindowLayout({
    required this.idx,
    required this.active,
    required this.rects,
    this.aspect,
  });

  final int idx;
  final bool active;
  final List<PaneRect> rects;

  /// 데스크톱 pane 영역의 가로÷세로. 없으면(옛 서버) 흔한 창 모양으로 그린다.
  final double? aspect;

  static WindowLayout fromJson(Map<String, Object?> j) {
    final panes = j['panes'];
    final a = (j['aspect'] as num?)?.toDouble();
    return WindowLayout(
      idx: (j['idx'] as num?)?.toInt() ?? 0,
      active: j['active'] == true,
      aspect: a != null && a > 0 ? a : null,
      rects: [
        if (panes is List)
          for (final p in panes)
            if (p is Map) ?PaneRect.fromJson(p.cast<String, Object?>()),
      ],
    );
  }
}

class Machine {
  const Machine({
    required this.label,
    String? route,
    required this.online,
    required this.panes,
  }) : route = route ?? label;
  final String label;

  /// HTTP/WS `m/<route>/`에 쓸 안정 식별자. 옛 응답은 표시 이름으로 폴백한다.
  final String route;
  final bool online;
  final List<Pane> panes;
}

class Me {
  const Me({required this.name, required this.owner, this.machine});
  final String name;
  final bool owner;
  final String? machine;
}

/// 데스크톱이 지금 쓰는 색(`GET design-tokens`). 격자를 그 화면과 같은 색으로 그린다 —
/// 폰만 다른 팔레트면 같은 학생 화면이 다른 물건처럼 보인다.
class DesignTokens {
  const DesignTokens({
    required this.dark,
    required this.bg,
    required this.fg,
    required this.accent,
    required this.ansi,
    required this.surface,
    required this.surfaceHover,
    required this.border,
    required this.text,
    required this.textDim,
    required this.onAccent,
    required this.danger,
    this.characterAccents = const {},
    this.minContrast = defaultMinContrast,
  });

  /// 데스크톱 설정 화면의 「Default」 프리셋 — 옛 서버는 값을 안 보낸다.
  static const defaultMinContrast = 2.5;

  DesignTokens withMinContrast(double v) => DesignTokens(
    dark: dark,
    bg: bg,
    fg: fg,
    accent: accent,
    ansi: ansi,
    surface: surface,
    surfaceHover: surfaceHover,
    border: border,
    text: text,
    textDim: textDim,
    onAccent: onAccent,
    danger: danger,
    characterAccents: characterAccents,
    minContrast: v,
  );

  final bool dark;
  final int bg;
  final int fg;
  final int accent;

  /// ANSI 0–15, ARGB.
  final List<int> ansi;

  /// 앱 화면(허브·상단 바·입력창)이 데스크톱과 같은 얼굴이 되도록 쓰는 색들.
  final int surface;
  final int surfaceHover;
  final int border;
  final int text;
  final int textDim;
  final int onAccent;
  final int danger;

  /// 학생 이름 → 그 학생의 색. pane 에 색이 없을 때 이름으로 찾는다.
  final Map<String, int> characterAccents;

  /// 셀이 스스로 고른 글자색(256색·트루컬러)을 바탕과 이 비율 이상 벌리는 바닥 —
  /// 데스크톱과 같은 값이어야 같은 화면이 같은 색으로 보인다.
  final double minContrast;

  /// `#rrggbb`·`#rrggbbaa` → 불투명 ARGB. 알파는 버린다 — 격자 배경은 늘 꽉 찬 색이다.
  static int? parseHex(Object? v) {
    if (v is! String) return null;
    final h = v.startsWith('#') ? v.substring(1) : v;
    if (h.length != 6 && h.length != 8) return null;
    final rgb = int.tryParse(h.substring(0, 6), radix: 16);
    return rgb == null ? null : 0xff000000 | rgb;
  }

  static DesignTokens? fromJson(Object? json) {
    if (json is! Map) return null;
    final palette = json['palette'];
    if (palette is! Map) return null;
    final bg = parseHex(palette['bg']);
    final fg = parseHex(palette['fg']);
    final ansiRaw = json['ansi'];
    if (bg == null || fg == null || ansiRaw is! List || ansiRaw.length < 16) {
      return null;
    }
    final accent = parseHex(palette['accent']) ?? fg;
    final ansi = <int>[];
    for (final c in ansiRaw.take(16)) {
      final v = parseHex(c);
      if (v == null) return null;
      ansi.add(v);
    }
    final dark = json['theme'] != 'light';
    final text = parseHex(palette['text']) ?? fg;
    final accentsRaw = json['character_accents'];
    final accents = <String, int>{
      if (accentsRaw is Map)
        for (final e in accentsRaw.entries)
          if (e.key is String && parseHex(e.value) != null)
            e.key as String: parseHex(e.value)!,
    };
    return DesignTokens(
      dark: dark,
      bg: bg,
      fg: fg,
      accent: accent,
      ansi: ansi,
      surface: parseHex(palette['surface']) ?? bg,
      surfaceHover:
          parseHex(palette['surface_hover']) ??
          parseHex(palette['surface']) ??
          bg,
      border: parseHex(palette['border']) ?? text,
      text: text,
      textDim: parseHex(palette['text_dim']) ?? text,
      onAccent:
          parseHex(palette['on_accent']) ?? (dark ? 0xff000000 : 0xffffffff),
      danger: parseHex(palette['danger']) ?? 0xffe0584e,
      characterAccents: accents,
      minContrast: switch (json['min_contrast']) {
        final num v => v.toDouble(),
        _ => defaultMinContrast,
      },
    );
  }
}

/// 서버 하나. 주소 뒤 `/` 까지가 루트라 모든 경로는 상대로 붙는다 — `/u/<slug>/`
/// 아래에서는 slug 가 자격이고, 그 자격은 상대경로에 저절로 따라간다.
class Server {
  Server(Uri root, {http.Client? client})
    : root = normalize(root),
      _client = client ?? http.Client();

  final Uri root;
  final http.Client _client;

  static Uri normalize(Uri u) =>
      u.path.endsWith('/') ? u : u.replace(path: '${u.path}/');

  /// 사람이 붙여 넣은 글을 주소로. 스킴이 없으면 https, 쿼리·조각은 버린다.
  static Uri? parse(String text) {
    var t = text.trim();
    if (t.isEmpty) return null;
    if (!t.contains('://')) t = 'https://$t';
    final u = Uri.tryParse(t);
    if (u == null || u.host.isEmpty) return null;
    return normalize(
      Uri(
        scheme: u.scheme,
        host: u.host,
        port: u.hasPort ? u.port : null,
        path: u.path.isEmpty ? '/' : u.path,
      ),
    );
  }

  static String _prefix(String? machine) =>
      machine == null ? '' : 'm/${Uri.encodeComponent(machine)}/';

  /// `%N` 같은 pane id 는 문자열로 붙이지 않는다 — queryParameters 가 `%25N` 으로
  /// 인코딩해야 서버가 제 id 로 읽는다.
  Uri uri(String path, {Map<String, String>? query, String? machine}) {
    final u = root.resolve('${_prefix(machine)}$path');
    return query == null ? u : u.replace(queryParameters: query);
  }

  Uri wsUri(
    String path, {
    required Map<String, String> query,
    String? machine,
  }) {
    final u = uri(path, query: query, machine: machine);
    return u.replace(scheme: u.scheme == 'https' ? 'wss' : 'ws');
  }

  /// 오류 문구·설정 화면용. slug 는 자격이라 가린다.
  String describe() {
    final p = root.path;
    final shown = p.startsWith('/u/') ? '/u/•••/' : p;
    final port = root.hasPort ? ':${root.port}' : '';
    return '${root.host}$port$shown';
  }

  Future<Object?> _getJson(
    String path, {
    Map<String, String>? query,
    String? machine,
  }) async {
    final http.Response res;
    try {
      res = await _client.get(uri(path, query: query, machine: machine));
    } catch (_) {
      throw ServerException('${describe()} 에 닿지 못했다');
    }
    if (res.statusCode != 200) {
      throw ServerException('${describe()} 응답 ${res.statusCode} ($path)');
    }
    try {
      return jsonDecode(utf8.decode(res.bodyBytes));
    } catch (_) {
      throw ServerException('${describe()} 응답을 읽지 못했다 ($path)');
    }
  }

  /// 색은 장식이라 실패해도 화면을 막지 않는다 — 못 받으면 null, 앱 기본색으로 간다.
  Future<DesignTokens?> designTokens({String? machine}) async {
    final Object? raw;
    try {
      raw = await _getJson('design-tokens', machine: machine);
    } on ServerException {
      return null;
    }
    final t = DesignTokens.fromJson(raw);
    if (t == null || (raw is Map && raw['min_contrast'] is num)) return t;
    // 옛 데스크톱은 토큰에 대비 바닥을 안 싣는다 — 설정 화면이 읽는 값에서 같은 것을 꺼낸다.
    // 그것도 없으면 프리셋 기본값으로 간다(색 하나 때문에 화면을 막지 않는다).
    try {
      final v = await _getJson('settings/values', machine: machine);
      if (v is Map && v['appearance'] is Map) {
        final m = (v['appearance'] as Map)['min_contrast'];
        if (m is num) return t.withMinContrast(m.toDouble());
      }
    } on ServerException {
      // 무시 — 기본값으로.
    }
    return t;
  }

  /// 데스크톱 설정 화면의 「외형」 값 그대로 — 테마·강조색·모서리 목록과 지금 고른 것.
  /// 폰이 그 화면을 흉내 내지 않고 같은 목록을 받아 같은 액션을 보낸다.
  Future<Map<String, Object?>?> appearance({String? machine}) async {
    final v = await _getJson('settings/values', machine: machine);
    if (v is! Map) return null;
    final a = v['appearance'];
    return a is Map ? a.cast<String, Object?>() : null;
  }

  /// 하단바 「브라우저 기기」와 같은 목록 — 고른 기계·후보·이 기계 이름·폰 여부.
  Future<BrowserTarget?> browserTarget({String? machine}) async {
    final v = await _getJson('settings/values', machine: machine);
    if (v is! Map) return null;
    final b = v['browser'];
    if (b is! Map) return null;
    return BrowserTarget(
      machine: b['machine'] as String? ?? '',
      candidates: [for (final c in (b['candidates'] as List?) ?? const []) '$c'],
      local: b['local'] as String? ?? '',
      phone: b['phone'] == true,
    );
  }

  /// 데스크톱 설정 액션 — 이름·인자가 설정 화면의 웹 쪽과 같다(`theme-mode`·`accent`·
  /// `shape`…). 데스크톱이 그 자리에서 바뀌고 색이 폰으로 되돌아온다.
  Future<void> settingsAction(
    String action, {
    String? id,
    String? label,
    String? machine,
  }) async {
    final http.Response res;
    try {
      res = await _client.post(
        uri('settings/action', machine: machine),
        headers: {'content-type': 'application/json'},
        body: jsonEncode({'action': action, 'id': ?id, 'label': ?label}),
      );
    } catch (_) {
      throw ServerException('${describe()} 에 닿지 못했다');
    }
    if (res.statusCode != 200) {
      throw ServerException('설정이 안 바뀌었다 (${res.statusCode})');
    }
  }

  Future<Me> me() async {
    final j = await _getJson('mobile/me');
    if (j is! Map) throw ServerException('${describe()} 는 카사텀이 아닌 것 같다');
    return Me(
      name: j['name'] as String? ?? '',
      owner: j['owner'] == true,
      machine: j['machine'] as String?,
    );
  }

  Future<List<Pane>> panes({String? machine}) async {
    final j = await _getJson('term/panes', machine: machine);
    return _panesFrom(j, machine: machine);
  }

  static List<Pane> _panesFrom(Object? j, {String? machine}) => [
    if (j is List)
      for (final e in j)
        if (e is Map)
          Pane.fromJson(e.cast<String, Object?>(), machine: machine),
  ];

  /// 방(window) 이름 — 인덱스가 pane 의 `window` 와 맞는다.
  Future<List<String>> sessions() async {
    final j = await _getJson('sessions');
    final labels = j is Map ? j['labels'] : null;
    return [
      if (labels is List)
        for (final l in labels) l is String ? l : '',
    ];
  }

  /// 방마다의 pane 배치. 서버가 못 주면(옛 버전) 빈 목록 — 허브는 목록만 그린다.
  Future<List<WindowLayout>> windows({String? machine}) async {
    final j = await _getJson('windows', machine: machine);
    final list = j is Map ? j['windows'] : null;
    return [
      if (list is List)
        for (final w in list)
          if (w is Map) WindowLayout.fromJson(w.cast<String, Object?>()),
    ];
  }

  Future<List<Machine>> machines() async {
    final j = await _getJson('machines');
    final list = j is Map ? j['machines'] : null;
    return [
      if (list is List)
        for (final m in list)
          if (m is Map)
            Machine(
              label: m['label'] as String? ?? '',
              route: m['route'] as String? ?? m['label'] as String? ?? '',
              online: m['online'] == true,
              panes: _panesFrom(
                m['panes'],
                machine: m['route'] as String? ?? m['label'] as String?,
              ),
            ),
    ];
  }

  /// 답장 한 줄. 서버가 Ctrl-U·bracketed paste·짧은 지연 뒤 Enter 를 맡으므로
  /// 여기서는 글만 준다 — 소켓으로 `글\r` 을 한 번에 쏘면 Enter 가 먹힌다.
  Future<void> send(String pane, String text, {String? machine}) async {
    final http.Response res;
    try {
      res = await _client.post(
        uri('send', query: {'surface': pane}, machine: machine),
        headers: {'content-type': 'application/json'},
        body: jsonEncode({'text': text, 'submit': true}),
      );
    } catch (_) {
      throw ServerException('${describe()} 에 닿지 못했다');
    }
    if (res.statusCode != 200) {
      throw ServerException('답장이 안 갔다 (${res.statusCode})');
    }
  }

  /// 애플에서 받은 푸시 토큰을 맡긴다 — 서버가 학생 대기·끝냄·쪽지 때 이 폰으로 쏜다.
  Future<bool> registerPushToken(String token, String env) async {
    final http.Response res;
    try {
      res = await _client.post(
        uri('term/push-token'),
        headers: {'content-type': 'application/json'},
        // 알림 확장이 학생 얼굴을 받아 올 주소 — 확장은 이 앱의 열쇠고리를 못 본다.
        body: jsonEncode({'token': token, 'env': env, 'root': root.toString()}),
      );
    } catch (_) {
      throw ServerException('${describe()} 에 닿지 못했다');
    }
    if (res.statusCode != 200) {
      throw ServerException('알림 등록이 안 됐다 (${res.statusCode})');
    }
    try {
      final reply = jsonDecode(res.body) as Map<String, dynamic>;
      if (reply['ok'] != true) throw ServerException('알림 등록이 안 됐다');
      return reply['ready'] == true;
    } catch (_) {
      throw ServerException('알림 등록 응답을 확인하지 못했다');
    }
  }

  /// 연결 서버를 바꾸거나 연결을 지울 때 옛 발신자의 등록을 걷는다.
  Future<void> unregisterPushToken(String token) async {
    try {
      final res = await _client.post(
        uri('term/push-token'),
        headers: {'content-type': 'application/json'},
        body: jsonEncode({'token': token, 'remove': true}),
      );
      if (res.statusCode != 200) throw ServerException('이전 알림 등록 해제가 안 됐다');
    } catch (_) {
      throw ServerException('이전 알림 등록 해제가 안 됐다');
    }
  }

  /// 화면 배치를 만지는 소켓 명령 — `POST cmd` 는 `kasaterm-cli` 와 이름·인자가 같다
  /// (surface.split·swap·close, window.new·rename·close). 서버가 허용 목록으로 거른다.
  Future<Map<String, dynamic>> cmd(
    String method,
    Map<String, Object?> params, {
    String? machine,
  }) async {
    final http.Response res;
    try {
      res = await _client.post(
        uri('cmd', machine: machine),
        headers: {'content-type': 'application/json'},
        body: jsonEncode({'method': method, 'params': params}),
      );
    } catch (_) {
      throw ServerException('${describe()} 에 닿지 못했다');
    }
    Object? body;
    try {
      body = jsonDecode(res.body);
    } catch (_) {
      body = null;
    }
    final map = body is Map<String, dynamic> ? body : <String, dynamic>{};
    if (res.statusCode != 200 || map['ok'] != true) {
      final err = map['error'];
      final why = err is Map
          ? (err['message'] ?? err.toString())
          : (err ?? 'HTTP ${res.statusCode}');
      throw ServerException('$why');
    }
    return map;
  }

  /// pane 닫기 — 데스크톱에서 × 를 누른 것과 같다(되살리기 대열에 남는다).
  Future<void> closePane(String pane, {String? machine}) async {
    final http.Response res;
    try {
      res = await _client.post(
        uri('close-pane', query: {'surface': pane}, machine: machine),
      );
    } catch (_) {
      throw ServerException('${describe()} 에 닿지 못했다');
    }
    if (res.statusCode != 200) {
      throw ServerException('pane 을 못 닫았다 (${res.statusCode})');
    }
  }

  /// `from` 옆에 셸 pane 하나 — 방향은 서버가 pane 모양을 보고 고른다.
  Future<void> splitPane(String from, {String? machine}) => cmd(
    'surface.split',
    {'from': from, 'direction': 'auto'},
    machine: machine,
  );

  Future<void> swapPanes(String a, String b, {String? machine}) =>
      cmd('surface.swap', {'a': a, 'b': b}, machine: machine);

  Future<void> newWindow({String? machine}) =>
      cmd('window.new', const {}, machine: machine);

  /// 방 이름은 그 방의 pane 하나로 짚는다 — 소켓 규약이 창 번호 대신 surface 를 받는다.
  Future<void> renameWindow(String surface, String title, {String? machine}) =>
      cmd('window.rename', {
        'surface_id': surface,
        'title': title,
      }, machine: machine);

  Future<void> closeWindow(int idx, {String? machine}) =>
      cmd('window.close', {'idx': idx}, machine: machine);

  Uri avatar(String slug, {String? machine}) =>
      uri('term/avatar/${Uri.encodeComponent(slug)}.png', machine: machine);

  /// 학생 쪽지 — 나쵸가 남긴 「시킨 것 → 한 것」 한 줄. 최근 것부터.
  Future<List<Note>> notes({String? machine}) async {
    final j = await _getJson('term/notes', machine: machine);
    return [
      if (j is List)
        for (final e in j)
          if (e is Map)
            Note.fromJson(e.cast<String, Object?>(), machine: machine),
    ];
  }

  Future<void> markNotesRead({
    List<int> ids = const [],
    bool all = false,
    String? machine,
  }) async {
    try {
      await _client.post(
        uri('term/notes/read', machine: machine),
        headers: {'content-type': 'application/json'},
        body: jsonEncode({'ids': ids, 'all': all}),
      );
    } catch (_) {
      throw ServerException('${describe()} 에 닿지 못했다');
    }
  }

  Uri noteImage(int id, {String? machine}) =>
      uri('term/notes/$id.png', machine: machine);

  void close() => _client.close();
}

/// 나쵸가 남긴 학생 쪽지 한 장(서버 notes.rs).
/// 학생이 사람에게 보여 줄 페이지가 갈 곳. `phone` 이면 쪽지로 온다.
class BrowserTarget {
  const BrowserTarget({
    required this.machine,
    required this.candidates,
    required this.local,
    required this.phone,
  });

  /// 고른 기계 라벨 — 빈 문자열이면 주소가 가리키는 그 기계.
  final String machine;
  final List<String> candidates;

  /// 그 기계가 명부에서 불리는 이름 — 다른 기계에 같은 선택을 전할 때 쓴다.
  final String local;
  final bool phone;
}

class Note {
  const Note({
    required this.id,
    required this.pane,
    required this.character,
    required this.kind,
    required this.summary,
    required this.asked,
    required this.did,
    required this.when,
    required this.read,
    required this.image,
    this.machine,
    this.url = '',
  });

  final int id;
  final String pane;
  final String character;

  /// kind=`link` — 누르면 사파리로 여는 주소.
  final String url;

  /// permission · question · waiting · idle · done_ok · done_fail · dead
  final String kind;
  final String summary;
  final String asked;
  final String did;
  final DateTime when;
  final bool read;

  /// pane 사진이 딸렸는가 — `Server.noteImage`.
  final bool image;
  final String? machine;

  String get key => '${machine ?? ''}|$id';

  Note copyWith({bool? read}) => Note(
    id: id,
    pane: pane,
    character: character,
    kind: kind,
    summary: summary,
    asked: asked,
    did: did,
    when: when,
    read: read ?? this.read,
    image: image,
    machine: machine,
    url: url,
  );

  static Note fromJson(Map<String, Object?> j, {String? machine}) => Note(
    id: (j['id'] as num?)?.toInt() ?? 0,
    pane: j['pane'] as String? ?? '',
    character: j['character'] as String? ?? '',
    kind: j['kind'] as String? ?? '',
    summary: j['summary'] as String? ?? '',
    asked: j['asked'] as String? ?? '',
    did: j['did'] as String? ?? '',
    when: DateTime.fromMillisecondsSinceEpoch(
      ((j['when'] as num?)?.toInt() ?? 0) * 1000,
    ),
    read: j['read'] == true,
    image: j['image'] == true,
    machine: machine,
    url: j['url'] as String? ?? '',
  );
}
