import 'package:flutter/material.dart';

import '../look.dart';

/// 입력칸 오른쪽 보내기 44×44 — 글이 있으면 강조 테·화살표, 없으면 기본 테·흐린 화살표.
/// 대화(`round`)는 A 이전 모습이라 늘 강조색으로 채운 원이다.
class SendButton extends StatelessWidget {
  const SendButton({
    super.key,
    required this.ready,
    required this.onPressed,
    this.busy = false,
    this.icon,
    this.round = false,
  });

  final bool ready;
  final bool busy;
  final VoidCallback? onPressed;
  final IconData? icon;
  final bool round;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final on = ready && !busy && onPressed != null;
    return SizedBox.square(
      dimension: Look.tap,
      child: IconButton(
        tooltip: '보내기',
        onPressed: busy ? null : onPressed,
        style: round
            ? IconButton.styleFrom(
                shape: const CircleBorder(),
                backgroundColor: scheme.primary,
                foregroundColor: scheme.onPrimary,
                disabledBackgroundColor: scheme.primary.withValues(alpha: 0.4),
                disabledForegroundColor: scheme.onPrimary,
              )
            : IconButton.styleFrom(
                shape: RoundedRectangleBorder(
                  borderRadius: Look.corners,
                  side: BorderSide(color: on ? scheme.primary : scheme.outline),
                ),
                foregroundColor: on ? scheme.primary : scheme.onSurfaceVariant,
              ),
        icon: busy
            ? SizedBox(
                width: 18,
                height: 18,
                child: CircularProgressIndicator(
                  strokeWidth: 2,
                  color: round ? scheme.onPrimary : null,
                ),
              )
            : Icon(icon ?? Icons.arrow_upward_rounded),
      ),
    );
  }
}

/// 대화 말풍선 — 내 말은 강조색, 상대 말은 올린 표면색으로 채우고 말한 쪽 아래 모서리를 꼬리로 좁힌다.
class SpeechBubble extends StatelessWidget {
  const SpeechBubble({
    super.key,
    required this.mine,
    required this.child,
    this.fill,
    this.maxWidth,
  });

  final bool mine;
  final Widget child;

  /// 기본 채움 대신 — 예약처럼 아직 안 간 내 말.
  final Color? fill;
  final double? maxWidth;

  /// 말풍선 안 글자색.
  static Color ink(BuildContext context, {required bool mine}) {
    final scheme = Theme.of(context).colorScheme;
    return mine ? scheme.onPrimary : scheme.onSurface;
  }

  /// 상대 말풍선 채움 — 데스크톱 토큰은 반투명일 수 있어 바탕에 섞어 둔다.
  static Color otherFill(BuildContext context) => Color.alphaBlend(
    Theme.of(context).colorScheme.surfaceContainerHighest,
    Theme.of(context).scaffoldBackgroundColor,
  );

  @override
  Widget build(BuildContext context) {
    const r = Radius.circular(Look.bubbleRadius);
    const tail = Radius.circular(Look.bubbleTail);
    return Container(
      constraints: maxWidth == null
          ? null
          : BoxConstraints(maxWidth: maxWidth!),
      padding: const EdgeInsets.symmetric(
        horizontal: Look.bubblePadX,
        vertical: Look.bubblePadY,
      ),
      decoration: BoxDecoration(
        color:
            fill ??
            (mine ? Theme.of(context).colorScheme.primary : otherFill(context)),
        borderRadius: BorderRadius.only(
          topLeft: r,
          topRight: r,
          bottomLeft: mine ? r : tail,
          bottomRight: mine ? tail : r,
        ),
      ),
      child: child,
    );
  }
}

/// 대화상자·시트 안 — 주 동작은 강조색으로 채운 단추(모서리 8). 모달은 A 이전 모습이라 테만 있는 전역 단추를 여기서 덮는다.
class ModalLook extends StatelessWidget {
  const ModalLook({super.key, required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Theme(
      data: theme.copyWith(
        filledButtonTheme: FilledButtonThemeData(
          style: FilledButton.styleFrom(
            backgroundColor: scheme.primary,
            foregroundColor: scheme.onPrimary,
            minimumSize: const Size(Look.tap, Look.buttonH),
            padding: const EdgeInsets.symmetric(horizontal: Look.buttonPadX),
            shape: RoundedRectangleBorder(
              borderRadius: BorderRadius.circular(Look.modalButtonRadius),
            ),
            textStyle: theme.textTheme.labelLarge,
            elevation: 0,
          ),
        ),
      ),
      child: child,
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
