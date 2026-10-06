import 'dart:async';
import 'dart:math' as math;
import '../background_grace.dart';
import '../device_shape.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/services.dart';

import '../claude_style.dart';
import '../grid_canvas.dart';
import '../hardware_keys.dart';
import '../live_input.dart';
import '../image_attachment.dart';
import '../photo_attachment_button.dart';
import '../hub_model.dart';
import '../look.dart';
import '../server.dart';
import '../status_style.dart';
import '../student_art.dart';
import '../term_session.dart';
import '../theme_prefs.dart';
import '../weather/scene.dart';
import 'conversation_view.dart';
import 'shell_blocks_view.dart';
import 'controls.dart';

/// 학생 하나의 화면. 위는 격자(또는 그림), 아래는 키 줄과 답장 입력창.
class TerminalScreen extends StatefulWidget {
  const TerminalScreen({
    super.key,
    required this.server,
    required this.pane,
    this.initialScroll,
    this.session,
    this.pickImage,
  });

  final Server server;
  final Pane pane;

  /// 검증용 — 열자마자 위로 이만큼(px) 넘긴 상태로.
  final double? initialScroll;
  @visibleForTesting
  final TermSession? session;
  @visibleForTesting
  final Future<Uint8List?> Function()? pickImage;

  @override
  State<TerminalScreen> createState() => _TerminalScreenState();
}

class _TerminalScreenState extends State<TerminalScreen> {
  late final TermSession _session =
      widget.session ?? TermSession(widget.server, widget.pane);
  final _input = TextEditingController();
  final _inputFocus = FocusNode();

  /// 대화 보기의 입력줄은 따로 — 바로 치기 칸의 글은 이미 화면 입력상자에 가 있어 같은 칸을 쓰면
  /// 대화에서 보낼 때 두 번 들어간다. 두 칸 다 살려 두니 쪽을 바꿔도 쓰던 글이 남는다.
  final _chatInput = TextEditingController();
  final _chatFocus = FocusNode();
  bool _ctrl = false;
  bool _sending = false;
  bool _attaching = false;

  /// 입력상자에 붙여 두고 아직 안 보낸 사진. 대화 보기는 상자를 안 보여 주니 입력줄 위에 띄운다.
  final List<Uint8List> _pendingPhotos = [];
  bool get _pendingAttachment => _pendingPhotos.isNotEmpty;

  /// 지금의 pane — 열 때 받은 것으로 시작해 목록을 다시 받아 갈아 끼운다. 셸에서
  /// claude 를 띄우면 이름·얼굴·상태가 따라오고, 사진 버튼도 그때 켜진다(2026-09-17
  /// 지적 「머리글은 셸인데 버튼은 되는데?」·「학생 이미지 안 보여」).
  late Pane _pane = widget.pane;
  Timer? _paneTimer;
  bool _paneRefreshing = false;

  /// 바로 치기(기본) — 확정된 글자가 곧바로 화면의 입력상자에 붙는다. 끄면 아래
  /// 칸에 적어 두었다 한 번에 보낸다(긴 글을 다듬을 때).
  bool _live = true;
  final _liveInput = LiveInput();
  String _composing = '';
  DateTime _lastLiveSend = DateTime.fromMillisecondsSinceEpoch(0);

  /// 입력칸을 프로그램이 비우는 동안 — 그 변화를 지우기로 보내지 않게.
  bool _resetting = false;

  /// 폰 폭으로 접어 보기(기본). 끄면 데스크톱 격자 그대로를 옆으로 밀어 읽는다.
  bool _wrap = true;

  /// 답장·키를 보낼 때마다 올린다 — 화면이 맨 아래로 내려간다.
  int _bottomTick = 0;

  void _toBottom() => setState(() => _bottomTick++);

  /// 터미널(0) ↔ 대화(1) 두 쪽. 손가락은 PageView 가 아니라 [_ViewSwipe] 가 받는다 —
  /// 기본 밀기엔 각도·시작 자리 판정이 없어 비스듬히 읽어 내리다 쪽이 넘어간다.
  late final PageController _pages = PageController(initialPage: _shownPage);
  Drag? _drag;

  /// 쪽 사이에 걸쳐 있는 동안 — 두 쪽 다 깨어 있어야 밀려 들어오는 쪽이 멈춰 보이지 않는다.
  bool _paging = false;
  late bool _lastSecond = _hasSecond(_pane);

  int get _shownPage =>
      _hasSecond(_pane) && paneView.value == PaneView.chat ? 1 : 0;

  @override
  void initState() {
    super.initState();
    paneView.addListener(_followView);
    BackgroundGrace.instance.addListener(_graceChanged);
    _inputFocus.onKeyEvent = _onHardwareKey;
    _session.connect();
    _startPaneRefresh();
  }

  void _startPaneRefresh() {
    _paneTimer ??= Timer.periodic(HubModel.pollEvery, (_) => _refreshPane());
  }

  void _stopPaneRefresh() {
    _paneTimer?.cancel();
    _paneTimer = null;
  }

  /// 허브와 같은 박자로 이 pane 한 줄만 다시 받는다. 실패는 조용히 — 다음 바퀴가 있다.
  Future<void> _refreshPane() async {
    if (_paneRefreshing || !mounted) return;
    _paneRefreshing = true;
    try {
      final panes = await widget.server.panes(machine: widget.pane.machine);
      final found = panes.where((p) => p.id == widget.pane.id).firstOrNull;
      if (found == null || !mounted || _samePane(found, _pane)) return;
      setState(() => _pane = found);
    } catch (_) {
      // 목록 한 번 못 받은 것 — 화면은 열 때 정보로 계속 그린다.
    } finally {
      _paneRefreshing = false;
    }
  }

  static bool _samePane(Pane a, Pane b) =>
      a.name == b.name &&
      a.slug == b.slug &&
      a.status == b.status &&
      a.kind == b.kind &&
      a.idleSecs == b.idleSecs &&
      a.doing == b.doing &&
      a.compactPct == b.compactPct &&
      a.session == b.session &&
      a.harness == b.harness &&
      a.cwd == b.cwd &&
      a.branch == b.branch &&
      a.mirrorOf == b.mirrorOf &&
      a.contextPct == b.contextPct &&
      a.closed == b.closed;

  @override
  void dispose() {
    _stopPaneRefresh();
    paneView.removeListener(_followView);
    BackgroundGrace.instance.removeListener(_graceChanged);
    _session.dispose();
    _input.dispose();
    _inputFocus.dispose();
    _chatInput.dispose();
    _chatFocus.dispose();
    _pages.dispose();
    super.dispose();
  }

  /// 다른 앱에 다녀오는 동안은 소켓을 그대로 둔다 — 돌아오면 화면이 이미 지금이다.
  void _graceChanged() {
    if (BackgroundGrace.instance.live) {
      _session.resume();
      _startPaneRefresh();
    } else {
      _session.pause();
      _stopPaneRefresh();
    }
  }

  /// 보낼 때 입력창의 글 전체만 읽는다 — 조합 중인 자모가 새어 나갈 길이 없다.
  Future<void> _send(TextEditingController field, FocusNode focus) async {
    final text = field.text;
    if (_sending || _attaching || (text.isEmpty && !_pendingAttachment)) return;
    setState(() {
      _sending = true;
      _bottomTick++;
    });
    try {
      if (text.isEmpty && _pendingAttachment) {
        _session.sendText('\r');
      } else if (_ctrl && text.length == 1 && field == _input) {
        _session.ctrl(text);
      } else if (_pendingAttachment) {
        await _session.replyAfterAttachment(text);
      } else {
        await _session.reply(text);
      }
      if (!mounted || widget.server.isClosed) return;
      field.clear();
      _ctrl = false;
      _pendingPhotos.clear();
    } on ServerException catch (e) {
      if (mounted) _toast(e.message);
    } finally {
      if (mounted) {
        setState(() => _sending = false);
        focus.requestFocus();
      }
    }
  }

  void _sendLive(List<int> bytes) {
    if (bytes.isEmpty) return;
    _session.sendBytes(bytes);
    _lastLiveSend = DateTime.now();
    _bottomTick++;
  }

  /// 하드웨어 키보드(아이패드) — 입력칸이 못 받는 키를 pane 으로. 터미널 입력칸에만 단다 —
  /// 대화 보기의 입력칸은 말풍선을 쓰는 칸이라 넘기지 않는다.
  KeyEventResult _onHardwareKey(FocusNode _, KeyEvent e) {
    final s = _session;
    if (e is KeyUpEvent || !s.canSend || _attaching) return KeyEventResult.ignored;
    final keys = HardwareKeyboard.instance;
    // Shift+Enter — 소프트 키 ⇧↵ 와 같은 줄바꿈. 칸의 글을 먼저 보내고 잇는다.
    if (_live &&
        keys.isShiftPressed &&
        (e.logicalKey == LogicalKeyboardKey.enter ||
            e.logicalKey == LogicalKeyboardKey.numpadEnter)) {
      _liveSubmit('\n');
      return KeyEventResult.handled;
    }
    final k = hardwareKey(
      e.logicalKey,
      e.physicalKey,
      ctrl: keys.isControlPressed,
      shift: keys.isShiftPressed,
      alt: keys.isAltPressed,
      meta: keys.isMetaPressed,
      fieldEmpty: _input.text.isEmpty,
      appCursor: s.grid.appCursor,
    );
    if (k == null) return KeyEventResult.ignored;
    // 적어 두는 칸의 글은 pane 에 아직 없다 — pane 이 비워도 그대로 둔다.
    if (_live && k.resetField) setState(_dropLive);
    s.sendBytes(k.bytes);
    _toBottom();
    return KeyEventResult.handled;
  }

  /// 입력칸을 비운다 — 이미 pane 에 간 글을 지우기로 보내지 않고.
  void _dropLive() {
    _resetting = true;
    _liveInput.reset();
    _input.clear();
    _resetting = false;
    _composing = '';
  }

  void _onLiveChanged(String _) {
    if (_resetting) return;
    _sendLive(_liveInput.update(_input.value));
    setState(() => _composing = _liveInput.composing);
  }

  /// 엔터 — 글자 바로 뒤에 붙여 보내면 Ink 가 엔터를 먹는다(서버 `send` 가 140ms 를
  /// 기다리는 이유와 같다). 마지막 글자에서 조금 떨어뜨려 보낸다. [enter] 가 LF 면 줄바꿈.
  Future<void> _liveSubmit([String enter = '\r']) async {
    if (_sending || _attaching) return;
    _sendLive(_liveInput.flush(_input.value));
    setState(_dropLive);
    final gap = DateTime.now().difference(_lastLiveSend);
    if (gap < TermSession.enterGap) {
      await Future<void>.delayed(TermSession.enterGap - gap);
    }
    if (!mounted || widget.server.isClosed) return;
    _session.sendText(enter);
    if (enter == '\r') _pendingPhotos.clear();
    _toBottom();
    _inputFocus.requestFocus();
  }

  void _toggleLive() {
    setState(() {
      _live = !_live;
      _dropLive();
    });
    _inputFocus.requestFocus();
  }

  void _toast(String text) {
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(text)));
  }

  String _stateText(TermSession s) => switch (s.state) {
    TermState.connecting => '연결 중…',
    // 웹 셸은 서버가 id 로 붙는 모든 연결을 미러로 보지만 데스크톱에 원본 화면이 없다.
    TermState.connected =>
      s.mirror && !_pane.isWebShell ? '데스크톱 화면 그대로' : '웹 셸',
    TermState.reconnecting => '다시 연결 중…',
    TermState.gone => '끝난 화면',
  };

  /// 데스크톱의 × 와 같다 — 되살리기 대열에 남는다. 닫히면 허브로 돌아간다.
  Future<void> _closePane(Pane pane) async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (ctx) => ModalLook(
        child: AlertDialog(
          content: Text('${pane.displayName} 을(를) 닫을까?'),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(ctx, false),
              child: const Text('아니'),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(ctx, true),
              child: const Text('닫기'),
            ),
          ],
        ),
      ),
    );
    if (ok != true || !mounted) return;
    try {
      await widget.server.closePane(pane.id, machine: pane.machine);
    } on ServerException catch (e) {
      if (!mounted) return;
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(SnackBar(content: Text(e.message)));
      return;
    }
    if (mounted) Navigator.of(context).pop();
  }

  /// 대화 기록이 있는 창인가 — 셸·웹 셸엔 학생이 없어 격자만 있다.
  static bool _canChat(Pane p) => !p.isShell && !p.isWebShell;

  /// 데스크톱 셸 칸 — 둘째 쪽이 명령 묶음(`ShellBlocksView`)이다. 폰이 연 웹 셸은 폰 제 터미널이라 뺀다.
  static bool _canBlocks(Pane p) => p.isShell && !p.isWebShell;

  /// 터미널 옆에 둘째 쪽(대화·명령 묶음)이 있나.
  static bool _hasSecond(Pane p) => _canChat(p) || _canBlocks(p);

  void _showTerminal() => _choose(PaneView.terminal);

  /// 단추·점·「터미널로 보기」 — 고른 쪽을 기억하고, 쪽은 [_followView] 가 옮긴다.
  void _choose(PaneView v) {
    paneView.value = v;
    const PaneViewPrefs().save(v);
  }

  /// 쪽을 [paneView] 에 맞춘다. 밀어서 바뀐 것이면 이미 그 쪽이라 그대로 둔다. 입력칸에 초점이
  /// 있었으면 새 쪽 입력칸으로 옮긴다 — 두 칸 다 살아 있어 자판이 내려갔다 올라오지 않는다.
  void _followView() {
    if (!mounted) return;
    final want = _shownPage;
    if (_inputFocus.hasFocus || _chatFocus.hasFocus) {
      (want == 1 ? _chatFocus : _inputFocus).requestFocus();
    }
    if (!_pages.hasClients || _drag != null) return;
    if ((_pages.page ?? want.toDouble()).round() == want) return;
    if (Look.still(context)) {
      _pages.jumpToPage(want);
    } else {
      _pages.animateToPage(
        want,
        duration: Look.viewFlip,
        curve: Curves.easeOutCubic,
      );
    }
  }

  /// 밀기를 받을 자리인가. 뒤로 가기 띠에서 시작한 손가락은 뒤로 가기 몫이고, 격자 그대로
  /// 보기는 손가락이 격자를 상하좌우로 끌어 읽으니 그 쪽에선 단추로만 바꾼다.
  bool _swipeStarts(Offset at) {
    if (!_hasSecond(_pane) || !_pages.hasClients) return false;
    final edge = math.max(MediaQuery.paddingOf(context).left, Look.backEdge);
    if (at.dx < edge) return false;
    return _wrap || (_pages.page ?? 0) >= 0.5;
  }

  void _swipeStart(DragStartDetails d) =>
      _drag = _pages.position.drag(d, () => _drag = null);

  /// 놓으면 가까운 쪽으로 미끄러진다. 동작 줄이기면 미끄러지지 않고 그 자리에서 바로 —
  /// 판정은 PageScrollPhysics 와 같다(튕긴 쪽으로 반 쪽 더 간 셈 치고 반올림).
  void _swipeEnd(DragEndDetails d) {
    if (!Look.still(context)) {
      _drag?.end(d);
      return;
    }
    final v = -(d.primaryVelocity ?? 0);
    final page = (_pages.page ?? 0) + (v == 0 ? 0 : v.sign * 0.5);
    _pages.jumpToPage(page.round().clamp(0, _hasSecond(_pane) ? 1 : 0));
  }

  /// 쪽이 반을 넘으면 그 쪽이 지금 보기다 — 전환 단추·입력줄·앱바 단추가 따라온다.
  bool _onPageScroll(ScrollNotification n) {
    if (n.depth != 0 || n.metrics.axis != Axis.horizontal) return false;
    _afterLayout(() {
      switch (n) {
        case ScrollStartNotification():
          if (!_paging) setState(() => _paging = true);
        case ScrollUpdateNotification():
          // 학생이 나가 대화 쪽이 사라지며 되돌아가는 것은 고른 보기를 바꾸지 않는다.
          if (!_hasSecond(_pane) || !_pages.hasClients) return;
          final v = (_pages.page ?? 0).round() >= 1
              ? PaneView.chat
              : PaneView.terminal;
          if (paneView.value != v) paneView.value = v;
        case ScrollEndNotification():
          if (_paging) setState(() => _paging = false);
          if (_hasSecond(_pane)) const PaneViewPrefs().save(paneView.value);
        default:
      }
    });
    return false;
  }

  /// 쪽 알림은 배치 중에도 온다(쪽 수가 줄어 되돌아갈 때). 그때 화면을 다시 세우면 안 되니 프레임 뒤로.
  void _afterLayout(VoidCallback fn) {
    if (SchedulerBinding.instance.schedulerPhase ==
        SchedulerPhase.persistentCallbacks) {
      SchedulerBinding.instance.addPostFrameCallback((_) {
        if (mounted) fn();
      });
    } else {
      fn();
    }
  }

  Widget _page(int index, int shown, Widget child) => _KeepPage(
    child: TickerMode(enabled: _paging || index == shown, child: child),
  );

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: Listenable.merge([_session, paneView]),
    builder: (context, _) {
      final theme = Theme.of(context);
      final scheme = theme.colorScheme;
      final s = _session;
      final pane = _pane;
      final accent = studentAccent(context, pane, s.tokens);
      final slug = pane.slug;
      final canChat = _canChat(pane);
      final blocks = _canBlocks(pane);
      final second = canChat || blocks;
      final chat = second && paneView.value == PaneView.chat;
      // 폰 폭으로 접어 보는 터미널에서 친 동안만 원본 격자를 폰 크기로 쥔다(TermSession.holdViewport).
      // 셸 칸은 쥐지 않는다 — 명령 묶음·줄여 보기로 원본 크기 없이 그린다(docs/mirror-render.md).
      final hold = _wrap && !chat && !blocks;
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) s.holdViewport = hold;
      });
      if (second != _lastSecond) {
        _lastSecond = second;
        WidgetsBinding.instance.addPostFrameCallback((_) => _followView());
      }
      // 자판이 뜨면 머리·전환 줄을 앱바 한 줄로 접는다 — 글 보이는 높이가 307pt(35%)까지 줄었다.
      final typing = MediaQuery.viewInsetsOf(context).bottom > 0;
      return _StudentFrame(
        accent: accent,
        child: Scaffold(
          appBar: AppBar(
            titleSpacing: 0,
            toolbarHeight: typing ? Look.tap : Look.appBarH,
            title: Row(
              children: [
                Hero(
                  tag: 'face-${pane.machine}-${pane.id}',
                  // 화면의 주인공은 프사(사진) — 목록의 도트가 여기로 날아와 얼굴이 된다.
                  child: StudentFace(
                    server: widget.server,
                    slug: slug,
                    url: slug == null
                        ? null
                        : widget.server.avatar(slug, machine: pane.machine),
                    shell: pane.isShell,
                    size: typing ? 28 : 40,
                  ),
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Row(
                        children: [
                          Flexible(
                            child: Text(
                              pane.displayName,
                              style: theme.textTheme.titleMedium,
                              overflow: TextOverflow.ellipsis,
                            ),
                          ),
                          if ((pane.session ?? '').isNotEmpty) ...[
                            const SizedBox(width: 6),
                            Flexible(child: SessionTag(pane.session!)),
                          ],
                          if ((pane.mirrorOf ?? '').isNotEmpty) ...[
                            const SizedBox(width: 6),
                            MirrorTag(pane.mirrorOf!),
                          ],
                        ],
                      ),
                      Row(
                        children: [
                          // 열 때의 상태 — 허브 칩과 같은 색·말. 연결 상태는 그 뒤에.
                          Flexible(
                            child: Builder(
                              builder: (context) {
                              final st = StatusStyle.of(pane, scheme);
                              return Row(
                                mainAxisSize: MainAxisSize.min,
                                children: [
                                  if (st.live)
                                    PulseDot(color: st.color, size: 6)
                                  else
                                    Icon(st.icon, size: 11, color: st.color),
                                  const SizedBox(width: 3),
                                  Flexible(
                                    child: Text(
                                      st.label,
                                      maxLines: 1,
                                      overflow: TextOverflow.ellipsis,
                                      style: theme.textTheme.labelSmall
                                          ?.copyWith(
                                            color: st.color,
                                            fontWeight: FontWeight.w600,
                                          ),
                                    ),
                                  ),
                                ],
                              );
                              },
                            ),
                          ),
                          Flexible(
                            child: Text(
                              '  ·  ${_stateText(s)}',
                              style: theme.textTheme.labelSmall?.copyWith(
                                color: scheme.onSurfaceVariant,
                              ),
                              overflow: TextOverflow.ellipsis,
                            ),
                          ),
                        ],
                      ),
                    ],
                  ),
                ),
              ],
            ),
            bottom: second && !typing ? PaneViewSwitch(shell: blocks) : null,
            actions: [
              // 전환 줄을 접은 동안에도 지금 보기가 보이게.
              if (second && typing)
                _ViewDots(
                  chat: chat,
                  onTap: () =>
                      _choose(chat ? PaneView.terminal : PaneView.chat),
                ),
              // 글자 선택·접기는 격자 얘기다 — 대화 보기에선 말풍선을 꾹 눌러 복사한다.
              if (!chat) ...[
                // 글자 선택 — 격자는 손가락으로 못 긁으니 화면 글자를 그대로 선택 상자에
                // 띄운다(2026-09-10 지시 「꾹 누르는 건 선택이 안 되는데 클립보드 기능」).
                IconButton(
                  tooltip: '글자 선택·복사',
                  onPressed: () => _selectText(s),
                  icon: const Icon(Icons.content_copy_outlined),
                ),
                IconButton(
                  tooltip: _wrap ? '데스크톱 격자 그대로 보기' : '폰 폭에 맞춰 보기',
                  isSelected: _wrap,
                  onPressed: () => setState(() => _wrap = !_wrap),
                  icon: const Icon(Icons.wrap_text),
                ),
              ],
              IconButton(
                tooltip: 'pane 닫기',
                onPressed: () => _closePane(pane),
                icon: const Icon(Icons.close),
              ),
            ],
          ),
          body: SafeArea(
            child: Column(
              children: [
                Expanded(
                  child: NotificationListener<ScrollNotification>(
                    onNotification: _onPageScroll,
                    child: RawGestureDetector(
                      behavior: HitTestBehavior.translucent,
                      gestures: {
                        _ViewSwipe:
                            GestureRecognizerFactoryWithHandlers<_ViewSwipe>(
                              () => _ViewSwipe(debugOwner: this),
                              (r) => r
                                ..starts = _swipeStarts
                                ..onStart = _swipeStart
                                ..onUpdate = (d) {
                                  _drag?.update(d);
                                }
                                ..onEnd = _swipeEnd
                                ..onCancel = () {
                                  _drag?.cancel();
                                },
                            ),
                      },
                      child: PageView(
                        controller: _pages,
                        physics: const NeverScrollableScrollPhysics(),
                        children: [
                          // 좌우 숨 — 글자가 화면 끝에 닿으면 답답하고, 0열에 잉크가 있는 글자가
                          // 잘려 보인다. 학생색 테는 화면 가장자리의 _StudentFrame 이 두른다.
                          _page(
                            0,
                            chat ? 1 : 0,
                            Padding(
                              padding: const EdgeInsets.fromLTRB(12, 4, 12, 4),
                              child: _view(s),
                            ),
                          ),
                          if (blocks)
                            _page(
                              1,
                              chat ? 1 : 0,
                              ShellBlocksView(
                                server: widget.server,
                                pane: pane,
                                session: s,
                                onTerminal: _showTerminal,
                                bottomTick: _bottomTick,
                                active: _paging || chat,
                              ),
                            ),
                          if (canChat)
                            _page(
                              1,
                              chat ? 1 : 0,
                              WeatherScene(
                                child: ConversationView(
                                  server: widget.server,
                                  pane: pane,
                                  session: s,
                                  accent: accent,
                                  onTerminal: _showTerminal,
                                  bottomTick: _bottomTick,
                                  active: _paging || chat,
                                ),
                              ),
                            ),
                        ],
                      ),
                    ),
                  ),
                ),
                if (s.note != null) _NoteBar(text: s.note!),
                // 두 입력줄을 다 살려 두고 지금 쪽 것만 보인다 — 쓰던 글·초점이 쪽을 바꿔도 남는다.
                Offstage(
                  offstage: chat,
                  child: _terminalComposer(s, pane, autofocus: !chat),
                ),
                if (blocks)
                  Offstage(
                    offstage: !chat,
                    child: ChatComposer(
                      controller: _chatInput,
                      focusNode: _chatFocus,
                      enabled:
                          s.state != TermState.gone && !_sending && !_attaching,
                      onSend: () => _send(_chatInput, _chatFocus),
                      hint: '명령 보내기',
                      command: true,
                      stopTip: '멈추기 (ctrl-c)',
                      // 셸은 도는지를 칸 상태로 모른다 — 프롬프트에서 눌러도 줄만 비워 해가 없다.
                      onStop: s.canSend ? () => s.sendText('\x03') : null,
                    ),
                  ),
                if (canChat)
                  Offstage(
                    offstage: !chat,
                    child: ChatComposer(
                      controller: _chatInput,
                      focusNode: _chatFocus,
                      enabled:
                          s.state != TermState.gone && !_sending && !_attaching,
                      onSend: () => _send(_chatInput, _chatFocus),
                      leading: _photoButton(s, pane, chat: true),
                      // 숨은 동안은 미리보기를 풀지 않는다 — 보이는 순간 다시 그린다.
                      photos: chat ? _pendingPhotos : const [],
                      onStop: pane.isBusy && s.canSend
                          ? () => s.sendText('\x1b')
                          : null,
                    ),
                  ),
              ],
            ),
          ),
        ),
      );
    },
  );

  Widget _terminalComposer(
    TermSession s,
    Pane pane, {
    required bool autofocus,
  }) => Column(
    mainAxisSize: MainAxisSize.min,
    children: [
      Row(
        children: [
          _photoButton(s, pane),
          Expanded(
            child: AbsorbPointer(
              absorbing: _attaching,
              child: _KeyBar(
                session: s,
                ctrl: _ctrl,
                onCtrl: () => setState(() => _ctrl = !_ctrl),
                onKey: _toBottom,
                onSubmit: () => setState(_pendingPhotos.clear),
              ),
            ),
          ),
        ],
      ),
      if (_live)
        _LiveBar(
          controller: _input,
          focusNode: _inputFocus,
          enabled: s.state != TermState.gone && !_attaching,
          autofocus: autofocus,
          onChanged: _onLiveChanged,
          onSubmit: _liveSubmit,
          onDraft: _toggleLive,
        )
      else
        _ReplyBar(
          controller: _input,
          focusNode: _inputFocus,
          enabled: s.state != TermState.gone && !_sending && !_attaching,
          onSend: () => _send(_input, _inputFocus),
          onLive: _toggleLive,
        ),
    ],
  );

  Widget _photoButton(TermSession s, Pane pane, {bool chat = false}) => PhotoAttachmentButton(
    server: widget.server,
    pane: pane,
    // 대화 보기는 입력줄 위 미리보기가 같은 말을 한다 — 알림 띠가 입력줄을 덮지 않게.
    confirm: !chat,
    pickImage: widget.pickImage ?? pickAttachmentImage,
    enabled:
        s.state == TermState.connected &&
        !_sending &&
        !pane.isShell &&
        !pane.isWebShell,
    disabledReason: pane.isShell || pane.isWebShell
        ? '학생이 도는 창에서만 사진을 붙일 수 있어요. 셸에서는 먼저 claude 를 띄워 주세요.'
        : s.state == TermState.connected
        ? '보내는 중이에요. 잠시 뒤 다시 눌러 주세요.'
        : '연결이 끊겨 사진을 붙일 수 없어요. 다시 연결되면 켜져요.',
    onBusy: (busy) => setState(() => _attaching = busy),
    onAttached: (photo) => setState(() {
      _pendingPhotos.add(photo);
      _bottomTick++;
    }),
  );

  /// 지난 줄과 살아 있는 화면을 글자로 이어 붙여 iOS 선택 손잡이가 붙는 상자에 띄운다.
  /// 끝 공백은 걷고 빈 줄 뭉치는 하나로 — 격자 그대로 붙이면 절반이 공백이다.
  Future<void> _selectText(TermSession s) {
    final lines = <String>[
      for (final r in s.history) r.map((x) => x.text).join().trimRight(),
      for (final r in s.grid.lines) r.map((x) => x.text).join().trimRight(),
    ];
    final buf = <String>[];
    for (final l in lines) {
      if (l.isEmpty && (buf.isEmpty || buf.last.isEmpty)) continue;
      buf.add(l);
    }
    final text = buf.join('\n').trim();
    return showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (sheet) => DraggableScrollableSheet(
        expand: false,
        initialChildSize: 0.7,
        minChildSize: 0.3,
        maxChildSize: 0.95,
        builder: (context, controller) => Column(
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(20, 0, 8, 4),
              child: Row(
                children: [
                  Text('글자 선택', style: Theme.of(context).textTheme.titleMedium),
                  const Spacer(),
                  TextButton.icon(
                    onPressed: () async {
                      await Clipboard.setData(ClipboardData(text: text));
                      if (sheet.mounted) Navigator.of(sheet).pop();
                    },
                    icon: const Icon(Icons.copy, size: 18),
                    label: const Text('전부 복사'),
                  ),
                ],
              ),
            ),
            Expanded(
              child: SingleChildScrollView(
                controller: controller,
                padding: const EdgeInsets.fromLTRB(16, 0, 16, 24),
                child: SelectableText(
                  text.isEmpty ? '(빈 화면)' : text,
                  style: const TextStyle(fontFamily: 'TermMono',
                    fontFamilyFallback: Look.flowMonoFallback, fontSize: 14),
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }

  Widget _view(TermSession s) {
    final tokens = s.tokens;
    final palette = TerminalPalette.forViewer(
      context,
      mode: phoneThemeMode.value,
      source: tokens,
    );
    if (_wrap) {
      final pane = _pane;
      return WrappedCanvas(
        grid: s.grid,
        history: s.history,
        historyVersion: s.historyVersion,
        version: s.grid.version + s.historyVersion,
        palette: palette,
        bottomTick: _bottomTick,
        initialScroll: widget.initialScroll,
        composing: _live ? _composing : null,
        onViewport: s.setViewport,
        onWheel: s.scrollsApp ? s.wheel : null,
        fullScreen: s.grid.alt,
        // 웹 셸엔 학생이 없다 — 데스크톱 pane 만 학생 꾸밈을 입는다.
        student: pane.isWebShell
            ? null
            : StudentStyle(
                slug: pane.slug,
                name: pane.name,
                accent: studentAccent(context, pane, tokens),
                bg: palette.bg,
                codex: pane.harness == 'codex',
                session: pane.session,
                branch: pane.branch,
                project: pane.cwd
                    .split('/')
                    .where((s) => s.isNotEmpty)
                    .lastOrNull,
                cwd: pane.cwd,
              ),
      );
    }
    return GridCanvas(
      grid: s.grid,
      version: s.grid.version,
      palette: palette,
      composing: _live ? _composing : null,
      onWheel: s.scrollsApp ? s.wheel : null,
    );
  }
}

/// 터미널 ↔ 대화 밀기의 손가락 판정(design.md 「터미널 ↔ 대화 밀기」).
///
/// - 가로가 세로의 [Look.swipeRatio] 배 이상일 때만 받는다. 세로 스크롤은 세로 18 에서 곧바로
///   받으니 그보다 비스듬한 밀기는 읽기 스크롤로 간다.
/// - 안쪽 가로 스크롤(코드 칸·표)은 더 깊어 먼저 판정받고 각도 조건도 없어, 밀 거리가 있으면
///   그쪽이 이긴다. 내용이 칸에 다 들어가 밀 거리가 없으면 쪽 넘김이 받는다.
/// - [starts] 가 거절한 자리(뒤로 가기 띠 등)에서 시작한 손가락은 아예 보지 않는다.
class _ViewSwipe extends HorizontalDragGestureRecognizer {
  // 다른 손이 없으면 손을 뗄 때 경기장이 남은 우리에게 이김을 넘긴다 — 문턱을 못 넘은
  // 비스듬한 밀기가 그때 튕김으로 쪽을 넘기지 않게, 문턱을 넘은 밀기만 끈다.
  _ViewSwipe({super.debugOwner}) {
    onlyAcceptDragOnThreshold = true;
  }

  bool Function(Offset global) starts = _anywhere;
  static bool _anywhere(Offset _) => true;
  Offset _moved = Offset.zero;

  @override
  bool isPointerAllowed(PointerEvent event) =>
      starts(event.position) && super.isPointerAllowed(event);

  @override
  void addAllowedPointer(PointerDownEvent event) {
    _moved = Offset.zero;
    super.addAllowedPointer(event);
  }

  @override
  void addAllowedPointerPanZoom(PointerPanZoomStartEvent event) {
    _moved = Offset.zero;
    super.addAllowedPointerPanZoom(event);
  }

  @override
  void handleEvent(PointerEvent event) {
    if (event is PointerMoveEvent) _moved += event.delta;
    if (event is PointerPanZoomUpdateEvent) _moved = event.pan;
    super.handleEvent(event);
  }

  @override
  bool hasSufficientGlobalDistanceToAccept(
    PointerDeviceKind pointerDeviceKind,
    double? deviceTouchSlop,
  ) =>
      super.hasSufficientGlobalDistanceToAccept(
        pointerDeviceKind,
        deviceTouchSlop,
      ) &&
      _moved.dx.abs() >= _moved.dy.abs() * Look.swipeRatio;
}

/// 밀어 둔 쪽도 그대로 — 대화를 다시 받거나 터미널 스크롤이 맨 아래로 돌아가지 않게.
class _KeepPage extends StatefulWidget {
  const _KeepPage({required this.child});

  final Widget child;

  @override
  State<_KeepPage> createState() => _KeepPageState();
}

class _KeepPageState extends State<_KeepPage>
    with AutomaticKeepAliveClientMixin {
  @override
  bool get wantKeepAlive => true;

  @override
  Widget build(BuildContext context) {
    super.build(context);
    return widget.child;
  }
}

/// 자판이 떠 전환 줄을 접었을 때의 지금 보기 — 왼쪽 점이 터미널, 오른쪽 점이 대화.
/// 누르면 다른 쪽으로 간다.
class _ViewDots extends StatelessWidget {
  const _ViewDots({required this.chat, required this.onTap});

  final bool chat;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    Widget dot(bool on) => Container(
      width: Look.viewDot,
      height: Look.viewDot,
      decoration: BoxDecoration(
        shape: BoxShape.circle,
        color: on ? scheme.primary : scheme.outline,
      ),
    );
    return IconButton(
      tooltip: chat ? '대화 보기 · 눌러 터미널로' : '터미널 보기 · 눌러 대화로',
      onPressed: onTap,
      icon: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          dot(!chat),
          const SizedBox(width: Look.viewDotGap),
          dot(chat),
        ],
      ),
    );
  }
}

class _NoteBar extends StatelessWidget {
  const _NoteBar({required this.text});

  final String text;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      width: double.infinity,
      decoration: BoxDecoration(border: Border(top: BorderSide(color: scheme.outline))),
      padding: const EdgeInsets.symmetric(horizontal: Look.pagePad, vertical: 8),
      child: Text(text, style: Theme.of(context).textTheme.bodySmall),
    );
  }
}

class _KeyBar extends StatelessWidget {
  const _KeyBar({
    required this.session,
    required this.ctrl,
    required this.onCtrl,
    required this.onKey,
    required this.onSubmit,
  });

  final TermSession session;
  final bool ctrl;
  final VoidCallback onCtrl;

  /// 키를 보낸 뒤 — 화면을 맨 아래로.
  final VoidCallback onKey;
  final VoidCallback onSubmit;

  @override
  Widget build(BuildContext context) {
    final s = session;
    VoidCallback tap(void Function() send) => () {
      send();
      onKey();
    };
    final keys = <Widget>[
      _Key(label: 'esc', onTap: tap(() => s.sendText('\x1b'))),
      _Key(label: 'tab', onTap: tap(() => s.sendText('\t'))),
      _Key(label: 'ctrl', selected: ctrl, onTap: onCtrl),
      _Key(label: '^C', onTap: tap(() => s.ctrl('c'))),
      _Key(icon: Icons.keyboard_arrow_left, onTap: tap(() => s.arrow('D'))),
      _Key(icon: Icons.keyboard_arrow_down, onTap: tap(() => s.arrow('B'))),
      _Key(icon: Icons.keyboard_arrow_up, onTap: tap(() => s.arrow('A'))),
      _Key(icon: Icons.keyboard_arrow_right, onTap: tap(() => s.arrow('C'))),
      _Key(
        icon: Icons.backspace_outlined,
        onTap: tap(() => s.sendText('\x7f')),
      ),
      // 줄바꿈 — 데스크톱의 Shift+Enter 와 같은 바이트(LF). claude·codex 둘 다
      // CR 은 제출, 맨 LF 는 줄바꿈으로 읽는다. 폰 자판엔 Shift+Enter 가 없어
      // 여러 줄 프롬프트를 칠 길이 없었다(2026-09-17 지적).
      _Key(label: '⇧↵', onTap: tap(() => s.sendText('\n'))),
      _Key(
        icon: Icons.keyboard_return,
        onTap: tap(() {
          s.sendText('\r');
          onSubmit();
        }),
      ),
    ];
    return SizedBox(
      height: 56,
      child: ListView.separated(
        scrollDirection: Axis.horizontal,
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 6),
        itemCount: keys.length,
        separatorBuilder: (_, _) => const SizedBox(width: 8),
        itemBuilder: (_, i) => keys[i],
      ),
    );
  }
}

class _Key extends StatelessWidget {
  const _Key({
    this.label,
    this.icon,
    required this.onTap,
    this.selected = false,
  });

  final String? label;
  final IconData? icon;
  final VoidCallback onTap;
  final bool selected;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    // 쌍둥이 결 — 테 없이 톤 채움, 켜진 ctrl 은 강조 물·글자.
    final ink = selected ? scheme.primary : scheme.onSurface;
    return Material(
      color: selected ? scheme.primary.withValues(alpha: 0.18) : scheme.surfaceContainerHigh,
      shape: RoundedRectangleBorder(borderRadius: Look.corners),
      child: InkWell(
        onTap: onTap,
        borderRadius: Look.corners,
        child: Container(
          constraints: const BoxConstraints(minWidth: Look.tap, minHeight: Look.tap),
          padding: const EdgeInsets.symmetric(horizontal: 10),
          alignment: Alignment.center,
          child: icon != null
              ? Icon(icon, size: Look.iconSize, color: ink)
              : Text(
                  label!,
                  style: TextStyle(
                    fontFamily: 'TermMono',
                    fontSize: Look.sub,
                    color: ink,
                  ),
                ),
        ),
      ),
    );
  }
}

class _ReplyBar extends StatelessWidget {
  const _ReplyBar({
    required this.controller,
    required this.focusNode,
    required this.enabled,
    required this.onSend,
    required this.onLive,
  });

  final TextEditingController controller;
  final FocusNode focusNode;
  final bool enabled;
  final VoidCallback onSend;

  /// 바로 치기로 돌아가기.
  final VoidCallback onLive;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(8, 0, 8, 8),
    child: Row(
      children: [
        IconButton(
          onPressed: onLive,
          icon: const Icon(Icons.keyboard_alt_outlined),
          tooltip: '바로 치기',
        ),
        Expanded(
          child: TextField(
            controller: controller,
            focusNode: focusNode,
            enabled: enabled,
            autocorrect: false,
            enableSuggestions: false,
            minLines: 1,
            maxLines: Look.inputMaxLines,
            textInputAction: TextInputAction.send,
            onSubmitted: (_) => onSend(),
            // 16px 아래로 내리면 iOS 가 포커스 때 화면을 확대한다.
            style: const TextStyle(fontSize: 16),
            decoration: const InputDecoration(hintText: '적어 두고 한 번에 보내기…'),
          ),
        ),
        const SizedBox(width: 8),
        ListenableBuilder(
          listenable: controller,
          builder: (context, _) => SendButton(
            ready: controller.text.trim().isNotEmpty,
            onPressed: enabled ? onSend : null,
          ),
        ),
      ],
    ),
  );
}

/// 바로 치기 칸 — 친 글자는 화면의 입력상자에 붙고, 이 칸에도 그대로 보인다(칸이
/// 비어 보이면 어디에 치는지 모른다 — 2026-09-06 「이러면 폼에는 없어」). 엔터로 보내면
/// 칸이 비고 화면의 입력상자는 그대로다.
class _LiveBar extends StatelessWidget {
  const _LiveBar({
    required this.controller,
    required this.focusNode,
    required this.enabled,
    required this.autofocus,
    required this.onChanged,
    required this.onSubmit,
    required this.onDraft,
  });

  final TextEditingController controller;
  final FocusNode focusNode;
  final bool enabled;

  /// 열 때 대화 쪽이면 끈다 — 숨은 칸이 초점을 잡아 자판이 뜨면 안 된다.
  final bool autofocus;
  final ValueChanged<String> onChanged;
  final VoidCallback onSubmit;

  /// 적어 두고 보내기로 바꾸기.
  final VoidCallback onDraft;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Padding(
      padding: const EdgeInsets.fromLTRB(8, 0, 8, 8),
      child: Row(
        children: [
          IconButton(
            onPressed: onDraft,
            icon: const Icon(Icons.edit_note),
            tooltip: '적어 두고 한 번에 보내기',
          ),
          Expanded(
            child: TextField(
              controller: controller,
              focusNode: focusNode,
              enabled: enabled,
              autofocus: autofocus,
              autocorrect: false,
              enableSuggestions: false,
              textCapitalization: TextCapitalization.none,
              smartDashesType: SmartDashesType.disabled,
              smartQuotesType: SmartQuotesType.disabled,
              maxLines: 1,
              textInputAction: TextInputAction.send,
              onChanged: onChanged,
              onSubmitted: (_) => onSubmit(),
              // 16px 아래로 내리면 iOS 가 포커스 때 화면을 확대한다.
              style: const TextStyle(fontSize: 16),
              decoration: InputDecoration(
                hintText: '치는 대로 화면에 붙는다',
                hintStyle: TextStyle(color: scheme.onSurfaceVariant),
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// 학생색 테를 폰 화면 가장자리 전체에 — 상태바·홈 막대까지 한 겹으로 감싸 그 학생의
/// 화면 안에 들어와 있는 느낌을 준다(2026-09-08 지시 「폰 화면 전체에 뜨게, 몰입감 있게」).
/// 바깥 선 하나에 안쪽으로 옅은 빛띠 하나. 모서리는 아이폰 화면 모서리를 따라 둥글다.
class _StudentFrame extends StatelessWidget {
  const _StudentFrame({required this.accent, required this.child});

  final Color accent;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    // 테는 창 전체에 두고 키보드가 그 위를 덮게 둔다 — 키보드 위로 테를 끌어올리면 밑변이
    // 따로 생겨 틀 안의 틀이 된다(2026-09-08 지시, Setlog 앱 참고: 키보드가 떠도 테는 그대로).
    final radius = BorderRadius.circular(
      screenCornerRadius(MediaQuery.sizeOf(context)),
    );
    Widget line(Color color, double width) => IgnorePointer(
      child: DecoratedBox(
        decoration: BoxDecoration(
          borderRadius: radius,
          border: Border.all(color: color, width: width),
        ),
      ),
    );
    return Stack(
      fit: StackFit.expand,
      children: [
        child,
        line(accent.withValues(alpha: 0.10), 9),
        line(accent, 2.5),
      ],
    );
  }
}
