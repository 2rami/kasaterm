import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';

import '../conversation.dart';
import '../links.dart';
import '../server.dart';
import '../status_style.dart';
import '../student_art.dart';
import '../term_session.dart';

/// 학생 화면의 두 얼굴 — 격자 그대로(터미널)와 말풍선(대화). 웹이 「웹 터미널」과
/// 「대화 보기」를 주소 둘로 가른 것(2026-08-25)을 한 화면 안의 전환으로 옮겼다.
enum PaneView { terminal, chat }

/// 마지막으로 고른 얼굴 — 다음 학생도 그 얼굴로 연다.
final paneView = ValueNotifier<PaneView>(PaneView.terminal);

class PaneViewPrefs {
  const PaneViewPrefs();

  static const _key = 'pane.view';
  static const _storage = FlutterSecureStorage();

  Future<PaneView> load() async {
    try {
      final v = await _storage.read(key: _key);
      return PaneView.values.firstWhere(
        (m) => m.name == v,
        orElse: () => PaneView.terminal,
      );
    } catch (_) {
      return PaneView.terminal;
    }
  }

  Future<void> save(PaneView v) async {
    try {
      await _storage.write(key: _key, value: v.name);
    } catch (_) {
      // 저장소가 막혀도 이번 실행은 고른 대로 보인다.
    }
  }
}

/// 앱바 밑의 「터미널 | 대화」. 폭을 반씩 나눠 엄지가 어디를 눌러도 닿게 한다.
class PaneViewSwitch extends StatelessWidget implements PreferredSizeWidget {
  const PaneViewSwitch({super.key});

  static const height = 44.0;

  @override
  Size get preferredSize => const Size.fromHeight(height);

  @override
  Widget build(BuildContext context) => ValueListenableBuilder(
    valueListenable: paneView,
    builder: (context, view, _) => Padding(
      padding: const EdgeInsets.fromLTRB(12, 0, 12, 4),
      child: SegmentedButton<PaneView>(
        expandedInsets: EdgeInsets.zero,
        showSelectedIcon: false,
        style: const ButtonStyle(visualDensity: VisualDensity.compact),
        segments: const [
          ButtonSegment(
            value: PaneView.terminal,
            icon: Icon(Icons.terminal, size: 18),
            label: Text('터미널'),
          ),
          ButtonSegment(
            value: PaneView.chat,
            icon: Icon(Icons.forum_outlined, size: 18),
            label: Text('대화'),
          ),
        ],
        selected: {view},
        onSelectionChanged: (s) {
          paneView.value = s.first;
          const PaneViewPrefs().save(s.first);
        },
      ),
    ),
  );
}

/// 대화 보기의 입력줄. 바로 치기가 없다 — 화면의 입력상자를 안 보고 치니 한 번에 보낸다.
class ChatComposer extends StatelessWidget {
  const ChatComposer({
    super.key,
    required this.controller,
    required this.focusNode,
    required this.enabled,
    required this.onSend,
    this.leading,
    this.onStop,
  });

  final TextEditingController controller;
  final FocusNode focusNode;
  final bool enabled;
  final VoidCallback onSend;
  final Widget? leading;

  /// 작업 중일 때만 — esc 로 멈춘다.
  final VoidCallback? onStop;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(4, 4, 8, 8),
    child: Row(
      crossAxisAlignment: CrossAxisAlignment.end,
      children: [
        ?leading,
        Expanded(
          child: TextField(
            controller: controller,
            focusNode: focusNode,
            enabled: enabled,
            minLines: 1,
            maxLines: 5,
            textInputAction: TextInputAction.newline,
            // 16px 아래로 내리면 iOS 가 포커스 때 화면을 확대한다.
            style: const TextStyle(fontSize: 16),
            decoration: const InputDecoration(
              hintText: '메시지 보내기',
              isDense: true,
            ),
          ),
        ),
        if (onStop != null) ...[
          const SizedBox(width: 4),
          IconButton(
            onPressed: onStop,
            icon: const Icon(Icons.stop_circle_outlined),
            tooltip: '멈추기 (esc)',
          ),
        ],
        const SizedBox(width: 4),
        IconButton.filled(
          onPressed: enabled ? onSend : null,
          icon: const Icon(Icons.send),
          tooltip: '보내기',
        ),
      ],
    ),
  );
}

/// 한 pane 의 대화를 말풍선으로. 격자 소켓(`session`)은 그대로 살려 두고 화면 글에서
/// 선택 메뉴를 읽어 단추로 세운다 — 승인·질문을 대화에서 바로 답한다.
class ConversationView extends StatefulWidget {
  const ConversationView({
    super.key,
    required this.server,
    required this.pane,
    required this.session,
    required this.accent,
    required this.onTerminal,
    this.bottomTick = 0,
  });

  final Server server;
  final Pane pane;
  final TermSession session;
  final Color accent;
  final VoidCallback onTerminal;

  /// 보낼 때마다 오른다 — 맨 아래로 내리고 곧바로 한 번 더 받는다.
  final int bottomTick;

  @override
  State<ConversationView> createState() => _ConversationViewState();
}

class _ConversationViewState extends State<ConversationView>
    with WidgetsBindingObserver {
  static const _pollEvery = Duration(milliseconds: 1500);

  final _conv = Conversation();
  final _scroll = ScrollController();
  final _open = Set<Object>.identity();
  List<Object> _rows = const [];
  int _rowsVersion = -1;
  Timer? _timer;
  bool _polling = false;
  bool _loaded = false;
  bool _missing = false;
  String? _error;
  bool _awayFromBottom = false;
  DateTime _menuHold = DateTime.fromMillisecondsSinceEpoch(0);

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _scroll.addListener(_onScroll);
    _start();
  }

  @override
  void didUpdateWidget(ConversationView old) {
    super.didUpdateWidget(old);
    if (old.bottomTick != widget.bottomTick) {
      _toBottom();
      Timer(const Duration(milliseconds: 400), _poll);
    }
  }

  @override
  void dispose() {
    _timer?.cancel();
    WidgetsBinding.instance.removeObserver(this);
    _scroll.dispose();
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    switch (state) {
      case AppLifecycleState.resumed:
        _start();
      case AppLifecycleState.paused:
      case AppLifecycleState.detached:
      case AppLifecycleState.hidden:
        _timer?.cancel();
        _timer = null;
      case AppLifecycleState.inactive:
        break;
    }
  }

  void _start() {
    _poll();
    _timer ??= Timer.periodic(_pollEvery, (_) => _poll());
  }

  Future<void> _poll() async {
    if (_polling || !mounted) return;
    _polling = true;
    try {
      final pane = widget.pane;
      final chunk = await widget.server.transcriptRaw(
        pane.id,
        _conv.offset,
        machine: pane.machine,
      );
      if (!mounted) return;
      if (chunk == null) {
        setState(() {
          _missing = true;
          _loaded = true;
          _error = null;
        });
        return;
      }
      if (!_conv.apply(chunk.raw, reset: chunk.reset, next: chunk.offset)) {
        // 세션이 바뀌었다 — 다음 바퀴가 꼬리부터 다시 받는다.
        _conv.clear();
        _open.clear();
      }
      if (_loaded && !_missing && _rowsVersion == _conv.version) return;
      setState(() {
        _loaded = true;
        _missing = false;
        _error = null;
      });
    } on ServerException catch (e) {
      // 한 번 못 받은 것 — 이미 그린 대화는 그대로 두고, 처음이면 이유를 보인다.
      if (mounted && !_loaded) setState(() => _error = e.message);
    } finally {
      _polling = false;
    }
  }

  void _onScroll() {
    final away = _scroll.hasClients && _scroll.offset > 240;
    if (away != _awayFromBottom) setState(() => _awayFromBottom = away);
  }

  void _toBottom() {
    if (!_scroll.hasClients) return;
    _scroll.animateTo(
      0,
      duration: const Duration(milliseconds: 220),
      curve: Curves.easeOut,
    );
  }

  List<Object> get rows {
    if (_rowsVersion != _conv.version) {
      _rows = groupRows(_conv.items);
      _rowsVersion = _conv.version;
    }
    return _rows;
  }

  /// 화면에서 읽은 메뉴의 한 칸을 고른다 — 지금 커서 자리에서 그만큼 ↑↓ 옮기고 Enter.
  /// 번호 단축키는 AskUserQuestion 의 다중 선택에서 뜻이 달라 쓰지 않는다.
  void _pick(PromptMenu menu, int i) {
    final s = widget.session;
    final delta = i - (menu.cursor < 0 ? 0 : menu.cursor);
    for (var k = 0; k < delta.abs(); k++) {
      s.arrow(delta > 0 ? 'B' : 'A');
    }
    s.sendText('\r');
    HapticFeedback.selectionClick();
    setState(() => _menuHold = DateTime.now().add(_menuHoldFor));
    _toBottom();
  }

  void _dismiss() {
    widget.session.sendText('\x1b');
    setState(() => _menuHold = DateTime.now().add(_menuHoldFor));
  }

  /// 고른 직후엔 화면이 아직 옛 메뉴를 그리고 있다 — 두 번 누르지 않게 잠깐 접는다.
  static const _menuHoldFor = Duration(milliseconds: 1200);

  PromptMenu? get _menu {
    if (DateTime.now().isBefore(_menuHold)) return null;
    final lines = [
      for (final row in widget.session.grid.lines)
        row.map((r) => r.text).join(),
    ];
    return parsePromptMenu(lines);
  }

  @override
  Widget build(BuildContext context) {
    final menu = widget.session.state == TermState.connected ? _menu : null;
    return Column(
      children: [
        Expanded(child: _body(context)),
        if (menu != null)
          _MenuCard(
            menu: menu,
            accent: widget.pane.kind == 'permission'
                ? StatusStyle.attention
                : widget.accent,
            onPick: (i) => _pick(menu, i),
            onDismiss: _dismiss,
            onTerminal: widget.onTerminal,
          ),
      ],
    );
  }

  Widget _body(BuildContext context) {
    final theme = Theme.of(context);
    if (!_loaded) {
      return _error == null
          ? const Center(child: CircularProgressIndicator.adaptive())
          : _Empty(
              icon: Icons.cloud_off_outlined,
              title: '대화를 못 받았어요',
              body: _error!,
              onTerminal: widget.onTerminal,
            );
    }
    final list = rows;
    if (_missing || list.isEmpty) {
      return _Empty(
        icon: Icons.forum_outlined,
        title: '아직 대화가 없어요',
        body: !_missing
            ? '말을 걸면 여기에 떠요.'
            : (widget.pane.mirrorOf ?? '').isNotEmpty
            ? '${widget.pane.mirrorOf} 의 거울이라 대화 기록은 그 기계에 있어요. 허브의 ${widget.pane.mirrorOf} 칸에서 열면 보여요.'
            : '이 창에 묶인 대화 기록이 아직 없어요. claude 가 첫 답을 하면 여기에 떠요.',
        onTerminal: widget.onTerminal,
      );
    }
    final pane = widget.pane;
    final face = StudentFace(
      slug: pane.slug,
      url: pane.slug == null
          ? null
          : widget.server.avatar(pane.slug!, machine: pane.machine),
      size: 30,
    );
    final md = _markdownStyle(theme);
    final typing = pane.isBusy;
    final extra = typing ? 1 : 0;
    return LayoutBuilder(
      builder: (context, box) {
        final maxBubble = box.maxWidth * 0.8;
        return Stack(
          children: [
            ListView.builder(
              controller: _scroll,
              reverse: true,
              padding: const EdgeInsets.fromLTRB(12, 8, 12, 12),
              itemCount: list.length + extra,
              itemBuilder: (context, i) {
                if (i < extra) {
                  return _Typing(face: face, label: pane.busyLabel);
                }
                final at = list.length - 1 - (i - extra);
                return _row(context, list, at, face, md, maxBubble);
              },
            ),
            if (_awayFromBottom)
              Positioned(
                right: 12,
                bottom: 12,
                child: IconButton.filledTonal(
                  onPressed: _toBottom,
                  tooltip: '맨 아래로',
                  icon: const Icon(Icons.keyboard_arrow_down),
                ),
              ),
          ],
        );
      },
    );
  }

  Widget _row(
    BuildContext context,
    List<Object> list,
    int at,
    Widget face,
    MarkdownStyleSheet md,
    double maxBubble,
  ) {
    final row = list[at];
    Object? sender(int j) {
      if (j < 0 || j >= list.length) return null;
      final r = list[j];
      return r is ChatBubble ? (r.mine ? '' : (r.from ?? '\u0000')) : null;
    }

    final me = sender(at);
    final head = me == null || sender(at - 1) != me;
    final tail = me == null || sender(at + 1) != me;
    return switch (row) {
      ChatBubble b => _Bubble(
        bubble: b,
        name: b.from ?? widget.pane.displayName,
        face: b.from == null ? face : null,
        accent: widget.accent,
        head: head,
        tail: tail,
        maxWidth: maxBubble,
        md: md,
      ),
      ToolRun r => _ToolRunCard(
        run: r,
        open: _open,
        live: widget.pane.isBusy,
        onToggle: (o) => setState(() {
          if (!_open.remove(o)) _open.add(o);
        }),
      ),
      ChatThinking t => _Fold(
        icon: Icons.psychology_outlined,
        label: '생각',
        text: t.text,
        open: _open.contains(t),
        onTap: () => setState(() {
          if (!_open.remove(t)) _open.add(t);
        }),
      ),
      ChatOutput o => _Fold(
        icon: Icons.subject,
        label: '출력',
        text: o.text,
        mono: true,
        open: _open.contains(o),
        onTap: () => setState(() {
          if (!_open.remove(o)) _open.add(o);
        }),
      ),
      ChatCommand c => _CommandChip(command: c),
      ChatAnswered a => _AnsweredCard(pairs: a.pairs ?? const []),
      ChatLaunch l => _Note(
        icon: Icons.call_split,
        text: '서브에이전트 · ${l.label}',
      ),
      ChatSystem s => _Note(icon: Icons.info_outline, text: s.text),
      ChatInterrupted() => const _Note(
        icon: Icons.stop_circle_outlined,
        text: '작업을 멈췄어요',
      ),
      _ => const SizedBox.shrink(),
    };
  }

  MarkdownStyleSheet _markdownStyle(ThemeData theme) {
    final scheme = theme.colorScheme;
    final body = theme.textTheme.bodyMedium?.copyWith(
      fontSize: 15,
      height: 1.5,
      color: scheme.onSurface,
    );
    final mono = TextStyle(
      fontFamily: 'TermMono',
      fontSize: 13,
      color: scheme.onSurface,
      backgroundColor: scheme.surfaceContainerHighest,
    );
    return MarkdownStyleSheet.fromTheme(theme).copyWith(
      p: body,
      listBullet: body,
      tableBody: body?.copyWith(fontSize: 13),
      tableHead: body?.copyWith(fontSize: 13, fontWeight: FontWeight.w700),
      h1: body?.copyWith(fontSize: 18, fontWeight: FontWeight.w700),
      h2: body?.copyWith(fontSize: 17, fontWeight: FontWeight.w700),
      h3: body?.copyWith(fontSize: 16, fontWeight: FontWeight.w700),
      code: mono,
      codeblockPadding: const EdgeInsets.all(10),
      codeblockDecoration: BoxDecoration(
        color: scheme.surfaceContainerHighest,
        borderRadius: BorderRadius.circular(8),
      ),
      blockquoteDecoration: BoxDecoration(
        border: Border(left: BorderSide(color: scheme.outline, width: 3)),
      ),
      blockquotePadding: const EdgeInsets.fromLTRB(10, 2, 0, 2),
      a: body?.copyWith(
        color: scheme.primary,
        decoration: TextDecoration.underline,
      ),
    );
  }
}

/// 모모톡의 선생님 말풍선 파랑 — 데스크톱 아로나와 같은 고정색이라 밝기와 무관하다.
const _teacherBlue = Color(0xff3493f9);
const _queuedBg = Color(0xfffff3d6);
const _queuedInk = Color(0xff5f5000);

class _Bubble extends StatelessWidget {
  const _Bubble({
    required this.bubble,
    required this.name,
    required this.face,
    required this.accent,
    required this.head,
    required this.tail,
    required this.maxWidth,
    required this.md,
  });

  final ChatBubble bubble;
  final String name;
  final Widget? face;
  final Color accent;
  final bool head;
  final bool tail;
  final double maxWidth;
  final MarkdownStyleSheet md;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final mine = bubble.mine;
    final bg = mine
        ? (bubble.queued ? _queuedBg : _teacherBlue)
        : scheme.surface;
    final ink = mine
        ? (bubble.queued ? _queuedInk : Colors.white)
        : scheme.onSurface;
    final r = const Radius.circular(16);
    final corner = const Radius.circular(4);
    final text = bubble.text;
    final content = Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        for (final img in bubble.images) _Photo(bytes: img),
        if (text.isNotEmpty)
          mine
              ? Text(
                  text,
                  style: TextStyle(fontSize: 15, height: 1.45, color: ink),
                )
              : MarkdownBody(
                  data: text,
                  styleSheet: md,
                  softLineBreak: true,
                  onTapLink: (_, href, _) {
                    if (href != null) showLinkSheet(context, href);
                  },
                ),
      ],
    );
    final box = GestureDetector(
      onLongPress: text.isEmpty ? null : () => _copy(context, text),
      child: Container(
        constraints: BoxConstraints(maxWidth: maxWidth),
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
        decoration: BoxDecoration(
          color: bg,
          borderRadius: BorderRadius.only(
            topLeft: r,
            topRight: r,
            bottomLeft: mine ? r : corner,
            bottomRight: mine ? corner : r,
          ),
          border: mine ? null : Border.all(color: scheme.outline),
        ),
        child: content,
      ),
    );
    final clock = tail && bubble.at != null ? _clock(bubble.at!) : null;
    final meta = theme.textTheme.labelSmall?.copyWith(
      color: scheme.onSurfaceVariant,
    );
    if (mine) {
      return Padding(
        padding: EdgeInsets.only(top: head ? 10 : 3),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.end,
          children: [
            if (bubble.queued && head)
              Padding(
                padding: const EdgeInsets.only(bottom: 3, right: 3),
                child: Text(
                  '예약 · 대기 중',
                  style: meta?.copyWith(
                    color: const Color(0xffb58a00),
                    fontWeight: FontWeight.w700,
                  ),
                ),
              ),
            Row(
              mainAxisAlignment: MainAxisAlignment.end,
              crossAxisAlignment: CrossAxisAlignment.end,
              children: [
                if (clock != null) ...[
                  Text(clock, style: meta),
                  const SizedBox(width: 5),
                ],
                Flexible(child: box),
              ],
            ),
          ],
        ),
      );
    }
    return Padding(
      padding: EdgeInsets.only(top: head ? 10 : 3),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.end,
        children: [
          SizedBox(width: 30, child: tail ? face ?? _Initial(name) : null),
          const SizedBox(width: 8),
          Flexible(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                if (head)
                  Padding(
                    padding: const EdgeInsets.only(left: 3, bottom: 3),
                    child: Text(
                      name,
                      style: theme.textTheme.labelMedium?.copyWith(
                        color: accent,
                        fontWeight: FontWeight.w800,
                      ),
                    ),
                  ),
                Row(
                  crossAxisAlignment: CrossAxisAlignment.end,
                  children: [
                    Flexible(child: box),
                    if (clock != null) ...[
                      const SizedBox(width: 5),
                      Text(clock, style: meta),
                    ],
                  ],
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }

  static String _clock(DateTime t) =>
      '${t.hour.toString().padLeft(2, '0')}:${t.minute.toString().padLeft(2, '0')}';
}

Future<void> _copy(BuildContext context, String text) async {
  await Clipboard.setData(ClipboardData(text: text));
  HapticFeedback.lightImpact();
  if (!context.mounted) return;
  ScaffoldMessenger.of(context)
    ..hideCurrentSnackBar()
    ..showSnackBar(const SnackBar(content: Text('복사했어요')));
}

/// 얼굴 없는 발신자(학생끼리 쪽지) — 이름 첫 글자.
class _Initial extends StatelessWidget {
  const _Initial(this.name);
  final String name;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return CircleAvatar(
      radius: 15,
      backgroundColor: scheme.surfaceContainerHighest,
      child: Text(
        name.isEmpty ? '?' : name.characters.first,
        style: TextStyle(fontSize: 13, color: scheme.onSurfaceVariant),
      ),
    );
  }
}

class _Photo extends StatelessWidget {
  const _Photo({required this.bytes});
  final Uint8List bytes;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.only(bottom: 6),
    child: GestureDetector(
      onTap: () => showDialog<void>(
        context: context,
        builder: (ctx) => GestureDetector(
          onTap: () => Navigator.of(ctx).pop(),
          child: InteractiveViewer(child: Image.memory(bytes)),
        ),
      ),
      child: ClipRRect(
        borderRadius: BorderRadius.circular(10),
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxHeight: 200),
          child: Image.memory(bytes, fit: BoxFit.contain),
        ),
      ),
    ),
  );
}

class _ToolRunCard extends StatelessWidget {
  const _ToolRunCard({
    required this.run,
    required this.open,
    required this.live,
    required this.onToggle,
  });

  final ToolRun run;
  final Set<Object> open;

  /// 학생이 아직 움직이는가 — 결과 없는 도구에 도는 표시를 달지 말지.
  final bool live;
  final ValueChanged<Object> onToggle;

  /// 이보다 많으면 접어 두고 끝의 둘만 보인다.
  static const _fold = 3;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final tools = run.tools;
    final key = tools.first;
    final expanded = tools.length <= _fold || open.contains(key);
    final shown = expanded ? tools : tools.sublist(tools.length - 2);
    return Padding(
      padding: const EdgeInsets.only(top: 8, left: 38),
      child: DecoratedBox(
        decoration: BoxDecoration(
          color: scheme.surfaceContainerHighest.withValues(alpha: 0.5),
          borderRadius: BorderRadius.circular(10),
          border: Border.all(color: scheme.outline),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            if (tools.length > _fold)
              InkWell(
                onTap: () => onToggle(key),
                child: ConstrainedBox(
                  constraints: const BoxConstraints(minHeight: 36),
                  child: Padding(
                    padding: const EdgeInsets.symmetric(horizontal: 10),
                    child: Row(
                      children: [
                        Icon(
                          expanded ? Icons.expand_less : Icons.expand_more,
                          size: 18,
                          color: scheme.onSurfaceVariant,
                        ),
                        const SizedBox(width: 6),
                        Text(
                          expanded
                              ? '도구 ${tools.length}개'
                              : '도구 ${tools.length}개 · 앞의 ${tools.length - 2}개 접힘',
                          style: theme.textTheme.labelMedium?.copyWith(
                            color: scheme.onSurfaceVariant,
                            fontWeight: FontWeight.w600,
                          ),
                        ),
                      ],
                    ),
                  ),
                ),
              ),
            for (final t in shown)
              _ToolRow(
                tool: t,
                open: open.contains(t),
                live: live,
                onTap: t.result == null || t.result!.trim().isEmpty
                    ? null
                    : () => onToggle(t),
              ),
          ],
        ),
      ),
    );
  }
}

class _ToolRow extends StatelessWidget {
  const _ToolRow({
    required this.tool,
    required this.open,
    required this.live,
    this.onTap,
  });

  final ChatTool tool;
  final bool open;
  final bool live;
  final VoidCallback? onTap;

  static IconData _icon(String name) => switch (name) {
    'Bash' || 'shell' || 'exec_command' || 'BashOutput' => Icons.terminal,
    'Read' => Icons.description_outlined,
    'Edit' ||
    'MultiEdit' ||
    'Write' ||
    'NotebookEdit' ||
    'apply_patch' => Icons.edit_outlined,
    'Grep' || 'Glob' || 'ToolSearch' => Icons.search,
    'WebFetch' || 'WebSearch' => Icons.public,
    _ when name.startsWith('Task') || name == 'TodoWrite' => Icons.checklist,
    _ => Icons.build_outlined,
  };

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final dim = scheme.onSurfaceVariant;
    final Widget trail;
    if (tool.error) {
      trail = Icon(Icons.error_outline, size: 16, color: scheme.error);
    } else if (!tool.done && live) {
      trail = const SizedBox(
        width: 14,
        height: 14,
        child: CircularProgressIndicator(strokeWidth: 2),
      );
    } else {
      trail = const SizedBox.shrink();
    }
    final result = tool.result ?? '';
    return InkWell(
      onTap: onTap,
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Row(
              children: [
                Icon(_icon(tool.name), size: 16, color: dim),
                const SizedBox(width: 8),
                Text(
                  toolLabel(tool.name),
                  style: const TextStyle(
                    fontFamily: 'TermMono',
                    fontSize: 12,
                    fontWeight: FontWeight.w700,
                  ),
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: Text(
                    tool.summary,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: theme.textTheme.bodySmall?.copyWith(color: dim),
                  ),
                ),
                const SizedBox(width: 6),
                trail,
              ],
            ),
            if (open && result.trim().isNotEmpty)
              Padding(
                padding: const EdgeInsets.only(top: 6),
                child: Text(
                  _clip(stripAnsi(result).trim()),
                  style: TextStyle(
                    fontFamily: 'TermMono',
                    fontSize: 11.5,
                    height: 1.35,
                    color: tool.error ? scheme.error : scheme.onSurface,
                  ),
                ),
              ),
          ],
        ),
      ),
    );
  }

  /// 결과가 수천 줄이면 폰이 멈춘다 — 앞 40줄만.
  static String _clip(String s) {
    final lines = s.split('\n');
    if (lines.length <= 40 && s.length <= 4000) return s;
    final head = lines.take(40).join('\n');
    final cut = head.length > 4000 ? head.substring(0, 4000) : head;
    return '$cut\n… (${lines.length}줄 중 앞부분)';
  }
}

/// 접어 두는 칸 — 생각·명령 출력.
class _Fold extends StatelessWidget {
  const _Fold({
    required this.icon,
    required this.label,
    required this.text,
    required this.open,
    required this.onTap,
    this.mono = false,
  });

  final IconData icon;
  final String label;
  final String text;
  final bool open;
  final VoidCallback onTap;
  final bool mono;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final dim = theme.colorScheme.onSurfaceVariant;
    final first = text.split('\n').first;
    return Padding(
      padding: const EdgeInsets.only(top: 8, left: 38),
      child: InkWell(
        onTap: onTap,
        borderRadius: BorderRadius.circular(8),
        child: ConstrainedBox(
          constraints: const BoxConstraints(minHeight: 36),
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 6),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    Icon(icon, size: 16, color: dim),
                    const SizedBox(width: 6),
                    Text(
                      label,
                      style: theme.textTheme.labelMedium?.copyWith(
                        color: dim,
                        fontWeight: FontWeight.w700,
                      ),
                    ),
                    const SizedBox(width: 8),
                    if (!open)
                      Expanded(
                        child: Text(
                          first,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: theme.textTheme.bodySmall?.copyWith(
                            color: dim,
                          ),
                        ),
                      ),
                  ],
                ),
                if (open)
                  Padding(
                    padding: const EdgeInsets.only(top: 4, left: 22),
                    child: Text(
                      text,
                      style: mono
                          ? TextStyle(
                              fontFamily: 'TermMono',
                              fontSize: 11.5,
                              height: 1.35,
                              color: dim,
                            )
                          : theme.textTheme.bodySmall?.copyWith(
                              color: dim,
                              fontStyle: FontStyle.italic,
                              height: 1.45,
                            ),
                    ),
                  ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

class _CommandChip extends StatelessWidget {
  const _CommandChip({required this.command});
  final ChatCommand command;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final c = command;
    final text = c.name == '!'
        ? '! ${c.args}'
        : [c.name, c.args].where((s) => s.isNotEmpty).join(' ');
    return Padding(
      padding: const EdgeInsets.only(top: 10),
      child: Align(
        alignment: Alignment.centerRight,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 6),
          decoration: BoxDecoration(
            borderRadius: BorderRadius.circular(8),
            border: Border.all(color: _teacherBlue),
          ),
          child: Text(
            text,
            style: TextStyle(
              fontFamily: 'TermMono',
              fontSize: 12.5,
              color: scheme.onSurface,
            ),
          ),
        ),
      ),
    );
  }
}

class _AnsweredCard extends StatelessWidget {
  const _AnsweredCard({required this.pairs});
  final List<(String, String)> pairs;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Padding(
      padding: const EdgeInsets.only(top: 10, left: 38),
      child: Container(
        padding: const EdgeInsets.all(10),
        decoration: BoxDecoration(
          borderRadius: BorderRadius.circular(10),
          border: Border.all(color: scheme.outline),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              '질문에 답함',
              style: theme.textTheme.labelSmall?.copyWith(
                color: scheme.onSurfaceVariant,
                fontWeight: FontWeight.w700,
              ),
            ),
            for (final (q, a) in pairs) ...[
              const SizedBox(height: 6),
              Text(q, style: theme.textTheme.bodySmall),
              const SizedBox(height: 2),
              Text(
                a,
                style: theme.textTheme.bodyMedium?.copyWith(
                  color: _teacherBlue,
                  fontWeight: FontWeight.w700,
                ),
              ),
            ],
          ],
        ),
      ),
    );
  }
}

/// 가운데 흐린 한 줄 — 압축·오류·중단·서브에이전트.
class _Note extends StatelessWidget {
  const _Note({required this.icon, required this.text});
  final IconData icon;
  final String text;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final dim = theme.colorScheme.onSurfaceVariant;
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 10),
      child: Row(
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          Icon(icon, size: 14, color: dim),
          const SizedBox(width: 5),
          Flexible(
            child: Text(
              text,
              maxLines: 2,
              overflow: TextOverflow.ellipsis,
              style: theme.textTheme.labelSmall?.copyWith(color: dim),
            ),
          ),
        ],
      ),
    );
  }
}

class _Typing extends StatelessWidget {
  const _Typing({required this.face, required this.label});
  final Widget face;
  final String label;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final dim = theme.colorScheme.onSurfaceVariant;
    return Padding(
      padding: const EdgeInsets.only(top: 12),
      child: Row(
        children: [
          SizedBox(width: 30, child: face),
          const SizedBox(width: 8),
          PulseDot(color: theme.colorScheme.primary, size: 7),
          const SizedBox(width: 6),
          Expanded(
            child: Text(
              label,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: theme.textTheme.labelMedium?.copyWith(color: dim),
            ),
          ),
        ],
      ),
    );
  }
}

class _Empty extends StatelessWidget {
  const _Empty({
    required this.icon,
    required this.title,
    required this.body,
    required this.onTerminal,
  });

  final IconData icon;
  final String title;
  final String body;
  final VoidCallback onTerminal;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final dim = theme.colorScheme.onSurfaceVariant;
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(32),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(icon, size: 40, color: dim),
            const SizedBox(height: 12),
            Text(title, style: theme.textTheme.titleSmall),
            const SizedBox(height: 6),
            Text(
              body,
              textAlign: TextAlign.center,
              style: theme.textTheme.bodySmall?.copyWith(color: dim),
            ),
            const SizedBox(height: 16),
            OutlinedButton.icon(
              onPressed: onTerminal,
              icon: const Icon(Icons.terminal, size: 18),
              label: const Text('터미널로 보기'),
            ),
          ],
        ),
      ),
    );
  }
}

/// 화면에 뜬 선택 메뉴 — 입력줄 바로 위에 붙어 대화를 읽다가 엄지로 고른다.
class _MenuCard extends StatelessWidget {
  const _MenuCard({
    required this.menu,
    required this.accent,
    required this.onPick,
    required this.onDismiss,
    required this.onTerminal,
  });

  final PromptMenu menu;
  final Color accent;
  final ValueChanged<int> onPick;
  final VoidCallback onDismiss;
  final VoidCallback onTerminal;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Container(
      constraints: BoxConstraints(
        maxHeight: MediaQuery.sizeOf(context).height * 0.45,
      ),
      margin: const EdgeInsets.fromLTRB(12, 0, 12, 6),
      decoration: BoxDecoration(
        color: scheme.surface,
        borderRadius: BorderRadius.circular(12),
        border: Border.all(color: accent, width: 1.5),
      ),
      child: SingleChildScrollView(
        padding: const EdgeInsets.fromLTRB(12, 10, 12, 4),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            if (menu.title.isNotEmpty)
              Padding(
                padding: const EdgeInsets.only(bottom: 8),
                child: Text(
                  menu.title,
                  style: theme.textTheme.titleSmall?.copyWith(
                    fontWeight: FontWeight.w700,
                  ),
                ),
              ),
            for (final (i, o) in menu.options.indexed)
              Padding(
                padding: const EdgeInsets.only(bottom: 6),
                child: OutlinedButton(
                  onPressed: () => onPick(i),
                  style: OutlinedButton.styleFrom(
                    minimumSize: const Size.fromHeight(44),
                    alignment: Alignment.centerLeft,
                    backgroundColor: o.current
                        ? accent.withValues(alpha: 0.10)
                        : null,
                    side: BorderSide(
                      color: o.current ? accent : scheme.outline,
                    ),
                    shape: RoundedRectangleBorder(
                      borderRadius: BorderRadius.circular(8),
                    ),
                  ),
                  child: Text(
                    '${o.index}. ${o.label}',
                    style: TextStyle(
                      fontSize: 15,
                      color: scheme.onSurface,
                      fontWeight: o.current ? FontWeight.w700 : null,
                    ),
                  ),
                ),
              ),
            Row(
              children: [
                TextButton(onPressed: onDismiss, child: const Text('취소 (esc)')),
                const Spacer(),
                TextButton(
                  onPressed: onTerminal,
                  child: const Text('터미널에서 보기'),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }
}
