import 'dart:async';

import 'package:flutter/material.dart';

import '../approvals.dart';
import '../look.dart';
import '../relay_account.dart';
import '../status_style.dart';
import '../twins_loading.dart';
import 'approval_key.dart';

/// 원격 승인 한 건 — 어느 기기·어느 학생·도구, 입력 원문 전부, [거절]·[허락]. 요청마다 한 번이고
/// 「항상 허락」은 없다(docs/remote-approval.md). 다른 곳에서 닫히면 이 화면이 그 사실을 말하고 단추를 거둔다.
class ApprovalScreen extends StatefulWidget {
  const ApprovalScreen({super.key, required this.center, required this.id});

  final ApprovalCenter center;
  final String id;

  @override
  State<ApprovalScreen> createState() => _ApprovalScreenState();
}

class _ApprovalScreenState extends State<ApprovalScreen> {
  Timer? _tick;
  bool _busy = false;
  String? _error;

  /// 비밀 요청을 받았는데 이 폰에 승인 열쇠가 없나 — 그러면 여기서 바로 만들러 간다.
  bool _noKey = false;

  Future<void> _checkKey() async {
    final none = await widget.center.signer.publicKey() == null;
    if (mounted && none != _noKey) setState(() => _noKey = none);
  }

  Future<void> _makeKey() async {
    final session = widget.center.session;
    if (session == null) return;
    await Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => ApprovalKeyScreen(
          api: () => RelayAccountApi(session.origin, session: session),
          signer: widget.center.signer,
        ),
      ),
    );
    await _checkKey();
  }

  @override
  void initState() {
    super.initState();
    unawaited(_checkKey());
    widget.center.addListener(_changed);
    _tick = Timer.periodic(const Duration(seconds: 1), (_) => _changed());
    if (widget.center.byId(widget.id) == null) unawaited(widget.center.refresh());
  }

  @override
  void dispose() {
    _tick?.cancel();
    widget.center.removeListener(_changed);
    super.dispose();
  }

  void _changed() {
    if (mounted) setState(() {});
  }

  Future<void> _decide(Approval a, bool allow) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await widget.center.decide(a, allow);
    } on AccountException catch (e) {
      _error = e.message;
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final a = widget.center.byId(widget.id);
    return TwinBackdrop(
      child: Scaffold(
        backgroundColor: Colors.transparent,
        appBar: AppBar(
          backgroundColor: Colors.transparent,
          title: Text(a?.isSecret == true ? '1Password 승인 요청' : '승인 요청'),
          leading: IconButton(
            tooltip: '닫기',
            icon: const Icon(Icons.close_rounded),
            onPressed: () => Navigator.of(context).maybePop(),
          ),
        ),
        body: a == null
            ? const Center(child: TwinsLoading())
            : _Body(
                approval: a,
                remainingMs: widget.center.remainingMs(a),
                busy: _busy,
                error: _error,
                onDecide: (allow) => unawaited(_decide(a, allow)),
                onMakeKey: a.isSecret && _noKey ? () => unawaited(_makeKey()) : null,
              ),
      ),
    );
  }
}

class _Body extends StatelessWidget {
  const _Body({
    required this.approval,
    required this.remainingMs,
    required this.busy,
    required this.error,
    required this.onDecide,
    this.onMakeKey,
  });

  final Approval approval;
  final int remainingMs;
  final bool busy;
  final String? error;
  final void Function(bool allow) onDecide;
  final VoidCallback? onMakeKey;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final a = approval;
    final dim = theme.textTheme.bodySmall?.copyWith(color: scheme.onSurfaceVariant, fontSize: Look.sub);
    final mono = TextStyle(
      fontFamily: 'TermMono',
      fontFamilyFallback: Look.flowMonoFallback,
      fontSize: 14,
      height: 1.45,
      color: scheme.onSurface,
    );
    final open = a.pending && remainingMs > 0;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Expanded(
          child: ListView(
            key: const Key('approval-body'),
            padding: const EdgeInsets.fromLTRB(Look.pagePad, 4, Look.pagePad, Look.groupGap),
            children: [
              DecoratedBox(
                decoration: TwinTone.of(context).cardBox(),
                child: Padding(
                  padding: const EdgeInsets.all(Look.cardPad),
                  child: Row(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Expanded(
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            Text(
                              a.student.isEmpty ? '학생' : a.student,
                              key: const Key('approval-student'),
                              style: theme.textTheme.titleMedium?.copyWith(
                                fontSize: Look.title,
                                fontWeight: FontWeight.w600,
                              ),
                            ),
                            const SizedBox(height: Look.rowGap),
                            Text(
                              [a.machine, if (a.pane.isNotEmpty) a.pane].join(' · '),
                              key: const Key('approval-machine'),
                              style: dim,
                            ),
                          ],
                        ),
                      ),
                      const SizedBox(width: 12),
                      _Chip(
                        text: a.tool,
                        color: scheme.primary,
                      ),
                    ],
                  ),
                ),
              ),
              if (a.cwd.isNotEmpty) ...[
                const SizedBox(height: Look.groupGap),
                Text('폴더', style: dim?.copyWith(fontWeight: FontWeight.w600)),
                const SizedBox(height: Look.groupTitleGap),
                SelectableText(a.cwd, style: mono.copyWith(color: scheme.onSurfaceVariant)),
              ],
              for (final f in a.fields) ...[
                const SizedBox(height: Look.groupGap),
                Text(f.label, style: dim?.copyWith(fontWeight: FontWeight.w600)),
                const SizedBox(height: Look.groupTitleGap),
                Container(
                  key: Key('approval-field-${f.name}'),
                  padding: const EdgeInsets.all(12),
                  decoration: BoxDecoration(
                    color: scheme.surfaceContainerHighest.withValues(alpha: 0.6),
                    borderRadius: Look.corners,
                  ),
                  child: SelectableText(f.text.isEmpty ? '(빈 글)' : f.text, style: mono),
                ),
              ],
              if (a.isSecret) ...[
                const SizedBox(height: Look.groupGap),
                _Band(
                  key: const Key('approval-secret-note'),
                  text: a.secretValid
                      ? '허락하면 이 참조를 이번 한 번만 읽어요. 값은 이 폰과 관문을 지나지 않고 요청한 프로세스에만 가요.'
                      : '요청 모양이 맞지 않아 허락할 수 없어요.',
                  color: a.secretValid ? scheme.primary : scheme.error,
                ),
                if (onMakeKey case final make?) ...[
                  const SizedBox(height: Look.rowGap * 2),
                  _Band(
                    key: const Key('approval-no-key'),
                    text: '이 폰에 Face ID 승인 열쇠가 없어요. 열쇠를 만들고 맥에서 지문을 맞춰 믿기를 누르면 허락할 수 있어요.',
                    color: scheme.error,
                  ),
                  const SizedBox(height: Look.rowGap * 2),
                  _Button(
                    key: const Key('approval-make-key'),
                    label: '열쇠 만들기',
                    fill: scheme.primary.withValues(alpha: 0.14),
                    ink: scheme.primary,
                    onPressed: make,
                  ),
                ],
              ],
              if (a.truncated) ...[
                const SizedBox(height: Look.groupGap),
                _Band(
                  key: const Key('approval-truncated'),
                  text: '원문이 길어 앞부분만 실렸어요. 허락은 원래 창에서만 할 수 있어요.',
                  color: scheme.error,
                ),
              ],
            ],
          ),
        ),
        SafeArea(
          top: false,
          child: Padding(
            padding: const EdgeInsets.fromLTRB(Look.pagePad, 8, Look.pagePad, Look.pagePad),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              mainAxisSize: MainAxisSize.min,
              children: [
                if (error case final e?) ...[
                  Semantics(
                    liveRegion: true,
                    child: Text(e, key: const Key('approval-error'), style: TextStyle(color: scheme.error)),
                  ),
                  const SizedBox(height: 8),
                ],
                if (open) ...[
                  Row(
                    children: [
                      Expanded(
                        child: Text(
                          _left(remainingMs),
                          key: const Key('approval-left'),
                          style: dim,
                        ),
                      ),
                      Text('이 요청 한 번만', style: dim),
                    ],
                  ),
                  const SizedBox(height: 8),
                  Row(
                    children: [
                      Expanded(
                        child: _Button(
                          key: const Key('approval-deny'),
                          label: '거절',
                          fill: scheme.error.withValues(alpha: Look.dangerTint),
                          ink: scheme.error,
                          onPressed: busy ? null : () => onDecide(false),
                        ),
                      ),
                      const SizedBox(width: Look.cardGap),
                      Expanded(
                        child: _Button(
                          key: const Key('approval-allow'),
                          label: a.isSecret ? 'Face ID 로 허락' : '허락',
                          fill: scheme.primary,
                          ink: scheme.onPrimary,
                          busy: busy,
                          onPressed: busy || a.truncated || (a.isSecret && !a.secretValid) ? null : () => onDecide(true),
                        ),
                      ),
                    ],
                  ),
                ] else
                  _Band(
                    key: const Key('approval-closed'),
                    text: a.pending ? '2분이 지나 원래 창으로 돌아갔어요' : a.closedLine,
                    color: a.state == 'allowed'
                        ? StatusStyle.successInk
                        : a.state == 'denied'
                        ? scheme.error
                        : scheme.onSurfaceVariant,
                  ),
              ],
            ),
          ),
        ),
      ],
    );
  }
}

String _left(int ms) {
  final s = (ms / 1000).ceil().clamp(0, 9999);
  return '${s ~/ 60}:${(s % 60).toString().padLeft(2, '0')} 남음 · 지나면 원래 창으로';
}

class _Chip extends StatelessWidget {
  const _Chip({required this.text, required this.color});
  final String text;
  final Color color;

  @override
  Widget build(BuildContext context) => Container(
    height: Look.chipH,
    padding: const EdgeInsets.symmetric(horizontal: Look.chipPadX),
    alignment: Alignment.center,
    decoration: BoxDecoration(color: color.withValues(alpha: 0.16), borderRadius: BorderRadius.circular(Look.chipH)),
    child: Text(
      text,
      style: TextStyle(fontSize: Look.chip, fontWeight: FontWeight.w600, color: color),
    ),
  );
}

class _Band extends StatelessWidget {
  const _Band({super.key, required this.text, required this.color});
  final String text;
  final Color color;

  @override
  Widget build(BuildContext context) => Container(
    constraints: const BoxConstraints(minHeight: Look.tap),
    padding: const EdgeInsets.symmetric(horizontal: Look.cardPad, vertical: 10),
    alignment: Alignment.centerLeft,
    decoration: BoxDecoration(color: color.withValues(alpha: Look.dangerTint), borderRadius: Look.corners),
    child: Text(text, style: TextStyle(color: color, fontSize: Look.body)),
  );
}

class _Button extends StatelessWidget {
  const _Button({
    super.key,
    required this.label,
    required this.fill,
    required this.ink,
    required this.onPressed,
    this.busy = false,
  });

  final String label;
  final Color fill;
  final Color ink;
  final VoidCallback? onPressed;
  final bool busy;

  @override
  Widget build(BuildContext context) {
    final off = onPressed == null;
    return SizedBox(
      height: Look.buttonH,
      child: Material(
        color: off ? fill.withValues(alpha: fill.a * 0.4) : fill,
        borderRadius: Look.corners,
        child: InkWell(
          borderRadius: Look.corners,
          onTap: onPressed,
          child: Center(
            child: busy
                ? SizedBox.square(dimension: 18, child: CircularProgressIndicator(strokeWidth: 2, color: ink))
                : Text(
                    label,
                    style: TextStyle(
                      fontSize: Look.body,
                      fontWeight: FontWeight.w600,
                      color: off ? ink.withValues(alpha: 0.5) : ink,
                    ),
                  ),
          ),
        ),
      ),
    );
  }
}

/// 앱이 앞에 있을 때 새 요청을 알리는 위쪽 띠. 누르면 [ApprovalScreen], 그 요청이 닫히면 저절로 걷힌다.
class ApprovalBanner extends StatelessWidget {
  const ApprovalBanner({super.key, required this.center, required this.id, required this.onOpen, required this.onDismiss});

  final ApprovalCenter center;
  final String id;
  final VoidCallback onOpen;
  final VoidCallback onDismiss;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return ListenableBuilder(
      listenable: center,
      builder: (context, _) {
        final a = center.byId(id);
        if (a == null || !a.pending) {
          WidgetsBinding.instance.addPostFrameCallback((_) => onDismiss());
          return const SizedBox.shrink();
        }
        return SafeArea(
          bottom: false,
          child: Padding(
            padding: const EdgeInsets.fromLTRB(Look.pagePad, 8, Look.pagePad, 0),
            child: DecoratedBox(
              decoration: TwinTone.of(context).cardBox(),
              child: Material(
                type: MaterialType.transparency,
                child: InkWell(
                  key: const Key('approval-banner'),
                  borderRadius: Look.cardCorners,
                  onTap: onOpen,
                  child: ConstrainedBox(
                    constraints: const BoxConstraints(minHeight: Look.row2),
                    child: Row(
                      children: [
                        const SizedBox(width: Look.stripeX),
                        Container(
                          width: Look.stripe,
                          height: Look.stripeH,
                          decoration: BoxDecoration(
                            color: StatusStyle.attention,
                            borderRadius: BorderRadius.circular(Look.stripe),
                          ),
                        ),
                        const SizedBox(width: 12),
                        Expanded(
                          child: Column(
                            mainAxisSize: MainAxisSize.min,
                            crossAxisAlignment: CrossAxisAlignment.start,
                            children: [
                              Text(
                                '${a.student.isEmpty ? '학생' : a.student} · ${a.isSecret ? '1Password 승인 요청' : '승인 요청'}',
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                                style: const TextStyle(fontSize: Look.body, fontWeight: FontWeight.w600),
                              ),
                              const SizedBox(height: Look.rowGap),
                              Text(
                                '${a.machine} · ${a.tool} · ${a.headline}',
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                                style: TextStyle(fontSize: Look.sub, color: scheme.onSurfaceVariant),
                              ),
                            ],
                          ),
                        ),
                        IconButton(
                          tooltip: '나중에',
                          icon: const Icon(Icons.close_rounded, size: Look.iconSize),
                          onPressed: onDismiss,
                        ),
                      ],
                    ),
                  ),
                ),
              ),
            ),
          ),
        );
      },
    );
  }
}
