import 'dart:async';

import 'package:flutter/material.dart';

import '../copy_notice.dart';
import '../look.dart';
import '../machine_look.dart';
import '../my_prompts.dart';
import '../pane_info.dart';
import '../server.dart';
import '../status_style.dart';
import 'dev_server.dart' show openShownLink;

/// 대화 보기 앱바의 Info 단추 — 데스크톱 Info 열의 맨 위 둘(「지금 보는 칸」 카드와 「내 프롬프트」)과
/// 터미널로 가는 길을 담은 아래 시트. 규칙 정본은 `docs/design.md` 「Info 탭」·「폰 Info 시트」.
Future<void> showPaneInfoSheet(
  BuildContext context, {
  required Server server,
  required Pane pane,
  required List<MyPrompt> Function() prompts,
  required ConversationJump jump,
  required VoidCallback onTerminal,
}) =>
    showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (_) => PaneInfoSheet(server: server, pane: pane, prompts: prompts, jump: jump, onTerminal: onTerminal),
    );

class PaneInfoSheet extends StatefulWidget {
  const PaneInfoSheet({
    super.key,
    required this.server,
    required this.pane,
    required this.prompts,
    required this.jump,
    required this.onTerminal,
  });

  final Server server;
  final Pane pane;
  final List<MyPrompt> Function() prompts;
  final ConversationJump jump;
  final VoidCallback onTerminal;

  @override
  State<PaneInfoSheet> createState() => _PaneInfoSheetState();
}

class _PaneInfoSheetState extends State<PaneInfoSheet> {
  FocusCard? _card;
  String? _issue;
  DateTime? _cardAt;
  bool _gone = false;
  Timer? _tick;
  Timer? _retry;
  Completer<void>? _wake;
  late final _checks = PromptChecks(widget.server.resumeScope);
  Set<String> _checked = const {};
  late final List<MyPrompt> _prompts = widget.prompts();
  Object? _current;

  @override
  void initState() {
    super.initState();
    _current = widget.jump.topOf({for (final p in _prompts) p.row});
    unawaited(_watch());
    unawaited(_checks.load().then((c) {
      if (mounted) setState(() => _checked = c);
    }));
    // 도구·승인의 나이는 받은 뒤 흐른 만큼 더해 그린다 — 데스크톱 카드와 같다.
    _tick = Timer.periodic(const Duration(seconds: 1), (_) {
      if (mounted && _card?.facts != null) setState(() {});
    });
  }

  @override
  void dispose() {
    _gone = true;
    _tick?.cancel();
    _retry?.cancel();
    if (_wake?.isCompleted == false) _wake!.complete();
    super.dispose();
  }

  /// 시트가 열려 있는 동안 원본의 `/term/pane-info` 에 매달린다 — 바뀔 때만 답이 온다.
  Future<void> _watch() async {
    String? since;
    while (!_gone && !widget.server.isClosed) {
      try {
        final answer = await widget.server.paneInfo(widget.pane, since: since);
        if (_gone) return;
        if (answer.error != null) {
          since = null;
          setState(() => _issue = paneInfoIssue(answer.error!));
          if (answer.error == 'update_needed') return;
          await _sleep(const Duration(seconds: 3));
          continue;
        }
        since = answer.seq;
        setState(() {
          _card = answer.card;
          _cardAt = DateTime.now();
          _issue = null;
        });
      } on ServerException {
        if (_gone) return;
        since = null;
        setState(() => _issue = paneInfoIssue('unreachable'));
        await _sleep(const Duration(seconds: 5));
      }
    }
  }

  /// 다시 묻기 전 쉼 — 시트를 닫으면 바로 깨어 끝난다.
  Future<void> _sleep(Duration d) {
    final wake = _wake = Completer<void>();
    _retry = Timer(d, () {
      if (!wake.isCompleted) wake.complete();
    });
    return wake.future;
  }

  Future<void> _toggle(MyPrompt p) async {
    final next = await _checks.toggle(p.checkKey);
    if (mounted) setState(() => _checked = next);
  }

  Future<void> _go(MyPrompt p) async {
    final nav = Navigator.of(context);
    final messenger = ScaffoldMessenger.maybeOf(context);
    nav.pop();
    final ok = await widget.jump.toTop(p.row);
    if (!ok) {
      messenger?.showSnackBar(const SnackBar(content: Text('대화 보기에서 그 말을 찾지 못했어요')));
    }
  }

  void _terminal() {
    Navigator.of(context).pop();
    widget.onTerminal();
  }

  void _openLink(FocusLink link) {
    if (link.path != null) {
      unawaited(copyAndTell(context, link.path!, done: '경로를 복사했어요'));
    } else if (link.port != null) {
      final nav = Navigator.of(context);
      nav.pop();
      unawaited(openShownLink(nav, widget.server, Uri.parse('http://localhost:${link.port}/'), machine: widget.pane.machine));
    }
  }

  @override
  Widget build(BuildContext context) {
    final media = MediaQuery.of(context);
    final elapsed = _cardAt == null ? 0 : DateTime.now().difference(_cardAt!).inMilliseconds;
    final lines = focusLines(_card, issue: _issue, elapsedMs: elapsed);
    return ConstrainedBox(
      constraints: BoxConstraints(maxHeight: media.size.height * 0.85),
      child: ListView(
        shrinkWrap: true,
        padding: EdgeInsets.fromLTRB(Look.pagePad, 0, Look.pagePad, Look.pagePad + media.padding.bottom),
        children: [
          _TerminalRow(onTap: _terminal),
          const SizedBox(height: 8),
          _FocusCardView(lines: lines, onLink: _openLink),
          if (_prompts.isNotEmpty) ...[
            const SizedBox(height: 8),
            _SectionHead('내 프롬프트 · ${_prompts.length}개'),
            for (final p in _prompts)
              _PromptRow(
                prompt: p,
                checked: _checked.contains(p.checkKey),
                current: identical(p.row, _current),
                onTap: () => unawaited(_go(p)),
                onCheck: () => unawaited(_toggle(p)),
              ),
          ],
        ],
      ),
    );
  }
}

/// 앱바에 있던 「터미널로 보기」 — 설정 줄과 같은 모양.
class _TerminalRow extends StatelessWidget {
  const _TerminalRow({required this.onTap});
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Material(
      color: TwinTone.of(context).card,
      borderRadius: Look.corners,
      child: InkWell(
        borderRadius: Look.corners,
        onTap: onTap,
        child: ConstrainedBox(
          constraints: const BoxConstraints(minHeight: Look.row1),
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 14),
            child: Row(
              children: [
                Icon(Icons.terminal, size: Look.iconSize, color: scheme.onSurfaceVariant),
                const SizedBox(width: 12),
                const Expanded(child: Text('터미널로 보기', style: TextStyle(fontSize: Look.body))),
                Icon(Icons.chevron_right, size: Look.iconSize, color: scheme.onSurfaceVariant),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// 「지금 보는 칸」 — 머리(칸 종류 · 칸 id) 아래 「이름 · 값」 표. 데스크톱 카드와 같은 줄, 폰 글 크기.
class _FocusCardView extends StatelessWidget {
  const _FocusCardView({required this.lines, required this.onLink});
  final List<FocusLine> lines;
  final ValueChanged<FocusLink> onLink;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final warnInk = machineInk(StatusStyle.attention, scheme.surface);
    final dim = scheme.onSurfaceVariant;
    return Container(
      padding: const EdgeInsets.fromLTRB(14, 10, 14, 10),
      decoration: BoxDecoration(color: TwinTone.of(context).card, borderRadius: Look.corners),
      child: LayoutBuilder(
        builder: (context, box) {
          // 이름 칸은 폭의 36%(최대 84) — 데스크톱 Info 표.
          final nameW = (box.maxWidth * 0.36).clamp(0.0, 84.0);
          Widget kv(String name, Widget value, {VoidCallback? onTap}) {
            final row = ConstrainedBox(
              constraints: BoxConstraints(minHeight: onTap == null ? 26 : Look.tap),
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.center,
                children: [
                  SizedBox(width: nameW, child: Text(name, style: TextStyle(fontSize: Look.sub, color: dim))),
                  Expanded(child: value),
                ],
              ),
            );
            return onTap == null ? row : InkWell(onTap: onTap, borderRadius: BorderRadius.circular(Look.radiusSm), child: row);
          }

          return Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              for (final line in lines)
                switch (line) {
                  FocusHead(:final title, :final note) => Padding(
                      padding: const EdgeInsets.only(bottom: 4),
                      child: Row(
                        children: [
                          Expanded(
                            child: Text(title, style: theme.textTheme.titleSmall?.copyWith(fontWeight: FontWeight.w600)),
                          ),
                          if (note.isNotEmpty) Text(note, style: TextStyle(fontSize: Look.sub, color: dim)),
                        ],
                      ),
                    ),
                  FocusText(:final text, :final warn) => Padding(
                      padding: const EdgeInsets.symmetric(vertical: 4),
                      child: Text(text, style: TextStyle(fontSize: Look.sub, color: warn ? warnInk : dim)),
                    ),
                  FocusKv(:final name, :final value, :final tip, :final warn) => Tooltip(
                      message: tip,
                      triggerMode: tip.isEmpty ? TooltipTriggerMode.manual : TooltipTriggerMode.longPress,
                      child: kv(
                        name,
                        Text(
                          value ?? '—',
                          maxLines: 2,
                          overflow: TextOverflow.ellipsis,
                          style: TextStyle(
                            fontSize: Look.sub,
                            color: value == null ? scheme.outline : (warn ? warnInk : scheme.onSurface),
                          ),
                        ),
                      ),
                    ),
                  FocusLink(:final name, :final value) => kv(
                      name,
                      Text(value, maxLines: 2, overflow: TextOverflow.ellipsis,
                          style: TextStyle(fontSize: Look.sub, color: scheme.primary)),
                      onTap: () => onLink(line),
                    ),
                },
            ],
          );
        },
      ),
    );
  }
}

class _SectionHead extends StatelessWidget {
  const _SectionHead(this.text);
  final String text;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return SizedBox(
      height: 30,
      child: Row(
        children: [
          Text(text, style: TextStyle(fontSize: Look.chip, color: scheme.onSurfaceVariant)),
          const SizedBox(width: 8),
          Expanded(child: Divider(height: 1, color: scheme.outlineVariant)),
        ],
      ),
    );
  }
}

/// 「내 프롬프트」 한 줄 — 왼쪽 체크칸(누름 44), 첫 줄, 오른쪽 친 시각. 지금 맨 위에 보이는 질문은 왼쪽 막대와 강조 글자.
class _PromptRow extends StatelessWidget {
  const _PromptRow({
    required this.prompt,
    required this.checked,
    required this.current,
    required this.onTap,
    required this.onCheck,
  });
  final MyPrompt prompt;
  final bool checked;
  final bool current;
  final VoidCallback onTap;
  final VoidCallback onCheck;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final mute = scheme.outline;
    final ink = current ? scheme.primary : (checked ? mute : scheme.onSurface);
    final first = prompt.text.trim().split('\n').first;
    return InkWell(
      onTap: onTap,
      borderRadius: BorderRadius.circular(Look.radiusSm),
      child: ConstrainedBox(
        constraints: const BoxConstraints(minHeight: Look.tap),
        child: Row(
          children: [
            SizedBox(
              width: 6,
              child: current
                  ? Container(width: 2, height: 12, color: scheme.primary)
                  : null,
            ),
            Semantics(
              checked: checked,
              label: checked ? '체크 풀기' : '끝낸 것으로 체크',
              child: InkResponse(
                onTap: onCheck,
                radius: 20,
                child: SizedBox(
                  width: Look.tap,
                  height: Look.tap,
                  child: Icon(
                    checked ? Icons.check_box : Icons.check_box_outline_blank,
                    size: Look.iconSize,
                    color: checked ? StatusStyle.successText(scheme) : mute,
                  ),
                ),
              ),
            ),
            if (prompt.asked != null)
              Padding(
                padding: const EdgeInsets.only(right: 6),
                child: Text('답', style: TextStyle(fontSize: Look.chip, color: mute)),
              ),
            Expanded(
              child: Text(
                first,
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(
                  fontSize: Look.sub + 1,
                  color: ink,
                  fontWeight: current ? FontWeight.w600 : null,
                  decoration: checked ? TextDecoration.lineThrough : null,
                  decorationColor: mute,
                ),
              ),
            ),
            if (prompt.at != null) ...[
              const SizedBox(width: 8),
              Text(promptClock(prompt.at!), style: TextStyle(fontSize: Look.chip, color: mute)),
            ],
          ],
        ),
      ),
    );
  }
}
