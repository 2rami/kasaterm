import 'dart:async';

import 'package:flutter/foundation.dart' show kIsWeb;
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:video_player/video_player.dart';

import '../inline_media.dart';
import '../look.dart';

/// 대화 카드에서 연 소리·영상. 카드를 누르면 곧바로 서고, [open] 이 준 곳(루프백 창구·받아 둔 파일)을 앱 안에서
/// 재생한다 — 바깥 앱·새 창으로 넘기지 않는다. 닫으면 플레이어(AVPlayer)와 창구를 함께 버려 받던 요청이 끊긴다.
/// 앱이 뒤로 가면 video_player 가 멈추고 돌아오면 잇는다.
class MediaPlayerScreen extends StatefulWidget {
  const MediaPlayerScreen({super.key, required this.media, required this.open});
  final InlineMedia media;

  /// 다시 시도마다 새로 부른다 — 카드를 누른 순간의 원본에 고정돼 있다.
  final Future<MediaLink> Function() open;

  static Route<void> route({
    required InlineMedia media,
    required Future<MediaLink> Function() open,
  }) => MaterialPageRoute<void>(
    fullscreenDialog: true,
    settings: const RouteSettings(name: 'inline-media'),
    builder: (_) => MediaPlayerScreen(media: media, open: open),
  );

  @override
  State<MediaPlayerScreen> createState() => _MediaPlayerScreenState();
}

class _MediaPlayerScreenState extends State<MediaPlayerScreen>
    with WidgetsBindingObserver {
  VideoPlayerController? _c;
  MediaLink? _link;
  late Future<void> _ready;

  /// 다시 시도마다 오른다. 늦게 열린 옛 창구는 붙이지 않고 닫는다.
  int _gen = 0;

  /// 앱이 뒤로 가던 때의 재생 뜻과 자리. 돌아와서 창구가 거둬져 있으면 새 플레이어가 이대로 잇는다 — 옛 플레이어는
  /// 그사이 끊긴 연결로 오류가 나 제 자리를 잃는다(video_player 가 값을 통째로 오류 값으로 바꾼다).
  ({bool playing, Duration at})? _away;

  /// 다시 세운 플레이어가 찾아가는 자리. 흘려 받는 원본은 찾기에 몇 초 걸리고, 그동안 video_player 는 자리를 0 으로
  /// 알린다 — 시계·막대는 이 자리를 보이고 화면은 받는 중으로 두며 누름을 막는다.
  Duration? _seeking;

  /// 마지막으로 제대로 알던 자리. 오류가 나면 video_player 가 값을 통째로 바꿔 자리를 0 으로 만든다 — 다시 시도·복귀는
  /// 이 자리에서 잇는다.
  Duration _known = Duration.zero;

  static const _hidden = {
    AppLifecycleState.hidden,
    AppLifecycleState.paused,
    AppLifecycleState.detached,
  };

  /// 막대를 끄는 동안의 자리(초). 놓을 때 한 번만 찾아간다.
  double? _drag;
  bool _resume = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _ready = _start();
  }

  /// 카드를 누른 것이 곧 재생하라는 뜻이라 열리면 바로 튼다. [at]·[play] 는 다시 열 때 옛 플레이어의 자리와 뜻.
  Future<void> _start({Duration at = Duration.zero, bool play = true}) async {
    final gen = ++_gen;
    final link = await widget.open();
    if (!mounted || gen != _gen) {
      unawaited(link.close());
      return;
    }
    final c = InlineMediaHub.instance.player(link.uri);
    c.addListener(() {
      final v = c.value;
      if (identical(c, _c) &&
          v.isInitialized &&
          !v.hasError &&
          _seeking == null) {
        _known = v.position;
      }
    });
    setState(() {
      _link = link;
      _c = c;
      _seeking = at > Duration.zero ? at : null;
      _known = at;
    });
    await c.initialize();
    if (!mounted || !identical(c, _c)) return;
    if (at > Duration.zero) {
      await c.seekTo(at);
      if (!mounted || !identical(c, _c)) return;
      setState(() => _seeking = null);
    }
    if (!play) return;
    // 여는 사이 앱이 뒤로 갔으면 숨어서 틀지 않는다 — video_player 는 paused 순간에만 멈추고 늦게 끝난 초기화는
    // 막지 않는다. 돌아올 때 _revive 가 튼다.
    if (_hidden.contains(WidgetsBinding.instance.lifecycleState)) {
      _away = (playing: true, at: _known);
      return;
    }
    await c.play();
  }

  /// 같은 원본으로 창구·플레이어를 새로 세워 [at] 에서 잇는다.
  void _reopen({required Duration at, required bool play}) {
    final old = _c, link = _link;
    _c = null;
    _link = null;
    _drag = null;
    _seeking = at > Duration.zero ? at : null;
    setState(() {
      _ready = _start(at: at, play: play);
    });
    // 이번 프레임의 빌더가 옛 플레이어에서 떨어진 뒤에 버린다 — 빌더가 버린 플레이어를 듣고 있지 않게.
    WidgetsBinding.instance.addPostFrameCallback((_) {
      old?.dispose();
      link?.close();
    });
  }

  void _retry() => _reopen(at: _known, play: true);

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state != AppLifecycleState.resumed) {
      final c = _c;
      if (c == null || !c.value.isInitialized || c.value.hasError) return;
      final v = c.value;
      final ended = _ended(v);
      // 뜻은 처음 벗어날 때 것 — paused 에서 video_player 가 스스로 멈춘다. 자리는 멈추기 직전까지 새로 잡는다.
      _away = (
        playing: _away?.playing ?? (v.isPlaying && !ended),
        at: ended ? v.duration : _known,
      );
      return;
    }
    final away = _away;
    _away = null;
    if (away != null) _revive(away);
  }

  /// 창구가 살아 있으면 video_player 가 스스로 잇는다. 거둬졌으면 같은 원본·자리로 새로 세운다 — 틀던 것만 이어
  /// 틀고, 멈춰 둔 것은 멈춘 채, 끝난 것은 끝에 둔다.
  Future<void> _revive(({bool playing, Duration at}) away) async {
    final link = _link, old = _c;
    if (link == null || old == null) return;
    final alive = await link.alive();
    if (!mounted || !identical(link, _link) || !identical(old, _c)) return;
    // 묻는 사이 다시 뒤로 갔으면 다음 복귀로 미룬다 — 뜻과 자리는 그대로 둔다.
    if (_hidden.contains(WidgetsBinding.instance.lifecycleState)) {
      _away ??= away;
      return;
    }
    if (!alive) return _reopen(at: away.at, play: away.playing);
    // 여는 사이 뒤로 가 미뤄 둔 재생 — video_player 는 제가 틀지 않은 것을 잇지 않는다.
    final v = old.value;
    if (away.playing && v.isInitialized && !v.hasError && !v.isPlaying) {
      await old.play();
    }
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _gen++;
    _c?.dispose();
    _link?.close();
    super.dispose();
  }

  bool _ended(VideoPlayerValue v) =>
      v.isCompleted ||
      (v.duration > Duration.zero && v.position >= v.duration && !v.isPlaying);

  Future<void> _toggle() async {
    final c = _c;
    if (c == null || !c.value.isInitialized || _seeking != null) return;
    final v = c.value;
    if (v.isPlaying) return c.pause();
    if (_ended(v)) await c.seekTo(Duration.zero);
    await c.play();
  }

  Future<void> _preview(String path) async {
    final messenger = ScaffoldMessenger.of(context);
    try {
      await const MethodChannel(
        'kasaterm/file_preview',
      ).invokeMethod<void>('previewPath', {'path': path});
    } on PlatformException catch (e) {
      messenger.showSnackBar(
        SnackBar(content: Text(e.message ?? '미리보기를 열지 못했어요')),
      );
    } on MissingPluginException {
      messenger.showSnackBar(
        const SnackBar(content: Text('이 기기는 미리보기를 지원하지 않아요')),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final video = widget.media.isVideo;
    final ink = video ? Colors.white : theme.colorScheme.onSurface;
    final dim = video ? Colors.white70 : theme.colorScheme.onSurfaceVariant;
    return Scaffold(
      backgroundColor: video ? Colors.black : null,
      body: SafeArea(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            SizedBox(
              height: Look.appBarH,
              child: Row(
                children: [
                  const SizedBox(
                    width: Look.pagePad - (Look.tap - Look.iconSize) / 2,
                  ),
                  IconButton(
                    onPressed: () => Navigator.of(context).maybePop(),
                    tooltip: '닫기',
                    color: ink,
                    iconSize: Look.iconSize,
                    constraints: const BoxConstraints.tightFor(
                      width: Look.tap,
                      height: Look.tap,
                    ),
                    icon: const Icon(Icons.close),
                  ),
                  const SizedBox(width: Look.mediaPad),
                  // 소리는 가운데 그림 밑에 이름이 선다 — 앱바에 또 쓰지 않는다.
                  if (video)
                    Expanded(
                      child: Text(
                        widget.media.title,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: TextStyle(
                          fontSize: Look.body,
                          fontWeight: FontWeight.w600,
                          color: ink,
                        ),
                      ),
                    ),
                  const SizedBox(width: Look.pagePad),
                ],
              ),
            ),
            Expanded(
              child: FutureBuilder<void>(
                future: _ready,
                builder: (context, snap) {
                  final c = _c;
                  // 창구를 여는 동안·첫 구간을 받는 동안 — 다 받기를 기다리지 않으니 이것이 첫 버퍼링이다.
                  final spinner = Center(
                    child: CircularProgressIndicator(
                      color: video ? Colors.white : null,
                    ),
                  );
                  if (c == null) {
                    return snap.hasError ? _failed(ink, dim) : spinner;
                  }
                  return ValueListenableBuilder<VideoPlayerValue>(
                    valueListenable: c,
                    builder: (context, v, _) {
                      if (snap.hasError || v.hasError) return _failed(ink, dim);
                      if (!v.isInitialized) return spinner;
                      return video
                          ? _stage(c, v)
                          : _AudioArt(
                              media: widget.media,
                              dim: dim,
                              buffering:
                                  (v.isBuffering && v.isPlaying) ||
                                  _seeking != null,
                            );
                    },
                  );
                },
              ),
            ),
            if (_c case final c?)
              ValueListenableBuilder<VideoPlayerValue>(
                valueListenable: c,
                builder: (context, v, _) => _controls(c, v, ink, dim),
              )
            else
              _controls(null, const VideoPlayerValue.uninitialized(), ink, dim),
          ],
        ),
      ),
    );
  }

  Widget _failed(Color ink, Color dim) {
    final link = _link;
    final path = link?.path;
    return _Failed(
      ink: ink,
      dim: dim,
      reason: link?.error,
      onRetry: _retry,
      onPreview: kIsWeb || path == null ? null : () => _preview(path),
    );
  }

  Widget _stage(VideoPlayerController c, VideoPlayerValue v) {
    final aspect = v.aspectRatio > 0 ? v.aspectRatio : 16 / 9;
    return GestureDetector(
      behavior: HitTestBehavior.opaque,
      onTap: _toggle,
      child: Stack(
        alignment: Alignment.center,
        children: [
          Center(
            child: AspectRatio(aspectRatio: aspect, child: VideoPlayer(c)),
          ),
          if ((v.isBuffering && v.isPlaying) || _seeking != null)
            const CircularProgressIndicator(color: Colors.white)
          else if (!v.isPlaying)
            IgnorePointer(
              child: SizedBox.square(
                dimension: Look.mediaPlay,
                child: DecoratedBox(
                  decoration: BoxDecoration(
                    color: Colors.black.withValues(alpha: 0.55),
                    shape: BoxShape.circle,
                  ),
                  child: Icon(
                    _ended(v) ? Icons.replay : Icons.play_arrow_rounded,
                    size: Look.mediaPlayIcon,
                    color: Colors.white,
                  ),
                ),
              ),
            ),
        ],
      ),
    );
  }

  Widget _controls(
    VideoPlayerController? c,
    VideoPlayerValue v,
    Color ink,
    Color dim,
  ) {
    final ready = v.isInitialized && !v.hasError && _seeking == null;
    final total = v.duration.inMilliseconds / 1000;
    final pos = _drag ?? (_seeking ?? v.position).inMilliseconds / 1000;
    // 길이를 모르는 동안(다시 세우는 중)에도 시계는 찾아갈 자리를 보인다.
    final at = pos.clamp(0.0, total > 0 ? total : pos);
    // AVPlayer 길이는 2.99초처럼 끝이 모자라게 온다 — 길이는 반올림해 카드의 「0:03」과 맞추고, 끝까지 간
    // 뒤에는 지난 시간도 길이와 같게 보인다.
    final length = Duration(seconds: (total + 0.5).floor());
    final shown = _drag == null && _seeking == null && _ended(v)
        ? length
        : Duration(milliseconds: (at * 1000).round());
    final clock = TextStyle(
      fontSize: Look.sub,
      color: dim,
      fontFeatures: const [FontFeature.tabularFigures()],
    );
    final scheme = Theme.of(context).colorScheme;
    final video = widget.media.isVideo;
    return Padding(
      padding: const EdgeInsets.fromLTRB(
        Look.pagePad,
        Look.mediaPad,
        Look.pagePad,
        Look.pagePad,
      ),
      child: Row(
        children: [
          SizedBox.square(
            dimension: Look.tap,
            child: IconButton.filled(
              onPressed: ready ? _toggle : null,
              tooltip: v.isPlaying ? '일시정지' : (_ended(v) ? '처음부터' : '재생'),
              padding: EdgeInsets.zero,
              iconSize: Look.mediaDotIcon,
              style: IconButton.styleFrom(
                shape: const CircleBorder(),
                backgroundColor: video ? Colors.white : null,
                foregroundColor: video ? Colors.black : null,
              ),
              icon: Icon(
                v.isPlaying
                    ? Icons.pause_rounded
                    : (_ended(v) ? Icons.replay : Icons.play_arrow_rounded),
              ),
            ),
          ),
          const SizedBox(width: Look.fieldGap),
          SizedBox(
            width: Look.mediaClockW,
            child: Text(clockLabel(shown), style: clock),
          ),
          Expanded(
            child: SliderTheme(
              data: SliderTheme.of(context).copyWith(
                activeTrackColor: video ? Colors.white : scheme.primary,
                inactiveTrackColor: (video ? Colors.white : scheme.onSurface)
                    .withValues(alpha: 0.24),
                thumbColor: video ? Colors.white : scheme.primary,
                overlayShape: const RoundSliderOverlayShape(
                  overlayRadius: Look.tap / 2,
                ),
                padding: const EdgeInsets.symmetric(horizontal: Look.fieldGap),
              ),
              child: Slider(
                value: total > 0 ? at : 0,
                max: total > 0 ? total : 1,
                semanticFormatterCallback: (s) =>
                    clockLabel(Duration(milliseconds: (s * 1000).round())),
                onChangeStart: !ready || c == null
                    ? null
                    : (_) {
                        _resume = c.value.isPlaying;
                        c.pause();
                      },
                onChanged: !ready || total <= 0
                    ? null
                    : (s) => setState(() => _drag = s),
                onChangeEnd: !ready || c == null
                    ? null
                    : (s) async {
                        await c.seekTo(
                          Duration(milliseconds: (s * 1000).round()),
                        );
                        if (!mounted) return;
                        setState(() => _drag = null);
                        if (_resume && identical(c, _c)) await c.play();
                      },
              ),
            ),
          ),
          SizedBox(
            width: Look.mediaClockW,
            child: Text(
              total > 0 ? clockLabel(length) : '',
              style: clock,
              textAlign: TextAlign.end,
            ),
          ),
        ],
      ),
    );
  }
}

class _AudioArt extends StatelessWidget {
  const _AudioArt({
    required this.media,
    required this.dim,
    required this.buffering,
  });
  final InlineMedia media;
  final Color dim;

  /// 틀었는데 다음 구간을 기다리는 중 — 시계가 멎은 까닭을 그림 자리에서 보인다.
  final bool buffering;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Center(
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: Look.pagePad),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            SizedBox.square(
              dimension: Look.twinsSmall,
              child: DecoratedBox(
                decoration: BoxDecoration(
                  color: scheme.primary.withValues(alpha: 0.18),
                  shape: BoxShape.circle,
                ),
                child: buffering
                    ? Padding(
                        padding: const EdgeInsets.all(Look.twinsSmall / 4),
                        child: CircularProgressIndicator(color: scheme.primary),
                      )
                    : Icon(
                        Icons.graphic_eq,
                        size: Look.twinsSmall / 2,
                        color: scheme.primary,
                      ),
              ),
            ),
            const SizedBox(height: Look.groupGap),
            Text(
              media.title,
              textAlign: TextAlign.center,
              maxLines: 2,
              overflow: TextOverflow.ellipsis,
              style: TextStyle(
                fontSize: Look.group,
                fontWeight: FontWeight.w600,
                color: scheme.onSurface,
              ),
            ),
            const SizedBox(height: Look.rowGap * 2),
            Text(
              media.caption,
              style: TextStyle(fontSize: Look.sub, color: dim),
            ),
          ],
        ),
      ),
    );
  }
}

class _Failed extends StatelessWidget {
  const _Failed({
    required this.ink,
    required this.dim,
    required this.reason,
    required this.onRetry,
    this.onPreview,
  });
  final Color ink;
  final Color dim;

  /// 원본에서 받다 난 까닭. 없으면 이 기기의 형식 문제로 본다.
  final String? reason;
  final VoidCallback onRetry;
  final VoidCallback? onPreview;

  @override
  Widget build(BuildContext context) => Center(
    child: Padding(
      padding: const EdgeInsets.symmetric(horizontal: Look.pagePad),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(
            reason ?? '이 파일은 폰에서 재생하지 못했어요',
            textAlign: TextAlign.center,
            style: TextStyle(fontSize: Look.body, color: ink),
          ),
          const SizedBox(height: Look.rowGap * 2),
          Text(
            reason != null
                ? '원본 기기가 켜져 있는지 보고 다시 시도해 주세요'
                : '형식을 이 기기가 지원하지 않을 수 있어요',
            textAlign: TextAlign.center,
            style: TextStyle(fontSize: Look.sub, color: dim),
          ),
          const SizedBox(height: Look.groupGap),
          Wrap(
            alignment: WrapAlignment.center,
            spacing: Look.fieldGap,
            runSpacing: Look.fieldGap,
            children: [
              FilledButton.tonal(
                onPressed: onRetry,
                child: const Text('다시 시도'),
              ),
              if (onPreview != null)
                FilledButton.tonal(
                  onPressed: onPreview,
                  child: const Text('미리보기로 열기'),
                ),
            ],
          ),
        ],
      ),
    ),
  );
}
