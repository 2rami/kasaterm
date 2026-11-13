import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';

import '../background_grace.dart';
import '../chat_markdown.dart';
import '../inline_result.dart';
import '../conversation.dart';
import '../conversation_cache.dart';
import '../file_link.dart';
import '../grid.dart' show DefaultColor, flagInverse;
import '../server.dart';
import '../server_image.dart';
import '../status_style.dart';
import '../student_art.dart';
import '../student_walk.dart';
import '../term_session.dart';
import '../look.dart';
import '../twins_loading.dart';
import 'controls.dart';
import 'conversation_asks.dart';
import 'engine_sheet.dart';

/// 학생 화면의 두 얼굴 — 격자 그대로(터미널)와 다시 그린 쪽(대화, 셸 칸은 명령 묶음). 웹이 「웹 터미널」과
/// 「대화 보기」를 주소 둘로 가른 것(2026-08-25)을 한 화면 안의 전환으로 옮겼다.
enum PaneView { terminal, chat }

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
    this.photos = const [],
    this.hint = '메시지 보내기',
    this.stopTip = '멈추기 (esc)',
    this.command = false,
  });

  final String hint;
  final String stopTip;

  /// 셸 명령 칸 — 자동 대문자·고침·똑똑한 따옴표가 명령을 바꾸지 않게 터미널 입력칸처럼 끈다.
  final bool command;

  final TextEditingController controller;
  final FocusNode focusNode;
  final bool enabled;
  final VoidCallback onSend;
  final Widget? leading;

  /// 입력상자에 붙여 두고 아직 안 보낸 사진 — 대화 보기는 상자의 `[Image #1]` 이 안 보인다.
  final List<Uint8List> photos;

  /// 작업 중일 때만 — esc 로 멈춘다.
  final VoidCallback? onStop;

  @override
  Widget build(BuildContext context) => Padding(
    // 앞 단추(사진)는 제 누름 영역에 숨을 갖고 있다 — 없으면 화면 여백만큼 띄운다.
    padding: EdgeInsets.fromLTRB(leading == null ? Look.pagePad : 4, 8, Look.pagePad, 8),
    child: Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        if (photos.isNotEmpty) _PendingPhotos(photos),
        _row(),
      ],
    ),
  );

  Widget _row() => Row(
    crossAxisAlignment: CrossAxisAlignment.end,
    children: [
      ?leading,
      Expanded(
        child: TextField(
          controller: controller,
          focusNode: focusNode,
          enabled: enabled,
          minLines: 1,
          maxLines: Look.inputMaxLines,
          textInputAction: TextInputAction.newline,
          autocorrect: !command,
          enableSuggestions: !command,
          smartDashesType: command ? SmartDashesType.disabled : null,
          smartQuotesType: command ? SmartQuotesType.disabled : null,
          // 16px 아래로 내리면 iOS 가 포커스 때 화면을 확대한다.
          style: const TextStyle(fontSize: 16),
          decoration: InputDecoration(hintText: hint),
        ),
      ),
      if (onStop != null) ...[
        const SizedBox(width: 4),
        IconButton(
          onPressed: onStop,
          icon: const Icon(Icons.stop_circle_outlined),
          tooltip: stopTip,
        ),
      ],
      const SizedBox(width: 8),
      ListenableBuilder(
        listenable: controller,
        builder: (context, _) => SendButton(
          ready: controller.text.trim().isNotEmpty || photos.isNotEmpty,
          onPressed: enabled ? onSend : null,
          round: true,
        ),
      ),
    ],
  );
}

class _PendingPhotos extends StatelessWidget {
  const _PendingPhotos(this.photos);
  final List<Uint8List> photos;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(Look.tap, 0, 0, Look.groupTitleGap),
    child: Semantics(
      label: '보낼 사진 ${photos.length}장',
      child: Row(
        children: [
          // 여러 장이면 옆으로 민다 — 안내 글은 늘 보인다.
          Flexible(
            child: SingleChildScrollView(
              scrollDirection: Axis.horizontal,
              child: Row(
                children: [
                  for (final (i, p) in photos.indexed) ...[
                    if (i > 0) const SizedBox(width: Look.groupTitleGap),
                    ClipRRect(
                      borderRadius: Look.smallCorners,
                      child: Image.memory(
                        p,
                        width: Look.attachThumb,
                        height: Look.attachThumb,
                        fit: BoxFit.cover,
                      ),
                    ),
                  ],
                ],
              ),
            ),
          ),
          const SizedBox(width: Look.groupTitleGap),
          Text(
            '보내면 함께 가요',
            style: TextStyle(
              fontSize: Look.sub,
              color: Theme.of(context).colorScheme.onSurfaceVariant,
            ),
          ),
        ],
      ),
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
    this.active = true,
    this.draft,
    this.conversation,
    this.readOnly = false,
  });

  final Server server;

  /// 화면이 쥔 기록 — 보낸 말을 기록보다 먼저 세운다([Conversation.echo]). 없으면 제 것을 쓴다.
  final Conversation? conversation;
  final bool readOnly;

  final Pane pane;
  final TermSession session;
  final Color accent;
  final VoidCallback onTerminal;

  /// 보낼 때마다 오른다 — 맨 아래로 내리고 곧바로 한 번 더 받는다.
  final int bottomTick;

  /// 보이는 쪽인가. 터미널 쪽으로 밀어 둔 동안은 살려만 두고 대화를 받지 않는다.
  final bool active;

  /// 대화 입력칸 — 승인 카드의 거절이 여기 쓴 글을 까닭으로 함께 보낸다.
  final TextEditingController? draft;

  @override
  State<ConversationView> createState() => _ConversationViewState();
}

class _ConversationViewState extends State<ConversationView> {
  static const _pollEvery = Duration(milliseconds: 1500);

  /// 서버가 다음 대화 행을 기다려 주는 한도. 그동안 타이머 바퀴는 `_polling` 에 걸려 쉰다.
  static const _waitMs = 15000;

  late final _conv = widget.conversation ?? Conversation();
  final _scroll = ScrollController();
  final _open = Set<Object>.identity();
  List<Object> _rows = const [];
  int _rowsVersion = -1;
  Timer? _timer;
  bool _polling = false;
  bool _loaded = false;
  bool _cached = false;
  bool _receivedLive = false;
  bool _bootstrapping = true;
  bool _missing = false;
  String? _error;
  bool _awayFromBottom = false;
  DateTime _menuHold = DateTime.fromMillisecondsSinceEpoch(0);

  /// 창이 남는 누름(상자 켜기·탭 옮기기·굴리기) 직후의 메뉴 — 화면이 바뀌기 전 두 번째 누름은 옛 커서로 셈하게 되니 막는다.
  PromptMenu? _busyMenu;
  DateTime _busyUntil = DateTime.fromMillisecondsSinceEpoch(0);

  /// 그 칸 mod 의 지금. 옛 데스크톱·mod 없는 칸이면 비어 있다.
  ModLive _live = const ModLive();
  bool _liveRunning = false;
  bool _liveGone = false;
  Timer? _liveRetry;
  Completer<void>? _liveWake;

  /// 결정을 못 보낸 요청 — 그 요청은 화면 키로 고른다.
  final _refused = <String>{};

  @override
  void initState() {
    super.initState();
    BackgroundGrace.instance.addListener(_graceChanged);
    _scroll.addListener(_onScroll);
    _loaded = widget.readOnly || _conv.items.isNotEmpty;
    unawaited(_bootstrap());
  }

  Future<void> _bootstrap() async {
    if (!widget.readOnly && _conv.items.isEmpty) {
      try { await _hydrate().timeout(const Duration(milliseconds: 150)); }
      on TimeoutException { /* A slow disk must not delay the source request. */ }
    }
    _bootstrapping = false;
    if (mounted && widget.active && BackgroundGrace.instance.live) _start();
  }

  Future<void> _hydrate() async {
    final cache = ConversationCache.of(widget.server);
    cache.warm([widget.pane], priority: true);
    try {
      final saved = await cache.repository.openPane(widget.pane);
      if (!mounted || widget.server.isClosed || _receivedLive || _conv.items.isNotEmpty || saved == null) return;
      _conv.hydrateArchive(saved.raw, next: saved.entry.cursor, source: saved.entry.source);
      setState(() { _loaded = true; _cached = true; });
    } catch (_) {
      // A bad cache is ignored; the source still gets a fresh request.
    }
  }

  @override
  void didUpdateWidget(ConversationView old) {
    super.didUpdateWidget(old);
    if (old.active != widget.active) _graceChanged();
    if (old.bottomTick != widget.bottomTick) {
      _toBottom();
      Timer(const Duration(milliseconds: 400), _poll);
    }
  }

  @override
  void dispose() {
    _timer?.cancel();
    _liveRetry?.cancel();
    if (_liveWake?.isCompleted == false) _liveWake!.complete();
    BackgroundGrace.instance.removeListener(_graceChanged);
    _scroll.dispose();
    super.dispose();
  }

  void _graceChanged() {
    if (BackgroundGrace.instance.live && widget.active) {
      _start();
    } else {
      _timer?.cancel();
      _timer = null;
    }
  }

  void _start() {
    if (widget.readOnly || _bootstrapping) return;
    _poll();
    _timer ??= Timer.periodic(_pollEvery, (_) => _poll());
    unawaited(_liveLoop());
  }

  bool get _liveWanted =>
      mounted && !widget.readOnly && widget.active && BackgroundGrace.instance.live && !_liveGone;

  /// 원본의 `/term/mod-live` 에 매달린다 — 바뀔 때만 답이 온다.
  Future<void> _liveLoop() async {
    if (_liveRunning) return;
    _liveRunning = true;
    int? seq;
    try {
      while (_liveWanted) {
        try {
          final next = await widget.server.modLive(
            widget.pane.id,
            machine: widget.pane.machine,
            seq: seq,
          );
          if (!mounted) return;
          if (next == null) {
            _liveGone = true;
            return;
          }
          seq = next.seq;
          setState(() => _live = next);
        } on ServerException catch (e) {
          // 닿았는데 거절한 것(옛 판·없는 칸)은 다시 묻지 않는다. 끊김만 잠깐 뒤 다시.
          final status = e.status ?? 0;
          if (status >= 400 && status < 500) {
            _liveGone = true;
            return;
          }
          seq = null;
          final wake = Completer<void>();
          _liveRetry = Timer(const Duration(seconds: 2), wake.complete);
          _liveWake = wake;
          await wake.future;
        }
      }
    } finally {
      _liveRunning = false;
    }
  }

  ModLive get _liveNow {
    if (_refused.isEmpty) return _live;
    return ModLive(
      live: _live.live,
      seq: _live.seq,
      session: _live.session,
      turnOpen: _live.turnOpen,
      compacting: _live.compacting,
      question: _live.question,
      tools: _live.tools,
      permissions: [for (final p in _live.permissions) if (!_refused.contains(p.id)) p],
    );
  }

  /// 승인 요청에 mod 결정으로 답한다(원격 승인 계약과 같은 요청 id). 거절이면 입력칸 글이 까닭이다.
  Future<void> _decide(ModPermission p, bool allow) async {
    final draft = widget.draft;
    final message = allow ? '' : (draft?.text.trim() ?? '');
    if (!allow) draft?.clear();
    HapticFeedback.selectionClick();
    setState(() => _menuHold = DateTime.now().add(_menuHoldFor));
    _toBottom();
    try {
      await widget.server.modDecide(
        widget.pane.id,
        session: _live.session,
        id: p.id,
        allow: allow,
        message: message,
        machine: widget.pane.machine,
      );
    } on ServerException {
      if (mounted) setState(() => _refused.add(p.id));
    }
  }

  /// 대화 안 카드가 사람 대신 칸에 넣는 말(로그인 돌아온 주소) — 입력칸으로 보낸 말과 같은 길로 간다.
  Future<void> _sendText(String text) async {
    final echo = _conv.echo(text);
    setState(() {});
    _toBottom();
    try {
      await widget.session.reply(text, chat: true);
    } on ServerException {
      _conv.withdraw(echo);
      if (mounted) setState(() {});
      rethrow;
    }
  }

  Future<void> _poll() async {
    if (_polling || !mounted || widget.readOnly) return;
    _polling = true;
    var again = false;
    try {
      final pane = widget.pane;
      final from = _conv.offset;
      final waited = _loaded && !_missing && !_cached && _receivedLive;
      final chunk = await widget.server.transcriptRaw(
        pane.id,
        from,
        machine: pane.machine,
        waitMs: waited ? _waitMs : null,
        source: _conv.source,
      );
      if (!mounted) return;
      if (chunk == null) {
        if (_cached) return;
        setState(() {
          _missing = true;
          _loaded = true;
          _error = null;
        });
        return;
      }
      _receivedLive = true;
      final wasCached = _cached;
      _cached = false;
      // 기다려 받은 새 줄이면 곧바로 다음 줄을 기다린다 — 타이머 박자를 안 기다린다.
      again = waited && !chunk.reset && chunk.offset > from && widget.active;
      if (!_conv.apply(chunk.raw, reset: chunk.reset, next: chunk.offset, source: chunk.source)) {
        // 세션이 바뀌었다 — 다음 바퀴가 꼬리부터 다시 받는다.
        _conv.clear();
        _open.clear();
      }
      if (_loaded && !_missing && !wasCached && _rowsVersion == _conv.version) return;
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
      if (again && mounted) unawaited(_poll());
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
      _rows = [...groupRows(_conv.items), ..._conv.echoes];
      _rowsVersion = _conv.version;
    }
    return _rows;
  }

  /// 화면에서 읽은 메뉴의 한 칸을 고른다 — 지금 커서 자리에서 그만큼 ↑↓ 옮기고(글 칸이면 [text] 를 치고) Enter.
  /// 번호 단축키는 AskUserQuestion 의 다중 선택에서 뜻이 달라 쓰지 않는다.
  void _pick(PromptMenu menu, int i, {String text = ''}) {
    final current = _menuToAct(menu);
    if (current == null || i < 0 || i >= current.options.length) return;
    // 승인 창의 「Yes」·「No」는 요청 id 로 답한다 — 화면 키는 그사이 다른 창이 뜨면 그것을 고른다.
    final decision = _liveNow.decisionFor(current, i);
    if (decision != null) {
      unawaited(_decide(decision.$2, decision.$1));
      return;
    }
    final step = menuStep(current, i, text: text);
    if (step == null || !widget.session.canSend) return;
    // 여러 개 고르기의 상자 켜고 끄기는 창이 그대로 남는다 — 카드를 접지 않고 화면이 바뀔 때까지만 막는다.
    _sendMenuKeys(current, step, stays: current.options[i].checked != null);
  }

  /// 질문 탭을 옮긴다 — Tab·Shift+Tab. 글 칸 자리에 커서가 있으면 Tab 이 안 먹어 한 칸 위로 먼저 뺀다.
  void _tab(PromptMenu menu, int j) {
    final current = _menuToAct(menu);
    if (current == null || !widget.session.canSend) return;
    final from = current.currentTab;
    if (from < 0 || j < 0 || j >= current.tabs.length || j == from) return;
    final s = widget.session;
    final cursor = current.cursor;
    if (cursor >= 0 && current.options[cursor].entry) s.arrow('A');
    for (var k = 0; k < (j - from).abs(); k++) {
      s.sendText(j > from ? '\t' : '\x1b[Z');
    }
    HapticFeedback.selectionClick();
    _markBusy(current);
  }

  /// 창이 낮아 굴러 숨은 선택지 쪽으로 커서를 옮긴다 — TUI 가 목록을 굴려 다음 화면에 보인다.
  void _reveal(PromptMenu menu, {required bool up}) {
    final current = _menuToAct(menu);
    if (current == null || !widget.session.canSend) return;
    final moves = menuReveal(current, up: up);
    if (moves == null) return;
    _sendMenuKeys(current, (moves: moves, text: '', enter: false), stays: true);
  }

  /// 누른 카드가 지금 화면의 메뉴와 같을 때만 그 메뉴를 준다. 직전 누름이 아직 화면에 안 닿았으면 null.
  PromptMenu? _menuToAct(PromptMenu menu) {
    final current = _menu;
    if (current == null || !_sameMenu(menu, current)) return null;
    final busy = _busyMenu;
    if (busy != null && DateTime.now().isBefore(_busyUntil) && _sameMenu(busy, current)) return null;
    return current;
  }

  void _sendMenuKeys(PromptMenu menu, MenuStep step, {required bool stays}) {
    final s = widget.session;
    for (var k = 0; k < step.moves.abs(); k++) {
      s.arrow(step.moves > 0 ? 'B' : 'A');
    }
    HapticFeedback.selectionClick();
    if (stays) {
      _markBusy(menu);
    } else {
      setState(() => _menuHold = DateTime.now().add(_menuHoldFor));
      _toBottom();
    }
    unawaited(_typeAndEnter(step));
  }

  Future<void> _typeAndEnter(MenuStep step) async {
    final s = widget.session;
    // 화살표 바로 뒤의 Enter 는 질문 창이 커서를 옮기기 전에 읽혀 앞 칸을 고른 적이 있다(가상 아이폰 실측).
    var moved = step.moves != 0;
    try {
      if (step.text.isNotEmpty) {
        moved ? await s.sendTextAfter(step.text, TermSession.enterGap) : s.sendText(step.text);
        moved = true;
      }
      if (step.enter) moved ? await s.sendTextAfter('\r', TermSession.enterGap) : s.sendText('\r');
    } on ServerException {
      // 그사이 끊겼다 — 창이 그대로 남아 있으니 다시 누르면 된다.
    }
  }

  void _markBusy(PromptMenu menu) => setState(() {
        _busyMenu = menu;
        _busyUntil = DateTime.now().add(_menuHoldFor);
      });

  void _dismiss() {
    if (!widget.session.canSend || _menu == null) return;
    widget.session.sendText('\x1b');
    setState(() => _menuHold = DateTime.now().add(_menuHoldFor));
  }

  /// 고른 직후엔 화면이 아직 옛 메뉴를 그리고 있다 — 두 번 누르지 않게 잠깐 접는다.
  static const _menuHoldFor = Duration(milliseconds: 1200);

  bool _sameMenu(PromptMenu a, PromptMenu b) => a.title == b.title &&
    a.options.length == b.options.length &&
    a.currentTab == b.currentTab &&
    a.options.indexed.every((entry) {
      final other = b.options[entry.$1];
      final option = entry.$2;
      return option.index == other.index && option.label == other.label &&
        option.current == other.current && option.checked == other.checked &&
        option.row == other.row;
    });

  PromptMenu? get _menu {
    if (DateTime.now().isBefore(_menuHold)) return null;
    final lines = <String>[];
    // 질문 탭의 지금 탭은 배경색으로만 칠해진다 — 그 줄의 칠한 글을 함께 넘긴다.
    final lit = <int, String>{};
    for (final (i, row) in widget.session.grid.lines.indexed) {
      lines.add(row.map((r) => r.text).join());
      final on = [for (final r in row) if (r.bg is! DefaultColor || r.flags & flagInverse != 0) r.text].join();
      if (on.trim().isNotEmpty) lit[i] = on;
    }
    final menu = parsePromptMenu(lines, lit: lit);
    // 화면의 계획 상자는 창 높이에서 잘린다 — mod 가 알린 계획 원문이 더 길면 그것을 싣는다.
    final live = _liveNow;
    if (menu != null && live.live && live.permissions.isNotEmpty &&
        live.permissions.first.tool == 'ExitPlanMode' && menu.options.any((o) => o.input)) {
      final plan = live.permissions.first.lines;
      if (plan.length > menu.context.length) return menu.withContext(['계획', ...plan]);
    }
    return menu;
  }

  @override
  Widget build(BuildContext context) {
    final connected = !widget.readOnly && widget.session.state == TermState.connected;
    final menu = connected ? _menu : null;
    final live = _liveNow;
    final asks = live.live && live.permissions.isNotEmpty;
    // 질문은 허락·거절로 답하는 것이 아니다 — 화면의 선택지를 기다린다.
    final decidable = asks && live.permissions.first.tool != 'AskUserQuestion';
    final accent = widget.pane.kind == 'permission' || asks
        ? StatusStyle.attention
        : widget.accent;
    final held = DateTime.now().isBefore(_menuHold);
    return Column(
      children: [
        if (_cached) Padding(
          padding: const EdgeInsets.symmetric(vertical: 4),
          child: Text('저장된 대화 · 연결 확인 중', style: TextStyle(fontSize: Look.sub,
              color: Theme.of(context).colorScheme.onSurfaceVariant)),
        ),
        // 고르는 카드가 높아 대화 쪽이 낮아지면 빈 화면 그림이 넘친다 — 카드를 덮어 그리지 않게 자른다.
        Expanded(child: ClipRect(child: DismissKeyboard(child: _body(context)))),
        ConversationAsks(
          pane: widget.pane,
          rows: _loaded ? rows : const [],
          active: widget.active && !widget.readOnly,
          toolsAsked: menu != null || (!connected && decidable) || held,
          onSend: _sendText,
        ),
        if (menu != null)
          _MenuCard(
            // 다른 창(다른 질문·다른 탭)이면 펼친 줄과 쓰던 글 칸을 새로 시작한다.
            key: ValueKey('${menu.title}\u0000${menu.currentTab}'),
            menu: menu,
            accent: accent,
            reasonFor: [
              for (var i = 0; i < menu.options.length; i++)
                if (live.decisionFor(menu, i)?.$1 == false) menu.options[i].label,
            ].firstOrNull,
            onPick: (i, text) => _pick(menu, i, text: text),
            onTab: (j) => _tab(menu, j),
            onReveal: (up) => _reveal(menu, up: up),
            onDismiss: _dismiss,
            onTerminal: widget.onTerminal,
          )
        // 화면을 아직 못 받았으면 mod 의 요청만으로 묻는다 — 도구와 입력 원문, 허락·거절.
        else if (!connected && decidable && !held)
          _MenuCard(
            menu: PromptMenu(
              '허락할까요?',
              const [
                PromptOption(1, '허락', current: false),
                PromptOption(2, '거절', current: false),
              ],
              context: [toolLabel(live.permissions.first.tool), ...live.permissions.first.lines],
            ),
            accent: accent,
            reasonFor: '거절',
            numbered: false,
            onPick: (i, _) => unawaited(_decide(live.permissions.first, i == 0)),
            onDismiss: null,
            onTerminal: widget.onTerminal,
          ),
        // 셸 명령 쪽과 같은 맥락 줄 — 앱바엔 경로가 들어갈 폭이 없다.
        if (widget.pane.cwd.isNotEmpty)
          PlaceLine(
            cwd: widget.pane.cwd,
            branch: widget.pane.branch,
            model: widget.pane.modelLabel,
            effort: widget.pane.effortLabel,
            harness: widget.pane.harness,
            colored: true,
            onEngine: widget.session.canSend && !widget.pane.isShell
                ? () => unawaited(showEngineSheet(context, server: widget.server, pane: widget.pane))
                : null,
            style: TextStyle(
              fontFamily: 'TermMono',
              fontFamilyFallback: Look.flowMonoFallback,
              fontSize: Look.sub,
              color: Theme.of(context).colorScheme.onSurfaceVariant,
            ),
          ),
      ],
    );
  }

  Widget _body(BuildContext context) {
    final theme = Theme.of(context);
    if (!_loaded) {
      return _error == null
          ? const TwinsLoading(label: '대화를 받는 중', size: Look.twinsSmall)
          : _Empty(
              icon: Icons.cloud_off_outlined,
              title: '대화를 못 받았어요',
              body: _error!,
              onTerminal: widget.onTerminal,
            );
    }
    final list = rows;
    final pane = widget.pane;
    final live = _liveNow;
    final typing = !widget.readOnly && (pane.isBusy || (live.live && (live.turnOpen || live.compacting)));
    // 기록이 아직 없어도 일하는 중이면 그 줄은 보인다 — 빈 화면이 「쉬는 중」으로 읽혔다.
    if (list.isEmpty && !typing) {
      if (widget.readOnly) return const Center(child: Text('이 저장본에는 표시할 대화가 없어요.'));
      return _Empty(
        icon: Icons.forum_outlined,
        title: '아직 대화가 없어요',
        body: !_missing
            ? '말을 걸면 여기에 떠요.'
            : (widget.pane.mirrorOf ?? '').isNotEmpty
            ? '${widget.pane.mirrorOf} 의 거울이라 대화 기록은 그 기계에 있어요. 허브의 ${widget.pane.mirrorOf} 칸에서 열면 보여요.'
            : '이 창에 묶인 대화 기록이 아직 없어요. claude 가 첫 답을 하면 여기에 떠요.',
        onTerminal: widget.onTerminal,
        // 학생 칸의 첫 화면은 그 학생이 선다 — 쌍둥이는 칸 주인이 없는 화면의 얼굴이다.
        art: pane.slug == null || pane.isShell
            ? null
            : StudentStanding(
                server: widget.server,
                slug: pane.slug!,
                machine: pane.machine,
                size: Look.twinsSmall,
              ),
      );
    }
    final Widget face = widget.readOnly ? _Initial(pane.displayName) : StudentFace(
      server: widget.server,
      slug: pane.slug,
      url: pane.slug == null
          ? null
          : widget.server.avatar(pane.slug!, machine: pane.machine),
      size: 30,
    );
    final md = _markdownStyle(theme);
    final extra = typing ? 1 : 0;
    return LayoutBuilder(
      builder: (context, box) {
        final maxBubble = box.maxWidth * 0.8;
        return Stack(
          children: [
            ListView.builder(
              controller: _scroll,
              reverse: true,
              keyboardDismissBehavior: ScrollViewKeyboardDismissBehavior.onDrag,
              padding: const EdgeInsets.fromLTRB(12, 8, 12, 12),
              itemCount: list.length + extra,
              itemBuilder: (context, i) {
                if (i < extra) {
                  final slug = pane.slug;
                  return _Typing(
                    // 하는 중 줄의 학생은 데스크톱 사이드바처럼 얼굴 상자에서 걷는다.
                    face: widget.readOnly || slug == null || pane.isShell
                        ? face
                        : StudentWalk(
                            server: widget.server,
                            slug: slug,
                            machine: pane.machine,
                            size: 30,
                            fallback: face,
                          ),
                    accent: widget.accent,
                    detail: live.doing ?? pane.busyDetail,
                  );
                }
                final at = list.length - 1 - (i - extra);
                return _row(context, list, at, face, md, maxBubble);
              },
            ),
            if (_awayFromBottom)
              Positioned(
                right: 12,
                bottom: 12,
                child: IconButton.outlined(
                  style: IconButton.styleFrom(
                    backgroundColor: Theme.of(context).scaffoldBackgroundColor,
                    side: BorderSide(color: Theme.of(context).colorScheme.outline),
                  ),
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

  /// 쪽지를 넣은 학생 얼굴 — 기록엔 이름(`⟦아로나⟧`)뿐이라 서버가 명부로 이름을 슬러그로 바꿔 그린다.
  /// 모르는 이름(카사텀·커스텀 캐릭터)은 첫 글자.
  Widget _senderFace(String name) => widget.readOnly ? _Initial(name) : ClipOval(
    child: ServerImage(
      server: widget.server,
      uri: widget.server.avatar(name, machine: widget.pane.machine),
      width: 30,
      height: 30,
      fit: BoxFit.cover,
      fallback: _Initial(name),
    ),
  );

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
      return r is ChatBubble ? (r.mine ? '' : (r.from ?? '\u0000'), r.via) : null;
    }

    final me = sender(at);
    final head = me == null || sender(at - 1) != me;
    final tail = me == null || sender(at + 1) != me;
    return switch (row) {
      ChatBubble b => _Bubble(
        server: widget.server,
        machine: widget.pane.machine,
        cwd: widget.pane.cwd,
        bubble: b,
        name: b.from ?? widget.pane.displayName,
        face: b.from == null ? face : _senderFace(b.from!),
        accent: widget.accent,
        head: head,
        tail: tail,
        maxWidth: maxBubble,
        md: md,
        offline: widget.readOnly,
      ),
      ToolRun r => _ToolRunCard(
        run: r,
        open: _open,
        live: !widget.readOnly && widget.pane.isBusy,
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

  MarkdownStyleSheet _markdownStyle(ThemeData theme) => chatMarkdownStyle(
    theme,
    base: theme.textTheme.bodyMedium!.copyWith(
      fontSize: 15,
      height: 1.5,
      color: theme.colorScheme.onSurface,
    ),
    // 상대 말풍선이 올린 표면색이라 코드 칸은 바탕색으로 떼어 낸다.
    codeBg: theme.scaffoldBackgroundColor,
  );
}


/// 말풍선의 원문 표 — 고르기 메뉴가 고른 글이 걸친 말풍선을 찾는다([selectedBubbleText]).
class _BubbleText {
  _BubbleText(this.text);
  final String text;
}

/// 고른 글이 말풍선 하나 안에 있으면 그 말풍선 원문 — 고르기 메뉴의 「말풍선 복사」. 상대 말은 마크다운 원문
/// 그대로(코드 울타리·링크 주소가 남는다), 그림·소리 표시 블록만 걷는다. [box] 는 [geometry] 의 좌표 칸이다.
/// 여러 말풍선에 걸치면 null — 그때는 메뉴에 안 선다.
String? selectedBubbleText(RenderBox box, SelectionGeometry geometry) {
  final rects = geometry.selectionRects;
  if (!geometry.hasSelection || rects.isEmpty) return null;
  _BubbleText? at(Offset local) {
    final hits = BoxHitTestResult();
    if (!box.hitTest(hits, position: local)) return null;
    for (final e in hits.path) {
      if (e.target case RenderMetaData(metaData: final _BubbleText b)) return b;
    }
    return null;
  }

  final first = at(rects.first.center);
  return first != null && identical(first, at(rects.last.center)) ? first.text : null;
}

class _Bubble extends StatelessWidget {
  const _Bubble({
    required this.server,
    this.machine,
    required this.cwd,
    required this.bubble,
    required this.name,
    required this.face,
    required this.accent,
    required this.head,
    required this.tail,
    required this.maxWidth,
    required this.md,
    this.offline = false,
  });

  final Server server;
  final String? machine;
  final String cwd;
  final ChatBubble bubble;
  final String name;
  final Widget? face;
  final Color accent;
  final bool head;
  final bool tail;
  final double maxWidth;
  final MarkdownStyleSheet md;
  final bool offline;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final mine = bubble.mine;
    final queued = mine && bubble.queued;
    final ink = queued ? scheme.onSurface : SpeechBubble.ink(context, mine: mine);
    final text = bubble.text;
    void onLink(String href) => offline
        ? ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('외부 파일과 링크는 이 텍스트 저장본에 포함되지 않아요.')))
        : showResultLink(context, server, href, cwd: cwd, machine: machine);
    final content = Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        for (final img in bubble.images) _Photo(bytes: img),
        if (bubble.omittedImages > 0) Text('이미지 ${bubble.omittedImages}장 · 저장본에 포함되지 않음', style: TextStyle(fontSize: Look.sub, color: ink)),
        if (text.isNotEmpty)
          mine
              ? LinkedText(
                  text,
                  style: TextStyle(fontSize: Look.body, height: 1.45, color: ink),
                  onLink: onLink,
                )
              : InlineResultBody(
                  server: server, machine: machine, text: text, style: md,
                  offline: offline,
                  onLink: onLink,
                ),
      ],
    );
    final bubbleBox = SpeechBubble(
      mine: mine,
      maxWidth: maxWidth,
      // 예약은 아직 안 간 말 — 강조색 대신 주의색 옅은 바탕(경고 띠와 같은 옅기).
      fill: queued
          ? Color.alphaBlend(
              StatusStyle.attention.withValues(alpha: Look.dangerTint),
              theme.scaffoldBackgroundColor,
            )
          : null,
      child: content,
    );
    final box = text.isEmpty
        ? bubbleBox
        : MetaData(
            metaData: _BubbleText(mine ? text : withoutInlineResults(text)),
            child: bubbleBox,
          );
    final clock = !tail
        ? null
        : bubble.sending
        ? '보내는 중'
        : bubble.at != null
        ? _clock(bubble.at!)
        : null;
    final meta = theme.textTheme.labelSmall?.copyWith(
      color: scheme.onSurfaceVariant,
    );
    if (mine) {
      return Padding(
        padding: EdgeInsets.only(top: head ? 10 : 3),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.end,
          children: [
            if (queued && head)
              Padding(
                padding: const EdgeInsets.only(bottom: 3, right: 3),
                child: Text(
                  '예약 · 대기 중',
                  style: theme.textTheme.labelMedium?.copyWith(color: StatusStyle.attention),
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
                // 아직 기록에 안 닿은 말 — 간 말과 같은 자리에 조금 옅게.
                Flexible(child: bubble.sending ? Opacity(opacity: 0.7, child: box) : box),
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
                    child: Text.rich(
                      TextSpan(
                        text: name,
                        style: theme.textTheme.labelMedium?.copyWith(color: accent),
                        children: [
                          if (bubble.via != null)
                            TextSpan(
                              text: ' · ${bubble.via}',
                              style: theme.textTheme.labelMedium?.copyWith(color: scheme.onSurfaceVariant),
                            ),
                        ],
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
        borderRadius: Look.corners,
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
          borderRadius: Look.corners,
          color: scheme.surfaceContainerHigh.withValues(alpha: 0.6),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            if (tools.length > _fold)
              InkWell(
                onTap: () => onToggle(key),
                child: ConstrainedBox(
                  constraints: const BoxConstraints(minHeight: Look.tap),
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
                    fontFamilyFallback: Look.flowMonoFallback,
                    fontSize: Look.chip,
                    fontWeight: FontWeight.w600,
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
                    fontFamilyFallback: Look.flowMonoFallback,
                    fontSize: Look.chip,
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
        borderRadius: Look.corners,
        child: ConstrainedBox(
          constraints: const BoxConstraints(minHeight: Look.tap),
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
                        fontWeight: FontWeight.w600,
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
                              fontFamilyFallback: Look.flowMonoFallback,
                              fontSize: Look.chip,
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
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
          decoration: ShapeDecoration(
            shape: const StadiumBorder(),
            color: scheme.primary.withValues(alpha: 0.16),
          ),
          child: Text(
            text,
            style: TextStyle(
              fontFamily: 'TermMono',
              fontFamilyFallback: Look.flowMonoFallback,
              fontSize: Look.sub,
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
          borderRadius: Look.corners,
          color: scheme.surfaceContainerHigh.withValues(alpha: 0.6),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              '질문에 답함',
              style: theme.textTheme.labelSmall?.copyWith(
                color: scheme.onSurfaceVariant,
                fontWeight: FontWeight.w600,
              ),
            ),
            for (final (q, a) in pairs) ...[
              const SizedBox(height: 6),
              Text(q, style: theme.textTheme.bodySmall),
              const SizedBox(height: 2),
              Text(
                a,
                style: theme.textTheme.bodyMedium?.copyWith(
                  color: scheme.primary,
                  fontWeight: FontWeight.w600,
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
  const _Typing({required this.face, required this.accent, this.detail});
  final Widget face;
  final Color accent;

  /// 「하는 중」 뒤의 사정 — 지금 도는 도구·백그라운드.
  final String? detail;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final style = theme.textTheme.labelMedium;
    // 흐린 한 줄은 일하는지 안 보였다 — 도구 묶음과 같은 판에 학생색 「하는 중」을 세운다.
    return Padding(
      padding: const EdgeInsets.only(top: 12),
      child: Row(
        children: [
          SizedBox(width: 30, child: face),
          const SizedBox(width: 8),
          Flexible(
            child: Container(
              constraints: const BoxConstraints(minHeight: Look.tap),
              padding: const EdgeInsets.symmetric(horizontal: 12),
              decoration: BoxDecoration(
                borderRadius: Look.corners,
                color: scheme.surfaceContainerHigh.withValues(alpha: 0.6),
              ),
              child: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  PulseDot(color: accent, size: 8),
                  const SizedBox(width: 8),
                  Text(
                    '하는 중',
                    style: style?.copyWith(color: accent, fontWeight: FontWeight.w600),
                  ),
                  if (detail != null && detail!.isNotEmpty) ...[
                    Text('  ·  ', style: style?.copyWith(color: scheme.onSurfaceVariant)),
                    Flexible(
                      child: Text(
                        detail!,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: style?.copyWith(color: scheme.onSurfaceVariant),
                      ),
                    ),
                  ],
                ],
              ),
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
    this.art,
  });

  final IconData icon;
  final String title;
  final String body;
  final VoidCallback onTerminal;

  /// 서 있는 그림. 없으면 쌍둥이.
  final Widget? art;

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
            // 빈·실패 화면은 서 있는 쌍둥이(design.md 「쌍둥이 결」). 뜻 아이콘은 제목 앞에 작게.
            ExcludeSemantics(child: art ?? const TwinsStage(t: 0, size: Look.twinsSmall)),
            const SizedBox(height: 12),
            Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(icon, size: Look.iconSize, color: dim),
                const SizedBox(width: 6),
                Flexible(child: Text(title, style: theme.textTheme.titleSmall)),
              ],
            ),
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
class _MenuCard extends StatefulWidget {
  const _MenuCard({
    super.key,
    required this.menu,
    required this.accent,
    required this.onPick,
    required this.onDismiss,
    required this.onTerminal,
    this.onTab,
    this.onReveal,
    this.reasonFor,
    this.numbered = true,
  });

  final PromptMenu menu;
  final Color accent;

  /// 고른 선택지 차례와 글 칸에 쓴 글(글 칸이 아니면 빈 글).
  final void Function(int i, String text) onPick;

  /// 질문 탭 옮기기. 지금 탭을 모르면 탭은 보이기만 한다.
  final ValueChanged<int>? onTab;

  /// 굴러 숨은 선택지 쪽으로 커서 옮기기 — true 면 위.
  final ValueChanged<bool>? onReveal;
  final VoidCallback? onDismiss;
  final VoidCallback onTerminal;

  /// 거절 선택지 이름 — 그것을 mod 결정으로 보내 입력칸 글이 까닭이 된다.
  final String? reasonFor;
  final bool numbered;

  /// 처음 펴는 창 안 줄 상한(데스크톱 `CTX_MAX`) — 그 뒤는 「N줄 더 보기」로 접었다가 누르면 다 편다.
  static const _contextMax = 8;

  @override
  State<_MenuCard> createState() => _MenuCardState();
}

class _MenuCardState extends State<_MenuCard> {
  bool _expanded = false;
  final _typed = TextEditingController();

  @override
  void dispose() {
    _typed.dispose();
    super.dispose();
  }

  void _answer(int i) {
    final text = _typed.text.trim();
    if (text.isEmpty) return;
    widget.onPick(i, text);
    _typed.clear();
  }

  String _label(PromptOption o) => widget.numbered && o.index > 0 ? '${o.index}. ${o.label}' : o.label;

  Widget _box(bool checked, Color accent, ColorScheme scheme) => Padding(
        padding: const EdgeInsets.only(right: 8),
        child: Icon(
          checked ? Icons.check_box : Icons.check_box_outline_blank,
          size: Look.iconSize,
          color: checked ? accent : scheme.onSurfaceVariant,
        ),
      );

  @override
  Widget build(BuildContext context) {
    final menu = widget.menu;
    final accent = widget.accent;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final mono = TextStyle(
      fontFamily: 'TermMono',
      fontSize: Look.sub,
      height: 1.4,
      color: scheme.onSurfaceVariant,
    );
    final all = menu.context;
    final folded = !_expanded && all.length > _MenuCard._contextMax;
    final lines = folded ? all.take(_MenuCard._contextMax - 1).toList() : all;
    final media = MediaQuery.of(context);
    final note = theme.textTheme.bodySmall?.copyWith(color: scheme.onSurfaceVariant);
    final canTab = widget.onTab != null && menu.currentTab >= 0;
    return Container(
      // 글 칸을 누르면 자판이 올라온다 — 자판을 뺀 높이로 재야 대화가 다 가려지지 않는다.
      constraints: BoxConstraints(
        maxHeight: (media.size.height - media.viewInsets.bottom) * 0.45,
      ),
      margin: const EdgeInsets.fromLTRB(12, 0, 12, 6),
      decoration: BoxDecoration(
        color: TwinTone.of(context).card,
        borderRadius: Look.cardCorners,
        border: Border.all(color: accent),
      ),
      child: SingleChildScrollView(
        padding: const EdgeInsets.fromLTRB(12, 10, 12, 4),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            if (menu.tabs.isNotEmpty)
              Padding(
                padding: const EdgeInsets.only(bottom: 4),
                child: Wrap(
                  spacing: 6,
                  children: [
                    for (final (j, t) in menu.tabs.indexed)
                      _TabChip(
                        tab: t,
                        accent: accent,
                        onTap: canTab && !t.current ? () => widget.onTab!(j) : null,
                      ),
                  ],
                ),
              ),
            // TUI 창 안의 줄 그대로 — 머리(도구·질문 이름)는 굵게, 원문은 고정폭으로.
            for (final (i, line) in lines.indexed)
              Text(
                line,
                style: i == 0
                    ? theme.textTheme.titleSmall?.copyWith(fontWeight: FontWeight.w600)
                    : mono,
              ),
            if (folded)
              Align(
                alignment: Alignment.centerLeft,
                child: TextButton(
                  onPressed: () => setState(() => _expanded = true),
                  child: Text('${all.length - lines.length}줄 더 보기'),
                ),
              ),
            if (lines.isNotEmpty) const SizedBox(height: 8),
            if (menu.title.isNotEmpty)
              Padding(
                padding: const EdgeInsets.only(bottom: 8),
                child: Text(
                  menu.title,
                  style: lines.isEmpty
                      ? theme.textTheme.titleSmall?.copyWith(fontWeight: FontWeight.w600)
                      : theme.textTheme.bodyMedium,
                ),
              ),
            // 「더 보기」는 숨은 자리에 — 굴러가는 칸 밖에 남은 「Chat about this」 앞이 숨을 수 있다.
            for (final (i, o) in menu.options.indexed) ...[
              if (widget.onReveal != null && o.row - 1 == menu.hiddenAbove) _reveal(up: true),
              Padding(
                padding: const EdgeInsets.only(bottom: 6),
                child: o.input ? _inputRow(i, o, accent, scheme) : _optionRow(i, o, accent, scheme, note),
              ),
              if (widget.onReveal != null && o.row + 1 == menu.hiddenBelow) _reveal(up: false),
            ],
            if (widget.reasonFor != null)
              Padding(
                padding: const EdgeInsets.only(bottom: 4),
                child: Text('「${widget.reasonFor}」는 입력칸에 쓴 글을 까닭으로 함께 보내요', style: note),
              ),
            Row(
              children: [
                if (widget.onDismiss != null)
                  TextButton(onPressed: widget.onDismiss, child: const Text('취소 (esc)')),
                const Spacer(),
                TextButton(
                  onPressed: widget.onTerminal,
                  child: const Text('터미널에서 보기'),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }

  Widget _optionRow(int i, PromptOption o, Color accent, ColorScheme scheme, TextStyle? note) => Semantics(
        checked: o.checked,
        child: OutlinedButton(
          onPressed: () => widget.onPick(i, ''),
          style: OutlinedButton.styleFrom(
            minimumSize: const Size.fromHeight(Look.buttonH),
            alignment: Alignment.centerLeft,
            side: BorderSide(color: o.current ? accent : scheme.outline),
          ),
          child: Row(
            children: [
              if (o.checked != null) _box(o.checked!, accent, scheme),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      _label(o),
                      style: TextStyle(
                        fontSize: Look.body,
                        color: o.current ? accent : scheme.onSurface,
                        fontWeight: o.current ? FontWeight.w600 : null,
                      ),
                    ),
                    if (o.note.isNotEmpty) Text(o.note, style: note),
                  ],
                ),
              ),
            ],
          ),
        ),
      );

  /// 글을 쳐서 답하는 칸 — 단추로 두면 빈 Enter 가 가서 질문 전체가 거절된다.
  Widget _inputRow(int i, PromptOption o, Color accent, ColorScheme scheme) => Row(
        children: [
          if (o.checked != null) _box(o.checked!, accent, scheme),
          Expanded(
            child: TextField(
              controller: _typed,
              minLines: 1,
              maxLines: 3,
              textInputAction: TextInputAction.send,
              onSubmitted: (_) => _answer(i),
              style: const TextStyle(fontSize: Look.body),
              decoration: InputDecoration(
                hintText: _label(o),
                enabledBorder: o.current
                    ? OutlineInputBorder(borderRadius: Look.corners, borderSide: BorderSide(color: accent))
                    : null,
                suffixIcon: ValueListenableBuilder(
                  valueListenable: _typed,
                  builder: (context, value, _) => IconButton(
                    tooltip: '이 글로 답하기',
                    onPressed: value.text.trim().isEmpty ? null : () => _answer(i),
                    icon: const Icon(Icons.arrow_upward, size: Look.iconSize),
                  ),
                ),
              ),
            ),
          ),
        ],
      );

  Widget _reveal({required bool up}) => Align(
        alignment: Alignment.centerLeft,
        child: TextButton.icon(
          onPressed: () => widget.onReveal!(up),
          icon: Icon(up ? Icons.keyboard_arrow_up : Icons.keyboard_arrow_down, size: Look.iconSize),
          label: Text(up ? '위 선택지 더 보기' : '아래 선택지 더 보기'),
        ),
      );
}

/// 질문 탭 하나 — 보이는 알약은 낮아도 누름 영역은 44.
class _TabChip extends StatelessWidget {
  const _TabChip({required this.tab, required this.accent, required this.onTap});
  final PromptTab tab;
  final Color accent;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final on = tab.current;
    return Semantics(
      button: onTap != null,
      selected: on,
      child: InkWell(
        onTap: onTap,
        borderRadius: Look.corners,
        child: ConstrainedBox(
          constraints: const BoxConstraints(minHeight: Look.tap, minWidth: Look.tap),
          child: Center(
            widthFactor: 1,
            child: Container(
              padding: const EdgeInsets.symmetric(horizontal: Look.chipPadX + 2, vertical: 4),
              decoration: BoxDecoration(
                color: on ? accent.withValues(alpha: 0.18) : null,
                border: Border.all(color: on ? accent : scheme.outline),
                borderRadius: BorderRadius.circular(Look.radiusSm),
              ),
              child: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  if (tab.done)
                    Padding(
                      padding: const EdgeInsets.only(right: 4),
                      child: Icon(Icons.check, size: 14, color: accent),
                    ),
                  Text(
                    tab.label,
                    style: TextStyle(
                      fontSize: Look.sub,
                      color: on ? accent : scheme.onSurface,
                      fontWeight: on ? FontWeight.w600 : null,
                    ),
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}
