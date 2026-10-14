import 'package:flutter/material.dart';

import '../look.dart';
import '../server.dart';

enum ShareFilter { all, images, documents }

List<ShareFile> matchingShareFiles(
  ShareListing listing,
  String query,
  ShareFilter filter,
) {
  final words = query
      .trim()
      .toLowerCase()
      .split(RegExp(r'\s+'))
      .where((word) => word.isNotEmpty);
  final files =
      <String, ShareFile>{
        for (final file in [
          ...listing.files,
          for (final folder in listing.folders) ...folder.files,
        ])
          file.path: file,
      }.values.where((file) {
        final included = switch (filter) {
          ShareFilter.all => true,
          ShareFilter.images => file.kind == ShareKind.image,
          ShareFilter.documents => [
            ShareKind.markdown,
            ShareKind.text,
            ShareKind.pdf,
            ShareKind.html,
          ].contains(file.kind),
        };
        final kind = switch (file.kind) {
          ShareKind.image => '이미지 사진 그림 image',
          ShareKind.markdown ||
          ShareKind.text ||
          ShareKind.pdf ||
          ShareKind.html => '문서 텍스트 document',
          ShareKind.video => '동영상 영상 video',
          ShareKind.audio => '음성 음악 audio',
          ShareKind.other => '파일 file',
        };
        final haystack =
            '${file.path} ${file.name} ${file.origin} ${file.kind.name} $kind'
                .toLowerCase();
        return included && words.every(haystack.contains);
      }).toList();
  files.sort((a, b) {
    final time = b.modified.compareTo(a.modified);
    return time == 0 ? a.path.compareTo(b.path) : time;
  });
  return files;
}

class ShareBrowserToolbar extends StatelessWidget {
  const ShareBrowserToolbar({
    super.key,
    required this.search,
    required this.recent,
    required this.filter,
    required this.onQuery,
    required this.onRecent,
    required this.onFilter,
    this.sync,
  });

  final TextEditingController search;
  final bool recent;
  final ShareFilter filter;
  final ValueChanged<String> onQuery;
  final ValueChanged<bool> onRecent;
  final ValueChanged<ShareFilter> onFilter;
  final ShareSync? sync;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final summary = sync == null ? '기기끼리 공유하는 파일' : sync!.summary;
    return Padding(
      padding: const EdgeInsets.fromLTRB(Look.pagePad, 0, Look.pagePad, 12),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          SizedBox(
            height: 48,
            child: TextField(
              key: const ValueKey('share-search'),
              controller: search,
              onChanged: onQuery,
              style: const TextStyle(fontSize: 16),
              maxLength: 512,
              decoration: InputDecoration(
                labelText: '공유 파일 검색',
                hintText: '이름·종류·기기',
                counterText: '',
                prefixIcon: const Icon(Icons.search, size: 20),
                suffixIcon: search.text.isEmpty
                    ? null
                    : IconButton(
                        tooltip: '검색 지우기',
                        onPressed: () {
                          search.clear();
                          onQuery('');
                        },
                        icon: const Icon(Icons.close, size: 20),
                      ),
              ),
            ),
          ),
          const SizedBox(height: 8),
          Row(
            children: [
              Expanded(
                child: SizedBox(
                  height: 44,
                  child: SegmentedButton<bool>(
                    showSelectedIcon: false,
                    segments: const [
                      ButtonSegment(
                        value: true,
                        icon: Icon(Icons.history, size: 18),
                        label: Text('최근'),
                      ),
                      ButtonSegment(
                        value: false,
                        icon: Icon(Icons.folder_outlined, size: 18),
                        label: Text('폴더'),
                      ),
                    ],
                    selected: {recent},
                    onSelectionChanged: (values) => onRecent(values.single),
                  ),
                ),
              ),
              const SizedBox(width: 8),
              PopupMenuButton<ShareFilter>(
                tooltip: '파일 종류',
                initialValue: filter,
                onSelected: onFilter,
                icon: Icon(
                  Icons.filter_list,
                  color: filter == ShareFilter.all
                      ? scheme.onSurfaceVariant
                      : scheme.primary,
                ),
                itemBuilder: (_) => const [
                  PopupMenuItem(value: ShareFilter.all, child: Text('모든 파일')),
                  PopupMenuItem(value: ShareFilter.images, child: Text('이미지')),
                  PopupMenuItem(
                    value: ShareFilter.documents,
                    child: Text('문서'),
                  ),
                ],
              ),
            ],
          ),
          const SizedBox(height: 8),
          Text(
            summary,
            maxLines: 2,
            overflow: TextOverflow.ellipsis,
            style: TextStyle(fontSize: 13, color: scheme.onSurfaceVariant),
          ),
        ],
      ),
    );
  }
}
