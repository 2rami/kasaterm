import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:url_launcher/url_launcher.dart';

import '../approvals.dart';
import '../conversation.dart';
import '../login_link.dart';
import '../look.dart';
import '../relay_account.dart';
import '../server.dart';
import '../status_style.dart';
import 'approval_key.dart';
import 'approval_screen.dart';
import 'oauth_sheet.dart';

/// 대화 보기 입력줄 위에 그 칸 학생이 사람에게 낸 일을 카드로 세운다(docs/conversation-asks.md).
/// - 승인 요청(키체인 비밀·도구 권한): 그 자리에서 허락·거절, 비밀은 Face ID. 다른 곳에서 닫히면 결과를 잠깐 말하고 걷힌다.
/// - 로그인 요청: 학생이 준 로그인 주소를 앱 안 로그인 창으로 열고, 돌아온 주소를 사람이 확인하면 그 학생에게 보낸다.
class ConversationAsks extends StatefulWidget {
  const ConversationAsks({
    super.key,
    required this.pane,
    required this.rows,
    required this.active,
    required this.toolsAsked,
    required this.onSend,
    this.center,
    this.authenticate,
    this.cancelAuthenticate,
  });

  final Pane pane;

  /// 대화 줄 — 사람이 마지막으로 말한 뒤의 학생 말에서 로그인 요청을 찾는다.
  final List<Object> rows;

  /// 보이는 쪽인가. 아니면 아무것도 안 세우고 위쪽 알림 띠에 맡긴다.
  final bool active;

  /// 화면 선택지 카드나 mod 요청 카드가 지금 도구 승인을 묻는 중(또는 막 답했다) — 같은 요청을 두 번 세우지 않는다.
  final bool toolsAsked;

  /// 학생에게 보낼 말(돌아온 주소). 실패하면 던진다.
  final Future<void> Function(String text) onSend;

  final ApprovalCenter? center;
  final WebAuthenticate? authenticate;
  final Future<void> Function()? cancelAuthenticate;

  @override
  State<ConversationAsks> createState() => _ConversationAsksState();
}

/// 사람이 「×」로 접은 로그인 요청 — 앱이 도는 동안만 기억한다.
final _dismissedLogins = <String>{};

class _ConversationAsksState extends State<ConversationAsks> {
  ApprovalCenter get _center => widget.center ?? ApprovalCenter.instance;

  /// 이 카드로 보인 적 있는 요청과, 닫힌 뒤 결과를 말하는 동안의 시계·다 말한 것.
  final _seen = <String>{};
  final _linger = <String, Timer>{};
  final _said = <String>{};
  final _busy = <String>{};
  final _errors = <String, String>{};
  final _decidedHere = <String>{};
  Timer? _tick;
  bool? _hasKey;
  bool _checkingKey = false;
  ModalRoute<Object?>? _route;

  static const _lingerFor = Duration(seconds: 4);

  @override
  void initState() {
    super.initState();
    _center.addListener(_changed);
    _register();
  }

  @override
  void didUpdateWidget(ConversationAsks old) {
    super.didUpdateWidget(old);
    if (old.center != widget.center) {
      (old.center ?? ApprovalCenter.instance).removeListener(_changed);
      (old.center ?? ApprovalCenter.instance).hideInPlace(this);
      _center.addListener(_changed);
    }
    _register();
  }

  @override
  void dispose() {
    _tick?.cancel();
    for (final t in _linger.values) {
      t.cancel();
    }
    _center
      ..removeListener(_changed)
      ..hideInPlace(this);
    super.dispose();
  }

  void _register() {
    final machine = widget.pane.machine;
    if (machine == null) {
      _center.hideInPlace(this);
      return;
    }
    _center.showInPlace(
      this,
      pane: widget.pane.id,
      machine: machine,
      visible: () => mounted && widget.active && (_route?.isCurrent ?? true),
    );
  }

  void _changed() {
    if (mounted) setState(() {});
  }

  /// 이 칸에서 보일 승인 요청 — 열린 것과, 닫힌 지 얼마 안 돼 결과를 말하는 것.
  List<Approval> _approvals() {
    final shown = <Approval>[];
    for (final a in _center.pending) {
      if (!_center.fromPane(a, pane: widget.pane.id, machine: widget.pane.machine)) continue;
      if (!a.isSecret && widget.toolsAsked && !_seen.contains(a.id)) continue;
      _seen.add(a.id);
    }
    for (final id in _seen.toList()) {
      final a = _center.byId(id);
      if (a == null) {
        _seen.remove(id);
        continue;
      }
      final open = a.pending && _center.remainingMs(a) > 0;
      if (open) {
        // 화면 선택지가 같은 도구 요청을 묻기 시작하면 그쪽에 맡긴다 — 거기서 답한 결과를 여기서 다시 말하지 않는다.
        if (!a.isSecret && widget.toolsAsked && !_busy.contains(id)) {
          _seen.remove(id);
          continue;
        }
        shown.add(a);
        continue;
      }
      if (_said.remove(id)) {
        _seen.remove(id);
        _linger.remove(id);
        _errors.remove(id);
        _decidedHere.remove(id);
        continue;
      }
      _linger.putIfAbsent(id, () => Timer(_lingerFor, () {
        _said.add(id);
        _changed();
      }));
      shown.add(a);
    }
    return shown;
  }

  void _keepTicking(bool want) {
    if (want && _tick == null) {
      _tick = Timer.periodic(const Duration(seconds: 1), (_) => _changed());
    } else if (!want) {
      _tick?.cancel();
      _tick = null;
    }
  }

  Future<void> _checkKey() async {
    _checkingKey = true;
    try {
      final has = await _center.signer.publicKey() != null;
      if (mounted && has != _hasKey) setState(() => _hasKey = has);
    } finally {
      _checkingKey = false;
    }
  }

  Future<void> _decide(Approval a, bool allow) async {
    if (_busy.contains(a.id)) return;
    HapticFeedback.selectionClick();
    setState(() {
      _busy.add(a.id);
      _errors.remove(a.id);
    });
    try {
      final done = await _center.decide(a, allow);
      if (!done.pending) _decidedHere.add(a.id);
    } on AccountException catch (e) {
      _errors[a.id] = e.message;
      if (e.code == 'no_key') _hasKey = false;
    } finally {
      _busy.remove(a.id);
      if (mounted) setState(() {});
    }
  }

  Future<void> _makeKey() async {
    final session = _center.session;
    if (session == null) return;
    await Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => ApprovalKeyScreen(
          api: () => RelayAccountApi(session.origin, session: session),
          signer: _center.signer,
        ),
      ),
    );
    await _checkKey();
  }

  void _openFull(Approval a) => unawaited(
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        fullscreenDialog: true,
        builder: (_) => ApprovalScreen(center: _center, id: a.id),
      ),
    ),
  );

  /// 사람이 마지막으로 말한 뒤 학생이 낸 로그인 요청.
  (String, LoginAsk)? _login() {
    final rows = widget.rows;
    for (var i = rows.length - 1, seen = 0; i >= 0 && seen < 40; i--, seen++) {
      final r = rows[i];
      if (r is! ChatBubble) continue;
      if (r.mine) return null;
      if (r.user) continue;
      final ask = findLoginAsk(r.text);
      if (ask == null) continue;
      final key = '${widget.pane.machine}|${widget.pane.id}|${ask.url}';
      return _dismissedLogins.contains(key) ? null : (key, ask);
    }
    return null;
  }

  @override
  Widget build(BuildContext context) {
    _route = ModalRoute.of(context);
    if (!widget.active) {
      _keepTicking(false);
      return const SizedBox.shrink();
    }
    final approvals = _approvals();
    _keepTicking(approvals.isNotEmpty);
    if (_hasKey == null && !_checkingKey && approvals.any((a) => a.isSecret)) unawaited(_checkKey());
    final login = _login();
    if (approvals.isEmpty && login == null) return const SizedBox.shrink();
    return ConstrainedBox(
      constraints: BoxConstraints(maxHeight: MediaQuery.sizeOf(context).height * 0.45),
      child: SingleChildScrollView(
        reverse: true,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            for (final a in approvals)
              _ApprovalCard(
                key: ValueKey('ask-approval-${a.id}'),
                approval: a,
                remainingMs: _center.remainingMs(a),
                busy: _busy.contains(a.id),
                error: _errors[a.id],
                decidedHere: _decidedHere.contains(a.id),
                noKey: a.isSecret && _hasKey == false,
                onDecide: (allow) => unawaited(_decide(a, allow)),
                onMakeKey: () => unawaited(_makeKey()),
                onMore: () => _openFull(a),
              ),
            if (login case (final key, final ask))
              _LoginCard(
                key: ValueKey('ask-login-$key'),
                ask: ask,
                student: widget.pane.displayName,
                onSend: widget.onSend,
                onDismiss: () => setState(() => _dismissedLogins.add(key)),
                authenticate: widget.authenticate ??
                    (!kIsWeb && defaultTargetPlatform == TargetPlatform.iOS ? systemWebAuthenticate : null),
                cancelAuthenticate: widget.cancelAuthenticate ?? cancelSystemWebAuthenticate,
              ),
          ],
        ),
      ),
    );
  }
}

/// 선택지 카드와 같은 판 — 판 색 모서리 18, 주의색 테(docs/design.md 「대화 보기」).
class _AskFrame extends StatelessWidget {
  const _AskFrame({required this.child, required this.border});
  final Widget child;
  final Color border;

  @override
  Widget build(BuildContext context) => Container(
    margin: const EdgeInsets.fromLTRB(12, 0, 12, 6),
    padding: const EdgeInsets.fromLTRB(12, 10, 12, 10),
    decoration: BoxDecoration(
      color: TwinTone.of(context).card,
      borderRadius: Look.cardCorners,
      border: Border.all(color: border),
    ),
    child: child,
  );
}

class _ApprovalCard extends StatelessWidget {
  const _ApprovalCard({
    super.key,
    required this.approval,
    required this.remainingMs,
    required this.busy,
    required this.error,
    required this.decidedHere,
    required this.noKey,
    required this.onDecide,
    required this.onMakeKey,
    required this.onMore,
  });

  final Approval approval;
  final int remainingMs;
  final bool busy;
  final String? error;
  final bool decidedHere;
  final bool noKey;
  final void Function(bool allow) onDecide;
  final VoidCallback onMakeKey;
  final VoidCallback onMore;

  static const _linesMax = 4;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final a = approval;
    final open = a.pending && remainingMs > 0;
    final dim = TextStyle(fontSize: Look.sub, color: scheme.onSurfaceVariant);
    final mono = TextStyle(
      fontFamily: 'TermMono',
      fontFamilyFallback: Look.flowMonoFallback,
      fontSize: Look.sub,
      height: 1.4,
      color: scheme.onSurface,
    );
    final lines = [
      for (final f in a.fields)
        for (final l in f.text.split('\n'))
          if (l.trim().isNotEmpty) (f.name, l),
    ];
    final shown = lines.length > _linesMax ? lines.take(_linesMax - 1).toList() : lines;
    final Widget foot;
    if (open) {
      final cantAllow = a.truncated || (a.isSecret && !a.secretValid);
      foot = Row(
        children: [
          Expanded(
            child: ApprovalButton(
              key: const Key('ask-deny'),
              label: '거절',
              fill: scheme.error.withValues(alpha: Look.dangerTint),
              ink: scheme.error,
              onPressed: busy ? null : () => onDecide(false),
            ),
          ),
          const SizedBox(width: Look.cardGap),
          Expanded(
            child: noKey
                ? ApprovalButton(
                    key: const Key('ask-make-key'),
                    label: 'Face ID 로 연결',
                    fill: scheme.primary.withValues(alpha: 0.14),
                    ink: scheme.primary,
                    onPressed: busy ? null : onMakeKey,
                  )
                : ApprovalButton(
                    key: const Key('ask-allow'),
                    label: a.isSecret ? 'Face ID 로 허락' : '허락',
                    fill: scheme.primary,
                    ink: scheme.onPrimary,
                    busy: busy,
                    onPressed: busy || cantAllow ? null : () => onDecide(true),
                  ),
          ),
        ],
      );
    } else {
      final allowed = a.state == 'allowed';
      foot = ApprovalBand(
        key: const Key('ask-result'),
        text: decidedHere
            ? (allowed ? (a.isSecret ? '허락했어요 · 이번 한 번만 꺼내요' : '허락했어요') : '거절했어요')
            : a.pending
            ? '2분이 지나 원래 창으로 돌아갔어요'
            : a.closedLine,
        color: allowed
            ? StatusStyle.successText(scheme)
            : a.state == 'denied'
            ? scheme.error
            : scheme.onSurfaceVariant,
      );
    }
    return Semantics(
      container: true,
      liveRegion: !open,
      child: _AskFrame(
        border: open ? StatusStyle.attention : scheme.outlineVariant,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          mainAxisSize: MainAxisSize.min,
          children: [
            Row(
              children: [
                Icon(
                  a.isSecret ? Icons.key_rounded : Icons.verified_user_outlined,
                  size: 18,
                  color: open ? StatusStyle.attention : scheme.onSurfaceVariant,
                ),
                const SizedBox(width: 6),
                Expanded(
                  child: Text(
                    a.isSecret ? a.secretTitle : '승인 요청 · ${toolLabel(a.tool)}',
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: theme.textTheme.titleSmall?.copyWith(fontWeight: FontWeight.w600),
                  ),
                ),
                if (open) Text(_left(remainingMs), key: const Key('ask-left'), style: dim),
                TextButton(key: const Key('ask-more'), onPressed: onMore, child: const Text('자세히')),
              ],
            ),
            for (final (name, line) in shown)
              Text(
                line,
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
                style: name == 'command' && a.isSecret ? mono.copyWith(color: scheme.onSurfaceVariant) : mono,
              ),
            if (lines.length > shown.length)
              Text('… ${lines.length - shown.length}줄 더 — 「자세히」에서 전부', style: dim),
            if (open && a.isSecret && !noKey)
              Padding(
                padding: const EdgeInsets.only(top: 6),
                child: Text('값은 이 폰을 지나지 않고 요청한 명령에만 가요.', style: dim),
              ),
            if (open && noKey)
              Padding(
                padding: const EdgeInsets.only(top: 6),
                child: Text(
                  '이 폰이 아직 비밀 승인에 연결되지 않았어요. 한 번 연결하면 여기서 바로 허락할 수 있어요.',
                  style: dim.copyWith(color: scheme.error),
                ),
              ),
            if (open && a.truncated)
              Padding(
                padding: const EdgeInsets.only(top: 6),
                child: Text('원문이 길어 앞부분만 왔어요. 허락은 원래 창에서만 할 수 있어요.', style: dim.copyWith(color: scheme.error)),
              ),
            if (error case final e?)
              Padding(
                padding: const EdgeInsets.only(top: 6),
                child: Text(e, key: const Key('ask-error'), style: TextStyle(fontSize: Look.sub, color: scheme.error)),
              ),
            const SizedBox(height: 8),
            foot,
          ],
        ),
      ),
    );
  }

  static String _left(int ms) {
    final s = (ms / 1000).ceil().clamp(0, 9999);
    return '${s ~/ 60}:${(s % 60).toString().padLeft(2, '0')}';
  }
}

enum _LoginPhase { idle, opening, caught, sending }

class _LoginCard extends StatefulWidget {
  const _LoginCard({
    super.key,
    required this.ask,
    required this.student,
    required this.onSend,
    required this.onDismiss,
    required this.authenticate,
    required this.cancelAuthenticate,
  });

  final LoginAsk ask;
  final String student;
  final Future<void> Function(String text) onSend;
  final VoidCallback onDismiss;
  final WebAuthenticate? authenticate;
  final Future<void> Function() cancelAuthenticate;

  @override
  State<_LoginCard> createState() => _LoginCardState();
}

class _LoginCardState extends State<_LoginCard> {
  _LoginPhase _phase = _LoginPhase.idle;
  Uri? _back;
  String? _error;

  Future<void> _open() async {
    final authenticate = widget.authenticate;
    setState(() {
      _phase = _LoginPhase.opening;
      _error = null;
    });
    if (authenticate == null) {
      // 시스템 로그인 창이 없는 기기 — 브라우저로 열고, 돌아온 주소는 복사해 붙여 넣는다.
      final opened = await launchUrl(widget.ask.url, mode: LaunchMode.externalApplication).catchError((Object _) => false);
      if (mounted) {
        setState(() {
          _phase = _LoginPhase.idle;
          if (!opened) _error = '브라우저를 열지 못했어요.';
        });
      }
      return;
    }
    try {
      final back = await catchLoginRedirect(widget.ask, authenticate: authenticate, cancel: widget.cancelAuthenticate);
      if (!mounted) return;
      setState(() {
        _back = back;
        _phase = back == null ? _LoginPhase.idle : _LoginPhase.caught;
      });
    } on LoginCatchException catch (e) {
      if (mounted) {
        setState(() {
          _phase = _LoginPhase.idle;
          _error = e.message;
        });
      }
    }
  }

  Future<void> _paste() async {
    final data = await Clipboard.getData(Clipboard.kTextPlain);
    final back = pastedLoginRedirect(data?.text);
    if (!mounted) return;
    setState(() {
      if (back == null) {
        _error = '복사한 글이 돌아온 주소(http://localhost:포트/…?code=…)가 아니에요.';
      } else {
        _back = back;
        _error = null;
        _phase = _LoginPhase.caught;
      }
    });
  }

  Future<void> _send() async {
    final back = _back;
    if (back == null) return;
    HapticFeedback.selectionClick();
    setState(() {
      _phase = _LoginPhase.sending;
      _error = null;
    });
    try {
      await widget.onSend('$back');
    } on ServerException catch (e) {
      if (mounted) {
        setState(() {
          _phase = _LoginPhase.caught;
          _error = e.message;
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final ask = widget.ask;
    final dim = TextStyle(fontSize: Look.sub, color: scheme.onSurfaceVariant);
    final mono = TextStyle(
      fontFamily: 'TermMono',
      fontFamilyFallback: Look.flowMonoFallback,
      fontSize: Look.sub,
      height: 1.4,
      color: scheme.onSurface,
    );
    final back = _back;
    final host = ask.host.contains(':') ? '[${ask.host}]' : ask.host;
    final caught = back != null && (_phase == _LoginPhase.caught || _phase == _LoginPhase.sending);
    return Semantics(
      container: true,
      child: _AskFrame(
        border: StatusStyle.attention,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          mainAxisSize: MainAxisSize.min,
          children: [
            Row(
              children: [
                const Icon(Icons.login_rounded, size: 18, color: StatusStyle.attention),
                const SizedBox(width: 6),
                Expanded(
                  child: Text(
                    caught ? '돌아온 주소를 ${widget.student}에게 보낼까요?' : '로그인 요청',
                    style: theme.textTheme.titleSmall?.copyWith(fontWeight: FontWeight.w600),
                  ),
                ),
                IconButton(
                  key: const Key('login-dismiss'),
                  tooltip: '접기',
                  icon: const Icon(Icons.close_rounded, size: Look.iconSize),
                  onPressed: _phase == _LoginPhase.opening || _phase == _LoginPhase.sending ? null : widget.onDismiss,
                ),
              ],
            ),
            if (caught)
              Text(maskLoginRedirect(back), key: const Key('login-back'), maxLines: 3, overflow: TextOverflow.ellipsis, style: mono)
            else ...[
              Text(ask.url.host, key: const Key('login-host'), style: mono),
              Text('돌아올 자리 $host:${ask.port}${ask.path} — 이 앱이 받아 둬요', style: dim),
            ],
            if (_error case final e?)
              Padding(
                padding: const EdgeInsets.only(top: 6),
                child: Text(e, key: const Key('login-error'), style: TextStyle(fontSize: Look.sub, color: scheme.error)),
              ),
            const SizedBox(height: 8),
            if (caught)
              Row(
                children: [
                  Expanded(
                    child: ApprovalButton(
                      key: const Key('login-cancel'),
                      label: '취소',
                      fill: scheme.surfaceContainerHighest,
                      ink: scheme.onSurface,
                      onPressed: _phase == _LoginPhase.sending
                          ? null
                          : () => setState(() {
                              _back = null;
                              _phase = _LoginPhase.idle;
                            }),
                    ),
                  ),
                  const SizedBox(width: Look.cardGap),
                  Expanded(
                    child: ApprovalButton(
                      key: const Key('login-send'),
                      label: '보내기',
                      fill: scheme.primary,
                      ink: scheme.onPrimary,
                      busy: _phase == _LoginPhase.sending,
                      onPressed: _phase == _LoginPhase.sending ? null : () => unawaited(_send()),
                    ),
                  ),
                ],
              )
            else if (_phase == _LoginPhase.opening)
              Row(
                children: [
                  const SizedBox.square(dimension: 18, child: CircularProgressIndicator(strokeWidth: 2)),
                  const SizedBox(width: 10),
                  Expanded(child: Text('로그인 창에서 마쳐 주세요', style: theme.textTheme.bodyMedium)),
                ],
              )
            else ...[
              ApprovalButton(
                key: const Key('login-open'),
                label: widget.authenticate == null ? '브라우저로 열기' : '앱에서 로그인',
                fill: scheme.primary,
                ink: scheme.onPrimary,
                onPressed: () => unawaited(_open()),
              ),
              Align(
                alignment: Alignment.centerLeft,
                child: TextButton(
                  key: const Key('login-paste'),
                  onPressed: () => unawaited(_paste()),
                  child: const Text('복사한 주소 붙여 넣기'),
                ),
              ),
            ],
          ],
        ),
      ),
    );
  }
}
