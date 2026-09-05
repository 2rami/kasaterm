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

  /// 다른 기계의 pane 이면 그 기계 이름 — 요청마다 `m/<이름>/` 접두가 붙는다.
  final String? machine;

  /// 무엇을 기다리나 — permission(승인) · question(질문·선택) · idle(답 없이 방치).
  /// 없으면 화면 글자로만 잡은 「답 기다림」.
  final String? kind;
  final String? waitingFor;

  /// 쉬기 시작한 지 몇 초 — 「방금 끝냄」과 「쉬는 중」을 가른다.
  final int? idleSecs;

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
    if (isBusy) return '작업 중';
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
    if (title.isNotEmpty) return title;
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
  );
}

class Machine {
  const Machine({
    required this.label,
    required this.online,
    required this.panes,
  });
  final String label;
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

  Future<List<Machine>> machines() async {
    final j = await _getJson('machines');
    final list = j is Map ? j['machines'] : null;
    return [
      if (list is List)
        for (final m in list)
          if (m is Map)
            Machine(
              label: m['label'] as String? ?? '',
              online: m['online'] == true,
              panes: _panesFrom(m['panes'], machine: m['label'] as String?),
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

  Uri avatar(String slug, {String? machine}) =>
      uri('term/avatar/${Uri.encodeComponent(slug)}.png', machine: machine);

  void close() => _client.close();
}
