import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';

import 'inline_media.dart';
import 'server.dart';

class InlineResultPart {
  const InlineResultPart.text(this.text) : id = null, media = false, rows = 0;
  const InlineResultPart.image(this.id) : text = null, media = false, rows = 0;
  const InlineResultPart.media(this.id, this.rows) : text = null, media = true;
  final String? text;
  final String? id;

  /// `[[km:…]]` 소리·영상 블록. 줄 수는 PC 터미널이 잡은 높이일 뿐이고 종류는 정보가 정한다.
  final bool media;
  final int rows;
}

List<InlineResultPart> inlineResultParts(String text) {
  final blocks = RegExp(
    r'^```(?:text)?[ \t]*\r?\n((?:[ \t]*\[\[k[im]:[0-9a-f]{16}:\d{2}/\d{2}\]\][ \t]*\r?\n)+)```[ \t]*(?:\r?\n|$)',
    multiLine: true,
  );
  final marker = RegExp(r'^\[\[(k[im]):([0-9a-f]{16}):(\d{2})/(\d{2})\]\]$');
  final parts = <InlineResultPart>[];
  var cursor = 0;
  for (final block in blocks.allMatches(text)) {
    final lines = block.group(1)!.trim().split('\n');
    final first = marker.firstMatch(lines.first.trim())!;
    final tag = first.group(1)!;
    final id = first.group(2)!;
    final total = int.parse(first.group(4)!);
    if (total < 1 || total > 24 || lines.length != total) continue;
    var valid = true;
    for (var i = 0; i < lines.length; i++) {
      final row = marker.firstMatch(lines[i].trim());
      if (row == null ||
          row.group(1) != tag ||
          row.group(2) != id ||
          int.parse(row.group(3)!) != i ||
          int.parse(row.group(4)!) != total) {
        valid = false;
        break;
      }
    }
    if (!valid) continue;
    if (cursor < block.start) {
      parts.add(InlineResultPart.text(text.substring(cursor, block.start)));
    }
    parts.add(
      tag == 'km'
          ? InlineResultPart.media(id, total)
          : InlineResultPart.image(id),
    );
    cursor = block.end;
  }
  if (cursor < text.length) {
    parts.add(InlineResultPart.text(text.substring(cursor)));
  }
  return parts;
}

class InlineResultBody extends StatelessWidget {
  const InlineResultBody({
    super.key,
    required this.server,
    required this.text,
    required this.style,
    required this.onLink,
    this.machine,
  });
  final Server server;
  final String text;
  final MarkdownStyleSheet style;
  final ValueChanged<String> onLink;
  final String? machine;

  @override
  Widget build(BuildContext context) => Column(
    mainAxisSize: MainAxisSize.min,
    crossAxisAlignment: CrossAxisAlignment.start,
    children: [
      for (final part in inlineResultParts(text))
        if (part.media)
          Padding(
            padding: const EdgeInsets.symmetric(vertical: 8),
            child: InlineMediaCard(
              server: server,
              id: part.id!,
              rows: part.rows,
              machine: machine,
            ),
          )
        else if (part.id case final String id)
          Padding(
            padding: const EdgeInsets.symmetric(vertical: 8),
            child: InlineResultImage(server: server, id: id, machine: machine),
          )
        else if (part.text!.trim().isNotEmpty)
          MarkdownBody(
            data: part.text!,
            styleSheet: style,
            softLineBreak: true,
            onTapLink: (_, href, _) {
              if (href != null) onLink(href);
            },
          ),
    ],
  );
}

class InlineResultImage extends StatefulWidget {
  const InlineResultImage({
    super.key,
    required this.server,
    required this.id,
    this.machine,
  });
  final Server server;
  final String id;
  final String? machine;
  @override
  State<InlineResultImage> createState() => _InlineResultImageState();
}

class _InlineResultImageState extends State<InlineResultImage> {
  Future<Uint8List> _load() => widget.server.imageBytes(
    widget.server.uri(
      'term/inline-result',
      query: {'id': widget.id},
      machine: widget.machine,
    ),
  );
  late Future<Uint8List> _bytes = _load();

  @override
  void didUpdateWidget(InlineResultImage oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.server != widget.server ||
        oldWidget.id != widget.id ||
        oldWidget.machine != widget.machine) {
      _bytes = _load();
    }
  }

  Widget _failed() => Row(
    mainAxisSize: MainAxisSize.min,
    children: [
      const Flexible(child: Text('이미지를 불러오지 못했어요')),
      TextButton(
        onPressed: () => setState(() => _bytes = _load()),
        child: const Text('다시 시도'),
      ),
    ],
  );

  @override
  Widget build(BuildContext context) => FutureBuilder<Uint8List>(
    future: _bytes,
    builder: (context, snapshot) {
      if (snapshot.hasError) return _failed();
      if (!snapshot.hasData) {
        return const SizedBox(
          height: 96,
          child: Center(
            child: SizedBox.square(
              dimension: 24,
              child: CircularProgressIndicator(strokeWidth: 2),
            ),
          ),
        );
      }
      return ConstrainedBox(
        constraints: const BoxConstraints(maxHeight: 360),
        child: Image.memory(
          snapshot.data!,
          fit: BoxFit.contain,
          alignment: Alignment.centerLeft,
          semanticLabel: '결과 이미지',
          errorBuilder: (_, _, _) => _failed(),
        ),
      );
    },
  );
}
