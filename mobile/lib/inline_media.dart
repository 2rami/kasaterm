import 'dart:convert';
import 'dart:math' show min;
import 'dart:typed_data';

import 'package:crypto/crypto.dart';
import 'package:flutter/material.dart';
import 'package:video_player/video_player.dart';

import 'inline_media_store.dart';
import 'look.dart';
import 'screens/media_player.dart';
import 'server.dart';

/// 재생 중 원본 기기에 한 번에 묻는 구간. 크면 첫 화면·탐색이 늦고, 작으면 관문 왕복이 늘어 끊긴다.
const inlineMediaChunk = 512 * 1024;
const inlineMediaMaxSize = 1024 * 1024 * 1024;

enum InlineMediaKind { audio, video }

/// `GET term/inline-media?id=` 의 답 — PC 가 등록한 소리·영상 하나.
@immutable
class InlineMedia {
  const InlineMedia({
    required this.id,
    required this.kind,
    required this.name,
    required this.mime,
    required this.size,
    required this.sha256,
    this.durationMs,
    this.width,
    this.height,
    this.poster = false,
  });
  final String id;
  final InlineMediaKind kind;
  final String name;
  final String mime;
  final int size;
  final String sha256;
  final int? durationMs;
  final int? width;
  final int? height;
  final bool poster;

  static final _hex64 = RegExp(r'^[0-9a-f]{64}$');

  /// 답을 그대로 믿지 않는다 — 다른 id·크기 상한·해시 모양이 어긋나면 null.
  static InlineMedia? fromJson(Object? raw, {required String id}) {
    if (raw is! Map || raw['version'] != 1 || raw['id'] != id) return null;
    final kind = switch (raw['kind']) {
      'audio' => InlineMediaKind.audio,
      'video' => InlineMediaKind.video,
      _ => null,
    };
    final size = raw['size'];
    final sha = raw['sha256'];
    final name = raw['name'];
    final mime = raw['mime'];
    if (kind == null ||
        size is! int ||
        size <= 0 ||
        size > inlineMediaMaxSize ||
        sha is! String ||
        !_hex64.hasMatch(sha) ||
        name is! String ||
        mime is! String) {
      return null;
    }
    int? positive(Object? v) => v is int && v > 0 ? v : null;
    return InlineMedia(
      id: id,
      kind: kind,
      name: name,
      mime: mime,
      size: size,
      sha256: sha,
      durationMs: positive(raw['duration_ms']),
      width: positive(raw['width']),
      height: positive(raw['height']),
      poster: raw['poster'] == true,
    );
  }

  Map<String, Object?> toJson() => {
    'version': 1,
    'id': id,
    'kind': kind.name,
    'name': name,
    'mime': mime,
    'size': size,
    'sha256': sha256,
    'duration_ms': durationMs,
    'width': width,
    'height': height,
    'poster': poster,
  };

  bool get isVideo => kind == InlineMediaKind.video;

  /// 화면에 보일 이름 — 경로가 섞여 와도 마지막 조각만.
  String get title {
    final leaf = name.split(RegExp(r'[/\\]')).last.trim();
    return leaf.isEmpty ? (isVideo ? '영상' : '소리') : leaf;
  }

  /// PC 등록소가 받는 확장자(`kasa_socket::inline_media`).
  static const _known = {
    'mp3', 'm4a', 'm4b', 'aac', 'wav', 'aif', 'aiff', 'caf', 'flac', //
    'mp4', 'm4v', 'mov',
  };

  /// 캐시 파일 확장자. AVPlayer 는 파일 확장자로 컨테이너를 고르니 이름 쪽이 모르는 것이면 mime 으로 정한다.
  String get extension {
    final dot = title.lastIndexOf('.');
    final ext = dot < 0 ? '' : title.substring(dot + 1).toLowerCase();
    if (_known.contains(ext)) return ext;
    return switch (mime.toLowerCase()) {
      'video/mp4' => 'mp4',
      'video/quicktime' => 'mov',
      'video/x-m4v' => 'm4v',
      'audio/mpeg' => 'mp3',
      'audio/mp4' || 'audio/x-m4a' || 'audio/m4a' => 'm4a',
      'audio/x-m4b' => 'm4b',
      'audio/aac' => 'aac',
      'audio/wav' || 'audio/x-wav' || 'audio/wave' => 'wav',
      'audio/flac' || 'audio/x-flac' => 'flac',
      'audio/aiff' || 'audio/x-aiff' => 'aiff',
      'audio/x-caf' => 'caf',
      _ => isVideo ? 'mp4' : 'm4a',
    };
  }

  /// 루프백 창구가 싣는 Content-Type. http 로 읽는 AVPlayer 는 이것으로 컨테이너를 고르니 등록소 mime 이
  /// 엉뚱해도 확장자로 맞춘다.
  String get streamMime => switch (extension) {
    'mov' => 'video/quicktime',
    'm4v' => 'video/x-m4v',
    'mp3' => 'audio/mpeg',
    'm4a' || 'm4b' => 'audio/mp4',
    'aac' => 'audio/aac',
    'wav' => 'audio/wav',
    'aif' || 'aiff' => 'audio/aiff',
    'caf' => 'audio/x-caf',
    'flac' => 'audio/flac',
    _ => 'video/mp4',
  };

  /// 카드 둘째 줄 — 「3:21 · 4.2MB」.
  String get caption => [
    if (durationMs case final ms?) clockLabel(Duration(milliseconds: ms)),
    sizeLabel(size),
  ].join(' · ');
}

String clockLabel(Duration d) {
  final s = d.inSeconds;
  final mm = (s ~/ 60 % 60).toString().padLeft(2, '0');
  final ss = (s % 60).toString().padLeft(2, '0');
  return s >= 3600 ? '${s ~/ 3600}:$mm:$ss' : '${s ~/ 60}:$ss';
}

String sizeLabel(int bytes) {
  if (bytes < 1024 * 1024) return '${(bytes / 1024).ceil()}KB';
  final mb = bytes / (1024 * 1024);
  if (mb < 1024) return '${mb.toStringAsFixed(mb < 10 ? 1 : 0)}MB';
  return '${(mb / 1024).toStringAsFixed(1)}GB';
}

/// 카드 정보를 기억하는 열쇠. 같은 id 라도 다른 연결·다른 기기면 다른 미디어다. 연결 범위(옛 주소의 slug)는
/// 자격이라 해시로만 남긴다.
String inlineMediaKey(Server server, String? machine, String id) => sha256
    .convert(utf8.encode('${server.resumeScope}|${machine ?? ''}|$id'))
    .toString()
    .substring(0, 32);

/// 다 받아 대조까지 끝난 파일과 카드 정보를 두는 곳(`inline_media_store_io.dart`). 새 파일은 더 받아 두지 않고
/// 전 판이 받아 둔 완성본만 다시 쓴다.
abstract interface class MediaStore {
  /// 다 받아 대조까지 끝난 파일 경로, 없으면 null.
  Future<String?> finished(InlineMedia m);
  Future<void> remember(String key, InlineMedia m, Uint8List? poster);
  Future<(InlineMedia, Uint8List?)?> recall(String key, String id);
}

/// 플레이어가 여는 곳 — 이 폰에 있는 완성 파일이거나, 원본 기기에서 필요한 구간만 받아 넘기는 루프백 창구.
abstract interface class MediaLink {
  Uri get uri;

  /// 이 폰의 파일 경로 — 미리보기(Quick Look)로 넘길 수 있다. 창구면 null.
  String? get path;

  /// 원본에서 조각을 받다 난 까닭. 재생이 실패하면 형식 탓 대신 이것을 보인다.
  String? get error;

  /// 받던 요청·연결을 모두 끊는다.
  Future<void> close();
}

class _FileLink implements MediaLink {
  _FileLink(String this.path);
  @override
  final String? path;
  @override
  Uri get uri => Uri.file(path!);
  @override
  String? get error => null;
  @override
  Future<void> close() async {}
}

typedef VideoPlayerFactory = VideoPlayerController Function(Uri uri);

typedef MediaBridgeOpener =
    Future<MediaLink> Function({
      required Server server,
      required String id,
      required String? machine,
      required InlineMedia media,
      required int chunk,
    });

/// 앱 전체에서 하나 — 저장소·플레이어·창구를 여는 길. 시험은 [debugReset] 으로 갈아 끼운다.
class InlineMediaHub {
  InlineMediaHub({
    Future<MediaStore?> Function()? store,
    this.chunk = inlineMediaChunk,
    VideoPlayerFactory? player,
    MediaBridgeOpener? bridge,
  }) : _openStore = store ?? openMediaStore,
       player = player ?? mediaController,
       _bridge = bridge ?? openMediaBridge;

  static InlineMediaHub instance = InlineMediaHub();

  @visibleForTesting
  static void debugReset(InlineMediaHub hub) => instance = hub;

  final Future<MediaStore?> Function() _openStore;
  late final Future<MediaStore?> store = _openStore().then<MediaStore?>(
    (s) => s,
    onError: (_) => null,
  );
  final int chunk;
  final VideoPlayerFactory player;
  final MediaBridgeOpener _bridge;

  /// 받아 둔 완성본이 있으면 그 파일, 없으면 이 연결·기기·id·정보에 고정한 창구.
  Future<MediaLink> open({
    required Server server,
    required String id,
    required String? machine,
    required InlineMedia media,
  }) async {
    final done = await (await store)?.finished(media);
    if (done != null) return _FileLink(done);
    return _bridge(
      server: server,
      id: id,
      machine: machine,
      media: media,
      chunk: chunk,
    );
  }
}

class _Loaded {
  const _Loaded(this.media, this.poster);
  final InlineMedia media;
  final Uint8List? poster;
}

/// 말풍선 속 소리·영상 카드. 블록이 보여도 정보·포스터만 받는다 — 누르면 곧바로 앱 안 플레이어를 열고, 플레이어가
/// 재생에 필요한 구간만 원본 기기에서 받는다.
class InlineMediaCard extends StatefulWidget {
  const InlineMediaCard({
    super.key,
    required this.server,
    required this.id,
    required this.rows,
    this.machine,
  });
  final Server server;
  final String id;
  final int rows;
  final String? machine;

  @override
  State<InlineMediaCard> createState() => _InlineMediaCardState();
}

class _InlineMediaCardState extends State<InlineMediaCard> {
  late Future<_Loaded> _info = _load();

  InlineMediaHub get _hub => InlineMediaHub.instance;

  Future<_Loaded> _load() async {
    final server = widget.server, id = widget.id, machine = widget.machine;
    final store = await _hub.store;
    final key = inlineMediaKey(server, machine, id);
    final InlineMedia media;
    try {
      final raw = await server.inlineMediaInfo(id, machine: machine);
      media =
          InlineMedia.fromJson(raw, id: id) ??
          (throw const ServerException('미디어 정보가 올바르지 않아요'));
    } on ServerException {
      // 원본 기기가 꺼졌거나 지웠어도 전에 본 카드면 기억한 정보로 선다 — 받아 둔 파일은 그대로 재생된다.
      final old = await store?.recall(key, id);
      if (old == null) rethrow;
      return _Loaded(old.$1, old.$2);
    }
    Uint8List? poster;
    if (media.poster) {
      try {
        poster = await server.inlineMediaPoster(id, machine: machine);
      } on ServerException {
        poster = null;
      }
    }
    try {
      await store?.remember(key, media, poster);
    } catch (_) {}
    return _Loaded(media, poster);
  }

  @override
  void didUpdateWidget(InlineMediaCard oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.server != widget.server ||
        oldWidget.id != widget.id ||
        oldWidget.machine != widget.machine) {
      _info = _load();
    }
  }

  void _open(InlineMedia media) {
    if (!mediaPlayable) {
      ScaffoldMessenger.maybeOf(context)?.showSnackBar(
        const SnackBar(content: Text('이 화면에서는 재생할 수 없어요. 앱에서 열어 주세요')),
      );
      return;
    }
    // 누른 순간의 연결·기기·id 를 붙잡아 넘긴다 — 열린 뒤 카드가 다른 미디어로 바뀌어도 플레이어는 이것만 묻는다.
    final hub = _hub, server = widget.server, id = widget.id;
    final machine = widget.machine;
    Navigator.of(context).push(
      MediaPlayerScreen.route(
        media: media,
        open: () =>
            hub.open(server: server, id: id, machine: machine, media: media),
      ),
    );
  }

  @override
  Widget build(BuildContext context) => FutureBuilder<_Loaded>(
    future: _info,
    builder: (context, snap) {
      // future 를 갈아도 FutureBuilder 는 옛 답(data·error)을 들고 있다 — 끝난 지금 것만 그린다.
      if (snap.connectionState != ConnectionState.done) {
        return _MediaPending(video: widget.rows > 4);
      }
      if (snap.hasError) {
        final e = snap.error;
        return _MediaFailed(
          message: e is ServerException ? e.message : '미디어를 불러오지 못했어요',
          onRetry: () => setState(() => _info = _load()),
        );
      }
      final loaded = snap.data!;
      final media = loaded.media;
      return media.isVideo
          ? _VideoCard(
              media: media,
              poster: loaded.poster,
              onTap: () => _open(media),
            )
          : _AudioCard(media: media, onTap: () => _open(media));
    },
  );
}

/// 카드의 둘째 줄 — 「3:21 · 4.2MB」.
class _Caption extends StatelessWidget {
  const _Caption({required this.media});
  final InlineMedia media;

  @override
  Widget build(BuildContext context) => Text(
    media.caption,
    style: TextStyle(
      fontSize: Look.sub,
      height: 1.3,
      color: Theme.of(context).colorScheme.onSurfaceVariant,
    ),
    maxLines: 1,
    overflow: TextOverflow.ellipsis,
  );
}

class _AudioCard extends StatelessWidget {
  const _AudioCard({required this.media, required this.onTap});
  final InlineMedia media;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Semantics(
      button: true,
      label: '소리 ${media.title}, ${media.caption}',
      excludeSemantics: true,
      child: Material(
        color: scheme.onSurface.withValues(alpha: Look.mediaTint),
        borderRadius: Look.corners,
        clipBehavior: Clip.antiAlias,
        child: InkWell(
          onTap: onTap,
          child: ConstrainedBox(
            constraints: const BoxConstraints(
              minHeight: Look.row2 + Look.mediaPad,
            ),
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: Look.mediaPad),
              child: Row(
                children: [
                  const _PlayDot(),
                  const SizedBox(width: Look.fieldGap),
                  Expanded(
                    child: Padding(
                      padding: const EdgeInsets.symmetric(
                        vertical: Look.mediaPad,
                      ),
                      child: Column(
                        mainAxisSize: MainAxisSize.min,
                        crossAxisAlignment: CrossAxisAlignment.stretch,
                        children: [
                          Text(
                            media.title,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: TextStyle(
                              fontSize: Look.body,
                              fontWeight: FontWeight.w600,
                              height: 1.3,
                              color: scheme.onSurface,
                            ),
                          ),
                          const SizedBox(height: Look.rowGap),
                          _Caption(media: media),
                        ],
                      ),
                    ),
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// 소리 카드 왼쪽의 채운 재생 원.
class _PlayDot extends StatelessWidget {
  const _PlayDot();

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return SizedBox.square(
      dimension: Look.tap,
      child: DecoratedBox(
        decoration: BoxDecoration(
          color: scheme.primary,
          shape: BoxShape.circle,
        ),
        child: Icon(
          Icons.play_arrow_rounded,
          size: Look.mediaDotIcon,
          color: scheme.onPrimary,
        ),
      ),
    );
  }
}

class _VideoCard extends StatelessWidget {
  const _VideoCard({
    required this.media,
    required this.poster,
    required this.onTap,
  });
  final InlineMedia media;
  final Uint8List? poster;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final w = media.width, h = media.height;
    final aspect = (w != null && h != null ? w / h : 16 / 9).clamp(
      Look.mediaMinAspect,
      Look.mediaMaxAspect,
    );
    return LayoutBuilder(
      builder: (context, box) {
        // 말풍선 폭에 맞추되 세로 영상은 높이 상한에서 폭을 줄인다 — 포스터가 화면을 혼자 먹지 않게.
        final room = box.maxWidth.isFinite ? box.maxWidth : Look.mediaMaxH;
        final width = min(room, Look.mediaMaxH * aspect);
        final dpr = MediaQuery.devicePixelRatioOf(context);
        return Semantics(
          button: true,
          label: '영상 ${media.title}, ${media.caption}',
          excludeSemantics: true,
          child: SizedBox(
            width: width,
            child: Material(
              color: scheme.onSurface.withValues(alpha: Look.mediaTint),
              borderRadius: Look.corners,
              clipBehavior: Clip.antiAlias,
              child: InkWell(
                onTap: onTap,
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    AspectRatio(
                      aspectRatio: aspect,
                      child: Stack(
                        fit: StackFit.expand,
                        children: [
                          ColoredBox(
                            color: Colors.black,
                            child: poster == null
                                ? null
                                : Image.memory(
                                    poster!,
                                    fit: BoxFit.contain,
                                    cacheWidth: (width * dpr).round(),
                                    gaplessPlayback: true,
                                    errorBuilder: (_, _, _) => const SizedBox(),
                                  ),
                          ),
                          Center(
                            child: SizedBox.square(
                              dimension: Look.mediaPlay,
                              child: DecoratedBox(
                                decoration: BoxDecoration(
                                  color: Colors.black.withValues(alpha: 0.55),
                                  shape: BoxShape.circle,
                                ),
                                child: const Icon(
                                  Icons.play_arrow_rounded,
                                  size: Look.mediaPlayIcon,
                                  color: Colors.white,
                                ),
                              ),
                            ),
                          ),
                        ],
                      ),
                    ),
                    ConstrainedBox(
                      constraints: const BoxConstraints(
                        minHeight: Look.tap + Look.mediaPad,
                      ),
                      child: Padding(
                        padding: const EdgeInsets.only(left: Look.fieldGap),
                        child: Row(
                          children: [
                            Expanded(
                              child: Padding(
                                padding: const EdgeInsets.symmetric(
                                  vertical: Look.mediaPad,
                                ),
                                child: Column(
                                  mainAxisSize: MainAxisSize.min,
                                  crossAxisAlignment:
                                      CrossAxisAlignment.stretch,
                                  children: [
                                    Text(
                                      media.title,
                                      maxLines: 1,
                                      overflow: TextOverflow.ellipsis,
                                      style: TextStyle(
                                        fontSize: Look.sub,
                                        fontWeight: FontWeight.w600,
                                        height: 1.3,
                                        color: scheme.onSurface,
                                      ),
                                    ),
                                    const SizedBox(height: Look.rowGap),
                                    _Caption(media: media),
                                  ],
                                ),
                              ),
                            ),
                            const SizedBox(width: Look.fieldGap),
                          ],
                        ),
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ),
        );
      },
    );
  }
}

class _MediaPending extends StatelessWidget {
  const _MediaPending({required this.video});
  final bool video;

  @override
  Widget build(BuildContext context) => SizedBox(
    height: video ? Look.mediaPendingVideo : Look.row2 + Look.mediaPad,
    child: const Center(
      child: SizedBox.square(
        dimension: 24,
        child: CircularProgressIndicator(strokeWidth: 2),
      ),
    ),
  );
}

class _MediaFailed extends StatelessWidget {
  const _MediaFailed({required this.message, required this.onRetry});
  final String message;
  final VoidCallback onRetry;

  @override
  Widget build(BuildContext context) => Row(
    mainAxisSize: MainAxisSize.min,
    children: [
      Flexible(child: Text(message)),
      TextButton(
        onPressed: onRetry,
        style: TextButton.styleFrom(
          minimumSize: const Size(Look.tap, Look.tap),
        ),
        child: const Text('다시 시도'),
      ),
    ],
  );
}
