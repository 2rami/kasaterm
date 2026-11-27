import 'dart:typed_data';

import 'package:crypto/crypto.dart';
import 'package:flutter/material.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';

import 'chat_markdown.dart';
import 'inline_card_visibility.dart';
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

/// 정지 그림 PNG 의 `kasaterm-anim` 값 — 움직이는 원본(GIF·WebP·APNG)의 SHA-256(소문자 hex). PC 가 IHDR 바로
/// 뒤에 쓰므로 첫 IDAT 전까지만 본다(`inline_result::load::read_mark` 와 같은 규칙). 없으면 정지 그림이다.
String? inlineAnimMark(Uint8List png) {
  const signature = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
  if (png.length < 8) return null;
  for (var i = 0; i < 8; i++) {
    if (png[i] != signature[i]) return null;
  }
  const key = 'kasaterm-anim';
  final data = ByteData.sublistView(png);
  var at = 8;
  while (at + 8 <= png.length) {
    final length = data.getUint32(at);
    final kind = String.fromCharCodes(png.sublist(at + 4, at + 8));
    if (kind == 'IDAT' || kind == 'IEND' || at + 8 + length > png.length) {
      return null;
    }
    if (kind == 'tEXt') {
      final body = png.sublist(at + 8, at + 8 + length);
      final zero = body.indexOf(0);
      if (zero == key.length &&
          String.fromCharCodes(body.sublist(0, zero)) == key) {
        final value = String.fromCharCodes(body.sublist(zero + 1));
        return RegExp(r'^[0-9a-f]{64}$').hasMatch(value) ? value : null;
      }
    }
    at += 12 + length;
  }
  return null;
}

/// 말풍선 통째 복사에 담을 원문 — 그림·소리 표시 블록은 붙여 넣은 곳에서 뜻이 없어 걷는다.
String withoutInlineResults(String text) => [
  for (final part in inlineResultParts(text)) ?part.text,
].join().replaceAll(RegExp(r'\n{3,}'), '\n\n').trim();

class InlineResultBody extends StatelessWidget {
  const InlineResultBody({
    super.key,
    required this.server,
    required this.text,
    required this.style,
    required this.onLink,
    this.machine,
    this.offline = false,
  });
  final Server server;
  final String text;
  final MarkdownStyleSheet style;
  final ValueChanged<String> onLink;
  final String? machine;
  final bool offline;

  @override
  Widget build(BuildContext context) => Column(
    mainAxisSize: MainAxisSize.min,
    crossAxisAlignment: CrossAxisAlignment.start,
    children: [
      for (final part in inlineResultParts(text))
        if (offline && part.id != null)
          Padding(
            padding: const EdgeInsets.symmetric(vertical: 8),
            child: Text(
              part.media ? '음성·영상 첨부 · 저장본에 포함되지 않음' : '이미지 첨부 · 저장본에 포함되지 않음',
            ),
          )
        else if (part.media)
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
            child: ValueListenableBuilder<Set<String>>(
              valueListenable: InlineCardVisibility.flipped,
              builder: (context, _, _) {
                final key = 'ki:${machine ?? ''}:$id';
                void flip() => InlineCardVisibility.toggle(key);
                // 가린 그림은 받지도 않는다 — 눈을 눌러야 원본 기기에서 가져온다.
                return InlineCardVisibility.isShown(key)
                    ? InlineResultImage(
                        server: server,
                        id: id,
                        machine: machine,
                        onHide: flip,
                      )
                    : HiddenCardBar(
                        icon: Icons.image_outlined,
                        title: '그림',
                        caption: '가려짐',
                        onShow: flip,
                      );
              },
            ),
          )
        else if (part.text!.trim().isNotEmpty)
          ChatMarkdownBody(
            data: part.text!,
            style: style,
            imageBuilder: offline
                ? (uri, title, alt) => Text(
                    alt?.isNotEmpty == true
                        ? '$alt · 저장되지 않은 이미지'
                        : '저장되지 않은 이미지',
                  )
                : null,
            onLink: onLink,
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
    this.onHide,
  });
  final Server server;
  final String id;
  final String? machine;

  /// 있으면 그림 오른쪽 위에 가리기 눈을 단다.
  final VoidCallback? onHide;
  @override
  State<InlineResultImage> createState() => _InlineResultImageState();
}

class _InlineResultImageState extends State<InlineResultImage> {
  /// 움직이는 그림이면 원본을 받아 표식의 SHA-256 과 맞을 때만 쓴다. 원본을 못 받거나 옛 PC 라 길이 없으면
  /// 첫 장(정지 그림)으로 선다 — `Image.memory` 가 GIF·WebP 를 스스로 돌린다.
  Future<Uint8List> _load() async {
    Uri at(String path) => widget.server.uri(
      path,
      query: {'id': widget.id},
      machine: widget.machine,
    );
    final png = await widget.server.imageBytes(at('term/inline-result'));
    final mark = inlineAnimMark(png);
    if (mark == null) return png;
    try {
      final anim = await widget.server.imageBytes(
        at('term/inline-result/anim'),
      );
      if (sha256.convert(anim).toString() == mark) return anim;
    } on Object catch (_) {}
    return png;
  }

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
      final image = Image.memory(
        snapshot.data!,
        fit: BoxFit.contain,
        alignment: Alignment.centerLeft,
        semanticLabel: '결과 이미지',
        errorBuilder: (_, _, _) => _failed(),
      );
      return ConstrainedBox(
        constraints: const BoxConstraints(maxHeight: 360),
        child: switch (widget.onHide) {
          final hide? => CardEye(onHide: hide, child: image),
          null => image,
        },
      );
    },
  );
}
