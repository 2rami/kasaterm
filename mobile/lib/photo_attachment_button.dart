import 'dart:typed_data';

import 'package:flutter/material.dart';

import 'image_attachment.dart';
import 'server.dart';

class PhotoAttachmentButton extends StatefulWidget {
  const PhotoAttachmentButton({
    super.key,
    required this.server,
    required this.pane,
    required this.enabled,
    this.onBusy,
    this.onAttached,
    this.pickImage = pickAttachmentImage,
  });

  final Server server;
  final Pane pane;
  final bool enabled;
  final ValueChanged<bool>? onBusy;
  final VoidCallback? onAttached;
  final Future<Uint8List?> Function() pickImage;

  @override
  State<PhotoAttachmentButton> createState() => _PhotoAttachmentButtonState();
}

class _PhotoAttachmentButtonState extends State<PhotoAttachmentButton> {
  bool _busy = false;
  bool _uploading = false;

  Future<void> _attach() async {
    if (_busy || !widget.enabled) return;
    // A navigation/focus change while the picker is open must not redirect an
    // image to the newly selected student or another authenticated server.
    final server = widget.server;
    final pane = widget.pane;
    setState(() => _busy = true);
    widget.onBusy?.call(true);
    try {
      final bytes = await widget.pickImage();
      if (bytes == null || !mounted) return;
      if (widget.pane.id != pane.id ||
          widget.pane.machine != pane.machine ||
          widget.server != server ||
          !(ModalRoute.of(context)?.isCurrent ?? true)) {
        return;
      }
      if (!widget.enabled) {
        _message('연결이 끊겨 사진을 보내지 않았어요. 다시 연결되면 첨부해 주세요.');
        return;
      }
      setState(() => _uploading = true);
      await server.pasteImage(pane.id, bytes, machine: pane.machine);
      if (!mounted) return;
      widget.onAttached?.call();
      _message('사진을 입력창에 첨부했어요. 보내기를 누르면 함께 전달돼요.');
    } on ServerException catch (error) {
      if (mounted) _message(error.message);
    } catch (_) {
      if (mounted) _message('사진을 불러오지 못했어요. 사진 접근 권한이나 파일을 확인해 주세요.');
    } finally {
      if (mounted) {
        setState(() {
          _busy = false;
          _uploading = false;
        });
        widget.onBusy?.call(false);
      }
    }
  }

  void _message(String message) {
    ScaffoldMessenger.of(
      context,
    ).showSnackBar(SnackBar(content: Text(message)));
  }

  @override
  Widget build(BuildContext context) => IconButton(
    tooltip: _busy ? (_uploading ? '사진 첨부 중' : '사진 선택 중') : '사진 첨부',
    onPressed: widget.enabled && !_busy ? _attach : null,
    icon: _busy
        ? const SizedBox(
            width: 18,
            height: 18,
            child: CircularProgressIndicator(strokeWidth: 2),
          )
        : const Icon(Icons.add_photo_alternate_outlined, size: 22),
  );
}
