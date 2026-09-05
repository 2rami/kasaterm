import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';

import 'fill_viewer.dart';

/// 그림 모드 한 장. 격자처럼 높이를 채우려면 크기를 알아야 해서 먼저 디코드한다 —
/// 새 장이 풀리기 전엔 이전 장을 그대로 둔다(깜빡임 없음). 서버가 400ms 마다 새 장을
/// 주므로 디코드가 늦은 장은 버리고 마지막 것만 쓴다.
class ShotView extends StatefulWidget {
  const ShotView({super.key, required this.bytes, required this.background});

  final Uint8List bytes;
  final Color background;

  @override
  State<ShotView> createState() => _ShotViewState();
}

class _ShotViewState extends State<ShotView> {
  ui.Image? _image;
  Uint8List? _latest;

  @override
  void initState() {
    super.initState();
    _decode(widget.bytes);
  }

  @override
  void didUpdateWidget(ShotView old) {
    super.didUpdateWidget(old);
    if (!identical(old.bytes, widget.bytes)) _decode(widget.bytes);
  }

  @override
  void dispose() {
    _image?.dispose();
    super.dispose();
  }

  Future<void> _decode(Uint8List bytes) async {
    _latest = bytes;
    final ui.Image image;
    try {
      image = await decodeImageFromList(bytes);
    } catch (_) {
      return;
    }
    if (!mounted || !identical(_latest, bytes)) {
      image.dispose();
      return;
    }
    final previous = _image;
    setState(() => _image = image);
    // 이전 장은 이번 프레임까지 그려질 수 있어 프레임이 지난 뒤 놓는다.
    if (previous != null) {
      WidgetsBinding.instance.addPostFrameCallback((_) => previous.dispose());
    }
  }

  @override
  Widget build(BuildContext context) {
    final image = _image;
    if (image == null) return ColoredBox(color: widget.background);
    final w = image.width.toDouble();
    final h = image.height.toDouble();
    return FillViewer(
      content: Size(w, h),
      background: widget.background,
      child: RawImage(
        image: image,
        width: w,
        height: h,
        filterQuality: FilterQuality.medium,
      ),
    );
  }
}
