import 'package:flutter/material.dart';

import '../look.dart';

/// 입력칸 오른쪽 보내기 44×44 — 글이 있으면 강조 채움·화살표, 없으면 톤 채움·흐린 화살표.
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
                shape: RoundedRectangleBorder(borderRadius: Look.corners),
                backgroundColor: on ? scheme.primary : scheme.surfaceContainerHigh,
                foregroundColor: on ? scheme.onPrimary : scheme.onSurfaceVariant,
                disabledBackgroundColor: scheme.surfaceContainerHigh,
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

/// 설정 묶음 — 흐린 제목 하나와 둥근 판, 안의 줄 사이는 글자 시작점부터 선(design.md 「설정」).
class SettingsGroup extends StatelessWidget {
  const SettingsGroup({super.key, this.title, required this.children, this.inset});

  final String? title;
  final List<Widget> children;

  /// 줄 사이 선이 시작하는 x — 기본은 `SettingsRow` 의 글자 시작점. 다른 줄(목록 타일)은 그 글자선을 준다.
  final double? inset;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final inset = this.inset ?? Look.cardPad + Look.tile + 12;
    return Padding(
      padding: const EdgeInsets.only(top: Look.groupGap),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          if (title != null)
            Padding(
              padding: const EdgeInsets.fromLTRB(4, 0, 4, Look.groupTitleGap),
              child: Text(
                title!,
                style: theme.textTheme.labelMedium?.copyWith(color: theme.colorScheme.onSurfaceVariant),
              ),
            ),
          DecoratedBox(
            decoration: TwinTone.of(context).cardBox(),
            child: ClipRRect(
              borderRadius: Look.cardCorners,
              child: Material(
                type: MaterialType.transparency,
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    for (final (i, row) in children.indexed) ...[
                      if (i > 0) Divider(height: 1, indent: inset),
                      row,
                    ],
                  ],
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// 설정 줄 하나 — 쌍둥이 물 아이콘 타일 · 이름 15/600 + 보조 13 · 오른쪽 값·›. 고르기 같은 조작은 [below] 로
/// 줄 아래에. [tone] 은 하늘(짝수)·호박(홀수).
class SettingsRow extends StatelessWidget {
  const SettingsRow({
    super.key,
    required this.icon,
    required this.title,
    this.subtitle,
    this.trailing,
    this.below,
    this.onTap,
    this.tone = 0,
    this.chevron = false,
    this.danger = false,
    this.logo,
  });

  final IconData icon;

  /// 타일 안에 아이콘 대신 그릴 그림(공급자 로고). 크기는 아이콘과 같다.
  final Widget? logo;
  final String title;
  final String? subtitle;
  final Widget? trailing;
  final Widget? below;
  final VoidCallback? onTap;
  final int tone;
  final bool chevron;
  final bool danger;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final (wash, ink) = TwinTone.of(context).pick(tone);
    final tileFill = danger ? scheme.errorContainer : wash;
    final tileInk = danger ? scheme.error : ink;
    final row = ConstrainedBox(
      constraints: const BoxConstraints(minHeight: Look.settingRow),
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: Look.cardPad, vertical: 10),
        child: Row(
          children: [
            Container(
              width: Look.tile,
              height: Look.tile,
              decoration: BoxDecoration(color: tileFill, borderRadius: Look.smallCorners),
              alignment: Alignment.center,
              child: logo ?? Icon(icon, size: Look.iconSize, color: tileInk),
            ),
            const SizedBox(width: 12),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                mainAxisSize: MainAxisSize.min,
                children: [
                  Text(
                    title,
                    style: theme.textTheme.titleSmall?.copyWith(color: danger ? scheme.error : null),
                  ),
                  if (subtitle != null) ...[
                    const SizedBox(height: Look.rowGap),
                    Text(
                      subtitle!,
                      style: theme.textTheme.bodySmall?.copyWith(color: scheme.onSurfaceVariant),
                    ),
                  ],
                ],
              ),
            ),
            if (trailing != null) ...[const SizedBox(width: 8), trailing!],
            if (chevron)
              Padding(
                padding: const EdgeInsets.only(left: 4),
                child: Icon(Icons.chevron_right_rounded, color: scheme.onSurfaceVariant),
              ),
          ],
        ),
      ),
    );
    final body = below == null
        ? row
        : Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              row,
              Padding(
                padding: const EdgeInsets.fromLTRB(Look.cardPad, 0, Look.cardPad, Look.cardPad),
                child: below,
              ),
            ],
          );
    return onTap == null ? body : InkWell(onTap: onTap, child: body);
  }
}

/// 글자 대신 아이콘으로 고르는 알약 — 줄 오른쪽에 선다. 고른 칸만 강조 물 위 강조 아이콘, 나머지는 흐린 아이콘.
/// 이름은 길게 누르면 뜨는 말풍선과 줄 보조글이 말한다(2026-10-01 「텍스트말고 아이콘으로」).
class IconChoice<T> extends StatelessWidget {
  const IconChoice({super.key, required this.options, required this.selected, this.onSelect});

  final List<(T, IconData, String)> options;
  final T? selected;

  /// 없으면 못 고르는 중(데스크톱에 보내는 중 등) — 아이콘이 흐려진다.
  final ValueChanged<T>? onSelect;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      height: Look.tap,
      padding: const EdgeInsets.all(Look.choiceInset),
      decoration: ShapeDecoration(shape: const StadiumBorder(), color: scheme.surfaceContainerHigh),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          for (final (value, icon, label) in options)
            Tooltip(
              message: label,
              child: Semantics(
                label: label,
                selected: value == selected,
                button: true,
                child: InkWell(
                  customBorder: const StadiumBorder(),
                  onTap: onSelect == null ? null : () => onSelect!(value),
                  child: AnimatedContainer(
                    duration: const Duration(milliseconds: 180),
                    width: Look.tap,
                    decoration: ShapeDecoration(
                      shape: const StadiumBorder(),
                      color: value == selected ? scheme.primary.withValues(alpha: 0.18) : Colors.transparent,
                    ),
                    child: Icon(
                      icon,
                      size: Look.iconSize,
                      color: value == selected
                          ? scheme.primary
                          : scheme.onSurfaceVariant.withValues(alpha: onSelect == null ? 0.5 : 1),
                    ),
                  ),
                ),
              ),
            ),
        ],
      ),
    );
  }
}

/// 목록을 못 받았을 때 맨 위 띠 — `danger` 12% 바탕 + 위험색 글, 오른쪽 「다시 시도」.
class ErrorBand extends StatelessWidget {
  const ErrorBand({super.key, required this.text, this.onRetry});

  final String text;
  final VoidCallback? onRetry;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Container(
      margin: const EdgeInsets.only(top: Look.cardGap),
      constraints: const BoxConstraints(minHeight: Look.tap),
      padding: EdgeInsets.only(left: Look.cardPad, right: onRetry == null ? Look.cardPad : 0),
      decoration: BoxDecoration(color: scheme.errorContainer, borderRadius: Look.corners),
      child: Row(
        children: [
          Expanded(
            child: Text(text, style: theme.textTheme.bodySmall?.copyWith(color: scheme.error)),
          ),
          if (onRetry != null)
            TextButton(
              onPressed: onRetry,
              style: TextButton.styleFrom(foregroundColor: scheme.error),
              child: const Text('다시 시도'),
            ),
        ],
      ),
    );
  }
}
