import 'dart:typed_data';

import 'package:flutter/material.dart';

import '../inline_media.dart';
import '../server.dart';

/// PDF 카드의 쪽 넘김. 쪽마다 PC 가 그린 PNG 를 받는다 — 폰은 PDF 를 풀지 않는다. 두 손가락으로 키우고, 옆으로 밀어
/// 넘긴다.
class PdfPagesScreen extends StatefulWidget {
  const PdfPagesScreen({
    super.key,
    required this.server,
    required this.media,
    this.machine,
  });

  static Route<void> route({
    required Server server,
    required InlineMedia media,
    String? machine,
  }) => MaterialPageRoute<void>(
    fullscreenDialog: true,
    settings: const RouteSettings(name: 'inline-pdf'),
    builder: (_) =>
        PdfPagesScreen(server: server, media: media, machine: machine),
  );

  final Server server;
  final InlineMedia media;
  final String? machine;

  @override
  State<PdfPagesScreen> createState() => _PdfPagesScreenState();
}

class _PdfPagesScreenState extends State<PdfPagesScreen> {
  int _page = 1;
  final _pages = <int, Future<Uint8List>>{};

  Future<Uint8List> _load(int n) => _pages.putIfAbsent(
    n,
    () => widget.server.inlineMediaPage(
      widget.media.id,
      n,
      machine: widget.machine,
    ),
  );

  @override
  Widget build(BuildContext context) {
    final total = widget.media.pages ?? 1;
    return Scaffold(
      appBar: AppBar(
        title: Text(
          widget.media.title,
          maxLines: 1,
          overflow: TextOverflow.ellipsis,
        ),
        actions: [
          Padding(
            padding: const EdgeInsets.only(right: 16),
            child: Center(child: Text('$_page / $total쪽')),
          ),
        ],
      ),
      body: PageView.builder(
        itemCount: total,
        onPageChanged: (i) => setState(() => _page = i + 1),
        itemBuilder: (context, i) => _Page(
          load: () => _load(i + 1),
          retry: () => setState(() => _pages.remove(i + 1)),
          label: '${i + 1}쪽',
        ),
      ),
    );
  }
}

class _Page extends StatelessWidget {
  const _Page({required this.load, required this.retry, required this.label});
  final Future<Uint8List> Function() load;
  final VoidCallback retry;
  final String label;

  @override
  Widget build(BuildContext context) => FutureBuilder<Uint8List>(
    future: load(),
    builder: (context, snap) {
      if (snap.hasError) {
        return Center(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Text('$label을 불러오지 못했어요'),
              TextButton(onPressed: retry, child: const Text('다시 시도')),
            ],
          ),
        );
      }
      if (!snap.hasData) {
        return const Center(
          child: SizedBox.square(
            dimension: 24,
            child: CircularProgressIndicator(strokeWidth: 2),
          ),
        );
      }
      return InteractiveViewer(
        maxScale: 5,
        child: Center(
          child: Image.memory(
            snap.data!,
            fit: BoxFit.contain,
            semanticLabel: label,
            gaplessPlayback: true,
          ),
        ),
      );
    },
  );
}
