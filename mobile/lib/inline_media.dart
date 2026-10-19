import 'dart:async';
import 'dart:convert';
import 'dart:math' show min;
import 'dart:typed_data';

import 'package:crypto/crypto.dart';
import 'package:flutter/material.dart';
import 'package:http/http.dart' as http;
import 'package:video_player/video_player.dart';

import 'inline_media_store.dart';
import 'look.dart';
import 'screens/media_player.dart';
import 'server.dart';

/// 클라이언트가 한 번에 받는 조각. 서버 상한은 8MiB 지만 관문 너머 한 요청이 오래 걸리면 진행이 멎어 보인다.
const inlineMediaChunk = 4 * 1024 * 1024;
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

/// 받은 파일을 두는 곳(`inline_media_store_io.dart`).
abstract interface class MediaStore {
  /// 다 받아 대조까지 끝난 파일 경로, 없으면 null.
  Future<String?> finished(InlineMedia m);

  /// 받는 중인 조각 — 전에 받다 만 것이 있으면 그 뒤부터 잇는다.
  Future<MediaPart> part(InlineMedia m);
  Future<void> remember(String key, InlineMedia m, Uint8List? poster);
  Future<(InlineMedia, Uint8List?)?> recall(String key, String id);
}

abstract interface class MediaPart {
  int get length;
  Future<void> add(Uint8List bytes);

  /// 크기·sha256 을 정보와 대조해 맞으면 완성 파일로 옮기고 그 경로. 틀리면 조각을 지우고 던진다.
  Future<String> finish();

  /// 지운다(사람이 취소).
  Future<void> discard();

  /// 남겨 둔다(실패 — 「다시 시도」가 이어 받는다).
  Future<void> close();
}

class MediaIntegrityException implements Exception {
  const MediaIntegrityException();
}

enum MediaPhase { idle, downloading, ready, failed }

typedef VideoPlayerFactory = VideoPlayerController Function(String path);

/// 한 파일 받기. 같은 내용(sha256)은 카드가 몇 개든 하나만 돈다 — 카드가 스크롤로 사라졌다 돌아와도 진행을 잇는다.
class MediaFetch extends ChangeNotifier {
  MediaFetch(this.media);
  final InlineMedia media;
  MediaPhase phase = MediaPhase.idle;
  int received = 0;
  String? error;
  String? path;
  Completer<void>? _abort;
  Future<String?>? _running;

  bool get busy => phase == MediaPhase.downloading;
  double get progress => media.size == 0 ? 0 : received / media.size;

  /// 받아서 경로를 준다. 취소·실패면 null(까닭은 [error]).
  Future<String?> run({
    required Server server,
    required String id,
    required String? machine,
    required MediaStore store,
    int chunk = inlineMediaChunk,
  }) => _running ??= _run(
    server,
    id,
    machine,
    store,
    chunk,
  ).whenComplete(() => _running = null);

  void cancel() {
    final abort = _abort;
    if (abort != null && !abort.isCompleted) abort.complete();
  }

  void _set(MediaPhase p, {String? error}) {
    phase = p;
    this.error = error;
    notifyListeners();
  }

  Future<String?> _run(
    Server server,
    String id,
    String? machine,
    MediaStore store,
    int chunk,
  ) async {
    final done = await store.finished(media);
    if (done != null) {
      path = done;
      received = media.size;
      _set(MediaPhase.ready);
      return done;
    }
    final abort = _abort = Completer<void>();
    _set(MediaPhase.downloading);
    MediaPart? part;
    try {
      part = await store.part(media);
      received = part.length;
      notifyListeners();
      while (part.length < media.size) {
        if (abort.isCompleted) throw const _Canceled();
        final bytes = await server.inlineMediaChunk(
          id,
          offset: part.length,
          length: min(chunk, media.size - part.length),
          machine: machine,
          abort: abort.future,
        );
        if (abort.isCompleted) throw const _Canceled();
        await part.add(bytes);
        received = part.length;
        notifyListeners();
      }
      final p = await part.finish();
      part = null;
      path = p;
      _set(MediaPhase.ready);
      return p;
    } on _Canceled {
      await part?.discard();
    } on http.RequestAbortedException {
      await part?.discard();
    } on MediaIntegrityException {
      part = null;
      received = 0;
      _set(MediaPhase.failed, error: '받은 파일이 원본과 달라요');
      return null;
    } on ServerException catch (e) {
      await part?.close();
      _set(MediaPhase.failed, error: e.message);
      return null;
    } catch (_) {
      await part?.close();
      _set(MediaPhase.failed, error: '미디어를 받지 못했어요');
      return null;
    } finally {
      _abort = null;
    }
    received = 0;
    _set(MediaPhase.idle);
    return null;
  }
}

class _Canceled implements Exception {
  const _Canceled();
}

/// 앱 전체에서 하나 — 저장소와 받는 중인 것들. 시험은 [debugReset] 으로 갈아 끼운다.
class InlineMediaHub {
  InlineMediaHub({
    Future<MediaStore?> Function()? store,
    this.chunk = inlineMediaChunk,
    VideoPlayerFactory? player,
  }) : _openStore = store ?? openMediaStore,
       player = player ?? mediaController;

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
  final _fetches = <String, MediaFetch>{};

  MediaFetch fetch(InlineMedia m) =>
      _fetches.putIfAbsent(m.sha256, () => MediaFetch(m));

  MediaFetch? running(InlineMedia m) => _fetches[m.sha256];
}

class _Loaded {
  const _Loaded(this.media, this.poster);
  final InlineMedia media;
  final Uint8List? poster;
}

/// 말풍선 속 소리·영상 카드. 블록이 보여도 아무것도 받지 않는다 — 정보·포스터만 받고, 누르면 그때 파일을 받아
/// 앱 안 플레이어로 연다.
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
  MediaFetch? _fetch;

  /// 카드가 가리키는 대상(연결·id·기기)이 바뀔 때마다 오른다. 기다리던 정보·받기가 끝났을 때 이 값이 다르면
  /// 옛 카드의 답이라 버린다 — 새 기기의 카드에 옛 진행을 붙이거나 옛 미디어를 열지 않는다.
  int _gen = 0;

  InlineMediaHub get _hub => InlineMediaHub.instance;

  Future<_Loaded> _load() async {
    final gen = _gen;
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
      _attach(gen, old.$1);
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
    _attach(gen, media);
    return _Loaded(media, poster);
  }

  void _attach(int gen, InlineMedia m) {
    if (gen != _gen || !mounted) return;
    final fetch = _hub.running(m);
    if (fetch == null || identical(fetch, _fetch)) return;
    _fetch?.removeListener(_changed);
    _fetch = fetch..addListener(_changed);
  }

  void _changed() {
    if (mounted) setState(() {});
  }

  @override
  void didUpdateWidget(InlineMediaCard oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.server != widget.server ||
        oldWidget.id != widget.id ||
        oldWidget.machine != widget.machine) {
      _gen++;
      _fetch?.removeListener(_changed);
      _fetch = null;
      _info = _load();
    }
  }

  @override
  void dispose() {
    _fetch?.removeListener(_changed);
    super.dispose();
  }

  Future<void> _open(InlineMedia media) async {
    final gen = _gen;
    final server = widget.server, id = widget.id, machine = widget.machine;
    final messenger = ScaffoldMessenger.maybeOf(context);
    final store = await _hub.store;
    if (gen != _gen || !mounted) return;
    if (store == null) {
      messenger?.showSnackBar(
        const SnackBar(content: Text('이 화면에서는 재생할 수 없어요. 앱에서 열어 주세요')),
      );
      return;
    }
    final fetch = _hub.fetch(media);
    _attach(gen, media);
    final path = await fetch.run(
      server: server,
      id: id,
      machine: machine,
      store: store,
      chunk: _hub.chunk,
    );
    // 받는 동안 다른 화면으로 갔거나 카드가 다른 미디어로 바뀌었으면 띄우지 않는다 — 받은 파일은 캐시에
    // 남아 다음 누름에 바로 열린다.
    if (path == null ||
        gen != _gen ||
        !mounted ||
        !(ModalRoute.of(context)?.isCurrent ?? true)) {
      return;
    }
    await Navigator.of(
      context,
    ).push(MediaPlayerScreen.route(media: media, path: path));
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
      final fetch = _fetch;
      final onTap = fetch?.busy ?? false ? null : () => _open(media);
      return media.isVideo
          ? _VideoCard(
              media: media,
              poster: loaded.poster,
              fetch: fetch,
              onTap: onTap,
            )
          : _AudioCard(media: media, fetch: fetch, onTap: onTap);
    },
  );
}

/// 받는 중·실패·평소의 아래 줄 — 소리 카드의 둘째 줄, 영상 카드의 포스터 아래.
class _FetchLine extends StatelessWidget {
  const _FetchLine({required this.media, required this.fetch});
  final InlineMedia media;
  final MediaFetch? fetch;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final sub = TextStyle(
      fontSize: Look.sub,
      height: 1.3,
      color: scheme.onSurfaceVariant,
    );
    final f = fetch;
    if (f != null && f.busy) {
      return Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          ClipRRect(
            borderRadius: BorderRadius.circular(Look.mediaBar),
            child: LinearProgressIndicator(
              value: f.received == 0 ? null : f.progress,
              minHeight: Look.mediaBar,
              backgroundColor: scheme.onSurface.withValues(alpha: 0.12),
            ),
          ),
          const SizedBox(height: Look.rowGap * 2),
          Text(
            '받는 중 ${sizeLabel(f.received)} / ${sizeLabel(media.size)}',
            style: sub.copyWith(
              fontFeatures: const [FontFeature.tabularFigures()],
            ),
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
          ),
        ],
      );
    }
    if (f != null && f.phase == MediaPhase.failed) {
      return Text(
        '${f.error ?? '받지 못했어요'} · 눌러서 다시',
        style: sub.copyWith(color: scheme.error),
        maxLines: 2,
        overflow: TextOverflow.ellipsis,
      );
    }
    return Text(
      media.caption,
      style: sub,
      maxLines: 1,
      overflow: TextOverflow.ellipsis,
    );
  }
}

class _CancelButton extends StatelessWidget {
  const _CancelButton({required this.fetch});
  final MediaFetch fetch;

  @override
  Widget build(BuildContext context) => IconButton(
    onPressed: fetch.cancel,
    tooltip: '받기 취소',
    constraints: const BoxConstraints.tightFor(
      width: Look.tap,
      height: Look.tap,
    ),
    iconSize: Look.iconSize,
    icon: const Icon(Icons.close),
  );
}

class _AudioCard extends StatelessWidget {
  const _AudioCard({required this.media, required this.fetch, this.onTap});
  final InlineMedia media;
  final MediaFetch? fetch;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final f = fetch;
    final busy = f?.busy ?? false;
    return Semantics(
      button: true,
      label: '소리 ${media.title}, ${media.caption}',
      excludeSemantics: !busy,
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
                  _PlayDot(fetch: f),
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
                          _FetchLine(media: media, fetch: f),
                        ],
                      ),
                    ),
                  ),
                  if (busy) _CancelButton(fetch: f!),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// 소리 카드 왼쪽의 채운 원 — 받는 중이면 내려받기 표, 받다 실패했으면 다시.
class _PlayDot extends StatelessWidget {
  const _PlayDot({required this.fetch});
  final MediaFetch? fetch;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final icon = switch (fetch?.phase) {
      MediaPhase.downloading => Icons.downloading,
      MediaPhase.failed => Icons.refresh,
      _ => Icons.play_arrow_rounded,
    };
    return SizedBox.square(
      dimension: Look.tap,
      child: DecoratedBox(
        decoration: BoxDecoration(
          color: scheme.primary,
          shape: BoxShape.circle,
        ),
        child: Icon(icon, size: Look.mediaDotIcon, color: scheme.onPrimary),
      ),
    );
  }
}

class _VideoCard extends StatelessWidget {
  const _VideoCard({
    required this.media,
    required this.poster,
    required this.fetch,
    this.onTap,
  });
  final InlineMedia media;
  final Uint8List? poster;
  final MediaFetch? fetch;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final f = fetch;
    final busy = f?.busy ?? false;
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
          excludeSemantics: !busy,
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
                                child: busy
                                    ? Padding(
                                        padding: const EdgeInsets.all(14),
                                        child: CircularProgressIndicator(
                                          value: f!.received == 0
                                              ? null
                                              : f.progress,
                                          strokeWidth: 3,
                                          color: Colors.white,
                                          backgroundColor: Colors.white24,
                                        ),
                                      )
                                    : Icon(
                                        f?.phase == MediaPhase.failed
                                            ? Icons.refresh
                                            : Icons.play_arrow_rounded,
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
                                    _FetchLine(media: media, fetch: f),
                                  ],
                                ),
                              ),
                            ),
                            if (busy)
                              _CancelButton(fetch: f!)
                            else
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
