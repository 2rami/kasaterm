import 'package:flutter/foundation.dart' show kIsWeb;
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:video_player/video_player.dart';

import '../inline_media.dart';
import '../look.dart';

/// 대화 카드에서 연 소리·영상. 받은 캐시 파일을 앱 안에서 재생한다 — 바깥 앱·새 창으로 넘기지 않는다.
/// 닫으면 플레이어를 버린다(AVPlayer 해제). 앱이 뒤로 가면 video_player 가 멈추고 돌아오면 잇는다.
class MediaPlayerScreen extends StatefulWidget {
  const MediaPlayerScreen({super.key, required this.media, required this.path});
  final InlineMedia media;
  final String path;

  static Route<void> route({
    required InlineMedia media,
    required String path,
  }) => MaterialPageRoute<void>(
    fullscreenDialog: true,
    settings: const RouteSettings(name: 'inline-media'),
    builder: (_) => MediaPlayerScreen(media: media, path: path),
  );

  @override
  State<MediaPlayerScreen> createState() => _MediaPlayerScreenState();
}

class _MediaPlayerScreenState extends State<MediaPlayerScreen> {
  late VideoPlayerController _c;
  late Future<void> _ready;

  /// 막대를 끄는 동안의 자리(초). 놓을 때 한 번만 찾아간다.
  double? _drag;
  bool _resume = false;

  @override
  void initState() {
    super.initState();
    _open();
  }

  void _open() {
    _c = InlineMediaHub.instance.player(widget.path);
    _ready = _start(_c);
  }

  /// 카드를 누른 것이 곧 재생하라는 뜻이라 열리면 바로 튼다.
  Future<void> _start(VideoPlayerController c) async {
    await c.initialize();
    if (mounted && identical(c, _c)) await c.play();
  }

  void _retry() {
    final old = _c;
    setState(_open);
    // 이번 프레임의 빌더가 새 플레이어로 갈아 붙은 뒤에 버린다 — 빌더가 버린 플레이어를 듣고 있지 않게.
    WidgetsBinding.instance.addPostFrameCallback((_) => old.dispose());
  }

  @override
  void dispose() {
    _c.dispose();
    super.dispose();
  }

  bool _ended(VideoPlayerValue v) =>
      v.isCompleted ||
      (v.duration > Duration.zero && v.position >= v.duration && !v.isPlaying);

  Future<void> _toggle() async {
    final v = _c.value;
    if (!v.isInitialized) return;
    if (v.isPlaying) return _c.pause();
    if (_ended(v)) await _c.seekTo(Duration.zero);
    await _c.play();
  }

  Future<void> _preview() async {
    final messenger = ScaffoldMessenger.of(context);
    try {
      await const MethodChannel(
        'kasaterm/file_preview',
      ).invokeMethod<void>('previewPath', {'path': widget.path});
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
                builder: (context, snap) =>
                    ValueListenableBuilder<VideoPlayerValue>(
                      valueListenable: _c,
                      builder: (context, v, _) {
                        if (snap.hasError || v.hasError) {
                          return _Failed(
                            ink: ink,
                            dim: dim,
                            onRetry: _retry,
                            onPreview: kIsWeb ? null : _preview,
                          );
                        }
                        if (!v.isInitialized) {
                          return Center(
                            child: CircularProgressIndicator(
                              color: video ? Colors.white : null,
                            ),
                          );
                        }
                        return video
                            ? _stage(v)
                            : _AudioArt(media: widget.media, dim: dim);
                      },
                    ),
              ),
            ),
            ValueListenableBuilder<VideoPlayerValue>(
              valueListenable: _c,
              builder: (context, v, _) => _controls(v, ink, dim),
            ),
          ],
        ),
      ),
    );
  }

  Widget _stage(VideoPlayerValue v) {
    final aspect = v.aspectRatio > 0 ? v.aspectRatio : 16 / 9;
    return GestureDetector(
      behavior: HitTestBehavior.opaque,
      onTap: _toggle,
      child: Stack(
        alignment: Alignment.center,
        children: [
          Center(
            child: AspectRatio(aspectRatio: aspect, child: VideoPlayer(_c)),
          ),
          if (v.isBuffering && v.isPlaying)
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

  Widget _controls(VideoPlayerValue v, Color ink, Color dim) {
    final ready = v.isInitialized && !v.hasError;
    final total = v.duration.inMilliseconds / 1000;
    final at = (_drag ?? v.position.inMilliseconds / 1000).clamp(
      0.0,
      total > 0 ? total : 0.0,
    );
    // AVPlayer 길이는 2.99초처럼 끝이 모자라게 온다 — 길이는 반올림해 카드의 「0:03」과 맞추고, 끝까지 간
    // 뒤에는 지난 시간도 길이와 같게 보인다.
    final length = Duration(seconds: (total + 0.5).floor());
    final shown = _drag == null && _ended(v)
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
                value: at,
                max: total > 0 ? total : 1,
                semanticFormatterCallback: (s) =>
                    clockLabel(Duration(milliseconds: (s * 1000).round())),
                onChangeStart: !ready
                    ? null
                    : (_) {
                        _resume = _c.value.isPlaying;
                        _c.pause();
                      },
                onChanged: !ready || total <= 0
                    ? null
                    : (s) => setState(() => _drag = s),
                onChangeEnd: !ready
                    ? null
                    : (s) async {
                        await _c.seekTo(
                          Duration(milliseconds: (s * 1000).round()),
                        );
                        if (!mounted) return;
                        setState(() => _drag = null);
                        if (_resume) await _c.play();
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
  const _AudioArt({required this.media, required this.dim});
  final InlineMedia media;
  final Color dim;

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
                child: Icon(
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
    required this.onRetry,
    this.onPreview,
  });
  final Color ink;
  final Color dim;
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
            '이 파일은 폰에서 재생하지 못했어요',
            textAlign: TextAlign.center,
            style: TextStyle(fontSize: Look.body, color: ink),
          ),
          const SizedBox(height: Look.rowGap * 2),
          Text(
            '형식을 이 기기가 지원하지 않을 수 있어요',
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
