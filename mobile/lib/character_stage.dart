import 'package:flutter/material.dart';

import 'student_art.dart';

/// Live2D 가 설 자리. 아직 폰에서 Live2D 를 못 돌리므로(SDK·라이선스 확인 전, 조사는
/// `live2d-spike` 기록) 같은 자리·같은 크기에 기존 캐릭터 그림을 세운다. 나중에 Live2D 가
/// 되면 이 위젯 안만 바뀌고 부르는 쪽은 그대로다.
///
/// 동작 줄이기를 켠 사람에게는 움직이는 그림 대신 정지한 얼굴을 보인다.
class CharacterStage extends StatelessWidget {
  const CharacterStage({super.key, required this.slug, this.size = 56});

  final String slug;
  final double size;

  /// 폰에서 Live2D 를 실제로 돌리는가. 거짓인 동안은 그림으로 대신한다.
  static const live2d = false;

  @override
  Widget build(BuildContext context) {
    final still = MediaQuery.maybeDisableAnimationsOf(context) ?? false;
    return Semantics(
      label: '캐릭터 그림',
      child: SizedBox.square(
        dimension: size,
        child: still
            ? StudentFace(slug: slug, size: size)
            : StudentSprite(slug: slug, size: size),
      ),
    );
  }
}
