import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:url_launcher/url_launcher.dart';

import '../look.dart';
import '../server.dart';
import '../twins_loading.dart';
import '../chat_markdown.dart' show chatMarkdownStyle;
import 'controls.dart';
import 'notes_sheet.dart' show timeAgo;

/// KASA-share — 학생들이 만든 시안·스크린샷·문서가 `<날짜>-<주제>/` 폴더로 쌓이는 곳.
/// 카사텀이 모든 기기에 같은 내용으로 맞춰 두니 폰은 주소의 기계 하나에서 읽는다.
/// [folder] 가 없으면 폴더 목록과 폴더 밖 낱장, 있으면 그 폴더 안.
class ShareScreen extends StatefulWidget {
  const ShareScreen({
    super.key,
    required this.server,
    this.folder,
    this.initial,
  });

  final Server server;
  final String? folder;

  /// 앞 화면이 이미 받은 목록 — 폴더에 들어갈 때 한 번 더 기다리지 않는다.
  final ShareListing? initial;

  @override
  State<ShareScreen> createState() => _ShareScreenState();
}

class _ShareScreenState extends State<ShareScreen> {
  ShareListing? _listing;
  String? _problem;

  @override
  void initState() {
    super.initState();
    _listing = widget.initial;
    if (_listing == null) _load();
  }

  Future<void> _load() async {
    try {
      final l = await widget.server.shareList();
      if (!mounted) return;
      setState(() {
        _listing = l;
        _problem = null;
      });
    } on ServerException catch (e) {
      if (!mounted) return;
      setState(() => _problem = e.message);
    }
  }

  ShareFolder? _folderOf(ShareListing l) {
    for (final f in l.folders) {
      if (f.name == widget.folder) return f;
    }
    return null;
  }

  @override
  Widget build(BuildContext context) {
    final l = _listing;
    final folder = l == null || widget.folder == null ? null : _folderOf(l);
    return TwinBackdrop(
      child: Scaffold(
        backgroundColor: Colors.transparent,
        appBar: AppBar(
          backgroundColor: Colors.transparent,
          title: Text(
            widget.folder == null
                ? (l?.name ?? 'KASA-share')
                : folder?.topic ?? widget.folder!,
          ),
        ),
        body: _body(context, l, folder),
      ),
    );
  }

  Widget _body(BuildContext context, ShareListing? l, ShareFolder? folder) {
    if (l == null) {
      return _Message(
        text: _problem ?? '불러오는 중',
        busy: _problem == null,
        onRetry: _problem == null ? null : _load,
        onRefresh: _load,
      );
    }
    final problem = _problem == null ? null : ErrorBand(text: '새로 못 읽었어요 · $_problem', onRetry: _load);
    if (widget.folder != null) {
      if (folder == null) {
        return _Message(
          text: '이 폴더가 이제 없어요 — 다른 기기에서 지웠을 수 있어요',
          banner: problem,
          onRefresh: _load,
        );
      }
      return _FolderView(
        server: widget.server,
        folder: folder,
        banner: problem,
        onRefresh: _load,
      );
    }
    if (l.isEmpty) {
      return _Message(
        text: '아직 올라온 결과물이 없어요.\n학생이 만든 시안·스크린샷·문서가 날짜별 폴더로 여기 쌓여요.',
        banner: problem,
        onRefresh: _load,
      );
    }
    return CustomScrollView(
      physics: twinsScroll,
      slivers: [
        twinsRefreshSliver(_load),
        SliverPadding(
          padding: const EdgeInsets.fromLTRB(Look.pagePad, 0, Look.pagePad, Look.groupGap),
          sliver: SliverList.list(
            children: [
              ?problem,
              if (l.folders.isNotEmpty)
                SettingsGroup(
                  inset: _rowInset(Look.thumbLg),
                  children: [
                    for (final f in l.folders)
                      _FolderRow(
                        server: widget.server,
                        folder: f,
                        onTap: () => Navigator.of(context).push(
                          MaterialPageRoute<void>(
                            builder: (_) => ShareScreen(
                              server: widget.server,
                              folder: f.name,
                              initial: l,
                            ),
                          ),
                        ),
                      ),
                  ],
                ),
              if (l.files.isNotEmpty)
                SettingsGroup(
                  title: l.folders.isNotEmpty ? '폴더 밖' : null,
                  inset: _rowInset(Look.thumb),
                  children: [for (final f in l.files) ShareFileRow(server: widget.server, file: f)],
                ),
            ],
          ),
        ),
      ],
    );
  }
}

class _FolderRow extends StatelessWidget {
  const _FolderRow({
    required this.server,
    required this.folder,
    required this.onTap,
  });

  final Server server;
  final ShareFolder folder;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final cover = folder.firstImage;
    final meta = [
      ?folder.day,
      '파일 ${folder.files.length}개',
      timeAgo(folder.modified),
    ].join(' · ');
    return ListTile(
          onTap: onTap,
          leading: _Thumb(
            server: server,
            file: cover,
            fallback: Icons.folder_outlined,
            size: Look.thumbLg,
          ),
          title: Text(
            folder.topic,
            maxLines: 2,
            overflow: TextOverflow.ellipsis,
          ),
          subtitle: Text(
            meta,
            style: TextStyle(color: scheme.onSurfaceVariant),
          ),
          trailing: Icon(
            Icons.chevron_right_rounded,
            color: scheme.onSurfaceVariant,
          ),
        );
  }
}

/// 판 안 줄 사이 선은 썸네일 뒤 글자선부터 — 목록 타일 안 여백 16 · 썸네일 · 사이 16.
double _rowInset(double thumb) => Look.pagePad * 2 + thumb;

/// 한 폴더 — 그림은 폭에 맞춘 격자로 먼저(고르는 일이 많다), 나머지는 줄로.
class _FolderView extends StatelessWidget {
  const _FolderView({required this.server, required this.folder, required this.onRefresh, this.banner});

  final Server server;
  final ShareFolder folder;
  final Future<void> Function() onRefresh;
  final Widget? banner;

  @override
  Widget build(BuildContext context) {
    final images = [
      for (final f in folder.files)
        if (f.kind == ShareKind.image && !f.tooLarge) f,
    ];
    final rest = [
      for (final f in folder.files)
        if (!images.contains(f)) f,
    ];
    return CustomScrollView(
      physics: twinsScroll,
      slivers: [
        twinsRefreshSliver(onRefresh),
        if (banner != null)
          SliverPadding(
            padding: const EdgeInsets.symmetric(horizontal: Look.pagePad),
            sliver: SliverToBoxAdapter(child: banner),
          ),
        if (folder.files.isEmpty)
          const SliverFillRemaining(
            hasScrollBody: false,
            child: Center(child: TwinsNotice(text: '빈 폴더예요')),
          ),
        if (images.isNotEmpty)
          SliverPadding(
            padding: const EdgeInsets.fromLTRB(Look.pagePad, Look.cardGap, Look.pagePad, 0),
            sliver: SliverGrid(
              gridDelegate: const SliverGridDelegateWithMaxCrossAxisExtent(
                maxCrossAxisExtent: 200,
                mainAxisSpacing: 12,
                crossAxisSpacing: 12,
                childAspectRatio: 0.82,
              ),
              delegate: SliverChildBuilderDelegate(
                (context, i) => _ImageTile(server: server, file: images[i]),
                childCount: images.length,
              ),
            ),
          ),
        SliverPadding(
          padding: const EdgeInsets.fromLTRB(Look.pagePad, 0, Look.pagePad, Look.groupGap),
          sliver: SliverList.list(
            children: [
              if (rest.isNotEmpty)
                SettingsGroup(
                  inset: _rowInset(Look.thumb),
                  children: [for (final f in rest) ShareFileRow(server: server, file: f)],
                ),
            ],
          ),
        ),
      ],
    );
  }
}

class _ImageTile extends StatelessWidget {
  const _ImageTile({required this.server, required this.file});

  final Server server;
  final ShareFile file;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return InkWell(
      onTap: () => openShareFile(context, server, file),
      borderRadius: Look.corners,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Expanded(
            child: Container(
              decoration: BoxDecoration(
                color: scheme.surfaceContainerHigh,
                borderRadius: Look.corners,
              ),
              clipBehavior: Clip.antiAlias,
              child: _NetImage(
                url: server.shareFileUri(file.path),
                fit: BoxFit.cover,
                cacheWidth: 400,
              ),
            ),
          ),
          const SizedBox(height: 6),
          Text(
            file.name,
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: theme.textTheme.bodySmall,
          ),
          Text(
            _meta(file),
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: theme.textTheme.labelSmall?.copyWith(
              color: scheme.onSurfaceVariant,
            ),
          ),
        ],
      ),
    );
  }
}

/// 파일 한 줄 — 그림이면 작은 썸네일, 아니면 종류 아이콘. 밖에서 여는 것은 오른쪽에 표시.
class ShareFileRow extends StatelessWidget {
  const ShareFileRow({super.key, required this.server, required this.file});

  final Server server;
  final ShareFile file;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final outside = !_inApp(file.kind);
    return ListTile(
          onTap: () => openShareFile(context, server, file),
          leading: _Thumb(
            server: server,
            file: file.kind == ShareKind.image && !file.tooLarge ? file : null,
            fallback: _kindIcon(file.kind),
            size: Look.thumb,
          ),
          title: Text(file.name, maxLines: 2, overflow: TextOverflow.ellipsis),
          subtitle: Text(
            file.tooLarge
                ? '${_meta(file)} · 32MB 가 넘어 폰에서 못 열어요'
                : _meta(file),
            style: TextStyle(
              color: file.tooLarge ? scheme.error : scheme.onSurfaceVariant,
            ),
          ),
          trailing: outside && !file.tooLarge
              ? Icon(
                  Icons.open_in_new_rounded,
                  size: 18,
                  color: scheme.onSurfaceVariant,
                )
              : null,
        );
  }
}

/// 누른 파일을 종류대로 연다 — 그림은 확대 창, 글은 앱 안, 나머지는 브라우저 창.
Future<void> openShareFile(
  BuildContext context,
  Server server,
  ShareFile file,
) async {
  final messenger = ScaffoldMessenger.of(context);
  if (file.tooLarge) {
    messenger.showSnackBar(
      const SnackBar(content: Text('32MB 가 넘어 폰에서 못 열어요')),
    );
    return;
  }
  switch (file.kind) {
    case ShareKind.image:
      await showDialog<void>(
        context: context,
        builder: (_) => _ImageDialog(server: server, file: file),
      );
    case ShareKind.markdown || ShareKind.text:
      await Navigator.of(context).push(
        MaterialPageRoute<void>(
          builder: (_) => ShareTextScreen(server: server, file: file),
        ),
      );
    default:
      // 앱 안 브라우저 창 — 주소에 든 slug 가 사파리 방문 기록에 남지 않는다.
      if (!await launchUrl(
        server.shareFileUri(file.path),
        mode: LaunchMode.inAppBrowserView,
      )) {
        messenger.showSnackBar(const SnackBar(content: Text('이 파일을 열지 못했어요')));
      }
  }
}

class _ImageDialog extends StatelessWidget {
  const _ImageDialog({required this.server, required this.file});

  final Server server;
  final ShareFile file;

  @override
  Widget build(BuildContext context) => Dialog(
    clipBehavior: Clip.antiAlias,
    child: Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        Flexible(
          child: InteractiveViewer(
            maxScale: 6,
            child: _NetImage(
              url: server.shareFileUri(file.path),
              fit: BoxFit.contain,
            ),
          ),
        ),
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 8, 8, 8),
          child: Row(
            children: [
              Expanded(
                child: Text(
                  file.name,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: Theme.of(context).textTheme.bodySmall,
                ),
              ),
              IconButton(
                tooltip: '닫기',
                onPressed: () => Navigator.of(context).pop(),
                icon: const Icon(Icons.close_rounded),
              ),
            ],
          ),
        ),
      ],
    ),
  );
}

/// 마크다운·텍스트를 앱 안에서. 마크다운 속 상대 경로 그림은 같은 폴더의 KASA-share 파일로 푼다.
class ShareTextScreen extends StatefulWidget {
  const ShareTextScreen({super.key, required this.server, required this.file});

  final Server server;
  final ShareFile file;

  @override
  State<ShareTextScreen> createState() => _ShareTextScreenState();
}

class _ShareTextScreenState extends State<ShareTextScreen> {
  String? _text;
  String? _problem;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    try {
      final t = await widget.server.shareText(widget.file.path);
      if (!mounted) return;
      setState(() {
        _text = t;
        _problem = null;
      });
    } on ServerException catch (e) {
      if (!mounted) return;
      setState(() => _problem = e.message);
    }
  }

  Uri? _resolve(Uri src) {
    if (src.hasScheme) {
      return src.isScheme('http') || src.isScheme('https') ? src : null;
    }
    final dir = widget.file.path.contains('/')
        ? widget.file.path.substring(0, widget.file.path.lastIndexOf('/') + 1)
        : '';
    final joined = Uri(
      scheme: 'share',
      path: '/$dir',
    ).resolveUri(Uri(path: src.path)).path;
    return widget.server.shareFileUri(Uri.decodeComponent(joined.substring(1)));
  }

  @override
  Widget build(BuildContext context) {
    final t = _text;
    final file = widget.file;
    return TwinBackdrop(
      child: Scaffold(
      backgroundColor: Colors.transparent,
      appBar: AppBar(
        backgroundColor: Colors.transparent,
        title: Text(file.name, maxLines: 1, overflow: TextOverflow.ellipsis),
        actions: [
          if (t != null)
            IconButton(
              tooltip: '전체 복사',
              onPressed: () {
                Clipboard.setData(ClipboardData(text: t));
                ScaffoldMessenger.of(
                  context,
                ).showSnackBar(const SnackBar(content: Text('복사했어요')));
              },
              icon: const Icon(Icons.copy_rounded),
            ),
        ],
      ),
      body: RefreshIndicator(
        onRefresh: _load,
        child: t == null
            ? _Message(
                text: _problem ?? '불러오는 중',
                busy: _problem == null,
                onRetry: _problem == null ? null : _load,
              )
            : file.kind == ShareKind.markdown
            ? Markdown(
                data: t,
                selectable: true,
                styleSheet: _shareMarkdownStyle(Theme.of(context)),
                physics: const AlwaysScrollableScrollPhysics(),
                padding: const EdgeInsets.fromLTRB(16, 12, 16, 32),
                imageBuilder: (uri, _, alt) {
                  final u = _resolve(uri);
                  return u == null
                      ? Text(alt ?? '')
                      : _NetImage(url: u, fit: BoxFit.contain);
                },
                onTapLink: (_, href, _) {
                  final u = href == null ? null : Uri.tryParse(href);
                  if (u != null &&
                      (u.isScheme('http') || u.isScheme('https'))) {
                    launchUrl(u, mode: LaunchMode.externalApplication);
                  }
                },
              )
            : SingleChildScrollView(
                physics: const AlwaysScrollableScrollPhysics(),
                padding: const EdgeInsets.fromLTRB(16, 12, 16, 32),
                child: SelectableText(
                  t,
                  style: const TextStyle(
                    fontFamily: 'TermMono',
                    fontFamilyFallback: Look.flowMonoFallback,
                    fontSize: 13,
                    height: 1.45,
                  ),
                ),
              ),
      ),
      ),
    );
  }
}

class _Thumb extends StatelessWidget {
  const _Thumb({
    required this.server,
    required this.file,
    required this.fallback,
    required this.size,
  });

  final Server server;
  final ShareFile? file;
  final IconData fallback;
  final double size;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final icon = Icon(fallback, color: scheme.onSurfaceVariant);
    final f = file;
    return Container(
      width: size,
      height: size,
      decoration: BoxDecoration(
        color: scheme.surfaceContainerHigh,
        borderRadius: Look.smallCorners,
      ),
      clipBehavior: Clip.antiAlias,
      alignment: Alignment.center,
      child: f == null
          ? icon
          : _NetImage(
              url: server.shareFileUri(f.path),
              fit: BoxFit.cover,
              cacheWidth: (size * 3).round(),
              fallback: icon,
            ),
    );
  }
}

/// 관문 너머라 큰 PNG 는 몇 초 걸린다 — 빈칸 대신 진행 표시, 실패하면 깨진 그림 아이콘.
class _NetImage extends StatelessWidget {
  const _NetImage({
    required this.url,
    required this.fit,
    this.cacheWidth,
    this.fallback,
  });

  final Uri url;
  final BoxFit fit;
  final int? cacheWidth;
  final Widget? fallback;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Image.network(
      url.toString(),
      fit: fit,
      cacheWidth: cacheWidth,
      loadingBuilder: (context, child, progress) => progress == null
          ? child
          : Center(
              child: SizedBox.square(
                dimension: 18,
                child: CircularProgressIndicator(
                  strokeWidth: 2,
                  value: progress.expectedTotalBytes == null
                      ? null
                      : progress.cumulativeBytesLoaded /
                            progress.expectedTotalBytes!,
                ),
              ),
            ),
      errorBuilder: (_, _, _) =>
          fallback ??
          Center(
            child: Icon(
              Icons.broken_image_outlined,
              color: scheme.onSurfaceVariant,
            ),
          ),
    );
  }
}

/// 목록 대신 한 마디 — 받는 중이면 깡총 쌍둥이, 아니면 서 있는 쌍둥이와 문장·다시 읽기.
class _Message extends StatelessWidget {
  const _Message({
    required this.text,
    this.busy = false,
    this.onRetry,
    this.banner,
    this.onRefresh,
  });

  final String text;
  final bool busy;
  final VoidCallback? onRetry;
  final Widget? banner;

  /// 당겨서 새로 고침 — 없으면(글 보기 화면은 바깥 당김이 맡는다) 그냥 목록.
  final Future<void> Function()? onRefresh;

  @override
  Widget build(BuildContext context) => CustomScrollView(
    physics: twinsScroll,
    slivers: [
      if (onRefresh != null) twinsRefreshSliver(onRefresh!),
      SliverPadding(
        padding: const EdgeInsets.fromLTRB(Look.pagePad, 0, Look.pagePad, Look.groupGap),
        sliver: SliverList.list(
          children: [
            ?banner,
            const SizedBox(height: Look.groupGap),
            if (busy)
              TwinsLoading(label: text, size: Look.twinsSmall)
            else
              TwinsNotice(
                text: text,
                action: onRetry == null
                    ? null
                    : OutlinedButton(onPressed: onRetry, child: const Text('다시 읽기')),
              ),
          ],
        ),
      ),
    ],
  );
}

bool _inApp(ShareKind k) => switch (k) {
  ShareKind.image || ShareKind.markdown || ShareKind.text => true,
  _ => false,
};

IconData _kindIcon(ShareKind k) => switch (k) {
  ShareKind.image => Icons.image_outlined,
  ShareKind.markdown => Icons.article_outlined,
  ShareKind.html => Icons.language_rounded,
  ShareKind.text => Icons.notes_rounded,
  ShareKind.pdf => Icons.picture_as_pdf_outlined,
  ShareKind.video => Icons.movie_outlined,
  ShareKind.audio => Icons.music_note_outlined,
  ShareKind.other => Icons.insert_drive_file_outlined,
};

String _meta(ShareFile f) => [
  formatBytes(f.size),
  if (f.origin.isNotEmpty) f.origin,
  timeAgo(f.modified),
].join(' · ');

String formatBytes(int n) {
  if (n < 1024) return '${n}B';
  if (n < 1024 * 1024) {
    return '${(n / 1024).toStringAsFixed(n < 10 * 1024 ? 1 : 0)}KB';
  }
  final mb = n / (1024 * 1024);
  return '${mb.toStringAsFixed(mb < 10 ? 1 : 0)}MB';
}

/// 공유 문서의 마크다운 글꼴 — 말풍선(`conversation_view.dart`)과 같은 크기·코드 바탕.
MarkdownStyleSheet _shareMarkdownStyle(ThemeData theme) => chatMarkdownStyle(
  theme,
  base: theme.textTheme.bodyMedium!.copyWith(
    fontSize: 15,
    height: 1.5,
    color: theme.colorScheme.onSurface,
  ),
  codeBg: theme.colorScheme.surfaceContainerHighest,
);
