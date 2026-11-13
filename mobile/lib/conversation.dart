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
    this.via,
    this.queued = false,
    this.sending = false,
  });

  /// user 턴 — 선생님 말이거나, [from] 이 있으면 사람 대신 다른 칸·카사텀이 넣은 말.
  final bool user;

  /// 사진이 붙으면 그 자리의 「(사진)」 표는 걷힌다 — 그래서 고칠 수 있다.
  String text;
  final DateTime? at;
  final String? from;

  /// [from] 이 넣은 말의 종류 — 쪽지·완료 보고.
  final String? via;

  /// 작업 중에 넣어 둔 예약 — 아직 학생이 안 읽었다.
  bool queued;

  /// 이 폰에서 보냈고 기록에는 아직 안 닿은 말([Conversation.echo]).
  final bool sending;
  final List<Uint8List> images = [];
  int omittedImages = 0;

  /// 붙은 사진 수만큼 앞에서부터 「(사진)」 표를 걷는다 — 사람이 친 「(사진)」은 뒤에 남는다.
  void addPhotos(Iterable<Uint8List> photos) {
    for (final p in photos) {
      images.add(p);
      final at = text.indexOf(_photoMark);
      if (at < 0) continue;
      text = (text.substring(0, at) + text.substring(at + _photoMark.length))
          .replaceAll(RegExp(r'[ \t]{2,}'), ' ')
          .trim();
    }
  }

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

  /// 고른 때(답 줄의 시각) — 물은 때가 아니다. 자리를 비운 사이 몇 시간 묵기도 한다.
  DateTime? at;
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
  Conversation({this.archived = false});
  final bool archived;
  bool _readingArchive = false;
  bool get _archive => archived || _readingArchive;

  void hydrateArchive(String raw, {required int next, required String source}) {
    _readingArchive = true;
    try { apply(raw, reset:true, next:next, source:source); }
    finally { _readingArchive = false; }
  }
  final List<ChatItem> items = [];

  /// 서버에 다음에 물을 자리(바이트). 0 이면 꼬리부터 다시.
  int offset = 0;

  /// 바뀔 때마다 오른다 — 화면이 이것만 보고 다시 그린다.
  int version = 0;

  /// [offset] 이 가리키는 기록 파일(서버 답의 `source`). 같은 칸 번호를 다음 칸이 물려받거나 칸이 세션을 갈아타면
  /// 달라진다 — codex 줄엔 `sessionId` 가 없어 줄만 보고는 못 가른다(10-13 옛 codex 대화 밑에 새 기록이 붙었다).
  String source = '';

  /// 이 폰에서 보내고 기록에 아직 안 닿은 말 — 보낸 순간 말풍선으로 선다. 기록에 같은 말이 오면 걷는다.
  final List<ChatBubble> echoes = [];

  String? _sessionId;
  final Map<String, ChatItem> _byToolId = {};
  final List<ChatBubble?> _queue = [];

  /// 방금 작업 중에 흡수된 예약 — 바로 뒤 `queued_command` 첨부가 그 말의 사진을 싣는다.
  ChatBubble? _absorbed;
  String? _lastCodexSay;
  String? _lastCodexPrompt;

  void clear() {
    items.clear();
    _byToolId.clear();
    _queue.clear();
    _sessionId = null;
    _lastCodexSay = null;
    _lastCodexPrompt = null;
    _absorbed = null;
    offset = 0;
    source = '';
    version++;
  }

  /// 보낸 말을 기록보다 먼저 세운다. 보내기가 실패하면 [withdraw].
  ChatBubble echo(String text, {List<Uint8List> photos = const []}) {
    final b = ChatBubble(
      user: true,
      text: text.trim(),
      at: DateTime.now(),
      sending: true,
    )..images.addAll(photos);
    echoes.add(b);
    version++;
    return b;
  }

  void withdraw(ChatBubble b) {
    if (echoes.remove(b)) version++;
  }

  /// 기록에 새로 선 내 말과 맞는 메아리를 걷는다. 끝내 안 닿은 것(메뉴가 먹은 글 등)은 오래 두지 않는다.
  void _settleEchoes(Iterable<ChatItem> fresh) {
    if (echoes.isEmpty) return;
    final before = echoes.length;
    for (final it in fresh) {
      if (it is! ChatBubble || !it.mine || it.sending) continue;
      final said = _squash(it.text);
      final i = echoes.indexWhere((e) {
        // 폰과 데스크톱 시계는 조금 어긋난다 — 보낸 때보다 한참 앞선 옛 말만 거른다.
        if (it.at != null && it.at!.isBefore(e.at!.subtract(_echoSkew))) return false;
        return e.text.isEmpty ? it.images.isNotEmpty : said.contains(_squash(e.text));
      });
      if (i >= 0) echoes.removeAt(i);
    }
    final now = DateTime.now();
    echoes.removeWhere((e) => now.difference(e.at!) > _echoLife);
    if (echoes.length != before) version++;
  }

  static const _echoSkew = Duration(seconds: 30);
  static const _echoLife = Duration(minutes: 2);

  static String _squash(String s) => s.replaceAll(RegExp(r'\s+'), ' ').trim();

  /// 서버 조각 하나. 세션이 바뀐 것을 알아채면 false — 호출부가 처음부터 다시 받는다.
  /// 서버는 파일이 줄어들 때만 꼬리를 다시 주므로, `/clear` 뒤 새 파일이 옛 자리보다
  /// 길면 새 파일의 중간부터 읽게 된다. 줄마다 붙은 sessionId 로 그 경우를 가른다.
  bool apply(String raw, {required bool reset, required int next, String source = ''}) {
    if (!reset && source.isNotEmpty && this.source.isNotEmpty && source != this.source) {
      return false;
    }
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
    final from = items.length;
    final touched = <ChatItem>[];
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
        final photoTarget = _claude(e);
        if (photoTarget != null) touched.add(photoTarget);
      }
    }
    _settleEchoes([...items.skip(from), ...touched]);
    offset = next;
    if (source.isNotEmpty) this.source = source;
    if (events.isNotEmpty || reset) version++;
    return true;
  }

  // ── claude ────────────────────────────────────────────────────────────────

  /// 앞서 선 말풍선에 사진을 붙였으면 그 말풍선 — 메아리 맞추기가 새 칸처럼 본다.
  ChatBubble? _claude(Map<String, Object?> ev) {
    if (ev['isSidechain'] == true) return null;
    final type = ev['type'];
    final at = _time(ev['timestamp']);
    switch (type) {
      case 'system':
        final text = _systemText(ev);
        if (text != null) items.add(ChatSystem(text));
        return null;
      case 'queue-operation':
        _queueOp(ev, at);
        return null;
      case 'attachment':
        final att = ev['attachment'];
        if (att is! Map) return null;
        switch (att['type']) {
          case 'hook_success':
            final msg = _hookMessage(att['stdout']);
            if (msg != null) items.add(ChatSystem(msg));
          case 'queued_command':
            return _queuedPhotos(att['prompt'], at);
        }
        return null;
      case 'user':
      case 'assistant':
        break;
      default:
        return null;
    }
    // 스킬 본문·caveat 처럼 사람이 친 적 없는 주입 — 선생님 말풍선으로 새면 안 된다.
    if (type == 'user' && ev['isMeta'] == true) return null;
    final message = ev['message'];
    final content = message is Map ? message['content'] : null;
    final user = type == 'user';
    if (user && _interrupted(content)) {
      if (items.isEmpty || items.last is! ChatInterrupted) {
        items.add(ChatInterrupted());
      }
      return null;
    }
    if (content is String) {
      if (user) {
        _userText(content, at);
      } else if (content.trim().isNotEmpty) {
        items.add(ChatBubble(user: false, text: content.trim(), at: at));
      }
      return null;
    }
    if (content is! List) return null;
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
          if (bytes == null && !_archive) break;
          final last = items.isEmpty ? null : items.last;
          if (last is ChatBubble && last.mine && last.at == at) {
            if (bytes == null) { last.omittedImages++; } else { last.addPhotos([bytes]); }
          } else {
            final image = ChatBubble(user: true, text: '', at: at);
            if (bytes == null) { image.omittedImages = 1; } else { image.images.add(bytes); }
            items.add(image);
          }
        case 'thinking':
          final t = block['thinking'];
          if (t is String && t.trim().isNotEmpty) {
            items.add(ChatThinking(t.trim()));
          }
        case 'tool_use':
          _toolUse(block);
        case 'tool_result':
          _toolResult(block, at);
      }
    }
    return null;
  }

  /// 작업 중에 보낸 말의 사진은 큐 기록(글만)이 아니라 흡수될 때의 `queued_command` 첨부에만 실린다.
  /// 그 말풍선을 글로 찾아 사진을 붙인다 — 없으면(큐 기록이 창 밖) 첨부만으로 세운다.
  ChatBubble? _queuedPhotos(Object? prompt, DateTime? at) {
    final absorbed = _absorbed;
    _absorbed = null;
    if (prompt is! List) return null;
    final photos = [
      for (final b in prompt)
        if (b is Map && b['type'] == 'image') ?_imageBytes(b['source']),
    ];
    if (photos.isEmpty) return null;
    final said = stripMeta(
      prompt
          .whereType<Map>()
          .where((b) => b['type'] == 'text')
          .map((b) => b['text'])
          .whereType<String>()
          .join('\n'),
    );
    if (isInjection(said)) return null;
    ChatBubble? target = absorbed != null && absorbed.text == said ? absorbed : null;
    for (var i = items.length - 1; target == null && i >= 0; i--) {
      final it = items[i];
      if (it is ChatBubble && it.mine && it.images.isEmpty && it.text == said) target = it;
    }
    if (target == null) {
      target = ChatBubble(user: true, text: said, at: at);
      items.add(target);
    }
    target.addPhotos(photos);
    return target;
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

  void _toolResult(Map<Object?, Object?> b, DateTime? at) {
    final id = b['tool_use_id'];
    final item = id is String ? _byToolId[id] : null;
    final text = _resultText(b['content']);
    if (item is ChatTool) {
      item.result = text;
      item.error = b['is_error'] == true;
    } else if (item is ChatAnswered) {
      item.pairs = answeredPairs(text, item.questions);
      item.at = at;
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
    final mate = relayedMessage(clean);
    items.add(
      mate == null
          ? ChatBubble(user: true, text: clean, at: at)
          : ChatBubble(user: true, text: mate.body, at: at, from: mate.name, via: mate.via),
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
        final mate = relayedMessage(clean);
        final b = ChatBubble(user: true, text: mate?.body ?? clean, at: at,
            from: mate?.name, via: mate?.via, queued: true);
        items.add(b);
        _queue.add(b);
      case 'dequeue' || 'popAll':
        final n = ev['operation'] == 'popAll' ? _queue.length : 1;
        for (var i = 0; i < n && _queue.isNotEmpty; i++) {
          final b = _queue.removeAt(0);
          if (b != null) items.remove(b);
        }
      case 'remove':
        final b = _queue.isEmpty ? null : _queue.removeAt(0);
        b?.queued = false;
        _absorbed = b;
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
        final images = p['images'] is List ? (p['images'] as List).length : 0;
        final local = p['local_images'] is List ? (p['local_images'] as List).length : 0;
        _codexSay(true, p['message'], at, omittedImages: _archive ? (images > local ? images : local) : 0);
      case ('event_msg', 'agent_message'):
        _codexSay(false, p['message'], at);
      case ('event_msg', 'item_completed'):
        final item = p['item'];
        if (item is! Map) return;
        switch (item['type']) {
          case 'UserMessage':
            final content = item['content'];
            final images = content is List ? content.whereType<Map>().where((b) => b['type'] == 'image' || b['type'] == 'input_image').length : 0;
            _codexSay(true, _codexText(content), at, omittedImages: _archive ? images : 0);
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

  void _codexSay(bool user, Object? raw, DateTime? at, {int omittedImages = 0}) {
    final t = raw is String ? raw.trim() : '';
    final signature = t.isEmpty && omittedImages > 0 ? '\u0000images:$omittedImages' : t;
    if (signature.isEmpty) return;
    if (signature == (user ? _lastCodexPrompt : _lastCodexSay)) {
      final last = items.lastOrNull;
      if (_archive && last is ChatBubble && last.mine && omittedImages > last.omittedImages) last.omittedImages = omittedImages;
      return;
    }
    if (user) {
      _lastCodexPrompt = signature;
    } else {
      _lastCodexSay = t;
      if (_archive && (_lastCodexPrompt?.startsWith('\u0000images:') ?? false)) _lastCodexPrompt = null;
    }
    final mate = user ? relayedMessage(stripMeta(t)) : null;
    items.add(ChatBubble(user: user, text: mate?.body ?? t, at: at,
        from: mate?.name, via: mate?.via)..omittedImages = omittedImages);
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
/// mod 가 `$.prompt.submit` 으로 넣은 말(tell·거울 입력)을 엔진은 「The `<mod>` plugin sent a message:」와 뒤 설명으로
/// 감싸 기록한다 — 사람이 보낸 말만 남긴다. 데스크톱 `unwrap_plugin_prompt` 와 같은 규칙.
final _pluginPrompt = RegExp(
  r'^The (\S+) plugin sent a message:[ \n]?([\s\S]*?)(\n\nThis is how Claude Code surfaces a prompt a plugin submits[\s\S]*)?$',
);

String stripMeta(String text) {
  // 붙여넣은 글(긴 tell 도)은 `<pasted_content id="…">글</pasted_content id="…">` 로 감겨 남는다 — 표만 걷는다.
  var s = (_pluginPrompt.firstMatch(text)?.group(2) ?? text).replaceAll(_pasted, '');
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
  return s.replaceAll(RegExp(r'\[Image #\d+\]'), _photoMark).trim();
}

/// 그림 자리표(`[Image #3]`)의 글 — 사진을 못 실은 말풍선은 이것으로 자리를 말한다.
const _photoMark = '(사진)';

final _pasted = RegExp(r'</?pasted_content\b[^>]*>');

final _injection = RegExp(
  r'\[Request interrupted|^\s*##\s*Context Usage|^\s*Caveat:\s|^\s*This session is being continued from a previous conversation',
  caseSensitive: false,
);

/// 사람이 친 말처럼 보이지만 하네스가 넣은 것 — 압축 요약 이어가기 등.
bool isInjection(String text) => _injection.hasMatch(text);

/// 사람이 아니라 다른 칸·카사텀·나쵸가 이 학생 입력칸에 넣은 말이면 보낸 쪽과 본문. 데스크톱
/// 발신 표식의 이름에서 기기를 뗀다 — `유즈@맥미니` → (`유즈`, `맥미니`). 데스크톱 `tell::split_sender`.
(String, String?) splitSender(String raw) {
  final at = raw.indexOf('@');
  if (at > 0 && at < raw.length - 1) {
    final who = raw.substring(0, at).trim();
    final machine = raw.substring(at + 1).trim();
    if (who.isNotEmpty && machine.isNotEmpty) return (who, machine);
  }
  return (raw.trim(), null);
}

/// `chat_view::parse::relayed` 와 같은 규칙 — 표식을 안 읽으면 남의 말이 선생님 말풍선에 담긴다.
({String name, String via, String body})? relayedMessage(String text) {
  final mate = teammateMessage(text);
  if (mate != null) return (name: mate.$1, via: '쪽지', body: mate.$2);
  final t = text.trim();
  final tell = RegExp(r'^⟦([^⟧\n]{1,48})⟧([\s\S]*)$').firstMatch(t);
  if (tell != null) {
    // 다른 기기 학생은 `⟦이름@기기⟧`(데스크톱 `tell::mark_sender_machine`) — 얼굴은 이름으로, 기기는 종류 자리에.
    final (name, machine) = splitSender(tell[1]!);
    return name.isEmpty
        ? null
        : (name: name, via: machine == null ? '쪽지' : '$machine 쪽지', body: tell[2]!.trim());
  }
  final done = RegExp(r'^\[(완료|실패|막힘|재시작 필요|승인 필요)\][ \t]*([^()\r\n]*)\((%[^()\s]+)\)\s*(?:—\s*)?([\s\S]*)$').firstMatch(t);
  if (done != null) {
    final name = done[2]!.trim();
    return (name: name.isEmpty ? done[3]! : name, via: '${done[1]} 보고', body: done[4]!.trim());
  }
  if (RegExp(r'^\[(완료|실패|막힘|재시작 필요|승인 필요)\]').hasMatch(t)) return null;
  if (t.startsWith('[쪽지 확인 못 함]')) {
    return (name: '카사텀', via: '쪽지 확인 못 함', body: t.substring('[쪽지 확인 못 함]'.length).trim());
  }
  final brief = assignedBrief(t);
  return brief == null ? null : (name: '카사텀', via: '맡긴 일', body: brief);
}

/// `summon`·제목 붙은 `tell` 이 새 일 끝에 다는 완료 보고 안내로 끝나는 말이면 그 안내 문단을 걷은 본문 —
/// 사람이 친 말엔 없다. 칸 밖 프로그램(나쵸 서버)은 `⟦이름⟧` 없이 맡겨, 그 일이 사람 말풍선에
/// 담겼다. 데스크톱 `chat_view::parse::assigned_brief` 와 같은 규칙.
String? assignedBrief(String text) {
  final cut = text.lastIndexOf('\n\n');
  if (cut < 0 || !text.startsWith('끝나면 `kasaterm-cli done ', cut + 2)) {
    return null;
  }
  var body = text.substring(0, cut);
  final start = body.lastIndexOf('\n\n');
  if (start >= 0 &&
      body.startsWith('Kasaterm: read `kasaterm-cli guide', start + 2)) {
    body = body.substring(0, start);
  }
  body = body.trim();
  return body.isEmpty ? null : body;
}

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
  const PromptOption(
    this.index,
    this.label, {
    required this.current,
    this.note = '',
    int? row,
    this.checked,
    this.input = false,
    bool? entry,
  })  : row = row ?? index - 1,
        entry = entry ?? input;

  /// 화면에 적힌 번호(1부터). 번호 없는 줄(여러 개 고르기의 「Submit」)은 0.
  final int index;
  final String label;
  final bool current;

  /// 이름 밑에 딸린 설명 줄(질문 창의 선택지 설명)이나 이름 옆 칸(`/model` 의 모델 설명). 없으면 빈 글.
  final String note;

  /// TUI 목록 안의 차례(0부터) — 굴러서 숨은 선택지와 번호 없는 줄까지 센 자리. ↑↓ 수는 이 차이다.
  final int row;

  /// 여러 개 고르기의 체크 상자. 상자 없는 선택지는 null.
  final bool? checked;

  /// 글을 쳐서 답하는 칸(질문의 「Type something」, 계획의 「Tell Claude what to change」). 빈 채로 Enter 를
  /// 보내면 답이 아니라 질문 전체 거절(「User declined to answer questions」)이 된다(2.1.323 실측).
  final bool input;

  /// 글 칸 자리 — 이미 글을 쳐 이름이 그 글로 바뀐 칸까지. 커서가 여기 있으면 Tab 이 탭을 안 옮긴다.
  final bool entry;
}

/// 질문 여러 개 창의 머리 탭 하나(`←  ☒ 언어  ☐ 테스트  ✔ Submit  →`).
class PromptTab {
  const PromptTab(this.label, {required this.done, required this.current});
  final String label;

  /// 답한 질문(☒).
  final bool done;

  /// 지금 탭 — 글자로는 안 갈리고 배경색으로만 칠해져 있어 화면이 준 색 칸([parsePromptMenu] 의 `lit`)으로 안다.
  final bool current;
}

class PromptMenu {
  const PromptMenu(
    this.title,
    this.options, {
    this.context = const [],
    this.tabs = const [],
    this.hiddenAbove,
    this.hiddenBelow,
  });
  final String title;
  final List<PromptOption> options;

  /// 제목 위 창 안의 줄들, 화면 차례 그대로 — 도구 이름·설명·명령 원문·계획. 데스크톱 `PromptMenu::context` 와 같은 재료.
  final List<String> context;

  final List<PromptTab> tabs;

  /// 창이 낮아 굴러서 숨은 선택지 바로 그 자리(row). 커서를 거기 옮기면 TUI 가 목록을 굴려 보여 준다.
  final int? hiddenAbove;
  final int? hiddenBelow;

  int get cursor => options.indexWhere((o) => o.current);
  bool get multi => options.any((o) => o.checked != null);
  int get currentTab => tabs.indexWhere((t) => t.current);

  PromptMenu withContext(List<String> lines) => PromptMenu(
        title,
        options,
        context: lines,
        tabs: tabs,
        hiddenAbove: hiddenAbove,
        hiddenBelow: hiddenBelow,
      );
}

/// 메뉴에서 한 번 할 일 — 커서에서 [moves] 칸(+면 ↓) 옮기고, [text] 를 치고, [enter] 면 Enter.
typedef MenuStep = ({int moves, String text, bool enter});

/// 선택지 [i] 를 고르는 키. 글 칸은 [text] 가 있어야 답이다 — 빈 글이면 null(빈 Enter 는 거절이 된다).
/// 여러 개 고르기의 글 칸은 치기만 해도 골라지니 Enter 를 안 보낸다(Enter 는 고름을 푼다).
MenuStep? menuStep(PromptMenu menu, int i, {String text = ''}) {
  if (i < 0 || i >= menu.options.length || menu.cursor < 0) return null;
  final o = menu.options[i];
  final moves = o.row - menu.options[menu.cursor].row;
  if (!o.input) return (moves: moves, text: '', enter: true);
  final typed = text.replaceAll(RegExp(r'[\r\n]+'), ' ').trim();
  if (typed.isEmpty) return null;
  return (moves: moves, text: typed, enter: o.checked == null);
}

/// 숨은 선택지 쪽으로 커서를 옮기는 칸 수. 그쪽에 숨은 것이 없으면 null.
int? menuReveal(PromptMenu menu, {required bool up}) {
  final target = up ? menu.hiddenAbove : menu.hiddenBelow;
  if (target == null || menu.cursor < 0) return null;
  return target - menu.options[menu.cursor].row;
}

/// 창을 가르는 가로줄(`────`·`▔▔▔▔`) — 창 머리 위나 질문 창 아래 칸막이.
bool _isRule(String t) => RegExp('[─━▔]').allMatches(t).length >= 8;

final _optionRe = RegExp(r'^\s*[│|]?\s*((?:[❯›●↑↓]\s*){0,2})(\d+)\.\s+(.+?)\s*[│|]?\s*$');
final _submitRe = RegExp(r'^\s*([❯›]?)\s*Submit\s*$');
final _checkboxRe = RegExp(r'^\[([ ✔✓xX×])\]\s+');
final _inputLabels = {'Type something.', 'Type something', 'Tell Claude what to change'};

/// `/model` 이 굴린 목록 끝에 다는 「… +2 models」 — 그쪽에 숨은 칸이 있다는 표식.
final _moreRe = RegExp(r'^\s*…\s*\+\d+');

/// 터미널 칸 수 — 한글·한자·전각은 두 칸. 접힌 줄을 가르는 데만 쓴다.
int _cells(String s) {
  var n = 0;
  for (final r in s.runes) {
    final wide = (r >= 0x1100 && r <= 0x115F) || (r >= 0x2E80 && r <= 0xA4CF) ||
        (r >= 0xAC00 && r <= 0xD7A3) || (r >= 0xF900 && r <= 0xFAFF) ||
        (r >= 0xFE30 && r <= 0xFE4F) || (r >= 0xFF00 && r <= 0xFF60) || (r >= 0xFFE0 && r <= 0xFFE6);
    n += wide ? 2 : 1;
  }
  return n;
}

/// 창 테(`│`)와 줄 끝의 굴림 표식(계획 상자 오른쪽 끝 `↓`)을 걷는다.
String _unframe(String line) =>
    line.replaceAll(RegExp(r'^[\s│|]+|[\s│|]+$'), '').replaceFirst(RegExp(r'\s{2,}[↑↓]$'), '');

/// A selected chevron distinguishes live choices from numbered response text. [lit] 은 줄 번호 → 그 줄에서
/// 배경색이 칠해진 글(질문 탭의 지금 탭을 가르는 데만 쓴다).
PromptMenu? parsePromptMenu(List<String> screen, {Map<int, String> lit = const {}}) {
  // (선택지, 줄, 굴림 표식 ↑/↓/'', 이름 옆 칸)
  final opts = <(PromptOption, int, String, String)>[];
  // Match the native bottom-anchored approval scan, not old choices in history.
  final last = screen.lastIndexWhere((line) => line.trim().isNotEmpty);
  if (last < 0) return null;
  final lines = [for (final line in screen) _leftOfPreview(line)];
  final start = _questionTop(screen, last) ?? (last - 13).clamp(0, last);
  for (var i = start; i <= last; i++) {
    final m = _optionRe.firstMatch(lines[i]);
    if (m == null) {
      // 여러 개 고르기의 번호 없는 「Submit」 — 체크 상자 선택지 바로 밑에만 선다.
      final s = _submitRe.firstMatch(lines[i]);
      if (s != null && opts.isNotEmpty && opts.last.$1.checked != null) {
        opts.add((PromptOption(0, 'Submit', current: s[1]!.isNotEmpty), i, '', ''));
      }
      continue;
    }
    final marks = m[1]!;
    final full = m[3]!;
    final gap = RegExp(r'\s{2,}').firstMatch(full);
    var label = (gap == null ? full : full.substring(0, gap.start)).trim();
    final side = gap == null ? '' : full.substring(gap.end).trim();
    bool? checked;
    final box = _checkboxRe.firstMatch(label);
    if (box != null) {
      checked = box[1] != ' ';
      label = label.substring(box.end);
    }
    opts.add((
      PromptOption(
        int.parse(m[2]!),
        label,
        current: marks.contains(RegExp('[❯›●]')),
        checked: checked,
        input: _inputLabels.contains(label),
      ),
      i,
      marks.contains('↑') ? '↑' : (marks.contains('↓') ? '↓' : ''),
      side,
    ));
  }
  if (opts.length < 2 || !opts.any((o) => o.$1.current)) return null;
  // 미리보기 질문 — 선택지 줄 오른쪽에 그림 상자가 섰다. 상자만 있는 줄(과 그 밑 메모 안내)은 메뉴의 일부가 아니고,
  // 선택지 설명은 왼쪽에 안 찍혀 이어지는 줄은 좁은 칸에 접힌 이름이다.
  final previewing = opts.any((o) => lines[o.$2] != screen[o.$2]);
  if (previewing) {
    for (var i = opts.first.$2 + 1; i <= last; i++) {
      final t = lines[i].trim();
      if (RegExp(r'^[│┌└├]').hasMatch(t) || t.startsWith('Notes:')) lines[i] = '';
    }
  }
  if (lines.skip(opts.last.$2 + 1).any((line) =>
      RegExp(r'^\s*[│|]?\s*[❯›]').hasMatch(line) && !_optionRe.hasMatch(line))) {
    return null;
  }
  // 흩어진 번호 줄은 메뉴가 아니다. 다만 AskUserQuestion 은 선택지마다 설명 줄이 딸려
  // 벌어지므로, 번호가 1부터 빈틈없이 이어지면 선택지당 서너 줄까지 봐준다. 창이 낮아 굴린 목록은
  // 앞이 숨어 2부터 서거나, 굴러가는 칸 밖에 남은 「Chat about this」 앞이 빈다. ↑·↓ 표식은 커서(❯)가
  // 그 가장자리에 서면 덮여 안 보이니(2.1.323 실측) 질문 창이면 표식 없이도 빈틈을 봐준다.
  final scrolled = opts.any((o) => o.$3.isNotEmpty) || opts.any((o) => o.$1.label == 'Chat about this');
  final numbers = [for (final o in opts) if (o.$1.index > 0) o.$1.index];
  var ordered = numbers.isNotEmpty && (numbers.first == 1 || scrolled);
  for (var k = 1; k < numbers.length && ordered; k++) {
    ordered = scrolled ? numbers[k] > numbers[k - 1] : numbers[k] == numbers[k - 1] + 1;
  }
  final spread = opts.last.$2 - opts.first.$2;
  if (spread > (ordered ? opts.length * 4 : opts.length + 2)) return null;
  final width = lines.fold<int>(0, (w, l) => _cells(l.trimRight()) > w ? _cells(l.trimRight()) : w);
  final first = opts.first.$2;
  var title = '';
  var titleAt = first;
  for (var i = first - 1; i >= 0 && i >= first - 6; i--) {
    final t = _unframe(lines[i]);
    if (t.isEmpty || _isRule(t) || RegExp(r'^[─—\-╭╰╮╯>❯›●]').hasMatch(t)) continue;
    title = t;
    titleAt = i;
    break;
  }
  // 창 폭에서 접힌 제목(`/model` 의 안내문, 좁은 칸의 긴 질문)은 윗줄이 폭 끝까지 찼다 — 이어 붙인다.
  while (title.isNotEmpty && titleAt > 0) {
    final above = _unframe(lines[titleAt - 1]);
    if (above.isEmpty || _isRule(above) || RegExp(r'^[─—\-╭╰╮╯>❯›●←╌┄┈·]').hasMatch(above)) break;
    final firstWord = title.split(' ').first;
    if (_cells(lines[titleAt - 1].trimRight()) + 1 + _cells(firstWord) <= width - 4) break;
    title = '$above $title';
    titleAt--;
  }
  // 글 칸 자리: 질문 창은 「Chat about this」 바로 앞(여러 개 고르기면 Submit 앞), 계획 승인은 「shift+tab」 설명이 딸린 칸.
  final chat = opts.indexWhere((o) => o.$1.label == 'Chat about this');
  var slot = chat - 1;
  if (slot >= 0 && opts[slot].$1.index == 0) slot--;
  var row = -1;
  final options = <PromptOption>[];
  int? hiddenAbove;
  int? hiddenBelow;
  for (var k = 0; k < opts.length; k++) {
    final (o, at, scroll, side) = opts[k];
    // 번호 없는 줄은 바로 앞 줄 다음 자리, 번호 줄은 번호에서 — 숨은 선택지를 건너뛰어도 자리가 맞는다.
    final submitsAbove = options.where((p) => p.index == 0).length;
    final before = row;
    row = o.index == 0 ? row + 1 : o.index - 1 + submitsAbove;
    // 앞에 건너뛴 자리가 있으면 거기 숨은 선택지가 있다 — 첫 줄 위면 위쪽, 줄 사이면 앞 줄 바로 밑.
    if (scroll == '↑' || (k == 0 && row > 0)) hiddenAbove = row - 1;
    if (k > 0 && row > before + 1) hiddenBelow ??= before + 1;
    if (scroll == '↓') hiddenBelow = row + 1;
    final end = k + 1 < opts.length ? opts[k + 1].$2 : (at + 5).clamp(0, lines.length);
    final numberCol = lines[at].indexOf(RegExp(r'\d'));
    final more = <String>[];
    for (var i = at + 1; i < end && o.index > 0; i++) {
      final line = lines[i];
      final indent = line.length - line.trimLeft().length;
      final t = line.trim();
      if (t.isEmpty || indent <= numberCol || _isRule(t) || _moreRe.hasMatch(line)) break;
      more.add(t);
    }
    var label = o.label;
    var note = side;
    if (more.isNotEmpty) {
      // 이름이 줄 끝까지 찼으면 다음 줄은 접힌 이름의 나머지다(Claude 의 긴 「Yes, and …」).
      if (previewing || _cells(lines[at].trimRight()) + 4 >= width) {
        label = '$label ${more.join(' ')}';
      } else {
        note = more.join(' ');
      }
    }
    options.add(PromptOption(
      o.index,
      label,
      current: o.current,
      note: note,
      row: row,
      checked: o.checked,
      input: o.input,
      entry: o.input || (k == slot && o.index > 0) || note == 'shift+tab to approve with this feedback',
    ));
  }
  final lastAt = opts.last.$2;
  if (lines.skip(lastAt + 1).take(2).any(_moreRe.hasMatch)) hiddenBelow ??= options.last.row + 1;
  if (lines.sublist((first - 2).clamp(0, first), first).any(_moreRe.hasMatch) && options.first.row > 0) {
    hiddenAbove ??= options.first.row - 1;
  }
  // 질문 여러 개(와 여러 개 고르기)의 머리 탭 줄 — 창 맨 위 칸막이 바로 밑.
  var tabs = const <PromptTab>[];
  var tabsAt = -1;
  for (var i = titleAt - 1; i >= 0 && i >= titleAt - 12; i--) {
    final t = _unframe(lines[i]);
    if (_isRule(t)) break;
    final found = _parseTabs(t, lit[i]);
    if (found.isNotEmpty) {
      tabs = found;
      tabsAt = i;
      break;
    }
  }
  if (tabs.isNotEmpty && !tabs.any((t) => t.current) && title == 'Ready to submit your answers?') {
    tabs = [
      for (final (k, t) in tabs.indexed)
        PromptTab(t.label, done: t.done, current: k == tabs.length - 1 && t.label == 'Submit'),
    ];
  }
  // 탭 창은 탭 줄 밑이 다 창 안이다 — 검토 창의 「● 질문」「→ 답」까지 싣는다.
  final context = tabsAt >= 0
      ? [
          for (var i = tabsAt + 1; i < titleAt; i++)
            if (_unframe(lines[i]).isNotEmpty) _unframe(lines[i]),
        ]
      : _windowContext(lines, titleAt);
  return PromptMenu(
    title,
    options,
    context: context,
    tabs: tabs,
    hiddenAbove: hiddenAbove,
    hiddenBelow: hiddenBelow,
  );
}

List<PromptTab> _parseTabs(String line, String? lit) {
  final m = RegExp(r'^←\s+(.+?)\s+→$').firstMatch(line);
  if (m == null) return const [];
  final on = lit?.trim() ?? '';
  final tabs = <PromptTab>[];
  for (final part in m[1]!.split(RegExp(r'\s{2,}'))) {
    final t = RegExp(r'^([☐☒✔✓])\s+(.+)$').firstMatch(part.trim());
    if (t == null) return const [];
    final label = t[2]!;
    tabs.add(PromptTab(
      label,
      done: t[1] == '☒',
      current: on.isNotEmpty && (on == part.trim() || on == label),
    ));
  }
  return tabs.length >= 2 ? tabs : const [];
}

/// 제목 위 창 안의 줄. 제목 바로 위가 칸막이면(계획 승인 — 계획 상자가 제목 위 칸막이 사이에 선다) 한 번 건너
/// 다음 칸막이까지 싣는다. 그 칸막이를 못 만나면 창이 아니니 아무것도 안 싣는다.
List<String> _windowContext(List<String> lines, int top) {
  final out = <String>[];
  var crossed = false;
  for (var i = top - 1; i >= 0 && i >= top - (crossed ? 40 : 16); i--) {
    final t = _unframe(lines[i]);
    if (t.isEmpty || RegExp(r'^[╌┄┈·]+$').hasMatch(t) || RegExp(r'^[↑↓]$').hasMatch(t)) continue;
    if (_isRule(t)) {
      if (out.isEmpty && !crossed) {
        crossed = true;
        continue;
      }
      return out.reversed.toList();
    }
    if (RegExp(r'^[╭╰•⏺●❯›>⎿✻✢✽]').hasMatch(t)) break;
    out.add(t);
  }
  return crossed ? const [] : out.reversed.toList();
}

/// 선택지 이름 뒤 두 칸 넘는 틈에 이어 선 상자(미리보기 그림)를 떼어 낸다. 글자 칸으로 자르지 않는다 — 한글은
/// 화면에서 두 칸인데 줄 글에선 한 자라 칸 번호가 줄마다 어긋난다. 줄 머리의 테(`│ 1. Yes │`)는 앞에 글이 없어 남는다.
String _leftOfPreview(String line) {
  final m = RegExp(r'(?<=\S)\s{2,}[│┌└├]').firstMatch(line);
  return m == null ? line : line.substring(0, m.start);
}

/// 질문 창(`Chat about this` 가 딸린 고르기)의 맨 위 칸막이. 미리보기가 붙으면 선택지가 그림 상자 높이만큼 위로
/// 밀려 바닥 13줄 밖에 선다(10-13 유우카 칸 — 대화 보기에 질문이 안 떴다). 창 안만 훑어 지난 대화의 번호 줄은 안 섞는다.
int? _questionTop(List<String> lines, int last) {
  final from = (last - 60).clamp(0, last);
  final bottom = lines.sublist(from, last + 1).map((l) => l.trim()).toList();
  if (!bottom.any((t) => t.contains('Chat about this')) ||
      !bottom.any((t) => t.startsWith('Enter to select'))) {
    return null;
  }
  // 가로줄만으로 된 줄 — 미리보기 상자의 `└────┘` 도 가로줄이 많지만 칸막이가 아니다.
  final rules = [
    for (var i = last; i >= from; i--)
      if (RegExp(r'^[─━]{8,}$').hasMatch(lines[i].trim())) i,
  ];
  return rules.length >= 2 ? rules[1] : null;
}

/// mod 가 알린 승인 요청 하나 — 도구와 입력 원문.
class ModPermission {
  const ModPermission({required this.id, required this.tool, required this.input, this.preview = ''});
  final String id;
  final String tool;
  final Object? input;
  final String preview;

  /// 카드에 펼 입력 원문 줄 — Bash 는 명령 전부, 파일 도구는 경로와 바뀔 내용, 그 밖엔 JSON. 데스크톱 `live::input_lines`.
  List<String> get lines {
    final m = input is Map ? input as Map : const {};
    String s(String k) => m[k] is String ? m[k] as String : '';
    final out = <String>[];
    switch (tool) {
      case 'Bash':
        out.addAll(s('command').split('\n'));
        if (s('description').isNotEmpty) out.add(s('description'));
      case 'Write':
        out.add(s('file_path'));
        out.addAll(s('content').split('\n'));
      case 'Edit' || 'MultiEdit' || 'NotebookEdit':
        out.add(s('file_path'));
        out.addAll(s('old_string').split('\n').map((l) => '- $l'));
        out.addAll(s('new_string').split('\n').map((l) => '+ $l'));
      case 'ExitPlanMode':
        out.addAll(s('plan').split('\n'));
      default:
        out.addAll(input == null ? [preview] : const JsonEncoder.withIndent('  ').convert(input).split('\n'));
    }
    return [for (final l in out) if (l.trim().isNotEmpty) l];
  }
}

/// 그 칸 mod 의 지금(`/term/mod-live`). 기록 파일은 메시지가 끝나야 써지니 「무엇을 하는 중」과 「무엇을 묻는 중」은
/// 이것으로 기다림 없이 그린다. 데스크톱 `chat_view/live.rs` 와 같은 규칙.
class ModLive {
  const ModLive({
    this.live = false,
    this.seq = 0,
    this.session = '',
    this.turnOpen = false,
    this.compacting = false,
    this.question = false,
    this.tools = const [],
    this.permissions = const [],
  });

  factory ModLive.fromJson(Map j) {
    String text(Object? m, String k) => m is Map && m[k] is String ? m[k] as String : '';
    return ModLive(
      live: j['live'] == true,
      seq: j['seq'] is int ? j['seq'] as int : 0,
      session: text(j, 'session'),
      turnOpen: j['turn_open'] == true,
      compacting: j['compacting'] == true,
      question: j['question'] != null,
      tools: [
        for (final t in (j['tools'] is List ? j['tools'] as List : const []))
          (tool: text(t, 'tool'), label: text(t, 'label')),
      ],
      // 자리에서 답한 요청은 원본이 닫아 여기 안 온다. 도구 호출이 승인을 감싸 같은 id 의 도구가 함께 돈다.
      permissions: [
        for (final p in (j['permissions'] is List ? j['permissions'] as List : const []))
            ModPermission(
              id: text(p, 'id'),
              tool: text(p, 'tool'),
              input: p is Map ? p['input'] : null,
              preview: text(p, 'preview'),
            ),
      ],
    );
  }

  final bool live;
  final int seq;
  final String session;
  final bool turnOpen;
  final bool compacting;
  final bool question;
  final List<({String tool, String label})> tools;
  final List<ModPermission> permissions;

  /// 작업 줄의 지금 하는 일. mod 칸이 아니면 null.
  String? get doing {
    if (!live) return null;
    if (compacting) return '대화 압축 중';
    if (permissions.isNotEmpty) {
      return switch (permissions.first.tool) {
        'AskUserQuestion' => '답 기다림',
        'ExitPlanMode' => '계획 승인 기다림',
        final tool => '승인 기다림 · ${toolLabel(tool)}',
      };
    }
    if (question) return '답 기다림';
    if (tools.isEmpty) return null;
    final t = tools.last;
    final name = toolLabel(t.tool);
    return t.label.isEmpty ? name : '$name · ${t.label}';
  }

  /// 화면 선택지 [i] 를 mod 결정으로 보낼 수 있으면 (허락인가, 그 요청). 승인 창 맨 앞 「Yes」와 맨 끝 「No」만 —
  /// 나머지(다시 묻지 않기·계획 승인·질문)는 TUI 가 하는 일을 그대로 하도록 화면 키로 고른다.
  (bool, ModPermission)? decisionFor(PromptMenu menu, int i) {
    if (!live || permissions.isEmpty || menu.options.isEmpty) return null;
    final p = permissions.first;
    if (p.tool == 'ExitPlanMode' || p.tool == 'AskUserQuestion') return null;
    if (menu.options.first.label != 'Yes' || i < 0 || i >= menu.options.length) return null;
    if (i == 0) return (true, p);
    final label = menu.options[i].label;
    if (i == menu.options.length - 1 && (label == 'No' || label.startsWith('No,'))) return (false, p);
    return null;
  }
}
