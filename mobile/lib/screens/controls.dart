import 'package:flutter/material.dart';

import '../look.dart';

/// 입력칸 오른쪽 보내기 44×44 — 글이 있으면 강조 테·화살표, 없으면 기본 테·흐린 화살표.
class SendButton extends StatelessWidget {
  const SendButton({super.key, required this.ready, required this.onPressed, this.busy = false, this.icon});

  final bool ready;
  final bool busy;
  final VoidCallback? onPressed;
  final IconData? icon;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final on = ready && !busy && onPressed != null;
    return SizedBox.square(
      dimension: Look.tap,
      child: IconButton(
        tooltip: '보내기',
        onPressed: busy ? null : onPressed,
        style: IconButton.styleFrom(
          shape: RoundedRectangleBorder(
            borderRadius: Look.corners,
            side: BorderSide(color: on ? scheme.primary : scheme.outline),
          ),
          foregroundColor: on ? scheme.primary : scheme.onSurfaceVariant,
        ),
        icon: busy
            ? const SizedBox(width: 18, height: 18, child: CircularProgressIndicator(strokeWidth: 2))
            : Icon(icon ?? Icons.arrow_upward_rounded),
      ),
    );
  }
}

/// 입력칸 위에 이름표 — 떠 있는 이름표는 옆 칸 테에 걸린다(개발 서버 창에서 「경로」가 포트 칸에 걸렸다).
class LabeledField extends StatelessWidget {
  const LabeledField({super.key, required this.label, required this.child});

  final String label;
  final Widget child;

  @override
  Widget build(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    mainAxisSize: MainAxisSize.min,
    children: [
      Text(
        label,
        style: Theme.of(context).textTheme.labelMedium?.copyWith(
          color: Theme.of(context).colorScheme.onSurfaceVariant,
        ),
      ),
      const SizedBox(height: 6),
      child,
    ],
  );
}
