import 'dart:math' as math;

import 'package:flutter/material.dart';

/// 내용(격자·그림)을 화면에 채워 보이고 핀치로 키운다. 폭에만 맞추면 넓은 pane(196열)이
/// 위쪽에 손톱만 하게 붙고 아래가 비므로, 폭 맞춤과 높이 맞춤 중 큰 쪽으로 채우고 옆으로
/// 밀어 읽게 한다. 작은 pane 이 커지지 않게 1.3배 상한을 뒀었는데 그러면 25행짜리 pane
/// 아래가 다시 비어 「꽉 안 찬다」(2026-09-05 실기 실측) — 상한 없이 채우고, 작다 싶으면
/// 핀치로 전체가 한눈에 들어오는 배율까지 줄인다. 미러 pane 은 크기를 못 바꾸므로
/// (데스크톱이 같이 좁아진다) 글꼴을 줄이는 대신 변환으로 맞춘다.
class FillViewer extends StatefulWidget {
  const FillViewer({
    super.key,
    required this.content,
    required this.background,
    required this.child,
    this.onVerticalPan,
  });

  /// 자식의 본래 크기 — 자식은 이 크기의 상자 안에 그려진다.
  final Size content;
  final Color background;
  final Widget child;

  /// 한 손가락 세로 끌기 중 격자 끝을 넘어선 몫(내용 좌표 px, 아래로 끌면 양수)을 넘겨받는다.
  /// 주면 세로로는 격자 안에서만 밀고 빈 여백으로 넘어가지 않는다 — 넘어선 몫은 앱이 스스로
  /// 굴리게 한다(Claude 전체 화면은 스크롤백이 없다, 2026-10-05 실기 「스크롤이 안 돼」).
  final ValueChanged<double>? onVerticalPan;

  static double fitFor(Size content, BoxConstraints box) {
    final w = math.max(content.width, 1.0);
    final h = math.max(content.height, 1.0);
    return math.max(box.maxWidth / w, box.maxHeight / h);
  }

  @override
  State<FillViewer> createState() => _FillViewerState();
}

class _FillViewerState extends State<FillViewer> {
  final _controller = TransformationController();
  double _fit = 1;
  double _boxH = 0;

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  /// 세로는 여기서 민다(InteractiveViewer 는 가로만) — 격자 위·아래 끝에서 멈추고 남는 몫을 넘긴다.
  void _panY(ScaleUpdateDetails d) {
    final pan = widget.onVerticalPan;
    if (pan == null || d.pointerCount != 1 || d.focalPointDelta.dy == 0) return;
    final m = _controller.value;
    final scale = m.getMaxScaleOnAxis();
    final t = m.getTranslation();
    final h = math.max(widget.content.height, 1.0) * scale;
    final want = t.y + d.focalPointDelta.dy;
    final y = want.clamp(math.min(0.0, _boxH - h), 0.0).toDouble();
    if (y != t.y) {
      _controller.value = m.clone()..setTranslationRaw(t.x, y, t.z);
    }
    if (want != y) pan((want - y) / scale);
  }

  /// 사용자가 손대지 않은 배율(= 직전 fit)일 때만 새 fit 을 적용한다 — 키운
  /// 상태를 프레임마다 되돌리면 핀치가 무의미해진다.
  void _applyFit(double fit) {
    final current = _controller.value.getMaxScaleOnAxis();
    final untouched = (current - _fit).abs() < 1e-3;
    _fit = fit;
    if (!untouched) return;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      // z 도 같은 배율로 — getMaxScaleOnAxis 가 세 축의 최대를 돌려주므로 z 를 1 로
      // 두면 1 보다 작은 배율이 늘 1 로 읽혀 「손대지 않음」 판정이 어긋난다.
      // InteractiveViewer 자신도 세 축을 같이 키운다.
      _controller.value = Matrix4.diagonal3Values(fit, fit, fit);
    });
  }

  @override
  Widget build(BuildContext context) => LayoutBuilder(
    builder: (context, constraints) {
      final w = math.max(widget.content.width, 1.0);
      final h = math.max(widget.content.height, 1.0);
      final fitW = constraints.maxWidth / w;
      final fitH = constraints.maxHeight / h;
      final fit = FillViewer.fitFor(widget.content, constraints);
      if ((fit - _fit).abs() > 1e-6) _applyFit(fit);
      _boxH = constraints.maxHeight;
      return ColoredBox(
        color: widget.background,
        child: ClipRect(
          child: InteractiveViewer(
            transformationController: _controller,
            constrained: false,
            minScale: math.min(math.min(fitW, fitH), fit),
            maxScale: 6,
            panAxis: widget.onVerticalPan == null
                ? PanAxis.free
                : PanAxis.horizontal,
            onInteractionUpdate: _panY,
            boundaryMargin: EdgeInsets.symmetric(
              horizontal: constraints.maxWidth,
              vertical: constraints.maxHeight,
            ),
            child: SizedBox(width: w, height: h, child: widget.child),
          ),
        ),
      );
    },
  );
}
