import 'package:flutter/material.dart';

import '../grid_canvas.dart';
import '../server.dart';
import '../term_session.dart';

/// 학생 하나의 화면. 위는 격자(또는 그림), 아래는 키 줄과 답장 입력창.
class TerminalScreen extends StatefulWidget {
  const TerminalScreen({super.key, required this.server, required this.pane});

  final Server server;
  final Pane pane;

  @override
  State<TerminalScreen> createState() => _TerminalScreenState();
}

class _TerminalScreenState extends State<TerminalScreen> with WidgetsBindingObserver {
  late final TermSession _session = TermSession(widget.server, widget.pane);
  final _input = TextEditingController();
  final _inputFocus = FocusNode();
  bool _ctrl = false;
  bool _sending = false;

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
    setState(() => _sending = true);
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
        TermState.connected => s.mirror ? '데스크톱 화면 그대로' : '웹 셸',
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
          return Scaffold(
            appBar: AppBar(
              title: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    widget.pane.name.isEmpty ? widget.pane.id : widget.pane.name,
                    style: theme.textTheme.titleMedium,
                  ),
                  Text(
                    _stateText(s),
                    style: theme.textTheme.labelSmall
                        ?.copyWith(color: scheme.onSurfaceVariant),
                  ),
                ],
              ),
              actions: [
                IconButton(
                  tooltip: s.picture ? '글자로 보기' : '그림으로 보기',
                  isSelected: s.picture,
                  onPressed: s.state == TermState.gone ? null : () => s.setPicture(!s.picture),
                  icon: const Icon(Icons.image_outlined),
                  selectedIcon: const Icon(Icons.image),
                ),
              ],
            ),
            body: SafeArea(
              child: Column(
                children: [
                  Expanded(child: _view(s)),
                  if (s.note != null) _NoteBar(text: s.note!),
                  _KeyBar(
                    session: s,
                    ctrl: _ctrl,
                    onCtrl: () => setState(() => _ctrl = !_ctrl),
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
    final bytes = s.shotBytes;
    if (s.picture && bytes != null) {
      return InteractiveViewer(
        maxScale: 6,
        child: Center(
          child: Image.memory(bytes, gaplessPlayback: true, fit: BoxFit.contain),
        ),
      );
    }
    return GridCanvas(
      grid: s.grid,
      version: s.grid.version,
      palette: TerminalPalette.of(context),
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
  const _KeyBar({required this.session, required this.ctrl, required this.onCtrl});

  final TermSession session;
  final bool ctrl;
  final VoidCallback onCtrl;

  @override
  Widget build(BuildContext context) {
    final s = session;
    final keys = <Widget>[
      _Key(label: 'esc', onTap: () => s.sendText('\x1b')),
      _Key(label: 'tab', onTap: () => s.sendText('\t')),
      _Key(label: 'ctrl', selected: ctrl, onTap: onCtrl),
      _Key(label: '^C', onTap: () => s.ctrl('c')),
      _Key(icon: Icons.keyboard_arrow_left, onTap: () => s.arrow('D')),
      _Key(icon: Icons.keyboard_arrow_down, onTap: () => s.arrow('B')),
      _Key(icon: Icons.keyboard_arrow_up, onTap: () => s.arrow('A')),
      _Key(icon: Icons.keyboard_arrow_right, onTap: () => s.arrow('C')),
      _Key(icon: Icons.backspace_outlined, onTap: () => s.sendText('\x7f')),
      _Key(icon: Icons.keyboard_return, onTap: () => s.sendText('\r')),
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
  const _Key({this.label, this.icon, required this.onTap, this.selected = false});

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
              ? Icon(icon, size: 18, color: selected ? scheme.onPrimary : scheme.onSurface)
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
