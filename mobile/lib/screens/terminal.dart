import '../device_shape.dart';
import 'dart:typed_data';
import 'package:flutter/material.dart';

import '../claude_style.dart';
import '../grid_canvas.dart';
import '../live_input.dart';
import '../image_attachment.dart';
import '../photo_attachment_button.dart';
import '../server.dart';
import '../status_style.dart';
import '../student_art.dart';
import '../term_session.dart';
import '../theme_prefs.dart';

/// 학생 하나의 화면. 위는 격자(또는 그림), 아래는 키 줄과 답장 입력창.
class TerminalScreen extends StatefulWidget {
  const TerminalScreen({
    super.key,
    required this.server,
    required this.pane,
    this.initialScroll,
    this.session,
    this.pickImage,
    this.onPaneCreated,
  });

  final Server server;
  final Pane pane;

  /// 상단바 「pane 추가」로 pane 을 만들었을 때 — 허브가 목록을 폴링 전에 다시 받는다.
  final Future<void> Function()? onPaneCreated;

  /// 검증용 — 열자마자 위로 이만큼(px) 넘긴 상태로.
  final double? initialScroll;
  @visibleForTesting
  final TermSession? session;
  @visibleForTesting
  final Future<Uint8List?> Function()? pickImage;

  @override
  State<TerminalScreen> createState() => _TerminalScreenState();
}

class _TerminalScreenState extends State<TerminalScreen>
    with WidgetsBindingObserver {
  late final TermSession _session =
      widget.session ?? TermSession(widget.server, widget.pane);
  final _input = TextEditingController();
  final _inputFocus = FocusNode();
  bool _ctrl = false;
  bool _sending = false;
  bool _attaching = false;
  bool _pendingAttachment = false;

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

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _session.connect();
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _session.dispose();
    _input.dispose();
    _inputFocus.dispose();
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    switch (state) {
      case AppLifecycleState.resumed:
        _session.resume();
      case AppLifecycleState.paused:
      case AppLifecycleState.detached:
      case AppLifecycleState.hidden:
        _session.pause();
      case AppLifecycleState.inactive:
        break;
    }
  }

  /// 보낼 때 입력창의 글 전체만 읽는다 — 조합 중인 자모가 새어 나갈 길이 없다.
  Future<void> _send() async {
    final text = _input.text;
    if (_sending || _attaching || (text.isEmpty && !_pendingAttachment)) return;
    setState(() {
      _sending = true;
      _bottomTick++;
    });
    try {
      if (text.isEmpty && _pendingAttachment) {
        _session.sendText('\r');
      } else if (_ctrl && text.length == 1) {
        _session.ctrl(text);
      } else {
        await _session.reply(text);
      }
      _input.clear();
      _ctrl = false;
      _pendingAttachment = false;
    } on ServerException catch (e) {
      _toast(e.message);
    } finally {
      if (mounted) setState(() => _sending = false);
      _inputFocus.requestFocus();
    }
  }

  void _sendLive(List<int> bytes) {
    if (bytes.isEmpty) return;
    _session.sendBytes(bytes);
    _lastLiveSend = DateTime.now();
    _bottomTick++;
  }

  void _onLiveChanged(String _) {
    if (_resetting) return;
    _sendLive(_liveInput.update(_input.value));
    setState(() => _composing = _liveInput.composing);
  }

  /// 엔터 — 글자 바로 뒤에 붙여 보내면 Ink 가 엔터를 먹는다(서버 `send` 가 140ms 를
  /// 기다리는 이유와 같다). 마지막 글자에서 조금 떨어뜨려 보낸다.
  Future<void> _liveSubmit() async {
    if (_sending || _attaching) return;
    _sendLive(_liveInput.flush(_input.value));
    _resetting = true;
    _liveInput.reset();
    _input.clear();
    _resetting = false;
    setState(() => _composing = '');
    final gap = DateTime.now().difference(_lastLiveSend);
    if (gap < _enterGap) await Future<void>.delayed(_enterGap - gap);
    _session.sendText('\r');
    _pendingAttachment = false;
    _toBottom();
    _inputFocus.requestFocus();
  }

  static const _enterGap = Duration(milliseconds: 150);

  void _toggleLive() {
    setState(() {
      _live = !_live;
      _composing = '';
      _liveInput.reset();
      _resetting = true;
      _input.clear();
      _resetting = false;
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
      s.mirror && !widget.pane.isWebShell ? '데스크톱 화면 그대로' : '웹 셸',
    TermState.reconnecting => '다시 연결 중…',
    TermState.gone => '끝난 화면',
  };

  /// 데스크톱의 × 와 같다 — 되살리기 대열에 남는다. 닫히면 허브로 돌아간다.
  Future<void> _closePane(Pane pane) async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
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

  /// 상단바의 「pane 추가」 — 데스크톱 pane 머리의 쪼개기·+ 와 같다. 보는 pane 옆에
  /// 쪼개거나 그 pane 안에 탭으로. 셸만 뜬다(claude 는 들어가서 켠다). 거울 pane 이면
  /// 원본 기계가 아니라 거울이 사는 기계에 명령이 가고, 그쪽 데스크톱이 「거울 옆
  /// split = 로컬 셸」 규칙(docs/mirror-viewer-lifecycle.md)을 지킨다. 만든 pane 으로
  /// 바로 옮겨 간다(뒤로 가면 허브).
  Future<void> _addPane(Pane pane) async {
    final how = await showModalBottomSheet<_AddHow>(
      context: context,
      showDragHandle: true,
      builder: (ctx) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            ListTile(title: Text('${pane.displayName} 에 pane 추가')),
            const Divider(height: 1),
            ListTile(
              leading: const Icon(Icons.vertical_split_outlined),
              title: const Text('옆에 쪼개기'),
              subtitle: const Text('셸 하나를 이 pane 옆에 — 데스크톱 배치가 갈린다'),
              onTap: () => Navigator.pop(ctx, _AddHow.split),
            ),
            ListTile(
              leading: const Icon(Icons.tab_outlined),
              title: const Text('탭으로'),
              subtitle: const Text('이 pane 안에 탭 하나 — 배치는 그대로'),
              onTap: () => Navigator.pop(ctx, _AddHow.tab),
            ),
            const SizedBox(height: 8),
          ],
        ),
      ),
    );
    if (how == null || !mounted) return;
    String? id;
    try {
      id = switch (how) {
        _AddHow.split => await widget.server.splitPane(
          pane.id,
          machine: pane.machine,
        ),
        _AddHow.tab => await widget.server.newTab(
          pane.id,
          machine: pane.machine,
        ),
      };
    } on ServerException catch (e) {
      if (mounted) _toast(e.message);
      return;
    }
    await widget.onPaneCreated?.call();
    if (!mounted) return;
    if (id == null) {
      _toast('pane 을 만들었다 — 목록에서 열어라');
      return;
    }
    final made = await _findPane(pane, id);
    if (!mounted) return;
    Navigator.of(context).pushReplacement(
      MaterialPageRoute<void>(
        builder: (_) => TerminalScreen(
          server: widget.server,
          pane: made,
          onPaneCreated: widget.onPaneCreated,
        ),
      ),
    );
  }

  /// 새 pane 을 목록에서 찾아 온다 — 데스크톱이 한 박자 늦게 실을 수 있어 몇 번 되묻고,
  /// 끝내 없으면 자리(id)만으로 셸 화면을 연다.
  Future<Pane> _findPane(Pane from, String id) async {
    for (var i = 0; i < 3; i++) {
      try {
        final panes = await widget.server.panes(machine: from.machine);
        for (final p in panes) {
          if (p.id == id) return p;
        }
      } on ServerException {
        break;
      }
      await Future<void>.delayed(const Duration(milliseconds: 400));
    }
    return Pane(
      id: id,
      name: '',
      title: '',
      status: '',
      window: from.window,
      cwd: from.cwd,
      machine: from.machine,
    );
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: _session,
    builder: (context, _) {
      final theme = Theme.of(context);
      final scheme = theme.colorScheme;
      final s = _session;
      final pane = widget.pane;
      final accent = studentAccent(context, pane, s.tokens);
      final slug = pane.slug;
      return _StudentFrame(
        accent: accent,
        child: Scaffold(
          appBar: AppBar(
            titleSpacing: 0,
            // 세 줄(이름·세션 / 상태·연결 / 상태줄)이 기본 56 에 안 들어간다.
            toolbarHeight: pane.statusParts.isEmpty ? kToolbarHeight : 84,
            title: Row(
              children: [
                Hero(
                  tag: 'face-${pane.machine}-${pane.id}',
                  // 화면의 주인공은 프사(사진) — 목록의 도트가 여기로 날아와 얼굴이 된다.
                  child: StudentFace(
                    slug: slug,
                    url: slug == null
                        ? null
                        : widget.server.avatar(slug, machine: pane.machine),
                    shell: pane.isShell,
                    size: 40,
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
                          Builder(
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
                      // 데스크톱 pane 머리와 같은 셋째 줄 — 하네스·모델·브랜치·컨텍스트·effort.
                      if (pane.statusParts.isNotEmpty)
                        PaneStatusLine(pane: pane),
                    ],
                  ),
                ),
              ],
            ),
            actions: [
              IconButton(
                tooltip: _wrap ? '데스크톱 격자 그대로 보기' : '폰 폭에 맞춰 보기',
                isSelected: _wrap,
                onPressed: () => setState(() => _wrap = !_wrap),
                icon: const Icon(Icons.wrap_text),
              ),
              IconButton(
                tooltip: 'pane 추가',
                onPressed: () => _addPane(pane),
                icon: const Icon(Icons.add_box_outlined),
              ),
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
                // 좌우 숨 — 글자가 화면 끝에 닿으면 답답하고, 0열에 잉크가 있는 글자가
                // 잘려 보인다. 학생색 테는 화면 가장자리의 _StudentFrame 이 두른다.
                Expanded(
                  child: Padding(
                    padding: const EdgeInsets.fromLTRB(12, 4, 12, 4),
                    child: _view(s),
                  ),
                ),
                if (s.note != null) _NoteBar(text: s.note!),
                Row(
                  children: [
                    PhotoAttachmentButton(
                      server: widget.server,
                      pane: pane,
                      pickImage: widget.pickImage ?? pickAttachmentImage,
                      enabled:
                          s.state == TermState.connected &&
                          !_sending &&
                          !pane.isShell &&
                          !pane.isWebShell,
                      onBusy: (busy) => setState(() => _attaching = busy),
                      onAttached: () => setState(() {
                        _pendingAttachment = true;
                        _bottomTick++;
                      }),
                    ),
                    Expanded(
                      child: AbsorbPointer(
                        absorbing: _attaching,
                        child: _KeyBar(
                          session: s,
                          ctrl: _ctrl,
                          onCtrl: () => setState(() => _ctrl = !_ctrl),
                          onKey: _toBottom,
                          onSubmit: () => _pendingAttachment = false,
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
                    onChanged: _onLiveChanged,
                    onSubmit: _liveSubmit,
                    onDraft: _toggleLive,
                  )
                else
                  _ReplyBar(
                    controller: _input,
                    focusNode: _inputFocus,
                    enabled:
                        s.state != TermState.gone && !_sending && !_attaching,
                    onSend: _send,
                    onLive: _toggleLive,
                  ),
              ],
            ),
          ),
        ),
      );
    },
  );

  Widget _view(TermSession s) {
    final tokens = s.tokens;
    final palette = TerminalPalette.forViewer(
      context,
      mode: phoneThemeMode.value,
      source: tokens,
    );
    if (_wrap) {
      final pane = widget.pane;
      return WrappedCanvas(
        grid: s.grid,
        history: s.history,
        historyVersion: s.historyVersion,
        version: s.grid.version + s.historyVersion,
        palette: palette,
        bottomTick: _bottomTick,
        initialScroll: widget.initialScroll,
        composing: _live ? _composing : null,
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
      color: scheme.surfaceContainerHighest,
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
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
      _Key(
        icon: Icons.keyboard_return,
        onTap: tap(() {
          s.sendText('\r');
          onSubmit();
        }),
      ),
    ];
    return SizedBox(
      height: 44,
      child: ListView.separated(
        scrollDirection: Axis.horizontal,
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 6),
        itemCount: keys.length,
        separatorBuilder: (_, _) => const SizedBox(width: 6),
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
    return Material(
      color: selected ? scheme.primary : scheme.surface,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(7),
        side: BorderSide(color: selected ? scheme.primary : scheme.outline),
      ),
      child: InkWell(
        onTap: onTap,
        borderRadius: BorderRadius.circular(7),
        child: Container(
          constraints: const BoxConstraints(minWidth: 40),
          padding: const EdgeInsets.symmetric(horizontal: 10),
          alignment: Alignment.center,
          child: icon != null
              ? Icon(
                  icon,
                  size: 18,
                  color: selected ? scheme.onPrimary : scheme.onSurface,
                )
              : Text(
                  label!,
                  style: TextStyle(
                    fontFamily: 'TermMono',
                    fontSize: 12,
                    color: selected ? scheme.onPrimary : scheme.onSurface,
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
            maxLines: 4,
            textInputAction: TextInputAction.send,
            onSubmitted: (_) => onSend(),
            // 16px 아래로 내리면 iOS 가 포커스 때 화면을 확대한다.
            style: const TextStyle(fontSize: 16),
            decoration: const InputDecoration(hintText: '적어 두고 한 번에 보내기…'),
          ),
        ),
        const SizedBox(width: 6),
        IconButton.filled(
          onPressed: enabled ? onSend : null,
          icon: const Icon(Icons.send),
          tooltip: '보내기',
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
    required this.onChanged,
    required this.onSubmit,
    required this.onDraft,
  });

  final TextEditingController controller;
  final FocusNode focusNode;
  final bool enabled;
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
              autofocus: true,
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
                isDense: true,
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

/// 「pane 추가」의 두 길 — 옆에 쪼개기 / 탭으로.
enum _AddHow { split, tab }
