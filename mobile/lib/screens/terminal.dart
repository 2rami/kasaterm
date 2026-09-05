import 'package:flutter/material.dart';

import '../claude_style.dart';
import '../grid_canvas.dart';
import '../server.dart';
import '../student_art.dart';
import '../term_session.dart';

/// 학생 하나의 화면. 위는 격자(또는 그림), 아래는 키 줄과 답장 입력창.
class TerminalScreen extends StatefulWidget {
  const TerminalScreen({super.key, required this.server, required this.pane});

  final Server server;
  final Pane pane;

  @override
  State<TerminalScreen> createState() => _TerminalScreenState();
}

class _TerminalScreenState extends State<TerminalScreen>
    with WidgetsBindingObserver {
  late final TermSession _session = TermSession(widget.server, widget.pane);
  final _input = TextEditingController();
  final _inputFocus = FocusNode();
  bool _ctrl = false;
  bool _sending = false;

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
    if (text.isEmpty || _sending) return;
    setState(() {
      _sending = true;
      _bottomTick++;
    });
    try {
      if (_ctrl && text.length == 1) {
        _session.ctrl(text);
      } else {
        await _session.reply(text);
      }
      _input.clear();
      _ctrl = false;
    } on ServerException catch (e) {
      _toast(e.message);
    } finally {
      if (mounted) setState(() => _sending = false);
      _inputFocus.requestFocus();
    }
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
      return Scaffold(
        appBar: AppBar(
          titleSpacing: 0,
          title: Row(
            children: [
              StudentSprite(
                slug: slug,
                url: slug == null
                    ? null
                    : widget.server.avatar(slug, machine: pane.machine),
                size: 40,
              ),
              const SizedBox(width: 8),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      pane.displayName,
                      style: theme.textTheme.titleMedium,
                      overflow: TextOverflow.ellipsis,
                    ),
                    Text(
                      _stateText(s),
                      style: theme.textTheme.labelSmall?.copyWith(
                        color: scheme.onSurfaceVariant,
                      ),
                    ),
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
          ],
        ),
        body: SafeArea(
          child: Column(
            children: [
              Expanded(
                child: Row(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    // 데스크톱 pane 의 학생색 리본 — 어느 학생 화면인지 색으로 안다.
                    Container(width: 3, color: accent),
                    Expanded(child: _view(s)),
                  ],
                ),
              ),
              if (s.note != null) _NoteBar(text: s.note!),
              _KeyBar(
                session: s,
                ctrl: _ctrl,
                onCtrl: () => setState(() => _ctrl = !_ctrl),
                onKey: _toBottom,
              ),
              _ReplyBar(
                controller: _input,
                focusNode: _inputFocus,
                enabled: s.state != TermState.gone && !_sending,
                onSend: _send,
              ),
            ],
          ),
        ),
      );
    },
  );

  Widget _view(TermSession s) {
    final tokens = s.tokens;
    final palette = tokens == null
        ? TerminalPalette.of(context)
        : TerminalPalette.fromTokens(tokens);
    if (_wrap) {
      final pane = widget.pane;
      return WrappedCanvas(
        grid: s.grid,
        history: s.history,
        version: s.grid.version + s.historyVersion,
        palette: palette,
        bottomTick: _bottomTick,
        // 웹 셸엔 학생이 없다 — 데스크톱 pane 만 학생 꾸밈을 입는다.
        student: pane.isWebShell
            ? null
            : StudentStyle(
                slug: pane.slug,
                name: pane.name,
                accent: studentAccent(context, pane, tokens),
                bg: palette.bg,
              ),
      );
    }
    return GridCanvas(grid: s.grid, version: s.grid.version, palette: palette);
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
  });

  final TermSession session;
  final bool ctrl;
  final VoidCallback onCtrl;

  /// 키를 보낸 뒤 — 화면을 맨 아래로.
  final VoidCallback onKey;

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
      _Key(icon: Icons.keyboard_return, onTap: tap(() => s.sendText('\r'))),
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
  });

  final TextEditingController controller;
  final FocusNode focusNode;
  final bool enabled;
  final VoidCallback onSend;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(8, 0, 8, 8),
    child: Row(
      children: [
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
            decoration: const InputDecoration(hintText: '답장…'),
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
