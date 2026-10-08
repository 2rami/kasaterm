import 'dart:async';

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../background_grace.dart';
import '../contrast.dart';
import '../grid.dart';
import '../grid_canvas.dart';
import '../look.dart';
import '../server.dart';
import '../shell_blocks.dart';
import '../status_style.dart';
import '../term_session.dart';
import '../theme_prefs.dart';
import '../twins_loading.dart';

/// 셸 칸을 「명령 + 결과」 카드로 — 데스크톱 원본이 모은 OSC 133 구간(`/term/blocks`)을
/// 폰 폭으로 다시 접어 보인다. 원본 칸 크기는 안 건드린다(`docs/mirror-render.md`).
/// 셸 통합이 없거나 전체 화면 프로그램이 도는 동안은 원본 격자를 줄여 보기만 한다.
/// 치수는 `docs/design.md` 「거울 셸 명령 묶음」.
class ShellBlocksView extends StatefulWidget {
  const ShellBlocksView({
    super.key,
    required this.server,
    required this.pane,
    required this.session,
    required this.onTerminal,
    this.onCommand,
    this.bottomTick = 0,
    this.active = true,
  });

  final Server server;
  final Pane pane;
  final TermSession session;
  final VoidCallback onTerminal;

  /// 명령 한 줄을 이 셸에 보낸다(입력칸에 쓰던 글은 건드리지 않는다) — 폴더 고리가 `cd` 를 친다.
  final Future<void> Function(String command)? onCommand;

  /// 보낼 때마다 오른다 — 맨 아래로 내려간다.
  final int bottomTick;

  /// 보이는 동안만 받는다.
  final bool active;

  @override
  State<ShellBlocksView> createState() => _ShellBlocksViewState();
}

/// 끝난 결과가 이 줄을 넘으면 앞 [_head]·뒤 [_tail] 만 두고 접는다.
const _foldOver = 24;
const _head = 10;
const _tail = 6;

/// 도는 명령은 끝이 중요하다 — 뒤 이만큼만.
const _live = 16;

const _mono = TextStyle(
  fontFamily: 'TermMono',
  fontFamilyFallback: Look.flowMonoFallback,
  fontSize: Look.sub,
  height: 1.35,
);

class _ShellBlocksViewState extends State<ShellBlocksView> {
  static const _waitMs = 15000;
  static const _pollEvery = Duration(seconds: 2);

  final _feed = ShellFeed();
  final _open = <int>{};
  final _fetching = <int>{};
  final _scroll = ScrollController();
  final _taps = <String, TapGestureRecognizer>{};
  Timer? _timer;
  bool _polling = false;
  bool _old = false;
  bool _away = false;
  String? _error;

  @override
  void initState() {
    super.initState();
    BackgroundGrace.instance.addListener(_graceChanged);
    _scroll.addListener(_onScroll);
    if (widget.active) _start();
  }

  @override
  void didUpdateWidget(ShellBlocksView old) {
    super.didUpdateWidget(old);
    if (old.active != widget.active) _graceChanged();
    if (old.bottomTick != widget.bottomTick) _toBottom();
  }

  @override
  void dispose() {
    _timer?.cancel();
    BackgroundGrace.instance.removeListener(_graceChanged);
    _scroll.dispose();
    for (final t in _taps.values) {
      t.dispose();
    }
    super.dispose();
  }

  void _graceChanged() {
    if (BackgroundGrace.instance.live && widget.active && !_old) {
      _start();
    } else {
      _timer?.cancel();
      _timer = null;
    }
  }

  void _start() {
    _poll();
    _timer ??= Timer.periodic(_pollEvery, (_) => _poll());
  }

  /// 목록은 거꾸로 쌓여 0 이 맨 아래다.
  void _onScroll() {
    final away = _scroll.hasClients && _scroll.offset > 240;
    if (away != _away) setState(() => _away = away);
  }

  void _toBottom() {
    if (_scroll.hasClients) _scroll.jumpTo(0);
  }

  Future<void> _poll() async {
    if (_polling || !mounted || _old) return;
    _polling = true;
    var again = false;
    try {
      final before = _feed.since;
      final answer = await widget.server.shellBlocks(
        widget.pane.id,
        machine: widget.pane.machine,
        since: _feed.since,
        have: _feed.have,
        waitMs: _feed.loaded ? _waitMs : null,
      );
      if (!mounted) return;
      if (answer == null) {
        if (!_feed.loaded) setState(() => _error = '$_where을 못 찾았어요');
        return;
      }
      setState(() {
        _feed.merge(answer);
        _error = null;
      });
      // 기다려 받은 바뀜이면 곧바로 다음 바뀜을 기다린다 — 타이머 박자를 안 기다린다.
      again = before != null && before != _feed.since && widget.active;
    } on ServerException catch (e) {
      if (!mounted) return;
      if (e.status == 404) {
        setState(() => _old = true);
        _timer?.cancel();
      } else if (!_feed.loaded) {
        setState(() => _error = e.message);
      }
    } finally {
      _polling = false;
      if (again && mounted) {
        // 도는 빌드처럼 계속 바뀌면 답이 바로바로 온다 — 숨 돌릴 틈을 둔다.
        Timer(const Duration(milliseconds: 250), _poll);
      }
    }
  }

  Future<void> _toggle(ShellBlock b) async {
    setState(() => _open.contains(b.id) ? _open.remove(b.id) : _open.add(b.id));
    if (!_open.contains(b.id) || b.gap == 0 || b.running) return;
    if (!_fetching.add(b.id)) return;
    try {
      final answer = await widget.server.shellBlocks(
        widget.pane.id,
        machine: widget.pane.machine,
        block: b.id,
      );
      final whole = ShellBlock.parse((answer?['blocks'] as List?)?.firstOrNull);
      if (whole != null && mounted) setState(() => _feed.replace(whole));
    } on ServerException catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(e.message)));
      }
    } finally {
      _fetching.remove(b.id);
    }
  }

  /// 명령이 도는 곳 — 데스크톱 칸이거나, 창 밖 셸 그 자체.
  String get _where => widget.pane.isWebShell ? '이 셸' : '데스크톱 칸';

  /// 폴더 고리 하나의 누름 — 같은 폴더는 같은 인식기를 다시 쓴다.
  GestureRecognizer _tapFor(String path) =>
      _taps[path] ??= TapGestureRecognizer()..onTap = () => _goTo(path);

  void _goTo(String path) {
    final send = widget.onCommand;
    if (send == null) return;
    if (_feed.running) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(content: Text('명령이 도는 중이라 지금은 못 가요 — 끝나면 눌러 주세요')),
      );
      return;
    }
    send('cd -- ${shellQuote(path)}');
  }

  void _copy(ShellBlock b) {
    Clipboard.setData(ClipboardData(text: '\$ ${b.cmd}\n${b.plain}'));
    ScaffoldMessenger.of(context).showSnackBar(
      const SnackBar(content: Text('명령과 결과를 복사했어요')),
    );
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: Listenable.merge([widget.session, phoneThemeMode]),
    builder: (context, _) {
      final s = widget.session;
      final palette = TerminalPalette.forViewer(
        context,
        mode: phoneThemeMode.value,
        source: s.tokens,
      );
      final action = TextButton(
        onPressed: widget.onTerminal,
        child: const Text('터미널로 보기'),
      );
      if (_old) {
        return Center(
          child: TwinsNotice(
            text: '이 데스크톱 판은 명령 묶음을 몰라요\n새 판에서 보여요',
            action: action,
          ),
        );
      }
      if (!_feed.loaded) {
        return Center(
          child: _error == null
              ? const TwinsLoading(label: '명령을 받는 중', size: Look.twinsSmall)
              : TwinsNotice(
                  text: _error!,
                  color: Theme.of(context).colorScheme.error,
                  action: action,
                ),
        );
      }
      if (!_feed.integration || _feed.alt || s.grid.alt) {
        return _shrunkGrid(context, s, palette);
      }
      return ColoredBox(
        color: palette.bg,
        child: Stack(
          children: [
            if (_feed.blocks.isEmpty)
              Center(
                child: TwinsNotice(
                  text: '아직 친 명령이 없어요\n아래에서 치면 $_where에서 돌아요',
                ),
              )
            else
              ListView.builder(
                controller: _scroll,
                reverse: true,
                padding: const EdgeInsets.fromLTRB(12, 8, 12, 8),
                itemCount: _feed.blocks.length,
                itemBuilder: (context, i) => _Card(
                  block: _feed.blocks[_feed.blocks.length - 1 - i],
                  palette: palette,
                  open: _open.contains(_feed.blocks[_feed.blocks.length - 1 - i].id),
                  onToggle: _toggle,
                  onCopy: _copy,
                  tapFor: widget.onCommand == null ? null : _tapFor,
                ),
              ),
            if (_away)
              Positioned(
                right: 12,
                bottom: 12,
                child: IconButton.filledTonal(
                  tooltip: '맨 아래로',
                  onPressed: _toBottom,
                  icon: const Icon(Icons.keyboard_arrow_down),
                ),
              ),
          ],
        ),
      );
    },
  );

  /// 셸 통합 없음·전체 화면 프로그램 — 원본 격자를 폭에 맞춰 줄여 보기만 한다.
  Widget _shrunkGrid(BuildContext context, TermSession s, TerminalPalette palette) {
    final theme = Theme.of(context);
    final why = _feed.integration
        ? '전체 화면 프로그램이 도는 중이에요 · $_where을 줄여 보여요(두 손가락으로 키워 봐요)'
        : '이 칸은 셸 통합이 없어 명령을 못 나눠요 · $_where을 줄여 보여요(두 손가락으로 키워 봐요)';
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(Look.pagePad, 6, Look.pagePad, 6),
          child: Text(
            why,
            style: theme.textTheme.bodySmall?.copyWith(
              fontSize: Look.sub,
              color: theme.colorScheme.onSurfaceVariant,
            ),
          ),
        ),
        Expanded(
          child: GridCanvas(
            grid: s.grid,
            version: s.grid.version,
            palette: palette,
            contain: true,
          ),
        ),
      ],
    );
  }
}

class _Card extends StatelessWidget {
  const _Card({
    required this.block,
    required this.palette,
    required this.open,
    required this.onToggle,
    required this.onCopy,
    this.tapFor,
  });

  final ShellBlock block;
  final TerminalPalette palette;
  final bool open;
  final ValueChanged<ShellBlock> onToggle;
  final ValueChanged<ShellBlock> onCopy;

  /// 폴더 고리의 누름 인식기. 없으면 고리를 그리지 않는다.
  final GestureRecognizer Function(String path)? tapFor;

  @override
  Widget build(BuildContext context) {
    final b = block;
    final fill = mixToward(palette.bg, palette.fg, 0.06);
    final dim = mixToward(palette.fg, palette.bg, 0.4);
    final (dot, meta) = _status(b, palette, dim, Theme.of(context).colorScheme.error);
    final lines = b.lines;
    final all = [for (var i = 0; i < lines.length; i++) i];
    // 접기: (보일 줄 번호, 접기 줄을 끼울 자리, 접기 줄 글). 번호는 고리가 가리키는 줄과 같다.
    final (List<int> shown, int? foldAt, String foldText) =
        b.running && !open && lines.length > _live
        ? (
            all.sublist(lines.length - _live),
            0,
            '앞 ${lines.length - _live + b.gap}줄',
          )
        : !b.running && !open && lines.length > _foldOver
        ? (
            [...all.take(_head), ...all.skip(lines.length - _tail)],
            _head,
            '가운데 ${lines.length - _head - _tail + b.gap}줄 더 보기',
          )
        : open && b.gap > 0
        ? (all, b.gapAt ?? lines.length, '${b.gap}줄 받는 중…')
        : open && lines.length > _foldOver
        ? (all, lines.length, '접기')
        : (all, null, '');
    final note = b.tui
        ? '전체 화면 프로그램이었어요 — 그 화면은 데스크톱 칸에만 있어요'
        : b.dropped > 0
        ? '너무 길어 앞 ${b.dropped}줄은 데스크톱도 버렸어요'
        : null;
    Widget out(List<int> part) => SelectableText.rich(
      TextSpan(
        style: _mono.copyWith(color: palette.fg),
        children: [
          for (var i = 0; i < part.length; i++) ...[
            if (i > 0) const TextSpan(text: '\n'),
            ..._lineSpans(part[i], fill),
          ],
        ],
      ),
    );
    Widget fold() => InkWell(
      onTap: () => onToggle(b),
      borderRadius: BorderRadius.circular(Look.radiusSm),
      child: ConstrainedBox(
        constraints: const BoxConstraints(minHeight: Look.tap),
        child: Row(
          children: [
            Icon(
              foldText == '접기' ? Icons.expand_less : Icons.expand_more,
              size: 18,
              color: dim,
            ),
            const SizedBox(width: 6),
            Text(foldText, style: TextStyle(fontSize: Look.sub, color: dim)),
          ],
        ),
      ),
    );
    final at = foldAt;
    return Padding(
      padding: const EdgeInsets.only(top: 8),
      child: DecoratedBox(
        decoration: BoxDecoration(
          borderRadius: Look.corners,
          color: fill,
          border: b.running ? Border.all(color: palette.cursor) : null,
        ),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(12, 4, 4, 10),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              InkWell(
                onLongPress: () => onCopy(b),
                child: ConstrainedBox(
                  constraints: const BoxConstraints(minHeight: Look.tap),
                  child: Row(
                    children: [
                      Container(
                        width: 8,
                        height: 8,
                        decoration: BoxDecoration(
                          color: dot,
                          shape: BoxShape.circle,
                        ),
                      ),
                      const SizedBox(width: 8),
                      Expanded(
                        child: Text(
                          b.cmd.isEmpty ? '(명령 줄을 못 읽었어요)' : b.cmd,
                          maxLines: 2,
                          overflow: TextOverflow.ellipsis,
                          style: _mono.copyWith(
                            color: b.cmd.isEmpty ? dim : palette.fg,
                            fontWeight: FontWeight.w600,
                          ),
                        ),
                      ),
                      const SizedBox(width: 8),
                      Text(meta, style: TextStyle(fontSize: Look.sub, color: dim)),
                      IconButton(
                        tooltip: '명령과 결과 복사',
                        onPressed: () => onCopy(b),
                        iconSize: 18,
                        color: dim,
                        icon: const Icon(Icons.content_copy_outlined),
                      ),
                    ],
                  ),
                ),
              ),
              Padding(
                padding: const EdgeInsets.only(right: 8),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    if (note != null)
                      Text(note, style: TextStyle(fontSize: Look.sub, color: dim)),
                    if (at == null)
                      if (shown.isNotEmpty) out(shown) else const SizedBox.shrink()
                    else ...[
                      if (at > 0) out(shown.sublist(0, at)),
                      fold(),
                      if (at < shown.length) out(shown.sublist(at)),
                    ],
                  ],
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }

  (Color, String) _status(ShellBlock b, TerminalPalette palette, Color dim, Color danger) {
    if (b.running) {
      final ran = DateTime.now().millisecondsSinceEpoch - b.startMs;
      final took = tookLabel(ran < 0 ? 0 : ran);
      return (palette.cursor, took.isEmpty ? '도는 중' : '도는 중 · $took');
    }
    final took = b.ms == null ? '' : tookLabel(b.ms!);
    String withTook(String head) => [head, took].where((t) => t.isNotEmpty).join(' · ');
    return switch (b.exit) {
      0 => (StatusStyle.success, took),
      final int code => (danger, withTook('종료 $code')),
      null => (dim, withTook('멈춤')),
    };
  }

  /// 한 줄을 조각으로 — 폴더 고리 자리는 조각을 갈라 강조색·밑줄과 누름을 단다.
  List<TextSpan> _lineSpans(int index, Color fill) {
    final runs = block.lines[index];
    final tap = tapFor;
    final links = block.links.where((l) => l.line == index).toList();
    if (tap == null || links.isEmpty) return [for (final r in runs) _span(r, fill)];
    final out = <TextSpan>[];
    var at = 0;
    for (final r in runs) {
      final chars = r.text.runes.toList();
      var from = 0;
      while (from < chars.length) {
        final pos = at + from;
        final link = links.where((l) => pos >= l.start && pos < l.start + l.length).firstOrNull;
        final end = link != null
            ? (link.start + link.length - at).clamp(from + 1, chars.length)
            : (links
                      .map((l) => l.start - at)
                      .where((s) => s > from)
                      .fold(chars.length, (m, s) => s < m ? s : m))
                  .clamp(from + 1, chars.length);
        final piece = Run(String.fromCharCodes(chars.sublist(from, end)), r.fg, r.bg, r.flags);
        final span = _span(piece, fill);
        out.add(
          link == null
              ? span
              : TextSpan(
                  text: span.text,
                  style: span.style?.copyWith(
                    color: palette.cursor,
                    decoration: TextDecoration.underline,
                    decorationColor: palette.cursor,
                  ),
                  recognizer: tap(link.path),
                ),
        );
        from = end;
      }
      at += chars.length;
    }
    return out;
  }

  /// 격자와 같은 색 규칙(grid_canvas `_RowCache`) — 반전·흐림·스스로 고른 색의 대비 바닥.
  TextSpan _span(Run r, Color fill) {
    final inverse = r.flags & flagInverse != 0;
    var fg = palette.resolve(inverse ? r.bg : r.fg, foreground: !inverse);
    final ownBg = inverse || r.bg is! DefaultColor;
    final bg = ownBg ? palette.resolve(inverse ? r.fg : r.bg, foreground: inverse) : fill;
    if (r.flags & flagDim != 0) {
      fg = mixToward(fg, bg, 0.55);
    } else if (namesOwnColor(inverse ? r.bg : r.fg)) {
      fg = enforceContrast(fg, bg, palette.minContrast);
    }
    return TextSpan(
      text: r.text,
      style: TextStyle(
        color: fg,
        backgroundColor: ownBg ? bg : null,
        fontWeight: r.flags & flagBold != 0 ? FontWeight.w600 : null,
        fontStyle: r.flags & flagItalic != 0 ? FontStyle.italic : null,
        decoration: r.flags & flagUnderline != 0 ? TextDecoration.underline : null,
      ),
    );
  }
}
