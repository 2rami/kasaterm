import 'package:flutter/material.dart';

import 'look.dart';

/// 대화 속 결과 카드(그림·음성·영상·PDF·웹·파일)의 가림. 그림·영상·PDF·파일은 가린 채, 음성·웹은 보인 채 서고
/// ([open]), 눈·띠로 이 실행 동안 뒤집는다 — PC `card_eye` 와 같은 규칙. 같은 카드는 어느 대화에서든 같이 바뀐다.
abstract final class InlineCardVisibility {
  /// 처음 모습에서 뒤집은 카드.
  static final flipped = ValueNotifier<Set<String>>(const {});

  /// 카드 모양을 보는 옛 시험이 쓰는 스위치 — 켜면 모든 카드가 보인 채로 선다.
  @visibleForTesting
  static bool showAll = false;

  /// [open] — 처음부터 보이는 카드(음성·웹). 가릴 큰 그림이 없어 가리면 누름만 한 번 늘었다.
  static bool isShown(String key, {bool open = false}) =>
      showAll || open != flipped.value.contains(key);

  static void toggle(String key) {
    final next = {...flipped.value};
    if (!next.remove(key)) next.add(key);
    flipped.value = next;
  }
}

/// 가린 카드의 한 줄 — 어디를 눌러도 보인다.
class HiddenCardBar extends StatelessWidget {
  const HiddenCardBar({
    super.key,
    required this.icon,
    required this.title,
    required this.caption,
    required this.onShow,
  });
  final IconData icon;
  final String title;
  final String caption;
  final VoidCallback onShow;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Semantics(
      button: true,
      label: '$title, $caption, 눌러서 보기',
      excludeSemantics: true,
      child: Material(
        color: scheme.onSurface.withValues(alpha: Look.mediaTint),
        borderRadius: Look.corners,
        clipBehavior: Clip.antiAlias,
        child: InkWell(
          onTap: onShow,
          child: ConstrainedBox(
            constraints: const BoxConstraints(
              minHeight: Look.tap + Look.mediaPad,
            ),
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: Look.fieldGap),
              child: Row(
                children: [
                  Icon(icon, size: 20, color: scheme.onSurfaceVariant),
                  const SizedBox(width: Look.fieldGap),
                  Expanded(
                    child: Column(
                      mainAxisSize: MainAxisSize.min,
                      crossAxisAlignment: CrossAxisAlignment.stretch,
                      children: [
                        Text(
                          title,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: TextStyle(
                            fontSize: Look.sub,
                            fontWeight: FontWeight.w600,
                            height: 1.3,
                            color: scheme.onSurface,
                          ),
                        ),
                        const SizedBox(height: Look.rowGap),
                        Text(
                          caption,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: TextStyle(
                            fontSize: Look.sub,
                            height: 1.3,
                            color: scheme.onSurfaceVariant,
                          ),
                        ),
                      ],
                    ),
                  ),
                  Icon(
                    Icons.visibility_off_outlined,
                    size: 20,
                    color: scheme.onSurfaceVariant,
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// 보이는 카드 오른쪽 위의 눈 — 누르면 다시 가린다. 그림 위에서도 보이게 바탕을 깐다.
class CardEye extends StatelessWidget {
  const CardEye({super.key, required this.child, required this.onHide});
  final Widget child;
  final VoidCallback onHide;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Stack(
      children: [
        child,
        Positioned(
          top: 4,
          right: 4,
          child: Semantics(
            button: true,
            label: '가리기',
            child: Material(
              color: scheme.surface.withValues(alpha: 0.85),
              shape: const CircleBorder(),
              clipBehavior: Clip.antiAlias,
              child: InkWell(
                onTap: onHide,
                child: SizedBox.square(
                  dimension: Look.tap,
                  child: Icon(
                    Icons.visibility_outlined,
                    size: 20,
                    color: scheme.onSurface,
                  ),
                ),
              ),
            ),
          ),
        ),
      ],
    );
  }
}
