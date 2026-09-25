import 'dart:convert';
import 'dart:typed_data';

/// 대화 보기의 한 칸. 데스크톱 아로나(`TerminalPeekPanel` 의 `eventsToItems`)가 jsonl 을
/// 말풍선으로 펴는 규칙의 폰판이다. 도구 결과·질문 답은 뒤 줄에서 와 앞 칸을 채우므로
/// 칸은 가변이다 — 서버가 덧붙은 줄만 주니 전체를 다시 펴지 않고 이어 붙인다.
sealed class ChatItem {}

class ChatBubble extends ChatItem {
  ChatBubble({
    required this.user,
    required this.text,
    this.at,
    this.from,
    this.queued = false,
  });

  /// user 턴 — 선생님 말이거나, [from] 이 있으면 다른 학생이 보낸 쪽지.
  final bool user;
  final String text;
  final DateTime? at;
  final String? from;

  /// 작업 중에 넣어 둔 예약 — 아직 학생이 안 읽었다.
  bool queued;
  final List<Uint8List> images = [];

  bool get mine => user && from == null;
}

class ChatThinking extends ChatItem {
  ChatThinking(this.text);
  final String text;
}

class ChatTool extends ChatItem {
  ChatTool(this.name, this.summary);
  final String name;
  final String summary;
  String? result;
  bool error = false;

  bool get done => result != null;
}

/// 슬래시 명령(`/model`)이나 `!` 셸 한 줄.
class ChatCommand extends ChatItem {
  ChatCommand(this.name, this.args);
  final String name;
  final String args;
}

/// 명령의 출력(`<local-command-stdout>`·`<bash-stdout>`).
class ChatOutput extends ChatItem {
  ChatOutput(this.text);
  final String text;
}

/// 답이 붙은 AskUserQuestion. 답이 오기 전엔 안 그린다 — 그동안은 화면에서 읽은
/// 선택지 카드가 맡는다.
class ChatAnswered extends ChatItem {
  ChatAnswered(this.questions);
  final List<String> questions;
  List<(String, String)>? pairs;
}

/// 서브에이전트·워크플로를 띄웠다는 한 줄.
class ChatLaunch extends ChatItem {
  ChatLaunch(this.label);
  final String label;
}

class ChatSystem extends ChatItem {
  ChatSystem(this.text);
  final String text;
}

class ChatInterrupted extends ChatItem {}

/// 한 pane 의 대화. claude 의 transcript 와 codex 의 rollout 을 같은 칸으로 편다.
class Conversation {
  final List<ChatItem> items = [];

  /// 서버에 다음에 물을 자리(바이트). 0 이면 꼬리부터 다시.
  int offset = 0;

  /// 바뀔 때마다 오른다 — 화면이 이것만 보고 다시 그린다.
  int version = 0;

  String? _sessionId;
  final Map<String, ChatItem> _byToolId = {};
  final List<ChatBubble?> _queue = [];
  String? _lastCodexSay;
  String? _lastCodexPrompt;

  void clear() {
    items.clear();
    _byToolId.clear();
    _queue.clear();
    _sessionId = null;
    _lastCodexSay = null;
    _lastCodexPrompt = null;
    offset = 0;
    version++;
  }

  /// 서버 조각 하나. 세션이 바뀐 것을 알아채면 false — 호출부가 처음부터 다시 받는다.
  /// 서버는 파일이 줄어들 때만 꼬리를 다시 주므로, `/clear` 뒤 새 파일이 옛 자리보다
  /// 길면 새 파일의 중간부터 읽게 된다. 줄마다 붙은 sessionId 로 그 경우를 가른다.
  bool apply(String raw, {required bool reset, required int next}) {
    final events = <Map<String, Object?>>[];
    for (final line in const LineSplitter().convert(raw)) {
      if (line.trim().isEmpty) continue;
      try {
        final v = jsonDecode(line);
        if (v is Map<String, Object?>) events.add(v);
      } catch (_) {
        // 쓰다 만 줄 — 서버가 잘라 주지만 꼬리 첫 줄이 깨져 올 수 있다.
      }
    }
    if (reset) clear();
    if (!reset && _sessionId != null) {
      for (final e in events) {
        final sid = e['sessionId'];
        if (sid is String && sid != _sessionId) return false;
      }
    }
    for (final e in events) {
      final sid = e['sessionId'];
      if (sid is String && e['isSidechain'] != true) _sessionId = sid;
      if (e['payload'] is Map) {
        _codex(e);
      } else {
        _claude(e);
      }
    }
    offset = next;
    if (events.isNotEmpty || reset) version++;
    return true;
  }

  // ── claude ────────────────────────────────────────────────────────────────

  void _claude(Map<String, Object?> ev) {
    if (ev['isSidechain'] == true) return;
    final type = ev['type'];
    final at = _time(ev['timestamp']);
    switch (type) {
      case 'system':
        final text = _systemText(ev);
        if (text != null) items.add(ChatSystem(text));
        return;
      case 'queue-operation':
        _queueOp(ev, at);
        return;
      case 'attachment':
        final att = ev['attachment'];
        if (att is Map && att['type'] == 'hook_success') {
          final msg = _hookMessage(att['stdout']);
          if (msg != null) items.add(ChatSystem(msg));
        }
        return;
      case 'user':
      case 'assistant':
        break;
      default:
        return;
    }
    // 스킬 본문·caveat 처럼 사람이 친 적 없는 주입 — 선생님 말풍선으로 새면 안 된다.
    if (type == 'user' && ev['isMeta'] == true) return;
    final message = ev['message'];
    final content = message is Map ? message['content'] : null;
    final user = type == 'user';
    if (user && _interrupted(content)) {
      if (items.isEmpty || items.last is! ChatInterrupted) {
        items.add(ChatInterrupted());
      }
      return;
    }
    if (content is String) {
      if (user) {
        _userText(content, at);
      } else if (content.trim().isNotEmpty) {
        items.add(ChatBubble(user: false, text: content.trim(), at: at));
      }
      return;
    }
    if (content is! List) return;
    for (final block in content) {
      if (block is! Map) continue;
      switch (block['type']) {
        case 'text':
          final t = block['text'];
          if (t is! String || t.trim().isEmpty) break;
          if (user) {
            _userText(t, at);
          } else {
            items.add(ChatBubble(user: false, text: t.trim(), at: at));
          }
        case 'image' when user:
          final bytes = _imageBytes(block['source']);
          if (bytes == null) break;
          final last = items.isEmpty ? null : items.last;
          if (last is ChatBubble && last.mine && last.at == at) {
            last.images.add(bytes);
          } else {
            items.add(
              ChatBubble(user: true, text: '', at: at)..images.add(bytes),
            );
          }
        case 'thinking':
          final t = block['thinking'];
          if (t is String && t.trim().isNotEmpty) {
            items.add(ChatThinking(t.trim()));
          }
        case 'tool_use':
          _toolUse(block);
        case 'tool_result':
          _toolResult(block);
      }
    }
  }

  void _toolUse(Map<Object?, Object?> b) {
    final id = b['id'] as String?;
    final name = b['name'] as String? ?? 'tool';
    final input = b['input'] is Map
        ? (b['input'] as Map).cast<String, Object?>()
        : const <String, Object?>{};
    final ChatItem item;
    switch (name) {
      case 'AskUserQuestion':
        final qs = input['questions'];
        item = ChatAnswered([
          if (qs is List)
            for (final q in qs)
              if (q is Map) (q['question'] ?? q['header'] ?? '질문').toString(),
        ]);
      case 'Agent' || 'Task':
        final type = input['subagent_type'];
        final desc = input['description'];
        item = ChatLaunch(
          [
            if (type is String && type.isNotEmpty) type,
            if (desc is String && desc.isNotEmpty) desc,
          ].join(' · '),
        );
      case 'Workflow':
        item = ChatLaunch('워크플로 ${_workflowName(input) ?? ''}'.trim());
      default:
        item = ChatTool(name, toolSummary(name, input));
    }
    items.add(item);
    if (id != null) _byToolId[id] = item;
  }

  void _toolResult(Map<Object?, Object?> b) {
    final id = b['tool_use_id'];
    final item = id is String ? _byToolId[id] : null;
    final text = _resultText(b['content']);
    if (item is ChatTool) {
      item.result = text;
      item.error = b['is_error'] == true;
    } else if (item is ChatAnswered) {
      item.pairs = answeredPairs(text, item.questions);
    }
  }

  void _userText(String raw, DateTime? at) {
    final cmd = _tag(raw, 'command-name');
    if (cmd != null) {
      items.add(
        ChatCommand(cmd.trim(), (_tag(raw, 'command-args') ?? '').trim()),
      );
      return;
    }
    final bash = _tag(raw, 'bash-input');
    if (bash != null) {
      items.add(ChatCommand('!', bash.trim()));
      return;
    }
    final out =
        _tag(raw, 'local-command-stdout') ??
        [
          _tag(raw, 'bash-stdout'),
          _tag(raw, 'bash-stderr'),
        ].whereType<String>().where((s) => s.trim().isNotEmpty).join('\n');
    if (out.trim().isNotEmpty) {
      items.add(ChatOutput(stripAnsi(out).trim()));
      return;
    }
    if (raw.contains('<bash-stdout>') ||
        raw.contains('<local-command-stdout>')) {
      return;
    }
    final clean = stripMeta(raw);
    if (clean.isEmpty || isInjection(clean)) return;
    final mate = teammateMessage(clean);
    items.add(
      mate == null
          ? ChatBubble(user: true, text: clean, at: at)
          : ChatBubble(user: true, text: mate.$2, at: at, from: mate.$1),
    );
  }

  /// 작업 중 보낸 예약은 정식 user 턴이 아니라 큐 조작으로만 남는다. enqueue 를 대기
  /// 말풍선으로 세우고, dequeue 로 꺼내지면 같은 글이 정식 턴으로 다시 오니 뺀다.
  void _queueOp(Map<String, Object?> ev, DateTime? at) {
    switch (ev['operation']) {
      case 'enqueue':
        final raw = ev['content'];
        final clean = raw is String ? stripMeta(raw) : '';
        if (clean.isEmpty || isInjection(clean)) {
          _queue.add(null);
          return;
        }
        final b = ChatBubble(user: true, text: clean, at: at, queued: true);
        items.add(b);
        _queue.add(b);
      case 'dequeue' || 'popAll':
        final n = ev['operation'] == 'popAll' ? _queue.length : 1;
        for (var i = 0; i < n && _queue.isNotEmpty; i++) {
          final b = _queue.removeAt(0);
          if (b != null) items.remove(b);
        }
      case 'remove':
        if (_queue.isNotEmpty) _queue.removeAt(0)?.queued = false;
    }
  }

  // ── codex ─────────────────────────────────────────────────────────────────

  void _codex(Map<String, Object?> ev) {
    final p = (ev['payload'] as Map).cast<String, Object?>();
    final at = _time(ev['timestamp']);
    switch ((ev['type'], p['type'])) {
      // 옛 판은 event_msg 로, 새 판은 item_completed 로 말을 적는다 — 한 판이 둘 다
      // 적기도 해서, 같은 쪽의 같은 글이 이어 오면 한 번만 세운다.
      case ('event_msg', 'user_message'):
        _codexSay(true, p['message'], at);
      case ('event_msg', 'agent_message'):
        _codexSay(false, p['message'], at);
      case ('event_msg', 'item_completed'):
        final item = p['item'];
        if (item is! Map) return;
        switch (item['type']) {
          case 'UserMessage':
            _codexSay(true, _codexText(item['content']), at);
          case 'AgentMessage':
            _codexSay(false, _codexText(item['content']), at);
        }
      case ('event_msg', 'agent_reasoning'):
        final t = (p['text'] as String? ?? '').trim();
        if (t.isNotEmpty) items.add(ChatThinking(t));
      case ('event_msg', 'turn_aborted'):
        items.add(ChatInterrupted());
      case ('response_item', 'function_call' || 'custom_tool_call'):
        final name = p['name'] as String? ?? 'tool';
        final args = p['arguments'] ?? p['input'];
        final input = _codexInput(args);
        if (name == 'spawn_agent') {
          items.add(ChatLaunch(toolSummary(name, input)));
          return;
        }
        final tool = ChatTool(name, toolSummary(name, input));
        items.add(tool);
        final id = p['call_id'];
        if (id is String) _byToolId[id] = tool;
      case (
        'response_item',
        'function_call_output' || 'custom_tool_call_output',
      ):
        final id = p['call_id'];
        final tool = id is String ? _byToolId[id] : null;
        if (tool is! ChatTool) return;
        final (text, error) = _codexOutput(p['output']);
        tool.result = text;
        tool.error = error;
    }
  }

  void _codexSay(bool user, Object? raw, DateTime? at) {
    final t = raw is String ? raw.trim() : '';
    if (t.isEmpty || t == (user ? _lastCodexPrompt : _lastCodexSay)) return;
    if (user) {
      _lastCodexPrompt = t;
    } else {
      _lastCodexSay = t;
    }
    items.add(ChatBubble(user: user, text: t, at: at));
  }

  static String _codexText(Object? content) => content is List
      ? content
            .whereType<Map>()
            .map((b) => b['text'])
            .whereType<String>()
            .join('\n')
      : '';

  static Map<String, Object?> _codexInput(Object? args) {
    if (args is String) {
      try {
        final v = jsonDecode(args);
        if (v is Map) return v.cast<String, Object?>();
      } catch (_) {
        // apply_patch 는 인자가 JSON 이 아니라 패치 글 그대로다.
      }
      final file = RegExp(
        r'\*\*\* (?:Update|Add|Delete) File: (.+)',
      ).firstMatch(args)?.group(1);
      return {'file_path': ?file, 'input': args};
    }
    if (args is Map) return args.cast<String, Object?>();
    return const {};
  }

  static (String, bool) _codexOutput(Object? out) {
    if (out is String) {
      try {
        final v = jsonDecode(out);
        if (v is Map) {
          final text = (v['output'] ?? '').toString();
          final meta = v['metadata'];
          final code = meta is Map ? meta['exit_code'] : v['exit_code'];
          return (text, code is int && code != 0);
        }
      } catch (_) {
        // 평문 출력.
      }
      return (out, false);
    }
    return (_resultText(out), false);
  }
}

/// 이어진 도구 호출 묶음. 한 턴에 도구가 수십 번이라 한 줄씩 세우면 말이 안 보인다.
class ToolRun {
  ToolRun(this.tools);
  final List<ChatTool> tools;
}

/// 칸을 그릴 줄로 — 이어진 도구는 한 묶음, 답 안 온 질문은 뺀다.
List<Object> groupRows(List<ChatItem> items) {
  final out = <Object>[];
  for (final it in items) {
    if (it is ChatAnswered && it.pairs == null) continue;
    if (it is ChatTool) {
      final last = out.isEmpty ? null : out.last;
      if (last is ToolRun) {
        last.tools.add(it);
      } else {
        out.add(ToolRun([it]));
      }
      continue;
    }
    out.add(it);
  }
  return out;
}

// ── 글 다듬기(데스크톱 아로나와 같은 규칙) ──────────────────────────────────────

const _metaBlocks = [
  ('<system-reminder>', '</system-reminder>'),
  ('<command-message>', '</command-message>'),
  ('<command-name>', '</command-name>'),
  ('<command-args>', '</command-args>'),
  ('<local-command-stdout>', '</local-command-stdout>'),
  ('<task-notification>', '</task-notification>'),
  ('<local-command-caveat>', '</local-command-caveat>'),
];

/// 시스템이 user 턴에 끼워 넣은 블록과 그림 자리표시 줄을 걷는다.
String stripMeta(String text) {
  var s = text;
  for (final (open, close) in _metaBlocks) {
    while (true) {
      final start = s.indexOf(open);
      if (start < 0) break;
      final end = s.indexOf(close, start + open.length);
      if (end < 0) {
        s = s.substring(0, start);
        break;
      }
      s = s.substring(0, start) + s.substring(end + close.length);
    }
  }
  s = s
      .split('\n')
      .where((l) {
        final t = l.trim();
        return !(t.startsWith('[Image: source:') ||
            t.startsWith('[Image: original ') ||
            (t.startsWith('[Image #') && t.endsWith(']')));
      })
      .join('\n');
  return s.replaceAll(RegExp(r'\[Image #\d+\]'), '(사진)').trim();
}

final _injection = RegExp(
  r'\[Request interrupted|^\s*##\s*Context Usage|^\s*Caveat:\s|^\s*This session is being continued from a previous conversation',
  caseSensitive: false,
);

/// 사람이 친 말처럼 보이지만 하네스가 넣은 것 — 압축 요약 이어가기 등.
bool isInjection(String text) => _injection.hasMatch(text);

/// `<teammate-message teammate_id="…">본문</teammate-message>` 이 턴 전체일 때만.
(String, String)? teammateMessage(String text) {
  final m = RegExp(
    r'^\s*<teammate-message\b([^>]*)>([\s\S]*?)</teammate-message>\s*$',
  ).firstMatch(text);
  if (m == null) return null;
  final name = RegExp(r'teammate_id="([^"]+)"').firstMatch(m[1]!)?.group(1);
  final body = m[2]!.trim();
  if (name == null || body.isEmpty || body.contains('<teammate-message')) {
    return null;
  }
  return (name.trim(), body);
}

/// AskUserQuestion 결과 글(`…answered: "질문"="답". …`)에서 질문↔답.
List<(String, String)> answeredPairs(String result, List<String> questions) {
  final found = <String, String>{};
  for (final m in RegExp(r'"([^"]+)"="([^"]*)"').allMatches(result)) {
    found[m[1]!] = m[2]!;
  }
  return [for (final q in questions) (q, found[q] ?? '—')];
}

final _ansi = RegExp(r'\x1b\[[0-9;?]*[ -/]*[@-~]');
String stripAnsi(String s) => s.replaceAll(_ansi, '');

/// 도구 한 줄의 꼬리 — 무엇에 썼는지를 사람 말로.
String toolSummary(String name, Map<String, Object?> input) {
  String? str(String k) {
    final v = input[k];
    if (v is String && v.trim().isNotEmpty) return v.trim();
    if (v is List && v.isNotEmpty && v.every((e) => e is String)) {
      final parts = v.cast<String>();
      // codex 셸은 ["bash","-lc","명령"] — 앞의 해석기는 사람이 읽을 거리가 아니다.
      return (parts.length >= 3 && parts[1] == '-lc'
              ? parts.last
              : parts.join(' '))
          .trim();
    }
    return null;
  }

  String base(String p) =>
      p.split('/').where((s) => s.isNotEmpty).lastOrNull ?? p;
  final picked = switch (name) {
    'Bash' => str('description') ?? str('command'),
    'Read' ||
    'Write' ||
    'Edit' ||
    'MultiEdit' ||
    'NotebookEdit' => str('file_path') == null ? null : base(str('file_path')!),
    'Grep' || 'Glob' => str('pattern'),
    'WebFetch' => str('url'),
    'WebSearch' || 'ToolSearch' => str('query'),
    'TaskCreate' => str('subject'),
    'Skill' => str('skill'),
    _ =>
      str('description') ??
          str('command') ??
          str('cmd') ??
          (str('file_path') == null ? null : base(str('file_path')!)) ??
          str('query') ??
          str('url') ??
          input.values.whereType<String>().firstOrNull?.trim(),
  };
  final line = (picked ?? '').split('\n').first;
  return line.length > 90 ? '${line.substring(0, 89)}…' : line;
}

/// `mcp__kasachrome__browser_click` → `browser_click`.
String toolLabel(String name) =>
    name.startsWith('mcp__') ? name.split('__').last : name;

String? _tag(String s, String name) =>
    RegExp('<$name>([\\s\\S]*?)</$name>').firstMatch(s)?.group(1);

bool _interrupted(Object? content) {
  final flat = content is String
      ? content
      : content is List
      ? content
            .whereType<Map>()
            .map((b) => b['text'] is String ? b['text'] as String : '')
            .join(' ')
      : '';
  return RegExp(r'^\s*\[Request interrupted by user').hasMatch(flat);
}

String _resultText(Object? content) {
  if (content is String) return content;
  if (content is List) {
    return content
        .whereType<Map>()
        .where((b) => b['type'] == 'text' && b['text'] is String)
        .map((b) => b['text'] as String)
        .join('\n');
  }
  return '';
}

Uint8List? _imageBytes(Object? source) {
  if (source is! Map || source['type'] != 'base64') return null;
  final data = source['data'];
  if (data is! String || data.isEmpty) return null;
  try {
    return base64Decode(data);
  } catch (_) {
    return null;
  }
}

DateTime? _time(Object? iso) =>
    iso is String ? DateTime.tryParse(iso)?.toLocal() : null;

String? _systemText(Map<String, Object?> ev) {
  switch (ev['subtype']) {
    case 'compact_boundary':
      final meta = ev['compactMetadata'];
      final pre = meta is Map ? meta['preTokens'] : null;
      return pre is num
          ? '대화를 압축했어요 · ${(pre / 1000).round()}k 토큰'
          : '대화를 압축했어요';
    case 'api_error':
      final err = ev['error'];
      final status = err is Map ? err['status'] : null;
      final attempt = ev['retryAttempt'];
      final max = ev['maxRetries'];
      return [
        'API 오류',
        if (status != null) '$status',
        if (attempt != null) '재시도 $attempt/$max',
      ].join(' · ');
  }
  return null;
}

String? _hookMessage(Object? stdout) {
  if (stdout is! String) return null;
  try {
    final v = jsonDecode(stdout.trim());
    final msg = v is Map ? v['systemMessage'] : null;
    return msg is String && msg.trim().isNotEmpty ? msg.trim() : null;
  } catch (_) {
    return null;
  }
}

String? _workflowName(Map<String, Object?> input) {
  final script = input['script'];
  if (script is! String) return null;
  return RegExp(r'''name:\s*['"]([^'"]+)['"]''').firstMatch(script)?.group(1);
}

// ── 화면에서 읽는 선택지 ────────────────────────────────────────────────────

class PromptOption {
  const PromptOption(this.index, this.label, {required this.current});

  /// 화면에 적힌 번호(1부터).
  final int index;
  final String label;
  final bool current;
}

class PromptMenu {
  const PromptMenu(this.title, this.options);
  final String title;
  final List<PromptOption> options;

  int get cursor => options.indexWhere((o) => o.current);
}

/// claude 의 선택 메뉴(권한·질문·`/model`)를 화면 글에서 찾는다. ❯ 커서가 실제로 찍힌
/// 줄이 있어야만 메뉴로 친다 — 답변 속 번호 목록을 선택지로 오인하지 않게.
PromptMenu? parsePromptMenu(List<String> lines) {
  final opts = <(PromptOption, int)>[];
  final re = RegExp(r'^\s*[│|]?\s*([❯●]?)\s*(\d+)\.\s+(.+?)\s*[│|]?\s*$');
  for (var i = 0; i < lines.length; i++) {
    final m = re.firstMatch(lines[i]);
    if (m == null) continue;
    final label = m[3]!.replaceFirst(RegExp(r'\s{2,}.*$'), '').trim();
    opts.add((
      PromptOption(int.parse(m[2]!), label, current: m[1]!.isNotEmpty),
      i,
    ));
  }
  if (opts.length < 2 || !opts.any((o) => o.$1.current)) return null;
  // 흩어진 번호 줄은 메뉴가 아니다. 다만 AskUserQuestion 은 선택지마다 설명 줄이 딸려
  // 벌어지므로, 번호가 1부터 빈틈없이 이어지면 선택지당 서너 줄까지 봐준다.
  final ordered = [
    for (final (i, o) in opts.indexed) o.$1.index == i + 1,
  ].every((b) => b);
  final spread = opts.last.$2 - opts.first.$2;
  if (spread > (ordered ? opts.length * 4 : opts.length + 2)) return null;
  final first = opts.first.$2;
  var title = '';
  for (var i = first - 1; i >= 0 && i >= first - 6; i--) {
    final t = lines[i].replaceAll(RegExp(r'^[\s│|]+|[\s│|]+$'), '');
    if (t.isEmpty || RegExp(r'^[─—\-╭╰╮╯>❯●]').hasMatch(t)) continue;
    title = t;
    break;
  }
  return PromptMenu(title, [for (final o in opts) o.$1]);
}
